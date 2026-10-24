//! Turning a material pass into a compiled shader plus its bindings.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use glam::Vec2;
use wpe_assets::material::MaterialPass;
use wpe_assets::tex::{TexFlags, TexFormat, Texture};
use wpe_shader::{CompiledShader, ShaderError, ShaderRequest, TexSlot};
use wpe_vfs::Vfs;

/// Parsed `.tex` files by material texture name; `None` caches a failure.
#[derive(Default)]
pub struct TextureStore {
    textures: HashMap<String, Option<Arc<Texture>>>,
}

/// Path of the `.tex` a material texture name refers to.
fn texture_path(vfs: &Vfs, name: &str) -> String {
    if name.ends_with(".tex") && vfs.contains(name) {
        name.to_owned()
    } else {
        format!("materials/{name}.tex")
    }
}

impl TextureStore {
    pub fn get(&mut self, vfs: &Vfs, name: &str) -> Option<Arc<Texture>> {
        self.textures
            .entry(name.to_owned())
            .or_insert_with(|| {
                let path = texture_path(vfs, name);
                let parsed = vfs
                    .read(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|b| Texture::parse(&b).map_err(|e| e.to_string()));
                parsed
                    .inspect_err(|e| {
                        // `_alias_*` names are placeholders WE resolves to white.
                        if !name.starts_with("_alias_") {
                            tracing::warn!("texture {name}: {e}");
                        }
                    })
                    .ok()
                    .map(Arc::new)
            })
            .clone()
    }

    /// Picture size (or first sprite frame size) of a texture.
    pub fn size(&mut self, vfs: &Vfs, name: &str) -> Option<Vec2> {
        let t = self.get(vfs, name)?;
        Some(match t.sprite_frames.first() {
            Some(f) if t.is_sprite() => Vec2::new(f.width, f.height),
            _ => Vec2::new(t.map_width as f32, t.map_height as f32),
        })
    }
}

/// What a texture slot is bound to.
#[derive(Debug, Clone)]
pub enum SlotTexture {
    /// A `.tex` asset.
    Asset(Arc<Texture>),
    /// A named render target (`_rt_*`), resolved by the renderer.
    RenderTarget(String),
}

/// A material pass ready to draw: compiled program, slot textures and
/// constant values keyed by uniform name.
pub struct PreparedMaterial {
    pub shader: CompiledShader,
    pub textures: Vec<Option<SlotTexture>>,
    /// Shader defaults overlaid with the material's constants.
    pub constants: HashMap<String, Vec<f32>>,
    /// Constants with keyframe animations: uniform, base value, animation.
    pub animated: Vec<(String, Vec<f32>, wpe_scene::Animation)>,
}

/// `TEX<N>FORMAT` value `common_fragment.h` compares a slot against.
fn format_define(format: TexFormat) -> Option<&'static str> {
    Some(match format {
        TexFormat::Rgba8 => "FORMAT_RGBA8888",
        TexFormat::Rgb565 => "FORMAT_RGB565",
        TexFormat::Bc1 => "FORMAT_DXT1",
        TexFormat::Bc2 => "FORMAT_DXT3",
        TexFormat::Bc3 => "FORMAT_DXT5",
        TexFormat::Bc7 => "FORMAT_BC7",
        TexFormat::Rg8 => "FORMAT_RG88",
        TexFormat::R8 => "FORMAT_R8",
        TexFormat::Rg16f => "FORMAT_RG1616F",
        TexFormat::R16f => "FORMAT_R16F",
        TexFormat::Rgba16f | TexFormat::Bc6h | TexFormat::Rgba1010102 => return None,
    })
}

fn slot_info(tex: Option<&SlotTexture>) -> TexSlot {
    match tex {
        None => TexSlot::default(),
        Some(SlotTexture::RenderTarget(_)) => TexSlot {
            enabled: true,
            components: [false; 3],
        },
        Some(SlotTexture::Asset(t)) => TexSlot {
            enabled: true,
            components: [TexFlags::COMPO1, TexFlags::COMPO2, TexFlags::COMPO3]
                .map(|f| t.flags.contains(f)),
        },
    }
}

fn resolve(vfs: &Vfs, store: &mut TextureStore, name: &str) -> Option<SlotTexture> {
    if name.starts_with("_rt_") {
        Some(SlotTexture::RenderTarget(name.to_owned()))
    } else {
        store.get(vfs, name).map(SlotTexture::Asset)
    }
}

/// Resolve and compile `pass`. `extra_combos` override the material's own.
pub fn prepare(
    vfs: &Vfs,
    store: &mut TextureStore,
    pass: &MaterialPass,
    extra_combos: &BTreeMap<String, String>,
) -> Result<PreparedMaterial, ShaderError> {
    let mut textures: Vec<Option<SlotTexture>> = pass
        .textures
        .iter()
        .map(|t| {
            t.as_deref()
                .filter(|n| !n.is_empty())
                .and_then(|n| resolve(vfs, store, n))
        })
        .collect();

    let slots: Vec<TexSlot> = textures.iter().map(|t| slot_info(t.as_ref())).collect();
    let info = wpe_shader::annotations(vfs, &pass.shader, &slots)?;
    for (&slot, name) in &info.default_textures {
        if textures.len() <= slot {
            textures.resize(slot + 1, None);
        }
        if textures[slot].is_none() {
            textures[slot] = resolve(vfs, store, name);
        }
    }

    let slots: Vec<TexSlot> = textures.iter().map(|t| slot_info(t.as_ref())).collect();
    let formats: Vec<Option<&str>> = textures
        .iter()
        .map(|t| match t {
            Some(SlotTexture::Asset(t)) => format_define(t.format),
            _ => None,
        })
        .collect();
    let mut combos: BTreeMap<String, String> = pass
        .combos
        .iter()
        .map(|(k, v)| (k.to_ascii_uppercase(), v.0.to_string()))
        .collect();
    combos.extend(extra_combos.iter().map(|(k, v)| (k.clone(), v.clone())));

    let shader = wpe_shader::compile(
        vfs,
        ShaderRequest {
            name: &pass.shader,
            combos: &combos,
            slots: &slots,
            formats: &formats,
        },
    )?;

    let mut constants = shader.info.defaults.clone();
    let mut animated = Vec::new();
    for (key, value) in &pass.constantshadervalues {
        match shader.info.uniform_for_constant(key) {
            Some(uniform) => {
                if let Some(anim) = value
                    .animation
                    .as_ref()
                    .and_then(wpe_scene::Animation::parse)
                {
                    animated.push((uniform.clone(), value.value.0.clone(), anim));
                }
                constants.insert(uniform, value.value.0.clone());
            }
            None => tracing::debug!("material constant {key} matches no uniform"),
        }
    }
    Ok(PreparedMaterial {
        shader,
        textures,
        constants,
        animated,
    })
}
