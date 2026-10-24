//! Turning resolved scene layers into render targets and frame steps,
//! including per-layer image-effect chains.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use glam::{Mat4, Vec2, Vec4};
use wpe_assets::Effect;
use wpe_assets::effect::{CommandKind, EffectPass};
use wpe_assets::material::{Blending, Material, MaterialPass, PassOverride};
use wpe_assets::tex::Texture;
use wpe_render::{
    BlendMode, Camera, CardMesh, DEFAULT_FORMAT, Draw, Gpu, GpuTexture, ImageDraw, MaterialDraw,
    ShaderProgram, Step, TargetDesc, TargetId, TargetSize, TexRef, upload_texture, wgpu,
};
use wpe_scene::{ImageLayer, apply_override};
use wpe_vfs::Vfs;

use crate::material::{self, SlotTexture, TextureStore};

/// Names under which the current chain input and framebuffer copies appear
/// in material texture slots.
const PREVIOUS: &str = "_rt_previous";
const FULL_FRAME: &str = "_rt_FullFrameBuffer";
const MIPMAPPED_FRAME: &str = "_rt_MipMappedFrameBuffer";

pub fn blend_mode(b: &Blending) -> BlendMode {
    match b {
        Blending::Normal | Blending::Disabled => BlendMode::Normal,
        Blending::Additive => BlendMode::Additive,
        Blending::TranslucentPa => BlendMode::TranslucentPremultiplied,
        _ => BlendMode::Translucent,
    }
}

fn target_format(format: Option<&str>) -> wgpu::TextureFormat {
    match format.map(str::to_ascii_lowercase).as_deref() {
        Some("r8") => wgpu::TextureFormat::R8Unorm,
        Some("rg88") => wgpu::TextureFormat::Rg8Unorm,
        Some("rgba16161616f" | "rgba16f") => wgpu::TextureFormat::Rgba16Float,
        Some("r16f") => wgpu::TextureFormat::R16Float,
        Some("rg1616f" | "rg16f") => wgpu::TextureFormat::Rg16Float,
        _ => DEFAULT_FORMAT,
    }
}

/// A material pass resolved against textures and render targets.
struct Resolved {
    program: Arc<ShaderProgram>,
    textures: Vec<Option<TexRef>>,
    constants: HashMap<String, Vec<f32>>,
    /// Some slot reads a copy of `_rt_default` made right before the draw.
    reads_frame: bool,
    uv_scale: Vec2,
}

/// Shader programs by shader name and final combos; `None` caches a failure.
type ProgramCache = HashMap<String, Option<Arc<ShaderProgram>>>;

pub struct FrameBuilder<'a> {
    gpu: &'a Gpu,
    vfs: &'a Vfs,
    store: TextureStore,
    uploaded: HashMap<usize, Arc<GpuTexture>>,
    programs: ProgramCache,
    white: Arc<GpuTexture>,
    targets: Vec<TargetDesc>,
    steps: Vec<Step>,
    frame_copy: Option<TargetId>,
    /// Draws using their compiled material, and all draws.
    pub stats: (usize, usize),
}

impl<'a> FrameBuilder<'a> {
    pub fn new(gpu: &'a Gpu, vfs: &'a Vfs, store: TextureStore, white: Texture) -> Self {
        Self {
            gpu,
            vfs,
            store,
            uploaded: HashMap::new(),
            programs: HashMap::new(),
            white: Arc::new(upload_texture(gpu, &white, "white")),
            targets: Vec::new(),
            steps: Vec::new(),
            frame_copy: None,
            stats: (0, 0),
        }
    }

    pub fn finish(self) -> (Vec<TargetDesc>, Vec<Step>) {
        (self.targets, self.steps)
    }

    fn target(&mut self, name: String, size: TargetSize, format: wgpu::TextureFormat) -> TargetId {
        self.targets.push(TargetDesc { name, size, format });
        TargetId(self.targets.len())
    }

    fn frame_copy(&mut self) -> TargetId {
        if let Some(id) = self.frame_copy {
            return id;
        }
        let id = self.target(
            FULL_FRAME.to_owned(),
            TargetSize::Screen { divisor: 1.0 },
            DEFAULT_FORMAT,
        );
        self.frame_copy = Some(id);
        id
    }

