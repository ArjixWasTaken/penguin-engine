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
    BlendMode, Camera, CardMesh, CullMode, DEFAULT_FORMAT, Draw, Gpu, GpuTexture, ImageDraw,
    MaterialDraw, Mesh, MeshData, RasterState, ShaderProgram, SpriteAnimation, SpriteFrame, Step,
    TargetDesc, TargetId, TargetSize, TexRef, upload_texture, wgpu,
};
use wpe_scene::{
    Animation, Bloom, DrawItem, HdrBloom, ImageLayer, ModelLayer, Motion, Parallax, apply_override,
};
use wpe_vfs::Vfs;

use crate::bones::{Bones, PuppetBones};
use crate::clock::{self, Clock, Clocks};
use crate::material::{self, SlotTexture, TextureStore};
use crate::model_data::ModelDatas;
use crate::video::VideoStream;

#[path = "volumetric.rs"]
mod volumetric;
pub use volumetric::Volumetrics;

/// A built frame: render targets, steps and what animates them.
pub struct Frame {
    pub targets: Vec<TargetDesc>,
    pub steps: Vec<Step>,
    /// The video each layer shows, by layer id, for scripts.
    pub layer_videos: HashMap<i32, Arc<VideoStream>>,
    /// Layers whose draws start switched off.
    pub hidden: Vec<i32>,
    /// Engine texture slots to bind while running.
    pub system_slots: Vec<SystemSlot>,
    /// Draws showing a layer's sprite sheet: layer id and step.
    pub sprite_steps: Vec<(i32, usize)>,
    pub bindings: Vec<Binding>,
    pub particles: Vec<ParticleInstance>,
    /// Draws of each on-screen layer, by layer id.
    pub layer_steps: HashMap<i32, LayerSteps>,
    /// Uniforms set in output texels (see
    /// [`wpe_render::SceneRenderer::set_texel_uniform`]): step, uniform, scale.
    pub texel_uniforms: Vec<(usize, String, [f32; 4])>,
}

/// A material slot showing an engine texture (`$mediaThumbnail`).
#[derive(Clone)]
pub struct SystemSlot {
    pub step: usize,
    pub slot: usize,
    pub name: String,
    /// What it shows while the engine has nothing: the material's texture.
    pub fallback: Arc<GpuTexture>,
}

/// A material pass of a layer: its own material (`effect` `None`) or a pass
/// of one of its effects.
#[derive(Debug, Clone)]
pub struct PassStep {
    /// Index in the scene object's `effects`.
    pub effect: Option<usize>,
    /// Index among the effect's material passes.
    pub pass: usize,
    pub step: usize,
    pub info: Arc<wpe_shader::ShaderInfo>,
}

