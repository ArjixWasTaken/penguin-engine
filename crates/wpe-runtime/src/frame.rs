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
    Mesh, MeshData, ShaderProgram, SpriteAnimation, SpriteFrame, Step, TargetDesc, TargetId,
    TargetSize, TexRef, upload_texture, wgpu,
};
use wpe_scene::{Animation, Bloom, ImageLayer, Motion, Parallax, apply_override};
use wpe_vfs::Vfs;

use crate::material::{self, SlotTexture, TextureStore};
use crate::video::VideoStream;

/// A built frame: render targets, steps and what animates them.
pub struct Frame {
    pub targets: Vec<TargetDesc>,
    pub steps: Vec<Step>,
    /// The video each layer shows, by layer id, for scripts.
    pub layer_videos: HashMap<i32, Arc<VideoStream>>,
    /// Layers whose draws start switched off.
    pub hidden: Vec<i32>,
    pub bindings: Vec<Binding>,
    pub particles: Vec<ParticleInstance>,
    /// Draws of each on-screen layer, by layer id.
    pub layer_steps: HashMap<i32, LayerSteps>,
}

/// The draws an on-screen layer produced.
#[derive(Debug, Clone, Default)]
pub struct LayerSteps {
    pub all: Vec<usize>,
    /// Draws placed with the layer's transform.
    pub model: Vec<usize>,
    /// Draws carrying the layer's colour uniforms, and whether `g_Color4`
    /// stays white in them.
    pub color: Vec<(usize, bool)>,
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
    animated: Vec<(String, Vec<f32>, Animation)>,
}

/// A simulated particle object and the draws its subsystems feed.
pub struct ParticleInstance {
    system: wpe_particles::ParticleSystem,
    /// Subsystem index and the step drawing it.
    draws: Vec<(usize, usize)>,
    motion: Motion,
    /// Seconds simulated so far.
    simulated: f32,
}

/// Longest simulation step, so screenshots at a given time are reproducible.
const PARTICLE_STEP: f32 = 1.0 / 30.0;

impl ParticleInstance {
    /// Simulate up to `time` and upload each subsystem's mesh.
    pub fn advance(
        &mut self,
        renderer: &mut wpe_render::SceneRenderer,
        time: f32,
        cursor: Option<glam::Vec3>,
    ) {
        let ctx = wpe_particles::UpdateContext {
            transform: self.motion.transform_at(time),
            cursor,
            ..Default::default()
        };
        while self.simulated < time {
            let dt = (time - self.simulated).min(PARTICLE_STEP);
            self.system.update(dt, &ctx);
            self.simulated += dt;
        }
        let mut mesh = wpe_particles::ParticleMesh::default();
        for &(index, step) in &self.draws {
            let sub = &self.system.subsystems()[index];
            sub.mesh(self.system.view(), &mut mesh);
            renderer.set_model(step, sub.model_matrix());
            renderer.set_mesh(
                step,
                Mesh::Triangles(MeshData {
                    vertex_count: mesh.vertex_count,
                    attributes: std::mem::take(&mut mesh.attributes),
                    indices: Some(std::mem::take(&mut mesh.indices)),
                }),
            );
        }
    }
}

/// A puppet-warped layer: its parsed model and animation state.
pub struct LayerPuppet {
    mdl: Arc<wpe_mdl::Mdl>,
    puppet: wpe_mdl::Puppet,
}

impl LayerPuppet {
    fn combos(&self) -> BTreeMap<String, String> {
        let bones = self.puppet.bone_count();
        if bones == 0 {
            return BTreeMap::new();
        }
        BTreeMap::from([
            ("SKINNING".to_owned(), "1".to_owned()),
            ("BONECOUNT".to_owned(), bones.to_string()),
        ])
    }