    fn upload(&mut self, tex: &Arc<Texture>, label: &str) -> Arc<GpuTexture> {
        let key = Arc::as_ptr(tex) as usize;
        self.uploaded
            .entry(key)
            .or_insert_with(|| Arc::new(upload_texture(self.gpu, tex, label)))
            .clone()
    }

    fn program(
        &mut self,
        shader: &wpe_shader::CompiledShader,
        name: &str,
    ) -> Option<Arc<ShaderProgram>> {
        let key = format!("{name}{:?}", shader.combos);
        self.programs
            .entry(key)
            .or_insert_with(|| {
                ShaderProgram::new(self.gpu, name, shader)
                    .inspect_err(|e| tracing::warn!("shader {name}: {e}"))
                    .ok()
                    .map(Arc::new)
            })
            .clone()
    }

    /// Compile `pass` and resolve its textures; `targets` maps render-target
    /// names (effect FBOs, the chain input) to targets.
    fn resolve(
        &mut self,
        label: &str,
        pass: &MaterialPass,
        targets: &HashMap<String, TargetId>,
    ) -> Option<Resolved> {
        let prepared = material::prepare(self.vfs, &mut self.store, pass, &BTreeMap::new())
            .inspect_err(|e| tracing::warn!("{label} ({}): {e}", pass.shader))
            .ok()?;
        let program = self.program(&prepared.shader, &pass.shader)?;
        let mut reads_frame = false;
        let mut textures = Vec::with_capacity(prepared.textures.len());
        for (slot, tex) in prepared.textures.iter().enumerate() {
            textures.push(match tex {
                Some(SlotTexture::Asset(t)) => Some(TexRef::Asset(
                    self.upload(t, &format!("{label} slot {slot}")),
                )),
                Some(SlotTexture::RenderTarget(name)) => {
                    if let Some(&id) = targets.get(name) {
                        Some(TexRef::Target(id))
                    } else if name == FULL_FRAME || name == MIPMAPPED_FRAME {
                        reads_frame = true;
                        Some(TexRef::Target(self.frame_copy()))
                    } else {
                        tracing::debug!("{label}: render target {name} not supported yet");
                        None
                    }
                }
                None => None,
            });
        }
        let uv_scale = match textures.first() {
            Some(Some(TexRef::Asset(t))) => Vec2::from_array(t.uv_scale()),
            _ => Vec2::ONE,
        };
        Some(Resolved {
            program,
            textures,
            constants: prepared.constants,
            reads_frame,
            uv_scale,
        })
    }

    fn push_draw(
        &mut self,
        target: TargetId,
        resolved: Resolved,
        model: Mat4,
        mesh: CardMesh,
        blend: BlendMode,
        camera: Camera,
    ) {
        if resolved.reads_frame {
            let copy = self.frame_copy();
            self.steps.push(Step::Copy {
                from: TargetId::DEFAULT,
                to: copy,
            });
        }
        self.stats.0 += 1;
        self.stats.1 += 1;
        self.steps.push(Step::Draw {
            target,
            draw: Draw::Material(MaterialDraw {
                program: resolved.program,
                textures: resolved.textures,
                constants: resolved.constants,
                model,
                mesh,
                blend,
                camera,
            }),
        });
    }

    pub fn layer(&mut self, layer: &ImageLayer) {
        let effects = self.effects(layer);
        if effects.is_empty() {
            if !layer.compose {
                self.plain_layer(layer);
            }
            return;
        }
        self.effect_chain(layer, &effects);
    }

    /// Visible effects, plus the synthetic one implementing `colorBlendMode`.
    fn effects(&self, layer: &ImageLayer) -> Vec<(Effect, Vec<PassOverride>)> {
        let mut effects: Vec<(Effect, Vec<PassOverride>)> = layer
            .effects
            .iter()
            .filter(|fx| fx.visible.value)
            .filter_map(|fx| {
                wpe_assets::load_json::<Effect>(self.vfs, &fx.file)
                    .inspect_err(|e| tracing::warn!("layer {:?} effect: {e}", layer.name))
                    .ok()
                    .map(|e| (e, fx.passes.clone()))
            })
            .collect();
        if !matches!(layer.color_blend_mode, 0 | 7 | 9) {
            let mut settings = PassOverride::default();
            settings.combos.insert(
                "BLENDMODE".to_owned(),
                wpe_assets::value::LenientI32(layer.color_blend_mode),
            );
            settings
                .combos
                .insert("BONECOUNT".to_owned(), wpe_assets::value::LenientI32(1));
            let effect = Effect {
                passes: vec![EffectPass::Material(
                    wpe_assets::effect::EffectMaterialPass {
                        material: "materials/util/effectpassthrough.json".to_owned(),
                        settings,
                        compose: false,
                    },
                )],
                ..Default::default()
            };
            effects.push((effect, Vec::new()));
        }
        effects
    }