/// The draws an on-screen layer produced.
#[derive(Debug, Clone, Default)]
pub struct LayerSteps {
    pub all: Vec<usize>,
    /// Material passes, for scripts setting their constants.
    pub passes: Vec<PassStep>,
    /// Effect functions (effect index, name) and the targets they clear.
    pub functions: HashMap<(usize, String), Vec<TargetId>>,
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
/// What the reflected camera sees, for floors that sample it.
const REFLECTION: &str = "_rt_Reflection";
const REFLECTION_BLUR: &str = "_rt_Reflection_blur";

pub fn blend_mode(b: &Blending) -> BlendMode {
    match b {
        Blending::Normal | Blending::Disabled => BlendMode::Normal,
        Blending::Additive => BlendMode::Additive,
        Blending::TranslucentPa => BlendMode::TranslucentPremultiplied,
        _ => BlendMode::Translucent,
    }
}

/// An effect FBO's format; colour FBOs take the scene's colour format.
fn target_format(format: Option<&str>, scene: wgpu::TextureFormat) -> wgpu::TextureFormat {
    match format.map(str::to_ascii_lowercase).as_deref() {
        Some("r8") => wgpu::TextureFormat::R8Unorm,
        Some("rg88") => wgpu::TextureFormat::Rg8Unorm,
        Some("rgba16161616f" | "rgba16f") => wgpu::TextureFormat::Rgba16Float,
        Some("r16f") => wgpu::TextureFormat::R16Float,
        Some("rg1616f" | "rg16f") => wgpu::TextureFormat::Rg16Float,
        _ => scene,
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
    /// Slots showing engine textures (step filled in when drawn).
    system: Vec<SystemSlot>,
    /// The shader's annotations, for naming material constants.
    info: Arc<wpe_shader::ShaderInfo>,
}

/// A simulated particle object and the draws its subsystems feed.
pub struct ParticleInstance {
    pub layer: i32,
    system: wpe_particles::ParticleSystem,
    /// Subsystem index and the step drawing it.
    draws: Vec<(usize, usize)>,
    motion: Motion,
    /// Seconds simulated so far.
    simulated: f32,
    paused: bool,
}

/// Longest simulation step, so screenshots at a given time are reproducible.
const PARTICLE_STEP: f32 = 1.0 / 30.0;

impl ParticleInstance {
    /// Replace the instance override with `json` (an `instanceoverride` object).
    pub fn set_override(&mut self, json: &serde_json::Value) {
        let over = wpe_particles::InstanceOverride::from_json(Some(json));
        self.system.set_override(&over);
    }

    /// Keep and emit `density` (0..1) of the particles asked for.
    pub fn set_density(&mut self, density: f32) {
        self.system.set_density(density);
    }

    pub fn emit(&mut self, count: u32) {
        self.system.emit(count);
    }

    /// Pause or resume the simulation; paused time is skipped, not caught up.
    pub fn set_playing(&mut self, playing: bool) {
        self.paused = !playing;
    }

    /// Clear every particle and pause.
    pub fn stop(&mut self) {
        let seed = self.system.seed();
        self.system.set_seed(seed);
        self.paused = true;
    }

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
        if self.paused {
            self.simulated = time;
        }
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
        self.morphed_mesh(0, uv_scale, &[])
    }

    /// Mesh `index` with blend shapes applied at `weights`.
    fn morphed_mesh(&self, index: usize, uv_scale: Vec2, weights: &[f32]) -> Mesh {
        mdl_mesh(&self.mdl.meshes[index], uv_scale, weights)
    }
}

/// An MDL mesh for the renderer, blend shapes applied at `weights`.
fn mdl_mesh(m: &wpe_mdl::MdlMesh, uv_scale: Vec2, weights: &[f32]) -> Mesh {
    {
        let n = m.positions.len();
        let positions = if weights.iter().any(|&w| w != 0.0) {
            m.morphed_positions(weights)
        } else {
            m.positions.clone()
        };
        let normals: Vec<f32> = if m.normals.len() == n {
            m.normals.iter().flatten().copied().collect()
        } else {
            [0.0, 0.0, 1.0].repeat(n)
        };
        let streams = vec![
            (
                "a_Position".to_owned(),
                3,
                positions.iter().flatten().copied().collect(),
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
        let mut streams = streams;
        // Lightmapped materials read both texcoord sets from one vec4.
        if m.texcoords.len() == n {
            let second = if m.texcoords1.len() == n {
                &m.texcoords1
            } else {
                &m.texcoords
            };
            streams.push((
                "a_TexCoordVec4".to_owned(),
                4,
                m.texcoords
                    .iter()
                    .zip(second)
                    .flat_map(|(t, l)| [t[0] * uv_scale.x, t[1] * uv_scale.y, l[0], l[1]])
                    .collect(),
            ));
        }
        if m.tangents.len() == n {
            streams.push((
                "a_Tangent4".to_owned(),
                4,
                m.tangents.iter().flatten().copied().collect(),
            ));
        }
        Mesh::Triangles(MeshData {
            vertex_count: n as u32,
            attributes: streams,
            indices: Some(m.indices.clone()),
        })
    }
}

/// The camera a layer composites with: the perspective one when it asks.
fn layer_camera(layer: &ImageLayer) -> Camera {
    if layer.perspective {
        Camera::Perspective
    } else {
        Camera::Scene
    }
}

/// Material for script geometry that names none.
fn default_model_pass() -> MaterialPass {
    MaterialPass {
        shader: "generic".to_owned(),
        blending: Blending::Normal,
        cullmode: wpe_assets::material::CullMode::Normal,
        depthtest: wpe_assets::material::Toggle::Enabled,
        depthwrite: wpe_assets::material::Toggle::Enabled,
        textures: vec![Some("util/white".to_owned())],
        ..Default::default()
    }
}

/// A value recomputed every frame and written into one draw.
pub enum Binding {
    /// The layer's animated and/or parallax-shifted transform, following a
    /// parent puppet's attachment point when it has one.
    Model {
        layer: i32,
        step: usize,
        motion: Motion,
        parallax: Option<LayerParallax>,
        attach: Option<(i32, String)>,
    },
    /// The layer's animated colour and alpha uniforms.
    Color {
        layer: i32,
        step: usize,
        motion: Motion,
        color: glam::Vec3,
        alpha: f32,
        /// `g_Color4` stays white (blend-mode passes).
        neutral: bool,
    },
    /// A puppet's skinning matrices (`g_Bones`) and blend shapes.
    Bones {
        layer: i32,
        step: usize,
        puppet: Arc<LayerPuppet>,
        /// Which of the model's meshes the draw is, and its UV scale, for
        /// re-uploading it morphed.
        mesh: usize,
        uv_scale: Vec2,
        /// Blend shape weights the draw's mesh has (empty: none applied).
        morphed: std::cell::RefCell<Vec<f32>>,
    },
    /// Script geometry (`createModelData`) re-uploaded when `applyData`
    /// changes it.
    ModelData {
        step: usize,
        id: i32,
        shape: usize,
        /// The data's generation the draw has.
        generation: std::cell::Cell<u64>,
    },
    /// An animated material constant.
    Constant {
        layer: i32,
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
    /// The pointer camera parallax follows: the smoothed cursor over the
    /// output, 0..1 with Y down (the centre while the scene is fitted).
    pub parallax: Vec2,
}

impl Binding {
    /// Give every animation this binding plays a clock (kept when it
    /// already has one).
    pub fn register_clocks(&self, clocks: &mut Clocks) {
        let mut add = |layer: i32, a: &Animation| {
            clocks.register(
                layer,
                clock::animation_key(&a.name),
                Clock::for_animation(a),
            );
        };
        match self {
            Self::Model { layer, motion, .. } | Self::Color { layer, motion, .. } => {
                motion.animations().for_each(|a| add(*layer, a))
            }
            Self::Constant {
                layer, animation, ..
            } => add(*layer, animation),
            Self::ModelData { .. } => {}
            Self::Bones { layer, puppet, .. } => {
                for (i, params) in puppet.puppet.layers().enumerate() {
                    // Seconds, rate applied (`fps` 1).
                    let mut clock = Clock::new(1.0, 0.0, false, true);
                    clock.rate = params.rate as f32;
                    clock.blend = params.blend as f32;
                    clock.visible = params.visible;
                    if let Some(anim) = puppet
                        .mdl
                        .animations
                        .iter()
                        .find(|a| a.id == params.animation)
                        && anim.fps > 0.0
                    {
                        clock.events = anim
                            .events
                            .iter()
                            .map(|e| (e.frame as f32 / anim.fps, e.name.clone()))
                            .collect();
                        clock.frames = anim.length as f32 / anim.fps;
                    }
                    clocks.register(*layer, clock::layer_key(i), clock);
                }
            }
        }
    }

    /// Make a puppet's bones known to scripts.
    pub fn register_bones(&self, bones: &mut Bones) {
        if let Self::Bones { layer, puppet, .. } = self {
            let names: Vec<String> = puppet
                .mdl
                .skeleton
                .as_ref()
                .map(|s| s.bones.iter().map(|b| b.name.clone()).collect())
                .unwrap_or_default();
            let mut state = PuppetBones::new(names, puppet.puppet.bone_parents());
            state.rest = (0..state.names.len())
                .map(|i| puppet.puppet.rest_world(i).unwrap_or(Mat4::IDENTITY))
                .collect();
            state.attachments = puppet
                .mdl
                .attachments
                .iter()
                .map(|a| crate::bones::Attachment {
                    name: a.name.clone(),
                    bone: a.bone,
                    transform: a.transform,
                })
                .collect();
            state.blend_weights = vec![0.0; puppet.puppet.blend_shape_count()];
            state.blend_names = puppet.puppet.blend_shape_names().to_vec();
            state.animations = puppet
                .mdl
                .animations
                .iter()
                .map(|a| (a.name.clone(), a.id))
                .collect();
            bones.register(*layer, state);
        }
    }

    pub fn apply(
        &self,
        renderer: &mut wpe_render::SceneRenderer,
        frame: FrameState,
        clocks: &Clocks,
        bones: &mut Bones,
        model_data: &ModelDatas,
    ) {
        let scene_time = frame.time;
        match self {
            Self::Model {
                layer,
                step,
                motion,
                parallax,
                attach,
            } => {
                let time = |a: &Animation| clocks.time_of(*layer, a, scene_time);
                let pose = attach.as_ref().and_then(|(parent, name)| {
                    let puppet = bones.puppets.get(parent)?;
                    puppet.attachment_pose(puppet.attachment(name)?)
                });
                let mut model = match pose {
                    Some(pose) => Motion {
                        parent: motion.parent * pose,
                        ..motion.clone()
                    }
                    .transform_at(time),
                    None => motion.transform_at(time),
                };
                if let Some(p) = parallax {
                    model =
                        Mat4::from_translation(p.offset(motion.position(), frame.parallax)) * model;
                }
                renderer.set_model(*step, model);
            }
            Self::Color {
                layer,
                step,
                motion,
                color,
                alpha,
                neutral,
            } => {
                let time = |a: &Animation| clocks.time_of(*layer, a, scene_time);
                let c = motion.color_at(*color, time);
                let a = motion.alpha_at(*alpha, time);
                if !neutral {
                    renderer.set_uniform(*step, "g_Color4", vec![c.x, c.y, c.z, a]);
                }
                renderer.set_uniform(*step, "g_Color", c.to_array().to_vec());
                renderer.set_uniform(*step, "g_UserAlpha", vec![a]);
                renderer.set_uniform(*step, "g_Alpha", vec![a]);
            }
            Self::ModelData {
                step,
                id,
                shape,
                generation,
            } => {
                let Some(model) = model_data.data.get(id) else {
                    return;
                };
                if model.generation != generation.get() {
                    generation.set(model.generation);
                    if let Some(shape) = model.shapes.get(*shape) {
                        renderer.set_mesh(*step, Mesh::Triangles(shape.mesh()));
                    }
                }
            }
            Self::Constant {
                layer,
                step,
                uniform,
                base,
                animation,
            } => {
                let time = |a: &Animation| clocks.time_of(*layer, a, scene_time);
                renderer.set_uniform(*step, uniform, animation.sample(time, base))
            }
            Self::Bones {
                layer,
                step,
                puppet,
                mesh,
                uv_scale,
                morphed,
            } => {
                let mut matrices = Vec::new();
                let mut state = bones.puppets.get_mut(layer);
                let parents = puppet.puppet.bone_parents();
                let layer_pose = |i: usize, params: &wpe_mdl::AnimationLayer| match clocks
                    .get(*layer, &clock::layer_key(i))
                {
                    Some(c) => wpe_mdl::LayerPose {
                        time: c.seconds() as f64,
                        blend: c.blend as f64,
                        visible: c.visible,
                    },
                    None => wpe_mdl::LayerPose {
                        time: scene_time as f64 * params.rate,
                        blend: params.blend,
                        visible: params.visible,
                    },
                };
                if *mesh == 0 && puppet.puppet.blend_shape_count() > 0 {
                    let base: Vec<f32> = state
                        .as_ref()
                        .map(|s| s.blend_weights.clone())
                        .unwrap_or_default();
                    let mut weights = Vec::new();
                    puppet
                        .puppet
                        .morph_weights_with(layer_pose, &base, &mut weights);
                    let mut morphed = morphed.borrow_mut();
                    let unchanged = if morphed.is_empty() {
                        weights.iter().all(|&w| w == 0.0)
                    } else {
                        *morphed == weights
                    };
                    if !unchanged {
                        renderer.set_mesh(*step, puppet.morphed_mesh(*mesh, *uv_scale, &weights));
                        morphed.clone_from(&weights);
                    }
                    if let Some(s) = state.as_deref_mut() {
                        s.current_weights = weights;
                    }
                }
                // Final bone worlds so far, for children of adjusted bones.
                let done = std::cell::RefCell::new(Vec::<Mat4>::with_capacity(parents.len()));
                puppet.puppet.pose_bones(
                    layer_pose,
                    |i, world| {
                        let mut done = done.borrow_mut();
                        let parent = parents
                            .get(i)
                            .copied()
                            .flatten()
                            .and_then(|p| done.get(p).copied())
                            .unwrap_or(Mat4::IDENTITY);
                        let world = match &state {
                            Some(s) => s.adjust(i, world, parent),
                            None => world,
                        };
                        done.push(world);
                        world
                    },
                    &mut matrices,
                );
                if let Some(state) = state {
                    state.worlds.clone_from(&matrices);
                }
                puppet.puppet.skin(&mut matrices);
                // `mat4x3` columns are written tightly; the block layout pads them.
                let bones = matrices
                    .iter()
                    .flat_map(|m| {
                        let c = m.to_cols_array();
                        [
                            c[0], c[1], c[2], c[4], c[5], c[6], c[8], c[9], c[10], c[12], c[13],
                            c[14],
                        ]
                    })
                    .collect();
                renderer.set_uniform(*step, "g_Bones", bones);
            }
        }
    }
}

/// An effect of a layer: definition, pass overrides, index in the object.
type LayerEffect = (Effect, Vec<PassOverride>, Option<usize>);

/// Time a frame build spent compiling shaders (preprocessing and SPIR-V),
/// creating GPU programs, and uploading textures (video streams included).
#[derive(Debug, Default, Clone, Copy)]
pub struct BuildTimings {
    pub shaders: std::time::Duration,
    pub programs: std::time::Duration,
    pub uploads: std::time::Duration,
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
    /// Which material pass the next draw is (effect index, pass index).
    current_pass: Option<(Option<usize>, usize)>,
    system_slots: Vec<SystemSlot>,
    sprite_steps: Vec<(i32, usize)>,
    bindings: Vec<Binding>,
    parallax: Option<(Parallax, Vec2)>,
    /// Model materials' light combos (`LIGHTS_*`), script geometry, and
    /// whether every model draws in perspective (3D scenes).
    light_combos: BTreeMap<String, String>,
    /// Combos every material gets: the scene's fog (`FOG_DIST`, ...).
    scene_combos: BTreeMap<String, String>,
    model_data: Option<&'a ModelDatas>,
    /// User properties material constants bind to (`{"user": ...}` values
    /// and `usershadervalues`).
    props: Option<&'a wpe_scene::UserProperties>,
    is_3d: bool,
    /// `_rt_Reflection` and its blur buffer, when the scene renders planar
    /// reflections ([`Self::enable_reflection`]).
    reflection: Option<(TargetId, TargetId)>,
    /// Mirror only the draws of `reflected` objects ([`Self::reflected_steps`])
    /// into the reflection, rather than every perspective draw.
    reflected_only: bool,
    reflected_steps: std::collections::HashSet<usize>,
    /// Draws using their compiled material, and all draws.
    pub stats: (usize, usize),
    /// Where the build's time went.
    pub timings: BuildTimings,
    /// Format of `_rt_default` and every target holding scene colour.
    scene_format: wgpu::TextureFormat,
    /// Uniforms in output texels: step, uniform, scale.
    texel_uniforms: Vec<(usize, String, [f32; 4])>,
}

/// What outlives one build of a wallpaper's frame: loaded textures, GPU
/// uploads, compiled shaders, puppets and video streams, so rebuilding after
/// scripts change the scene's structure is cheap.
pub struct FrameCache {
    pub store: TextureStore,
    shaders: material::ShaderCache,
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
    /// The GPU copy of texture `name`, once a build has uploaded it.
    #[cfg_attr(not(feature = "web"), allow(dead_code))]
    pub fn uploaded(&mut self, vfs: &Vfs, name: &str) -> Option<Arc<GpuTexture>> {
        let texture = self.store.get(vfs, name)?;
        let key = Arc::as_ptr(&texture) as usize;
        self.uploaded.get(&key).map(|(_, t)| t.clone())
    }

    /// Replace texture `name`, dropping the old one's GPU copy.
    #[cfg_attr(not(feature = "web"), allow(dead_code))]
    pub fn replace(&mut self, vfs: &Vfs, name: &str, texture: Texture) {
        if let Some(old) = self.store.get(vfs, name) {
            self.uploaded.remove(&(Arc::as_ptr(&old) as usize));
        }
        self.store.insert(name, texture);
    }

    pub fn new(gpu: &Gpu, store: TextureStore, white: Texture) -> Self {
        Self {
            store,
            shaders: Default::default(),
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
            current_pass: None,
            system_slots: Vec::new(),
            sprite_steps: Vec::new(),
            bindings: Vec::new(),
            parallax: None,
            light_combos: BTreeMap::new(),
            scene_combos: BTreeMap::new(),
            model_data: None,
            props: None,
            is_3d: false,
            reflection: None,
            reflected_only: false,
            reflected_steps: Default::default(),
            particles: Vec::new(),
            current_layer: None,
            layer_steps: HashMap::new(),
            stats: (0, 0),
            timings: BuildTimings::default(),
            scene_format: DEFAULT_FORMAT,
            texel_uniforms: Vec::new(),
        }
    }

    pub fn finish(self) -> (Frame, FrameCache) {
        let frame = Frame {
            targets: self.targets,
            steps: self.steps,
            layer_videos: self.layer_videos,
            hidden: self.hidden,
            system_slots: self.system_slots,
            sprite_steps: self.sprite_steps,
            bindings: self.bindings,
            particles: self.particles,
            layer_steps: self.layer_steps,
            texel_uniforms: self.texel_uniforms,
        };
        (frame, self.cache)
    }

    /// The user properties material constants are bound to.
    pub fn set_user_properties(&mut self, props: &'a wpe_scene::UserProperties) {
        self.props = Some(props);
    }

    /// `pass` with its user-bound constants (`{"user": "name"}` values and
    /// `usershadervalues`, property → constant) set from the user
    /// properties; `None` when nothing is bound.
    fn bind_user_constants(&self, pass: &MaterialPass) -> Option<MaterialPass> {
        let props = self.props?;
        let bound = pass.constantshadervalues.values().any(|v| v.user.is_some());
        if !bound && pass.usershadervalues.is_empty() {
            return None;
        }
        let mut pass = pass.clone();
        for value in pass.constantshadervalues.values_mut() {
            if value.user.is_some() {
                value.value = props.value(value);
                value.user = None;
            }
        }
        for (property, constant) in &pass.usershadervalues {
            let Some(value) = props
                .get(property)
                .and_then(|v| wpe_assets::value::Dynamic::from_value(v.clone()).ok())
            else {
                continue;
            };
            pass.constantshadervalues.insert(constant.clone(), value);
        }
        Some(pass)
    }

    /// Render HDR scenes into [`wpe_render::HDR_FORMAT`] targets.
    pub fn set_hdr(&mut self, hdr: bool) {
        self.scene_format = if hdr {
            wpe_render::HDR_FORMAT
        } else {
            DEFAULT_FORMAT
        };
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
            self.scene_format,
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
        let started = std::time::Instant::now();
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
        self.timings.uploads += started.elapsed();
        gpu_tex
    }

    fn program(
        &mut self,
        shader: &wpe_shader::CompiledShader,
        name: &str,
    ) -> Option<Arc<ShaderProgram>> {
        let key = format!("{name}{:?}", shader.combos);
        let (gpu, timings) = (self.gpu, &mut self.timings);
        self.cache
            .programs
            .entry(key)
            .or_insert_with(|| {
                let started = std::time::Instant::now();
                let program = ShaderProgram::new(gpu, name, shader)
                    .inspect_err(|e| tracing::warn!("shader {name}: {e}"))
                    .ok()
                    .map(Arc::new);
                timings.programs += started.elapsed();
                program
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
        // With shadow mapping or a cookie light on, every lit material
        // samples the shadow atlas or the cookie, image layers included:
        // they all get the light combos. Every material gets the fog's.
        let lit = self.shadow_mapping() || self.light_cookie();
        let merged: BTreeMap<String, String>;
        let combos = if lit || !self.scene_combos.is_empty() {
            let light = if lit {
                self.light_combos.clone()
            } else {
                BTreeMap::new()
            };
            merged = light
                .into_iter()
                .chain(self.scene_combos.clone())
                .chain(combos.clone())
                .collect();
            &merged
        } else {
            combos
        };
        let bound = self.bind_user_constants(pass);
        let pass = bound.as_ref().unwrap_or(pass);
        let started = std::time::Instant::now();
        let decoding = self.cache.store.decode_time;
        let prepared = material::prepare(
            self.vfs,
            &mut self.cache.store,
            &mut self.cache.shaders,
            pass,
            combos,
        )
        .inspect_err(|e| tracing::warn!("{label} ({}): {e}", pass.shader));
        // Texture decoding is counted by the store.
        self.timings.shaders += started
            .elapsed()
            .saturating_sub(self.cache.store.decode_time - decoding);
        let prepared = prepared.ok()?;
        let program = self.program(&prepared.shader, &pass.shader)?;
        let mut reads_frame = false;
        let mut system = Vec::new();
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
                Some(SlotTexture::System(name, fallback)) => {
                    let fallback = match fallback {
                        Some(t) => self.upload(t, &format!("{label} slot {slot}")),
                        None => self.cache.white.clone(),
                    };
                    system.push(SystemSlot {
                        step: 0,
                        slot,
                        name: name.clone(),
                        fallback: fallback.clone(),
                    });
                    Some(TexRef::Asset(fallback))
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
                    } else if name == volumetric::SHADOW_ATLAS {
                        Some(TexRef::ShadowAtlas)
                    } else if let (REFLECTION, Some((reflection, _))) =
                        (name.as_str(), self.reflection)
                    {
                        Some(TexRef::Target(reflection))
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
            system,
            info: Arc::new(prepared.shader.info.clone()),
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
        self.push_draw_with(
            target,
            resolved,
            model,
            mesh,
            blend,
            camera,
            RasterState::default(),
        );
    }

    /// [`Self::push_draw`] with depth testing and culling (3D models).
    #[allow(clippy::too_many_arguments)]
    fn push_draw_with(
        &mut self,
        target: TargetId,
        resolved: Resolved,
        model: Mat4,
        mesh: impl Into<Mesh>,
        blend: BlendMode,
        camera: Camera,
        raster: RasterState,
    ) {
        if resolved.reads_frame {
            let copy = self.frame_copy();
            self.push_copy(TargetId::DEFAULT, copy);
        }
        if let Some((effect, pass)) = self.current_pass.take() {
            let step = self.steps.len();
            let info = resolved.info.clone();
            if let Some(steps) = self.current_steps() {
                steps.passes.push(PassStep {
                    effect,
                    pass,
                    step,
                    info,
                });
            }
        }
        self.stats.0 += 1;
        self.stats.1 += 1;
        let step = self.steps.len();
        if let Some(steps) = self.current_steps() {
            steps.all.push(step);
        }
        for slot in resolved.system {
            self.system_slots.push(SystemSlot { step, ..slot });
        }
        if !resolved.sprites.is_empty()
            && let Some(layer) = self.building
        {
            self.sprite_steps.push((layer, step));
        }
        for (uniform, base, animation) in resolved.animated {
            self.bindings.push(Binding::Constant {
                layer: self.building.unwrap_or(-1),
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
                raster,
                effect_of: None,
            }),
        });
    }

    /// Add every layer: first the hidden or visible layers other layers
    /// sample, rendered offscreen, then the rest onto the screen. Hidden
    /// layers are drawn too, switched off, so scripts can show them; hidden
    /// particle systems are left out to spare their simulation.
    /// What model layers need: the scene's light combos, script geometry
    /// and whether the scene is 3D.
    pub fn set_models(
        &mut self,
        light_combos: BTreeMap<String, String>,
        model_data: &'a ModelDatas,
        is_3d: bool,
    ) {
        self.light_combos = light_combos;
        self.model_data = Some(model_data);
        self.is_3d = is_3d;
    }

    /// Combos every material gets (the scene's fog).
    pub fn set_scene_combos(&mut self, combos: BTreeMap<String, String>) {
        self.scene_combos = combos;
    }

    /// Add every layer and model in draw order (see [`Self::layers`]).
    pub fn objects(&mut self, layers: &[ImageLayer], models: &[ModelLayer], order: &[DrawItem]) {
        if models.is_empty() {
            return self.layers(layers);
        }
        let mut pending: Vec<ImageLayer> = Vec::new();
        for item in order {
            match *item {
                DrawItem::Layer(i) => pending.push(layers[i].clone()),
                DrawItem::Model(i) => {
                    if !pending.is_empty() {
                        self.layers(&std::mem::take(&mut pending));
                    }
                    self.model_layer(&models[i]);
                }
            }
        }
        if !pending.is_empty() {
            self.layers(&pending);
        }
    }

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
                self.scene_format,
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
    /// eighth size, then add it back over `_rt_default`; or the HDR mip
    /// chain ([`Self::hdr_bloom`]).
    pub fn bloom(&mut self, bloom: Bloom) {
        if let Some(chain) = bloom.mip_chain {
            return self.hdr_bloom(bloom, chain);
        }
        let screen = |divisor| TargetSize::Screen { divisor };
        let format = self.scene_format;
        let scene = self.target("_rt_Bloom_Scene".to_owned(), screen(1.0), format);
        let quarter = self.target("_rt_Bloom_Quarter".to_owned(), screen(4.0), format);
        let eighth = self.target("_rt_Bloom_Eighth".to_owned(), screen(8.0), format);
        let result = self.target("_rt_Bloom_Result".to_owned(), screen(8.0), format);
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

    /// HDR scenes' mip-chain bloom (`postprocessing` `ultra`/`displayhdr`):
    /// a soft-knee bright pass of the frame into a half-size mip, 4-tap
    /// downsamples to 1/16 size, additive 4-tap upsamples back to the half
    /// size mip, and `combine_hdr` adding that over the frame, clamped.
    fn hdr_bloom(&mut self, bloom: Bloom, chain: HdrBloom) {
        const MIPS: usize = 4;
        let format = self.scene_format;
        let screen = |divisor| TargetSize::Screen { divisor };
        let scene = self.target("_rt_Bloom_Scene".to_owned(), screen(1.0), format);
        let mut names = HashMap::from([("_rt_Bloom_Scene".to_owned(), scene)]);
        // Mip n is at 1/2^n size; mip 0 is the frame copy.
        let mut mips = vec![scene];
        for n in 1..=MIPS {
            let name = format!("_rt_Bloom_Mip{n}");
            let id = self.target(name.clone(), screen((1 << n) as f32), format);
            names.insert(name, id);
            mips.push(id);
        }
        self.steps.push(Step::Copy {
            from: TargetId::DEFAULT,
            to: scene,
        });

        // Soft knee from threshold - knee to threshold + knee, the knee a
        // fraction of the threshold (floored: the shader divides by it).
        let threshold = bloom.threshold;
        let knee = (threshold * chain.feather.clamp(0.0, 1.0)).max(1e-4);
        let mut passes = Vec::new();
        for n in 0..MIPS {
            // Bright pass from the frame, then plain downsamples.
            let combo = (n == 0).then_some("BLOOM");
            passes.push((n, n + 1, combo));
        }
        for n in (1..MIPS).rev() {
            passes.push((n + 1, n, Some("UPSAMPLE")));
        }
        for (from, to, combo) in passes {
            let pass = MaterialPass {
                shader: "hdr_downsample".to_owned(),
                blending: Blending::Normal,
                textures: vec![Some(format!("_rt_Bloom_{}", mip_name(from)))],
                ..Default::default()
            };
            let combos = combo
                .map(|c| BTreeMap::from([(c.to_owned(), "1".to_owned())]))
                .unwrap_or_default();
            let label = format!("bloom hdr_downsample {from}->{to}");
            let Some(mut r) = self.resolve_with(&label, &pass, &names, &combos) else {
                tracing::warn!("bloom disabled: {label} failed");
                return;
            };
            match combo {
                Some("BLOOM") => {
                    let c = &mut r.constants;
                    let params = vec![threshold, threshold - knee, 2.0 * knee, 0.25 / knee];
                    c.insert("g_BloomBlendParams".to_owned(), params);
                    c.insert("g_BloomTint".to_owned(), bloom.tint.to_array().to_vec());
                    c.insert("g_BloomStrength".to_owned(), vec![bloom.strength]);
                }
                Some(_) => {
                    // Neutral: `bloomhdrscatter` compounds over the levels.
                    r.constants.insert("g_BloomScatter".to_owned(), vec![1.0]);
                }
                None => {}
            }
            // Corner offsets of half a source texel.
            let d = 0.5 * (1 << from) as f32;
            let step = self.steps.len();
            self.texel_uniforms
                .push((step, "g_RenderVar0".to_owned(), [-d, -d, d, d]));
            let blend = if from > to {
                BlendMode::Additive
            } else {
                BlendMode::Normal
            };
            self.push_draw(
                mips[to],
                r,
                Mat4::IDENTITY,
                CardMesh::FULLSCREEN,
                blend,
                Camera::Fixed(Mat4::IDENTITY),
            );
        }

        // `combine_hdr` with LINEAR: the frame plus the bloom, clamped.
        // (Its non-linear branch decodes sRGB for a linear backbuffer;
        // scene colour here stays in display space throughout.)
        let pass = MaterialPass {
            shader: "combine_hdr".to_owned(),
            blending: Blending::Normal,
            textures: vec![
                Some("_rt_Bloom_Scene".to_owned()),
                Some("_rt_Bloom_Mip1".to_owned()),
            ],
            ..Default::default()
        };
        let combos = BTreeMap::from([("LINEAR".to_owned(), "1".to_owned())]);
        let Some(mut r) = self.resolve_with("bloom combine_hdr", &pass, &names, &combos) else {
            tracing::warn!("bloom disabled: combine_hdr failed");
            return;
        };
        r.constants
            .insert("g_RenderVar0".to_owned(), vec![1.0, 0.0, 0.0, 0.0]);
        self.push_draw(
            TargetId::DEFAULT,
            r,
            Mat4::IDENTITY,
            CardMesh::FULLSCREEN,
            BlendMode::Normal,
            Camera::Fixed(Mat4::IDENTITY),
        );
    }

    /// Render planar reflections: create `_rt_Reflection` (which materials
    /// may then sample) before adding objects, then add the passes filling
    /// it with [`Self::reflection_passes`]. Wallpaper Engine does this in 3D
    /// scenes with camera paths (its `"reflected_perspective"` camera).
    ///
    /// With `reflected_only`, only objects flagged `reflected` are mirrored.
    pub fn enable_reflection(&mut self, reflected_only: bool) {
        if self.reflection.is_some() {
            return;
        }
        self.reflected_only = reflected_only;
        let screen = TargetSize::Screen { divisor: 1.0 };
        let format = self.scene_format;
        let reflection = self.target(REFLECTION.to_owned(), screen, format);
        let blur = self.target(REFLECTION_BLUR.to_owned(), screen, format);
        self.reflection = Some((reflection, blur));
    }

    /// Mirror every perspective draw onto the screen, except those reading
    /// the reflection, into `_rt_Reflection` with the reflected camera, then
    /// soften it with a separable `blur_k3`. The renderer runs these before
    /// the rest of the frame.
    pub fn reflection_passes(&mut self) {
        let Some((reflection, blur)) = self.reflection else {
            return;
        };
        let sources: Vec<usize> =
            self.steps
                .iter()
                .enumerate()
                .filter_map(|(i, step)| match step {
                    Step::Draw {
                        target: TargetId::DEFAULT,
                        draw: Draw::Material(d),
                    } if matches!(d.camera, Camera::Perspective)
                        && (!self.reflected_only || self.reflected_steps.contains(&i))
                        && !d.textures.iter().any(
                            |t| matches!(t, Some(TexRef::Target(id)) if *id == reflection),
                        ) =>
                    {
                        Some(i)
                    }
                    _ => None,
                })
                .collect();
        tracing::debug!("{} draws mirrored into {REFLECTION}", sources.len());
        for source in sources {
            self.steps.push(Step::Mirror {
                target: reflection,
                source,
            });
        }
        let names = HashMap::from([
            (REFLECTION.to_owned(), reflection),
            (REFLECTION_BLUR.to_owned(), blur),
        ]);
        for (input, vertical, target) in
            [(REFLECTION, "0", blur), (REFLECTION_BLUR, "1", reflection)]
        {
            let pass = MaterialPass {
                shader: "blur_k3".to_owned(),
                blending: Blending::Normal,
                textures: vec![Some(input.to_owned())],
                ..Default::default()
            };
            let combos = BTreeMap::from([("VERTICAL".to_owned(), vertical.to_owned())]);
            let Some(r) = self.resolve_with("reflection blur", &pass, &names, &combos) else {
                tracing::warn!("reflection blur: blur_k3 failed");
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
                // Flag 4: drawn through the scene's perspective camera.
                if sub.perspective() {
                    Camera::Perspective
                } else {
                    Camera::Scene
                },
            );
            draws.push((index, self.steps.len() - 1));
        }
        if !draws.is_empty() {
            self.particles.push(ParticleInstance {
                layer: layer.id,
                system,
                draws,
                motion: layer.motion.clone(),
                simulated: 0.0,
                paused: false,
            });
        }
    }

    /// Draw `layer` onto the screen, or into `offscreen` filling it.
    /// A model's draws: its MDL meshes or script shapes, opaque ones first.
    fn model_layer(&mut self, m: &ModelLayer) {
        self.current_layer = Some(m.id);
        self.building = Some(m.id);
        let camera = if self.is_3d || m.perspective {
            Camera::Perspective
        } else {
            Camera::Scene
        };
        // (pass, mesh, mesh index, puppet)
        type Part = (MaterialPass, Mesh, usize, Option<Arc<LayerPuppet>>);
        let mut parts: Vec<Part> = Vec::new();
        let mut combos = self.light_combos.clone();
        if let Some(id) = crate::model_data::id_of(&m.model) {
            let shapes = self
                .model_data
                .and_then(|d| d.data.get(&id))
                .map(|d| d.shapes.clone())
                .unwrap_or_default();
            for (i, shape) in shapes.iter().enumerate() {
                let pass = match &shape.material {
                    Some(path) => self.model_pass(path),
                    None => Some(default_model_pass()),
                };
                if let Some(pass) = pass {
                    parts.push((pass, Mesh::Triangles(shape.mesh()), i, None));
                }
            }
        } else if let Some(path) = &m.mdl {
            let Some(mdl) = self.load_mdl(path) else {
                return;
            };
            let puppet = Arc::new(LayerPuppet {
                puppet: wpe_mdl::Puppet::new(&mdl, &m.animation_layers),
                mdl: mdl.clone(),
            });
            combos.extend(puppet.combos());
            for (i, mesh) in mdl.meshes.iter().enumerate() {
                let material = mesh
                    .skins
                    .get(m.skin)
                    .map(String::as_str)
                    .or(mesh.material.as_deref());
                let Some(pass) = material.and_then(|p| self.model_pass(p)) else {
                    continue;
                };
                parts.push((
                    pass,
                    mdl_mesh(mesh, Vec2::ONE, &[]),
                    i,
                    Some(puppet.clone()),
                ));
            }
        }
        // Translucent parts after opaque ones, each group in file order.
        parts.sort_by_key(|(pass, ..)| pass.blending == Blending::Translucent);
        let first = self.steps.len();
        for (pass, mesh, index, puppet) in parts {
            let label = format!("model {:?} part {index}", m.name);
            let Some(mut r) = self.resolve_with(&label, &pass, &HashMap::new(), &combos) else {
                continue;
            };
            for (name, value) in [
                ("g_Color4", vec![1.0; 4]),
                ("g_Color", vec![1.0; 3]),
                ("g_Alpha", vec![1.0]),
                ("g_UserAlpha", vec![1.0]),
                ("g_Brightness", vec![1.0]),
            ] {
                r.constants.entry(name.to_owned()).or_insert(value);
            }
            let blend = blend_mode(&pass.blending);
            let raster = RasterState {
                depth_test: pass.depthtest.is_enabled(),
                // Blended parts don't hide what's behind them.
                depth_write: pass.depthwrite.is_enabled()
                    && !matches!(blend, BlendMode::Translucent | BlendMode::Additive),
                cull: match pass.cullmode {
                    wpe_assets::material::CullMode::Normal => CullMode::Back,
                    _ => CullMode::None,
                },
            };
            let uv_scale = r.uv_scale;
            let mesh = match &puppet {
                Some(p) => p.morphed_mesh(index, uv_scale, &[]),
                None => mesh,
            };
            self.current_pass = Some((None, 0));
            self.push_draw_with(
                TargetId::DEFAULT,
                r,
                m.transform,
                mesh,
                blend,
                camera,
                raster,
            );
            let step = self.steps.len() - 1;
            if let Some(id) = crate::model_data::id_of(&m.model) {
                let generation = self
                    .model_data
                    .and_then(|d| d.data.get(&id))
                    .map_or(0, |d| d.generation);
                self.bindings.push(Binding::ModelData {
                    step,
                    id,
                    shape: index,
                    generation: generation.into(),
                });
            }
            self.bind_puppet_mesh(puppet, index, uv_scale);
            if let Some(steps) = self.current_steps() {
                steps.model.push(step);
            }
            let attach = m.attachment.clone().zip(m.parent).map(|(a, p)| (p, a));
            if m.motion.is_transform_animated() || attach.is_some() {
                self.bindings.push(Binding::Model {
                    layer: m.id,
                    step,
                    motion: m.motion.clone(),
                    parallax: None,
                    attach,
                });
            }
        }
        if m.reflected {
            self.reflected_steps.extend(first..self.steps.len());
        }
        if !m.visible && self.steps.len() > first {
            self.hidden.push(m.id);
        }
        self.current_layer = None;
        self.building = None;
    }

    /// A model part's material pass, with the defaults 3D models get.
    fn model_pass(&mut self, path: &str) -> Option<MaterialPass> {
        let text = self
            .vfs
            .read_to_string(path)
            .inspect_err(|e| tracing::warn!("model material {path}: {e}"))
            .ok()?;
        wpe_assets::material::parse_model_material(&text)
            .inspect_err(|e| tracing::warn!("model material {path}: {e}"))
            .ok()?
            .passes
            .into_iter()
            .next()
    }

    /// A parsed `.mdl`, cached by path.
    fn load_mdl(&mut self, path: &str) -> Option<Arc<wpe_mdl::Mdl>> {
        self.cache
            .mdls
            .entry(path.to_owned())
            .or_insert_with(|| {
                self.vfs
                    .read(path)
                    .map_err(|e| e.to_string())
                    .and_then(|b| wpe_mdl::Mdl::parse(&b).map_err(|e| e.to_string()))
                    .inspect_err(|e| tracing::warn!("model {path}: {e}"))
                    .ok()
                    .map(Arc::new)
            })
            .clone()
    }

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
    /// The layer's effects with their pass overrides and index in the scene
    /// object (`None` for the synthetic blend-mode effect).
    fn effects(&self, layer: &ImageLayer) -> Vec<LayerEffect> {
        let mut effects: Vec<LayerEffect> = layer
            .effects
            .iter()
            .zip(&layer.effect_indices)
            .filter_map(|(fx, &index)| {
                wpe_assets::load_json::<Effect>(self.vfs, &fx.file)
                    .inspect_err(|e| tracing::warn!("layer {:?} effect: {e}", layer.name))
                    .ok()
                    .map(|e| (e, fx.passes.clone(), Some(index)))
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
            effects.push((effect, Vec::new(), None));
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
    fn bind_puppet(&mut self, puppet: Option<Arc<LayerPuppet>>, uv_scale: Vec2) {
        self.bind_puppet_mesh(puppet, 0, uv_scale);
    }

    /// Upload the model's bones into the draw of its mesh `mesh` just pushed.
    fn bind_puppet_mesh(&mut self, puppet: Option<Arc<LayerPuppet>>, mesh: usize, uv_scale: Vec2) {
        if let Some(puppet) = puppet.filter(|p| p.puppet.bone_count() > 0) {
            self.bindings.push(Binding::Bones {
                layer: self.building.unwrap_or(-1),
                step: self.steps.len() - 1,
                puppet,
                mesh,
                uv_scale,
                morphed: Default::default(),
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
        let attach = layer
            .attachment
            .clone()
            .zip(layer.parent)
            .map(|(a, p)| (p, a));
        if layer.motion.is_transform_animated() || parallax.is_some() || attach.is_some() {
            self.bindings.push(Binding::Model {
                layer: layer.id,
                step,
                motion: layer.motion.clone(),
                parallax,
                attach,
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
                layer: layer.id,
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
                let uv_scale = r.uv_scale;
                let mesh = match &puppet {
                    Some(p) => p.mesh(r.uv_scale),
                    None => CardMesh {
                        size: layer.size,
                        uv_scale: r.uv_scale,
                    }
                    .into(),
                };
                self.current_pass = Some((None, 0));
                if layer.fullscreen {
                    // Fullscreen materials place their quad in clip space.
                    self.push_draw(
                        offscreen.unwrap_or(TargetId::DEFAULT),
                        r,
                        Mat4::IDENTITY,
                        CardMesh::FULLSCREEN,
                        match offscreen {
                            Some(_) => BlendMode::Normal,
                            None => Self::final_blend(layer),
                        },
                        Camera::Fixed(Mat4::IDENTITY),
                    );
                    self.bind_layer(layer, false, false);
                    return;
                }
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
                        layer_camera(layer),
                    ),
                }
                self.bind_layer(layer, offscreen.is_none(), false);
                self.bind_puppet(puppet, uv_scale);
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
        effects: &[LayerEffect],
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
        let format = self.scene_format;
        let mut prev = self.target(format!("_rt_pingpong_a_{id}"), base_size, format);
        let mut out = self.target(format!("_rt_pingpong_b_{id}"), base_size, format);

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
            (layer_camera(layer), layer.transform, mesh.into())
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
        let uv_scale = base.uv_scale;
        self.current_pass = Some((None, 0));
        self.push_draw(prev, base, model, mesh, base_blend, camera);
        self.bind_puppet(puppet, uv_scale);
        self.bind_layer(layer, layer.compose && !layer.fullscreen, false);

        let first_effect = self.steps.len();
        let mut last_output: Option<usize> = None;
        for (effect, overrides, scene_effect) in effects {
            let scene_effect = *scene_effect;
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
                    target_format(fbo.format.as_deref(), self.scene_format),
                );
                names.insert(format!("_rt_{}", fbo.name), t);
            }
            if let Some(index) = scene_effect {
                for (name, function) in &effect.functions {
                    if function.action != "clear" {
                        tracing::debug!("effect function {name}: {} unsupported", function.action);
                        continue;
                    }
                    let targets = function
                        .fbos
                        .iter()
                        .filter_map(|f| {
                            names
                                .get(&format!("_rt_{}", f.trim_start_matches("_rt_")))
                                .copied()
                        })
                        .collect();
                    if let Some(steps) = self.current_steps() {
                        steps.functions.insert((index, name.clone()), targets);
                    }
                }
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
                        self.current_pass = Some((scene_effect, material_index - 1));
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
        // The passes find the layer on the output through its final draw.
        let half = if layer.fullscreen {
            Vec2::ONE
        } else {
            layer.size / 2.0
        };
        for step in &mut self.steps[first_effect..] {
            if let Step::Draw {
                draw: Draw::Material(d),
                ..
            } = step
            {
                d.effect_of = Some((index, half));
            }
        }
        let Step::Draw {
            draw: Draw::Material(draw),
            ..
        } = &mut self.steps[index]
        else {
            return;
        };
        if !layer.fullscreen {
            draw.camera = layer_camera(layer);
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
/// `Scene` for mip 0 (the frame copy), else `Mip<n>`.
fn mip_name(n: usize) -> String {
    if n == 0 {
        "Scene".to_owned()
    } else {
        format!("Mip{n}")
    }
}

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