    /// The puppet mesh in layer-local pixels; UVs scaled onto a padded texture.
    fn mesh(&self, uv_scale: Vec2) -> Mesh {
        let m = &self.mdl.meshes[0];
        let n = m.positions.len();
        let normals: Vec<f32> = if m.normals.len() == n {
            m.normals.iter().flatten().copied().collect()
        } else {
            [0.0, 0.0, 1.0].repeat(n)
        };
        let streams = vec![
            (
                "a_Position".to_owned(),
                3,
                m.positions.iter().flatten().copied().collect(),
            ),
            (
                "a_TexCoord".to_owned(),
                2,
                m.texcoords
                    .iter()
                    .flat_map(|t| [t[0] * uv_scale.x, t[1] * uv_scale.y])
                    .collect(),
            ),
            ("a_Normal".to_owned(), 3, normals),
            (
                "a_BlendIndices".to_owned(),
                4,
                m.blend_indices
                    .iter()
                    .flatten()
                    .map(|&i| i as f32)
                    .collect(),
            ),
            (
                "a_BlendWeights".to_owned(),
                4,
                m.blend_weights.iter().flatten().copied().collect(),
            ),
        ];
        Mesh::Triangles(MeshData {
            vertex_count: n as u32,
            attributes: streams,
            indices: Some(m.indices.clone()),
        })
    }
}

/// A value recomputed every frame and written into one draw.
pub enum Binding {
    /// The layer's animated and/or parallax-shifted transform.
    Model {
        step: usize,
        motion: Motion,
        parallax: Option<LayerParallax>,
    },
    /// The layer's animated colour and alpha uniforms.
    Color {
        step: usize,
        motion: Motion,
        color: glam::Vec3,
        alpha: f32,
        /// `g_Color4` stays white (blend-mode passes).
        neutral: bool,
    },
    /// A puppet's skinning matrices (`g_Bones`).
    Bones {
        step: usize,
        puppet: Arc<LayerPuppet>,
    },
    /// An animated material constant.
    Constant {
        step: usize,
        uniform: String,
        base: Vec<f32>,
        animation: Animation,
    },
}

/// What camera parallax needs to shift one layer.
#[derive(Debug, Clone, Copy)]
pub struct LayerParallax {
    pub settings: Parallax,
    pub depth: Vec2,
    /// Scene size; the camera looks at its centre.
    pub scene: Vec2,
}

impl LayerParallax {
    /// Translation WE applies: the layer's offset from the camera plus the
    /// mouse offset, scaled by depth and the scene amount.
    fn offset(&self, position: glam::Vec3, pointer: Vec2) -> glam::Vec3 {
        let mouse = Vec2::new(0.5 - pointer.x, pointer.y - 0.5)
            * self.scene
            * self.settings.mouse_influence;
        let shift =
            (position.truncate() - self.scene / 2.0 + mouse) * self.depth * self.settings.amount;
        shift.extend(0.0)
    }
}

/// Per-frame inputs bindings are evaluated against.
#[derive(Debug, Clone, Copy)]
pub struct FrameState {
    pub time: f32,
    /// Smoothed cursor position over the output, 0..1 with Y down.
    pub pointer: Vec2,
}

impl Binding {
    pub fn apply(&self, renderer: &mut wpe_render::SceneRenderer, frame: FrameState) {
        let time = frame.time;
        match self {
            Self::Model {
                step,
                motion,
                parallax,
            } => {
                let mut model = motion.transform_at(time);
                if let Some(p) = parallax {
                    model =
                        Mat4::from_translation(p.offset(motion.position(), frame.pointer)) * model;
                }
                renderer.set_model(*step, model);
            }
            Self::Color {
                step,
                motion,
                color,
                alpha,
                neutral,
            } => {
                let c = motion.color_at(*color, time);
                let a = motion.alpha_at(*alpha, time);
                if !neutral {
                    renderer.set_uniform(*step, "g_Color4", vec![c.x, c.y, c.z, a]);
                }
                renderer.set_uniform(*step, "g_Color", c.to_array().to_vec());
                renderer.set_uniform(*step, "g_UserAlpha", vec![a]);
                renderer.set_uniform(*step, "g_Alpha", vec![a]);
            }
            Self::Constant {
                step,
                uniform,
                base,
                animation,
            } => renderer.set_uniform(*step, uniform, animation.sample(time, base)),
            Self::Bones { step, puppet } => {
                let mut padded = Vec::new();
                puppet.puppet.bones(time as f64, &mut padded);
                // `mat4x3` columns are written tightly; the block layout pads them.
                let bones = padded
                    .chunks_exact(4)
                    .flat_map(|column| column[..3].to_vec())
                    .collect();
                renderer.set_uniform(*step, "g_Bones", bones);
            }
        }
    }
}