    fn layer_constants(
        layer: &ImageLayer,
        constants: &mut HashMap<String, Vec<f32>>,
        neutral_color: bool,
    ) {
        let c = layer.color;
        let color4 = if neutral_color {
            vec![1.0; 4]
        } else {
            vec![c.x, c.y, c.z, layer.alpha]
        };
        for (name, value) in [
            ("g_Color4", color4),
            ("g_Color", vec![c.x, c.y, c.z]),
            ("g_UserAlpha", vec![layer.alpha]),
            ("g_Alpha", vec![layer.alpha]),
            ("g_Brightness", vec![layer.brightness]),
        ] {
            constants.insert(name.to_owned(), value);
        }
    }

    fn final_blend(layer: &ImageLayer) -> BlendMode {
        match layer.color_blend_mode {
            7 => BlendMode::Normal,
            9 => BlendMode::Additive,
            _ => blend_mode(&layer.material.blending),
        }
    }

    fn plain_layer(&mut self, layer: &ImageLayer) {
        let label = format!("layer {:?}", layer.name);
        match self.resolve(&label, &layer.material, &HashMap::new()) {
            Some(mut r) => {
                Self::layer_constants(layer, &mut r.constants, false);
                let mesh = CardMesh {
                    size: layer.size,
                    uv_scale: r.uv_scale,
                };
                self.push_draw(
                    TargetId::DEFAULT,
                    r,
                    layer.transform,
                    mesh,
                    Self::final_blend(layer),
                    Camera::Scene,
                );
            }
            None => self.fallback(layer),
        }
    }

    /// The built-in stand-in shader, for layers whose material failed.
    fn fallback(&mut self, layer: &ImageLayer) {
        let texture = match layer.texture(0) {
            Some(name) if !name.starts_with("_rt_") => match self.store.get(self.vfs, name) {
                Some(tex) => self.upload(&tex, name),
                None => return,
            },
            None if layer.solid => self.white.clone(),
            _ => return,
        };
        self.stats.1 += 1;
        self.steps.push(Step::Draw {
            target: TargetId::DEFAULT,
            draw: Draw::Image(ImageDraw {
                model: layer.transform,
                size: layer.size,
                color: Vec4::from((layer.color * layer.brightness, layer.alpha)),
                blend: blend_mode(&layer.material.blending),
                texture,
            }),
        });
    }

