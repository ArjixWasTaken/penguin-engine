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
    ShaderProgram, SpriteAnimation, SpriteFrame, Step, TargetDesc, TargetId, TargetSize, TexRef,
    upload_texture, wgpu,
};
use wpe_scene::{Bloom, ImageLayer, apply_override};
use wpe_vfs::Vfs;

use crate::material::{self, SlotTexture, TextureStore};
use crate::video::VideoStream;

/// A built frame: render targets, steps and the video streams to feed.
pub struct Frame {
    pub targets: Vec<TargetDesc>,
    pub steps: Vec<Step>,
    pub videos: Vec<(Arc<GpuTexture>, VideoStream)>,
}

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
    sprites: Vec<SpriteAnimation>,
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
    /// Offscreen outputs of layers other layers sample, by layer id.
    links: HashMap<i32, TargetId>,
    /// Sprite-sheet pages after the first, by texture and page index.
    pages: HashMap<(usize, usize), Arc<GpuTexture>>,
    /// Video textures and the streams feeding them.
    videos: Vec<(Arc<GpuTexture>, VideoStream)>,
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
            links: HashMap::new(),
            videos: Vec::new(),
            pages: HashMap::new(),
            stats: (0, 0),
        }
    }

    pub fn finish(self) -> Frame {
        Frame {
            targets: self.targets,
            steps: self.steps,
            videos: self.videos,
        }
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
        if let Some(t) = self.uploaded.get(&key) {
            return t.clone();
        }
        let gpu_tex = match &tex.video {
            Some(mp4) => {
                // Only the first mip is refreshed per frame, so drop the rest.
                let mut single = (**tex).clone();
                for image in &mut single.images {
                    image.mips.truncate(1);
                }
                let gpu_tex = Arc::new(upload_texture(self.gpu, &single, label));
                let size = gpu_tex.texture.size();
                match VideoStream::start(mp4, size.width, size.height) {
                    Ok(stream) => self.videos.push((gpu_tex.clone(), stream)),
                    Err(e) => tracing::warn!("{label}: video playback unavailable: {e}"),
                }
                gpu_tex
            }
            None => Arc::new(upload_texture(self.gpu, tex, label)),
        };
        self.uploaded.insert(key, gpu_tex.clone());
        gpu_tex
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
        let mut sprites = Vec::new();
        let mut textures = Vec::with_capacity(prepared.textures.len());
        for (slot, tex) in prepared.textures.iter().enumerate() {
            textures.push(match tex {
                Some(SlotTexture::Asset(t)) => {
                    let label = format!("{label} slot {slot}");
                    if t.is_sprite() {
                        sprites.push(self.sprite(t, slot, &label));
                    }
                    Some(TexRef::Asset(self.upload(t, &label)))
                }
                Some(SlotTexture::RenderTarget(name)) => {
                    let link = link_id(name).and_then(|id| self.links.get(&id).copied());
                    if let Some(&id) = targets.get(name) {
                        Some(TexRef::Target(id))
                    } else if let Some(id) = link {
                        Some(TexRef::Target(id))
                    } else if name == FULL_FRAME
                        && pass.shader == "genericimage2"
                        && !pass.combos.contains_key("BLENDMODE")
                    {
                        None
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
            Some(Some(TexRef::Asset(t))) if !sprites.iter().any(|s| s.slot == 0) => {
                Vec2::from_array(t.uv_scale())
            }
            _ => Vec2::ONE,
        };
        Some(Resolved {
            program,
            textures,
            constants: prepared.constants,
            reads_frame,
            uv_scale,
            sprites,
        })
    }

    /// A sprite sheet's frames, with one texture per page.
    fn sprite(&mut self, tex: &Arc<Texture>, slot: usize, label: &str) -> SpriteAnimation {
        let key = Arc::as_ptr(tex) as usize;
        let pages = (0..tex.images.len())
            .map(|page| {
                if page == 0 {
                    return self.upload(tex, label);
                }
                self.pages
                    .entry((key, page))
                    .or_insert_with(|| {
                        let mut single = (**tex).clone();
                        single.images = vec![tex.images[page].clone()];
                        Arc::new(upload_texture(
                            self.gpu,
                            &single,
                            &format!("{label} page {page}"),
                        ))
                    })
                    .clone()
            })
            .collect();
        let frames = tex
            .sprite_frames
            .iter()
            .map(|f| SpriteFrame {
                page: f.image as usize,
                duration: f.frametime,
                rotation: [f.x_axis[0], f.x_axis[1], f.y_axis[0], f.y_axis[1]],
                translation: [f.x, f.y],
            })
            .collect();
        SpriteAnimation {
            slot,
            frames,
            pages,
        }
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
                sprites: resolved.sprites,
            }),
        });
    }

    /// Add every layer: first the hidden or visible layers other layers
    /// sample, rendered offscreen, then the visible ones onto the screen.
    pub fn layers(&mut self, layers: &[ImageLayer]) {
        let mut referenced: Vec<i32> = layers.iter().flat_map(linked_ids).collect();
        referenced.sort_unstable();
        referenced.dedup();
        for id in referenced {
            let Some(layer) = layers.iter().find(|l| l.id == id) else {
                continue;
            };
            let w = layer.size.x.round().max(1.0) as u32;
            let h = layer.size.y.round().max(1.0) as u32;
            let link = self.target(
                format!("_rt_link_{id}"),
                TargetSize::Fixed(w, h),
                DEFAULT_FORMAT,
            );
            self.layer(layer, Some(link));
            self.links.insert(id, link);
        }
        for layer in layers.iter().filter(|l| l.visible) {
            self.layer(layer, None);
        }
    }

    /// Scene bloom: bright-pass and blur a copy of the frame at quarter and
    /// eighth size, then add it back over `_rt_default`.
    pub fn bloom(&mut self, bloom: Bloom) {
        let screen = |divisor| TargetSize::Screen { divisor };
        let scene = self.target("_rt_Bloom_Scene".to_owned(), screen(1.0), DEFAULT_FORMAT);
        let quarter = self.target("_rt_Bloom_Quarter".to_owned(), screen(4.0), DEFAULT_FORMAT);
        let eighth = self.target("_rt_Bloom_Eighth".to_owned(), screen(8.0), DEFAULT_FORMAT);
        let result = self.target("_rt_Bloom_Result".to_owned(), screen(8.0), DEFAULT_FORMAT);
        let names: HashMap<String, TargetId> = [
            ("_rt_Bloom_Scene", scene),
            ("_rt_Bloom_Quarter", quarter),
            ("_rt_Bloom_Eighth", eighth),
            ("_rt_Bloom_Result", result),
        ]
        .into_iter()
        .map(|(n, t)| (n.to_owned(), t))
        .collect();
        self.steps.push(Step::Copy {
            from: TargetId::DEFAULT,
            to: scene,
        });
        let constant =
            |v: Vec<f32>| wpe_assets::value::Dynamic::new(wpe_assets::value::FloatVec(v));
        let passes: [(&str, &[&str], TargetId); 4] = [
            ("downsample_quarter_bloom", &["_rt_Bloom_Scene"], quarter),
            ("downsample_eighth_blur_v", &["_rt_Bloom_Quarter"], eighth),
            ("blur_h_bloom", &["_rt_Bloom_Eighth"], result),
            (
                "combine",
                &["_rt_Bloom_Scene", "_rt_Bloom_Result"],
                TargetId::DEFAULT,
            ),
        ];
        for (shader, inputs, target) in passes {
            let mut pass = MaterialPass {
                shader: shader.to_owned(),
                blending: Blending::Normal,
                textures: inputs.iter().map(|t| Some((*t).to_owned())).collect(),
                ..Default::default()
            };
            if shader == "downsample_quarter_bloom" {
                let c = &mut pass.constantshadervalues;
                c.insert("bloomstrength".to_owned(), constant(vec![bloom.strength]));
                c.insert("bloomthreshold".to_owned(), constant(vec![bloom.threshold]));
                c.insert(
                    "bloomtint".to_owned(),
                    constant(bloom.tint.to_array().to_vec()),
                );
            }
            let Some(r) = self.resolve(&format!("bloom {shader}"), &pass, &names) else {
                tracing::warn!("bloom disabled: {shader} failed");
                return;
            };
            self.push_draw(
                target,
                r,
                Mat4::IDENTITY,
                CardMesh::FULLSCREEN,
                BlendMode::Normal,
                Camera::Fixed(Mat4::IDENTITY),
            );
        }
    }

    /// Draw `layer` onto the screen, or into `offscreen` filling it.
    fn layer(&mut self, layer: &ImageLayer, offscreen: Option<TargetId>) {
        let effects = self.effects(layer);
        if effects.is_empty() {
            if !layer.compose {
                self.plain_layer(layer, offscreen);
            }
            return;
        }
        self.effect_chain(layer, &effects, offscreen);
    }

    /// Visible effects, plus the synthetic one implementing `colorBlendMode`.
    fn effects(&self, layer: &ImageLayer) -> Vec<(Effect, Vec<PassOverride>)> {
        let mut effects: Vec<(Effect, Vec<PassOverride>)> = layer
            .effects
            .iter()
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

    fn plain_layer(&mut self, layer: &ImageLayer, offscreen: Option<TargetId>) {
        let label = format!("layer {:?}", layer.name);
        match self.resolve(&label, &layer.material, &HashMap::new()) {
            Some(mut r) => {
                Self::layer_constants(layer, &mut r.constants, false);
                let mesh = CardMesh {
                    size: layer.size,
                    uv_scale: r.uv_scale,
                };
                match offscreen {
                    Some(target) => self.push_draw(
                        target,
                        r,
                        Mat4::IDENTITY,
                        mesh,
                        BlendMode::Normal,
                        Camera::Fixed(local_camera(layer.size)),
                    ),
                    None => self.push_draw(
                        TargetId::DEFAULT,
                        r,
                        layer.transform,
                        mesh,
                        Self::final_blend(layer),
                        Camera::Scene,
                    ),
                }
            }
            None if offscreen.is_none() => self.fallback(layer),
            None => {}
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

    fn effect_chain(
        &mut self,
        layer: &ImageLayer,
        effects: &[(Effect, Vec<PassOverride>)],
        offscreen: Option<TargetId>,
    ) {
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
            let ortho = local_camera(layer.size);
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

        // The last pass writing the chain output composites onto the screen
        // (or fills the layer's offscreen target).
        let Some(index) = last_output else { return };
        let Step::Draw {
            target,
            draw: Draw::Material(draw),
        } = &mut self.steps[index]
        else {
            return;
        };
        if let Some(offscreen) = offscreen {
            *target = offscreen;
            return;
        }
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

/// Orthographic camera framing a `size` card centred on the origin.
fn local_camera(size: Vec2) -> Mat4 {
    let (hw, hh) = (size.x / 2.0, size.y / 2.0);
    glam::camera::rh::proj::directx::orthographic(-hw, hw, -hh, hh, -1e4, 1e4)
}

const COMPOSITE_PREFIX: &str = "_rt_imageLayerComposite_";

/// Layer id of `_rt_imageLayerComposite_<id>[_a|_b]`.
fn link_id(name: &str) -> Option<i32> {
    let rest = name.strip_prefix(COMPOSITE_PREFIX)?;
    let digits = rest.split('_').next()?;
    digits.parse().ok()
}

/// Ids of other layers `layer` samples through composite textures.
fn linked_ids(layer: &ImageLayer) -> Vec<i32> {
    let overrides = layer.effects.iter().flat_map(|fx| &fx.passes);
    layer
        .material
        .textures
        .iter()
        .chain(overrides.flat_map(|o| &o.textures))
        .flatten()
        .filter_map(|t| link_id(t))
        .filter(|&id| id != layer.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_ids() {
        assert_eq!(link_id("_rt_imageLayerComposite_42_a"), Some(42));
        assert_eq!(link_id("_rt_imageLayerComposite_7"), Some(7));
        assert_eq!(link_id("_rt_FullFrameBuffer"), None);
    }
}