/// Shader programs by shader name and final combos; `None` caches a failure.
type ProgramCache = HashMap<String, Option<Arc<ShaderProgram>>>;

pub struct FrameBuilder<'a> {
    gpu: &'a Gpu,
    vfs: &'a Vfs,
    cache: FrameCache,
    targets: Vec<TargetDesc>,
    steps: Vec<Step>,
    frame_copy: Option<TargetId>,
    /// Offscreen outputs of layers other layers sample, by layer id.
    links: HashMap<i32, TargetId>,
    particles: Vec<ParticleInstance>,
    /// The on-screen layer whose draws are being added.
    current_layer: Option<i32>,
    layer_steps: HashMap<i32, LayerSteps>,
    /// The first video each layer uploads (its own texture comes first).
    layer_videos: HashMap<i32, Arc<VideoStream>>,
    /// The layer being built, on screen or not.
    building: Option<i32>,
    /// Layers drawn but initially hidden.
    hidden: Vec<i32>,
    bindings: Vec<Binding>,
    parallax: Option<(Parallax, Vec2)>,
    /// Draws using their compiled material, and all draws.
    pub stats: (usize, usize),
}

/// What outlives one build of a wallpaper's frame: loaded textures, GPU
/// uploads, compiled shaders, puppets and video streams, so rebuilding after
/// scripts change the scene's structure is cheap.
pub struct FrameCache {
    pub store: TextureStore,
    /// Uploads by source texture address; the source is kept alive so the
    /// address can't be reused by another texture.
    uploaded: HashMap<usize, (Arc<Texture>, Arc<GpuTexture>)>,
    programs: ProgramCache,
    white: Arc<GpuTexture>,
    /// Parsed puppet models by path; `None` caches a failure.
    mdls: HashMap<String, Option<Arc<wpe_mdl::Mdl>>>,
    /// Sprite-sheet pages after the first, by texture and page index.
    pages: HashMap<(usize, usize), Arc<GpuTexture>>,
    /// Video textures and the streams feeding them.
    pub videos: Vec<(Arc<GpuTexture>, Arc<VideoStream>)>,
    /// Streams by the texture they play, keyed like `uploaded`.
    video_textures: HashMap<usize, Arc<VideoStream>>,
}

impl FrameCache {
    pub fn new(gpu: &Gpu, store: TextureStore, white: Texture) -> Self {
        Self {
            store,
            uploaded: HashMap::new(),
            programs: HashMap::new(),
            white: Arc::new(upload_texture(gpu, &white, "white")),
            mdls: HashMap::new(),
            pages: HashMap::new(),
            videos: Vec::new(),
            video_textures: HashMap::new(),
        }
    }
}

impl<'a> FrameBuilder<'a> {
    pub fn new(gpu: &'a Gpu, vfs: &'a Vfs, cache: FrameCache) -> Self {
        Self {
            gpu,
            vfs,
            cache,
            targets: Vec::new(),
            steps: Vec::new(),
            frame_copy: None,
            links: HashMap::new(),
            layer_videos: HashMap::new(),
            building: None,
            hidden: Vec::new(),
            bindings: Vec::new(),
            parallax: None,
            particles: Vec::new(),
            current_layer: None,
            layer_steps: HashMap::new(),
            stats: (0, 0),
        }
    }