    fn effect_chain(&mut self, layer: &ImageLayer, effects: &[(Effect, Vec<PassOverride>)]) {
        let id = layer.id;
        let w = layer.size.x.round().max(1.0) as u32;
        let h = layer.size.y.round().max(1.0) as u32;
        let base_size = if layer.fullscreen {
            TargetSize::Screen { divisor: 1.0 }
        } else {
            TargetSize::Fixed(w, h)
        };
        let mut prev = self.target(format!("_rt_pingpong_a_{id}"), base_size, DEFAULT_FORMAT);
        let mut out = self.target(format!("_rt_pingpong_b_{id}"), base_size, DEFAULT_FORMAT);

        // The layer's own image, untransformed, filling the first buffer.
        let label = format!("layer {:?}", layer.name);
        let Some(mut base) = self.resolve(&label, &layer.material, &HashMap::new()) else {
            return self.fallback(layer);
        };
        Self::layer_constants(layer, &mut base.constants, false);
        let (camera, model, mesh) = if layer.fullscreen {
            (
                Camera::Fixed(Mat4::IDENTITY),
                Mat4::IDENTITY,
                CardMesh::FULLSCREEN,
            )
        } else if layer.compose {
            let mesh = CardMesh {
                size: layer.size,
                uv_scale: Vec2::ONE,
            };
            (Camera::Scene, layer.transform, mesh)
        } else {
            let (hw, hh) = (layer.size.x / 2.0, layer.size.y / 2.0);
            let ortho = glam::camera::rh::proj::directx::orthographic(-hw, hw, -hh, hh, -1e4, 1e4);
            let mesh = CardMesh {
                size: layer.size,
                uv_scale: base.uv_scale,
            };
            (Camera::Fixed(ortho), Mat4::IDENTITY, mesh)
        };
        let base_blend = blend_mode(&layer.material.blending);
        self.push_draw(prev, base, model, mesh, base_blend, camera);

        let mut last_output: Option<usize> = None;
        for (effect, overrides) in effects {
            let mut names: HashMap<String, TargetId> = HashMap::new();
            names.insert(PREVIOUS.to_owned(), prev);
            for fbo in &effect.fbos {
                let divisor = fbo.scale.max(1);
                let size = match base_size {
                    TargetSize::Fixed(..) => {
                        TargetSize::Fixed((w / divisor).max(1), (h / divisor).max(1))
                    }
                    TargetSize::Screen { .. } => TargetSize::Screen {
                        divisor: divisor as f32,
                    },
                };
                let t = self.target(
                    format!("_rt_{}_{id}", fbo.name),
                    size,
                    target_format(fbo.format.as_deref()),
                );
                names.insert(format!("_rt_{}", fbo.name), t);
            }
            let fbo = |name: &str| {
                if name == "previous" {
                    PREVIOUS.to_owned()
                } else {
                    format!("_rt_{name}")
                }
            };

            let mut material_index = 0;
            for pass in &effect.passes {
                match pass {
                    EffectPass::Command(cmd) => {
                        let (Some(&from), Some(&to)) =
                            (names.get(&fbo(&cmd.source)), names.get(&fbo(&cmd.target)))
                        else {
                            tracing::warn!("effect {}: unknown command buffers", effect.name);
                            continue;
                        };
                        match cmd.command {
                            CommandKind::Copy => self.steps.push(Step::Copy { from, to }),
                            _ => tracing::warn!(
                                "effect {}: swap commands not supported",
                                effect.name
                            ),
                        }
                    }
                    EffectPass::Material(mp) => {
                        let Ok(material) =
                            wpe_assets::load_json::<Material>(self.vfs, &mp.material)
                        else {
                            tracing::warn!(
                                "effect {}: material {} missing",
                                effect.name,
                                mp.material
                            );
                            continue;
                        };
                        let Some(mut pass) = material.passes.into_iter().next() else {
                            continue;
                        };
                        apply_override(&mut pass, &mp.settings);
                        if let Some(over) = overrides.get(material_index) {
                            apply_override(&mut pass, over);
                        }
                        material_index += 1;
                        for bind in &mp.settings.bind {
                            let slot = bind.index.max(0) as usize;
                            if pass.textures.len() <= slot {
                                pass.textures.resize(slot + 1, None);
                            }
                            pass.textures[slot] = Some(fbo(&bind.name));
                        }
                        if pass.textures.is_empty() {
                            pass.textures.push(None);
                        }
                        let own_input = pass.textures[0].as_deref().is_none_or(|t| {
                            t.is_empty() || t.starts_with("_rt_imageLayerComposite_")
                        });
                        if own_input {
                            pass.textures[0] = Some(PREVIOUS.to_owned());
                        }

                        let target = mp
                            .settings
                            .target
                            .as_deref()
                            .and_then(|t| names.get(&fbo(t)).copied());
                        let Some(mut r) =
                            self.resolve(&format!("{label} {}", effect.name), &pass, &names)
                        else {
                            continue;
                        };
                        let neutral = pass.combos.contains_key("BLENDMODE");
                        Self::layer_constants(layer, &mut r.constants, neutral);
                        let is_output = target.is_none();
                        self.push_draw(
                            target.unwrap_or(out),
                            r,
                            Mat4::IDENTITY,
                            CardMesh::FULLSCREEN,
                            BlendMode::Normal,
                            Camera::Fixed(Mat4::IDENTITY),
                        );
                        if is_output {
                            last_output = Some(self.steps.len() - 1);
                        }
                    }
                }
            }
            std::mem::swap(&mut prev, &mut out);
        }

        // The last pass writing the chain output composites onto the screen.
        let Some(index) = last_output else { return };
        let Step::Draw {
            target,
            draw: Draw::Material(draw),
        } = &mut self.steps[index]
        else {
            return;
        };
        *target = TargetId::DEFAULT;
        draw.blend = Self::final_blend(layer);
        if !layer.fullscreen {
            draw.camera = Camera::Scene;
            draw.model = layer.transform;
            draw.mesh = CardMesh {
                size: layer.size,
                uv_scale: Vec2::ONE,
            };
        }
    }
}