    pub fn finish(self) -> (Frame, FrameCache) {
        let frame = Frame {
            targets: self.targets,
            steps: self.steps,
            layer_videos: self.layer_videos,
            hidden: self.hidden,
            bindings: self.bindings,
            particles: self.particles,
            layer_steps: self.layer_steps,
        };
        (frame, self.cache)
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
        let gpu_tex = self.upload_once(tex, key, label);
        if let (Some(id), Some(stream)) = (self.building, self.cache.video_textures.get(&key)) {
            self.layer_videos
                .entry(id)
                .or_insert_with(|| stream.clone());
        }
        gpu_tex
    }

    fn upload_once(&mut self, tex: &Arc<Texture>, key: usize, label: &str) -> Arc<GpuTexture> {
        if let Some((_, t)) = self.cache.uploaded.get(&key) {
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
                match VideoStream::start(self.gpu, mp4, &gpu_tex) {
                    Ok(stream) => {
                        let stream = Arc::new(stream);
                        self.cache.video_textures.insert(key, stream.clone());
                        self.cache.videos.push((gpu_tex.clone(), stream));
                    }
                    Err(e) => tracing::warn!("{label}: video playback unavailable: {e}"),
                }
                gpu_tex
            }
            None => Arc::new(upload_texture(self.gpu, tex, label)),
        };
        self.cache
            .uploaded
            .insert(key, (tex.clone(), gpu_tex.clone()));
        gpu_tex
    }

    fn program(
        &mut self,
        shader: &wpe_shader::CompiledShader,
        name: &str,
    ) -> Option<Arc<ShaderProgram>> {
        let key = format!("{name}{:?}", shader.combos);
        self.cache
            .programs
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
        self.resolve_with(label, pass, targets, &BTreeMap::new())
    }

    /// [`Self::resolve`] with combos overriding the material's.
    fn resolve_with(
        &mut self,
        label: &str,
        pass: &MaterialPass,
        targets: &HashMap<String, TargetId>,
        combos: &BTreeMap<String, String>,
    ) -> Option<Resolved> {
        let prepared = material::prepare(self.vfs, &mut self.cache.store, pass, combos)
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
            animated: prepared.animated,
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
                self.cache
                    .pages
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

    /// A copy belonging to the current layer, hidden along with it.
    fn push_copy(&mut self, from: TargetId, to: TargetId) {
        let step = self.steps.len();
        if let Some(steps) = self.current_steps() {
            steps.all.push(step);
        }
        self.steps.push(Step::Copy { from, to });
    }

    fn push_draw(
        &mut self,
        target: TargetId,
        resolved: Resolved,
        model: Mat4,
        mesh: impl Into<Mesh>,
        blend: BlendMode,
        camera: Camera,
    ) {
        if resolved.reads_frame {
            let copy = self.frame_copy();
            self.push_copy(TargetId::DEFAULT, copy);
        }
        self.stats.0 += 1;
        self.stats.1 += 1;
        let step = self.steps.len();
        if let Some(steps) = self.current_steps() {
            steps.all.push(step);
        }
        for (uniform, base, animation) in resolved.animated {
            self.bindings.push(Binding::Constant {
                step,
                uniform,
                base,
                animation,
            });
        }
        self.steps.push(Step::Draw {
            target,
            draw: Draw::Material(MaterialDraw {
                program: resolved.program,
                textures: resolved.textures,
                constants: resolved.constants,
                model,
                mesh: mesh.into(),
                blend,
                camera,
                sprites: resolved.sprites,
            }),
        });
    }

    /// Add every layer: first the hidden or visible layers other layers
    /// sample, rendered offscreen, then the rest onto the screen. Hidden
    /// layers are drawn too, switched off, so scripts can show them; hidden
    /// particle systems are left out to spare their simulation.
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
        for layer in layers {
            if layer.visible {
                self.layer(layer, None);
            } else if layer.particle.is_none() {
                self.layer(layer, None);
                self.hidden.push(layer.id);
            }
        }
        self.current_layer = None;
        self.building = None;
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

    /// A particle object: one draw per renderable subsystem, whose meshes
    /// are regenerated every frame from the simulation.
    fn particle_layer(&mut self, layer: &ImageLayer, source: &wpe_scene::ParticleSource) {
        let mut system = match wpe_particles::ParticleSystem::load(
            self.vfs,
            &source.path,
            source.instance_override.as_ref(),
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("particles {:?}: {e}", layer.name);
                return;
            }
        };
        system.set_seed(layer.id as u64);
        let mut draws = Vec::new();
        for (index, sub) in system.subsystems().iter().enumerate() {
            if !sub.is_renderable() {
                continue;
            }
            let Some(mut pass) = sub.material_def().and_then(|m| m.passes.first().cloned()) else {
                continue;
            };
            pass.shader = sub.shader().to_owned();
            let label = format!("particles {:?}/{}", layer.name, sub.name());
            let Some(mut r) = self.resolve_with(&label, &pass, &HashMap::new(), &sub.combos())
            else {
                continue;
            };
            r.constants.extend(sub.uniforms());
            r.sprites.clear();
            self.push_draw(
                TargetId::DEFAULT,
                r,
                sub.model_matrix(),
                Mesh::Triangles(MeshData::default()),
                blend_mode(&pass.blending),
                Camera::Scene,
            );
            draws.push((index, self.steps.len() - 1));
        }
        if !draws.is_empty() {
            self.particles.push(ParticleInstance {
                system,
                draws,
                motion: layer.motion.clone(),
                simulated: 0.0,
            });
        }
    }

    /// Draw `layer` onto the screen, or into `offscreen` filling it.
    fn layer(&mut self, layer: &ImageLayer, offscreen: Option<TargetId>) {
        self.current_layer = offscreen.is_none().then_some(layer.id);
        self.building = Some(layer.id);
        if let Some(source) = &layer.particle {
            if offscreen.is_none() {
                self.particle_layer(layer, source);
            }
            return;
        }
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

    /// The puppet deforming `layer`, if its model has one that parses.
    fn puppet(&mut self, layer: &ImageLayer) -> Option<Arc<LayerPuppet>> {
        let path = layer.puppet.as_deref()?;
        let mdl = self
            .cache
            .mdls
            .entry(path.to_owned())
            .or_insert_with(|| {
                self.vfs
                    .read(path)
                    .map_err(|e| e.to_string())
                    .and_then(|b| wpe_mdl::Mdl::parse(&b).map_err(|e| e.to_string()))
                    .inspect_err(|e| tracing::warn!("puppet {path}: {e}"))
                    .ok()
                    .map(Arc::new)
            })
            .clone()?;
        mdl.meshes.first()?;
        let puppet = wpe_mdl::Puppet::new(&mdl, &layer.animation_layers);
        Some(Arc::new(LayerPuppet { mdl, puppet }))
    }

    /// Upload the puppet's bones into the draw just pushed, every frame.
    fn bind_puppet(&mut self, puppet: Option<Arc<LayerPuppet>>) {
        if let Some(puppet) = puppet.filter(|p| p.puppet.bone_count() > 0) {
            self.bindings.push(Binding::Bones {
                step: self.steps.len() - 1,
                puppet,
            });
        }
    }

    /// Parallax for a layer drawn with the scene camera, if it moves at all.
    fn layer_parallax(&self, layer: &ImageLayer) -> Option<LayerParallax> {
        let (settings, scene) = self.parallax?;
        (layer.parallax_depth != Vec2::ZERO && settings.amount != 0.0).then_some(LayerParallax {
            settings,
            depth: layer.parallax_depth,
            scene,
        })
    }

    fn bind_model(&mut self, layer: &ImageLayer, step: usize) {
        if let Some(steps) = self.current_steps() {
            steps.model.push(step);
        }
        let parallax = self.layer_parallax(layer);
        if layer.motion.is_transform_animated() || parallax.is_some() {
            self.bindings.push(Binding::Model {
                step,
                motion: layer.motion.clone(),
                parallax,
            });
        }
    }

    /// Enable camera parallax for layers added afterwards.
    pub fn set_parallax(&mut self, parallax: Option<Parallax>, scene: Vec2) {
        self.parallax = parallax.map(|p| (p, scene));
    }

    /// Bind the layer's animations to the draw just pushed.
    fn bind_layer(&mut self, layer: &ImageLayer, model: bool, neutral: bool) {
        let step = self.steps.len() - 1;
        if let Some(steps) = self.current_steps() {
            steps.color.push((step, neutral));
        }
        let m = &layer.motion;
        if model {
            self.bind_model(layer, step);
        }
        if m.color_animation.is_some() || m.alpha_animation.is_some() {
            self.bindings.push(Binding::Color {
                step,
                motion: m.clone(),
                color: layer.color,
                alpha: layer.alpha,
                neutral,
            });
        }
    }

    fn current_steps(&mut self) -> Option<&mut LayerSteps> {
        let id = self.current_layer?;
        Some(self.layer_steps.entry(id).or_default())
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
        let puppet = self.puppet(layer);
        let combos = puppet.as_ref().map(|p| p.combos()).unwrap_or_default();
        match self.resolve_with(&label, &layer.material, &HashMap::new(), &combos) {
            Some(mut r) => {
                Self::layer_constants(layer, &mut r.constants, false);
                let mesh = match &puppet {
                    Some(p) => p.mesh(r.uv_scale),
                    None => CardMesh {
                        size: layer.size,
                        uv_scale: r.uv_scale,
                    }
                    .into(),
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
                self.bind_layer(layer, offscreen.is_none(), false);
                self.bind_puppet(puppet);
            }
            None if offscreen.is_none() => self.fallback(layer),
            None => {}
        }
    }

    /// The built-in stand-in shader, for layers whose material failed.
    fn fallback(&mut self, layer: &ImageLayer) {
        let texture = match layer.texture(0) {
            Some(name) if !name.starts_with("_rt_") => match self.cache.store.get(self.vfs, name) {
                Some(tex) => self.upload(&tex, name),
                None => return,
            },
            None if layer.solid => self.cache.white.clone(),
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
        let puppet = if layer.fullscreen || layer.compose {
            None
        } else {
            self.puppet(layer)
        };
        let combos = puppet.as_ref().map(|p| p.combos()).unwrap_or_default();
        let Some(mut base) = self.resolve_with(&label, &layer.material, &HashMap::new(), &combos)
        else {
            return self.fallback(layer);
        };
        Self::layer_constants(layer, &mut base.constants, false);
        let (camera, model, mesh) = if layer.fullscreen {
            (
                Camera::Fixed(Mat4::IDENTITY),
                Mat4::IDENTITY,
                Mesh::from(CardMesh::FULLSCREEN),
            )
        } else if layer.compose {
            let mesh = CardMesh {
                size: layer.size,
                uv_scale: Vec2::ONE,
            };
            (Camera::Scene, layer.transform, mesh.into())
        } else {
            let ortho = local_camera(layer.size);
            let mesh = match &puppet {
                Some(p) => p.mesh(base.uv_scale),
                None => CardMesh {
                    size: layer.size,
                    uv_scale: base.uv_scale,
                }
                .into(),
            };
            (Camera::Fixed(ortho), Mat4::IDENTITY, mesh)
        };
        let base_blend = blend_mode(&layer.material.blending);
        self.push_draw(prev, base, model, mesh, base_blend, camera);
        self.bind_puppet(puppet);
        self.bind_layer(layer, layer.compose && !layer.fullscreen, false);

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
                            CommandKind::Copy => self.push_copy(from, to),
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
                        self.bind_layer(layer, false, neutral);
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
            }
            .into();
            self.bind_model(layer, index);
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
