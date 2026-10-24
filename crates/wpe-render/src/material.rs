//! Drawing with compiled Wallpaper Engine shaders.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use glam::{Mat3, Mat4, Vec2, Vec3};
use wpe_shader::reflect::{Scalar, TextureDim, TextureKind, UniformMember};
use wpe_shader::{CompiledShader, Reflection};

use crate::targets::{DepthBuffers, TargetId, Targets};
use crate::{BlendMode, Gpu, GpuTexture};

#[derive(Debug, thiserror::Error)]
pub enum ProgramError {
    #[error("SPIR-V rejected: {0}")]
    Spirv(String),
}

/// A compiled vertex + fragment program with its bind group layout. All
/// resources live in group 0: the uniform block at binding 0 plus a
/// texture/sampler pair per texture slot.
pub struct ShaderProgram {
    label: String,
    vertex: wgpu::ShaderModule,
    fragment: wgpu::ShaderModule,
    pub reflection: Reflection,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    vertex_attributes: Vec<wgpu::VertexAttribute>,
    vertex_stride: u64,
    /// `None` caches a pipeline wgpu rejected.
    pipelines: Mutex<HashMap<PipelineKey, Option<wgpu::RenderPipeline>>>,
}

fn module(gpu: &Gpu, label: &str, words: &[u32]) -> Result<wgpu::ShaderModule, ProgramError> {
    let module =
        wpe_shader::reflect::parse_spirv(words).map_err(|e| ProgramError::Spirv(e.to_string()))?;
    Ok(gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Naga(Cow::Owned(module)),
        }))
}

fn vertex_format(scalar: Scalar, components: u8) -> wgpu::VertexFormat {
    use wgpu::VertexFormat as F;
    match (scalar, components) {
        (Scalar::Int, 1) => F::Sint32,
        (Scalar::Int, 2) => F::Sint32x2,
        (Scalar::Int, 3) => F::Sint32x3,
        (Scalar::Int, _) => F::Sint32x4,
        (Scalar::Uint, 1) => F::Uint32,
        (Scalar::Uint, 2) => F::Uint32x2,
        (Scalar::Uint, 3) => F::Uint32x3,
        (Scalar::Uint, _) => F::Uint32x4,
        (_, 1) => F::Float32,
        (_, 2) => F::Float32x2,
        (_, 3) => F::Float32x3,
        _ => F::Float32x4,
    }
}

impl ShaderProgram {
    pub fn new(gpu: &Gpu, label: &str, shader: &CompiledShader) -> Result<Self, ProgramError> {
        let device = &gpu.device;
        let r = &shader.reflection;
        let mut entries = Vec::new();
        if r.uniform_size > 0 {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            });
        }
        for t in r.textures.values() {
            let view_dimension = match t.dim {
                TextureDim::D2 => wgpu::TextureViewDimension::D2,
                TextureDim::D3 => wgpu::TextureViewDimension::D3,
                TextureDim::Cube => wgpu::TextureViewDimension::Cube,
                TextureDim::D2Array => wgpu::TextureViewDimension::D2Array,
            };
            let (sample_type, sampler) = match t.kind {
                TextureKind::Filterable => (
                    wgpu::TextureSampleType::Float { filterable: true },
                    wgpu::SamplerBindingType::Filtering,
                ),
                TextureKind::Unfilterable => (
                    wgpu::TextureSampleType::Float { filterable: false },
                    wgpu::SamplerBindingType::NonFiltering,
                ),
                TextureKind::Comparison => (
                    wgpu::TextureSampleType::Depth,
                    wgpu::SamplerBindingType::Comparison,
                ),
            };
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: t.texture,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type,
                    view_dimension,
                    multisampled: false,
                },
                count: None,
            });
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: t.sampler,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Sampler(sampler),
                count: None,
            });
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });

        let mut offset = 0;
        let vertex_attributes = r
            .vertex_inputs
            .iter()
            .map(|input| {
                let format = vertex_format(input.scalar, input.components);
                let attr = wgpu::VertexAttribute {
                    format,
                    offset,
                    shader_location: input.location,
                };
                offset += format.size();
                attr
            })
            .collect();

        Ok(Self {
            label: label.to_owned(),
            vertex: module(gpu, label, &shader.vertex)?,
            fragment: module(gpu, label, &shader.fragment)?,
            reflection: shader.reflection.clone(),
            layout,
            pipeline_layout,
            vertex_attributes,
            vertex_stride: offset,
            pipelines: Mutex::new(HashMap::new()),
        })
    }

    fn pipeline(&self, device: &wgpu::Device, key: PipelineKey) -> Option<wgpu::RenderPipeline> {
        let mut pipelines = self.pipelines.lock().unwrap_or_else(|e| e.into_inner());
        pipelines
            .entry(key)
            .or_insert_with(|| {
                let PipelineKey {
                    blend,
                    format,
                    topology,
                    cull,
                    depth,
                } = key;
                let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
                let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("material"),
                    layout: Some(&self.pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &self.vertex,
                        entry_point: None,
                        compilation_options: Default::default(),
                        buffers: &[Some(wgpu::VertexBufferLayout {
                            array_stride: self.vertex_stride.max(4),
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &self.vertex_attributes,
                        })],
                    },
                    primitive: wgpu::PrimitiveState {
                        topology,
                        cull_mode: cull.face(),
                        ..Default::default()
                    },
                    depth_stencil: depth.state(),
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &self.fragment,
                        entry_point: None,
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend: blend.state(),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                });
                match pollster::block_on(scope.pop()) {
                    None => Some(pipeline),
                    Some(e) => {
                        tracing::warn!("{}: pipeline rejected: {e}", self.label);
                        None
                    }
                }
            })
            .clone()
    }
}

/// Everything a material pipeline varies by besides its program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PipelineKey {
    blend: BlendMode,
    format: wgpu::TextureFormat,
    topology: wgpu::PrimitiveTopology,
    cull: CullMode,
    depth: PassDepth,
}

/// Format of the depth buffers 3D draws test against.
pub(crate) const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Which triangles a draw skips, by winding (counter-clockwise is front, as
/// in OpenGL). Materials say `"cullmode": "nocull" | "normal" | "back" | "front"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CullMode {
    #[default]
    None,
    Back,
    Front,
}

impl CullMode {
    /// A material's `cullmode`; `"normal"` culls back faces and anything
    /// unknown culls nothing.
    pub fn parse(value: &str) -> Self {
        match value {
            "normal" | "back" => Self::Back,
            "front" => Self::Front,
            _ => Self::None,
        }
    }

    fn face(self) -> Option<wgpu::Face> {
        match self {
            Self::None => None,
            Self::Back => Some(wgpu::Face::Back),
            Self::Front => Some(wgpu::Face::Front),
        }
    }
}

/// Depth and culling state of a draw. The default (no depth, no culling) is
/// how every 2D layer draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RasterState {
    /// Skip fragments behind what is already drawn (`depthtest`).
    pub depth_test: bool,
    /// Record this draw's depth for later draws (`depthwrite`).
    pub depth_write: bool,
    pub cull: CullMode,
}

impl RasterState {
    /// Whether the draw needs a depth buffer on its target.
    pub fn uses_depth(&self) -> bool {
        self.depth_test || self.depth_write
    }
}

/// The depth state a pipeline is built with: from its draw, and whether the
/// pass it is recorded in has a depth attachment. Every pipeline in a pass
/// with one must declare it, so draws without depth get an always-pass,
/// no-write state there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum PassDepth {
    /// The pass has no depth attachment.
    #[default]
    None,
    Attached {
        test: bool,
        write: bool,
    },
}

impl PassDepth {
    pub(crate) fn new(raster: RasterState, attached: bool) -> Self {
        if attached {
            Self::Attached {
                test: raster.depth_test,
                write: raster.depth_write,
            }
        } else {
            Self::None
        }
    }

    pub(crate) fn state(self) -> Option<wgpu::DepthStencilState> {
        let Self::Attached { test, write } = self else {
            return None;
        };
        Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(write),
            depth_compare: Some(if test {
                wgpu::CompareFunction::LessEqual
            } else {
                wgpu::CompareFunction::Always
            }),
            stencil: Default::default(),
            bias: Default::default(),
        })
    }
}

/// Named per-vertex attribute streams, matched to shader inputs by name
/// (`a_Position`, `a_TexCoord`, ...), drawn as an indexed or plain triangle list.
#[derive(Debug, Clone, Default)]
pub struct MeshData {
    pub vertex_count: u32,
    /// Attribute name, component count, and `vertex_count × components` values.
    pub attributes: Vec<(String, u8, Vec<f32>)>,
    pub indices: Option<Vec<u32>>,
}

/// Geometry of a material draw.
#[derive(Debug, Clone)]
pub enum Mesh {
    /// A textured card (triangle strip).
    Card(CardMesh),
    Triangles(MeshData),
}

impl From<CardMesh> for Mesh {
    fn from(card: CardMesh) -> Self {
        Self::Card(card)
    }
}

impl Mesh {
    fn topology(&self) -> wgpu::PrimitiveTopology {
        match self {
            Self::Card(_) => wgpu::PrimitiveTopology::TriangleStrip,
            Self::Triangles(_) => wgpu::PrimitiveTopology::TriangleList,
        }
    }

    pub(crate) fn data(&self) -> std::borrow::Cow<'_, MeshData> {
        match self {
            Self::Card(card) => {
                let corners = card.corners();
                std::borrow::Cow::Owned(MeshData {
                    vertex_count: 4,
                    attributes: vec![
                        (
                            "a_Position".to_owned(),
                            3,
                            corners.iter().flat_map(|c| c.0).collect(),
                        ),
                        (
                            "a_TexCoord".to_owned(),
                            2,
                            corners.iter().flat_map(|c| c.1).collect(),
                        ),
                    ],
                    indices: None,
                })
            }
            Self::Triangles(data) => std::borrow::Cow::Borrowed(data),
        }
    }
}

/// A card's vertices: centred on the origin, `size` pixels, UVs scaled
/// onto the picture inside a padded texture.
#[derive(Debug, Clone, Copy)]
pub struct CardMesh {
    pub size: Vec2,
    pub uv_scale: Vec2,
}

impl CardMesh {
    /// The full-screen `-1..1` quad effect passes draw.
    pub const FULLSCREEN: Self = Self {
        size: Vec2::new(2.0, 2.0),
        uv_scale: Vec2::ONE,
    };

    fn corners(&self) -> [([f32; 3], [f32; 2]); 4] {
        let (l, r) = (-self.size.x / 2.0, self.size.x / 2.0);
        let (b, t) = (-self.size.y / 2.0, self.size.y / 2.0);
        let (u, v) = (self.uv_scale.x, self.uv_scale.y);
        [
            ([l, t, 0.0], [0.0, 0.0]),
            ([l, b, 0.0], [0.0, v]),
            ([r, t, 0.0], [u, 0.0]),
            ([r, b, 0.0], [u, v]),
        ]
    }
}

/// Bytes of the uniform block, written by member name.
pub struct UniformBlock {
    data: Vec<u8>,
}

impl UniformBlock {
    fn new(size: u32) -> Self {
        Self {
            data: vec![0; size as usize],
        }
    }

    /// Write `values` to the member, converting to its scalar type and
    /// padding matrix columns and array elements per std140. Extra values
    /// are dropped; missing ones stay as they were.
    fn write(&mut self, member: &UniformMember, values: &[f32]) {
        let ty = member.ty;
        let per_element = (ty.rows as usize) * (ty.columns as usize);
        let elem_stride = if ty.array_stride > 0 {
            ty.array_stride as usize
        } else {
            member.size as usize
        };
        // A `vec3` array may be given as `vec4`s (a fourth value per element
        // that other shaders declaring `vec4` read), which is then skipped.
        let given = if ty.rows == 3 && ty.columns == 1 && values.len() == ty.count as usize * 4 {
            4
        } else {
            per_element.max(1)
        };
        for (i, v) in values.iter().enumerate() {
            let elem = i / given;
            if elem >= ty.count as usize {
                break;
            }
            let within = i % given;
            if within >= per_element {
                continue;
            }
            let (col, row) = (within / ty.rows as usize, within % ty.rows as usize);
            let at = member.offset as usize
                + elem * elem_stride
                + col * ty.column_stride() as usize
                + row * 4;
            let Some(slot) = self.data.get_mut(at..at + 4) else {
                break;
            };
            let bytes = match ty.scalar {
                Scalar::Float => v.to_le_bytes(),
                Scalar::Int => (*v as i32).to_le_bytes(),
                Scalar::Uint => (*v as u32).to_le_bytes(),
                Scalar::Bool => ((*v != 0.0) as u32).to_le_bytes(),
            };
            slot.copy_from_slice(&bytes);
        }
    }
}

/// Values every material may read, refreshed each frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameGlobals {
    pub time: f32,
    /// Fraction of the day elapsed, 0..1.
    pub daytime: f32,
    /// Cursor position over the output, 0..1 with Y down.
    pub pointer: Vec2,
    pub parallax: Vec2,
    pub view_proj: Mat4,
    /// Size of the output being drawn into.
    pub screen: Vec2,
    pub eye: Vec3,
    /// View-projection of the scene's perspective camera ([`Camera::Perspective`]).
    pub perspective: Mat4,
    /// Its view and projection halves, for programs that declare them.
    pub perspective_view: Mat4,
    pub perspective_proj: Mat4,
    /// Its position in world space (`g_EyePosition`).
    pub perspective_eye: Vec3,
    /// The `"reflected_perspective"` camera that mirror draws (planar
    /// reflections into `_rt_Reflection`) use: view, projection, their
    /// product and eye.
    pub reflected_view: Mat4,
    pub reflected_proj: Mat4,
    pub reflected: Mat4,
    pub reflected_eye: Vec3,
}

/// Where a texture slot's image comes from.
#[derive(Clone)]
pub enum TexRef {
    Asset(Arc<GpuTexture>),
    Target(TargetId),
    /// The depth buffer of a target, as drawn so far this frame (cleared to
    /// the far plane when nothing drew depth there yet). Only programs that
    /// read it with `texelFetch` or depth comparison can bind it.
    Depth(TargetId),
    /// `_rt_shadowAtlas`, the shadow maps of the scene's shadow-casting
    /// lights ([`crate::SceneRenderer::set_shadow_lights`]).
    ShadowAtlas,
}

/// One frame of a sprite-sheet animation.
#[derive(Debug, Clone, Copy)]
pub struct SpriteFrame {
    /// Index into [`SpriteAnimation::pages`].
    pub page: usize,
    pub duration: f32,
    /// `g_Texture<N>Rotation`: the frame's x and y axes in UV units.
    pub rotation: [f32; 4],
    /// `g_Texture<N>Translation`: the frame's origin in UV units.
    pub translation: [f32; 2],
}

/// A sprite sheet bound to one texture slot.
#[derive(Clone)]
pub struct SpriteAnimation {
    pub slot: usize,
    pub frames: Vec<SpriteFrame>,
    /// One texture per sheet page.
    pub pages: Vec<Arc<GpuTexture>>,
}

impl SpriteAnimation {
    /// Seconds per loop through all frames.
    pub fn duration(&self) -> f32 {
        self.frames.iter().map(|f| f.duration.max(0.0)).sum()
    }

    fn frame_at(&self, time: f32) -> Option<&SpriteFrame> {
        let total: f32 = self.frames.iter().map(|f| f.duration.max(0.0)).sum();
        if total <= 0.0 {
            return self.frames.first();
        }
        let mut t = time.rem_euclid(total);
        self.frames.iter().find(|f| {
            t -= f.duration.max(0.0);
            t < 0.0
        })
    }
}

/// The view-projection a draw uses.
#[derive(Debug, Clone, Copy)]
pub enum Camera {
    /// The scene camera, fitted to the output each frame.
    Scene,
    /// A fixed matrix (layer-local or effect cameras).
    Fixed(Mat4),
    /// The scene's perspective camera (3D models), from
    /// [`FrameGlobals::perspective`].
    Perspective,
}

/// A draw's camera resolved for one frame.
struct View {
    view: Mat4,
    proj: Mat4,
    view_proj: Mat4,
    eye: Vec3,
}

impl Camera {
    /// Orthographic cameras have no separate view: it is the identity and
    /// the projection is the whole matrix.
    fn resolve(self, globals: &FrameGlobals) -> View {
        let flat = |m: Mat4| View {
            view: Mat4::IDENTITY,
            proj: m,
            view_proj: m,
            eye: globals.eye,
        };
        match self {
            Self::Scene => flat(globals.view_proj),
            Self::Fixed(m) => flat(m),
            Self::Perspective => View {
                view: globals.perspective_view,
                proj: globals.perspective_proj,
                view_proj: globals.perspective,
                eye: globals.perspective_eye,
            },
        }
    }

    /// The camera mirror draws use.
    fn reflected(globals: &FrameGlobals) -> View {
        View {
            view: globals.reflected_view,
            proj: globals.reflected_proj,
            view_proj: globals.reflected,
            eye: globals.reflected_eye,
        }
    }
}

/// Inverse transpose of `model`'s linear part, scaled back to unit
/// determinant: normals follow a non-uniform scale's shape but not its size,
/// so the tangents stock shaders never renormalize keep their length.
fn normal_matrix(model: Mat4) -> Mat3 {
    let linear = Mat3::from_mat4(model);
    let det = linear.determinant();
    if det.abs() <= 1e-12 {
        return linear;
    }
    linear.inverse().transpose() * det.abs().cbrt()
}

/// Write the uniforms that depend on the camera.
fn write_camera(
    block: &mut UniformBlock,
    r: &Reflection,
    model: Mat4,
    perspective: bool,
    camera: &View,
) {
    let view_proj = camera.view_proj;
    let mvp = view_proj * model;
    let mat4 = |m: Mat4| m.to_cols_array().to_vec();
    let values: [(&str, Vec<f32>); 16] = [
        ("g_ModelViewProjectionMatrix", mat4(mvp)),
        ("g_ModelViewProjectionMatrixInverse", mat4(mvp.inverse())),
        ("g_ModelMatrix", mat4(model)),
        ("g_ModelMatrixInverse", mat4(model.inverse())),
        ("g_AltModelMatrix", mat4(model)),
        ("g_ViewProjectionMatrix", mat4(view_proj)),
        ("g_ViewProjectionMatrixInverse", mat4(view_proj.inverse())),
        ("g_AltViewProjectionMatrix", mat4(view_proj)),
        ("g_ViewMatrix", mat4(camera.view)),
        ("g_ViewMatrixInverse", mat4(camera.view.inverse())),
        ("g_ProjectionMatrix", mat4(camera.proj)),
        ("g_ProjectionMatrixInverse", mat4(camera.proj.inverse())),
        ("g_ModelViewMatrix", mat4(camera.view * model)),
        (
            "g_ModelViewMatrixInverse",
            mat4((camera.view * model).inverse()),
        ),
        ("g_EyePosition", camera.eye.to_array().to_vec()),
        // Volumetric light passes unproject screen positions and depths
        // back into the world with it.
        ("g_EffectModelMatrix", mat4(view_proj.inverse())),
    ];
    for (name, v) in values {
        if let Some(member) = r.uniforms.get(name) {
            block.write(member, &v);
        }
    }
    // The camera's axes in world space (billboards, rim lighting). Other
    // cameras leave them to the draw's constants (particles set them).
    if perspective {
        let rows = camera.view.transpose();
        for (name, axis) in [
            ("g_ViewRight", rows.x_axis),
            ("g_ViewUp", rows.y_axis),
            ("g_ViewForward", -rows.z_axis),
        ] {
            if let Some(member) = r.uniforms.get(name) {
                block.write(member, &axis.truncate().normalize_or_zero().to_array());
            }
        }
    }
}

/// `g_LightsPosition` (a `vec4` per light) with every light mirrored below
/// the y = 0 floor, for the reflected camera.
fn mirror_lights(positions: &[f32]) -> Vec<f32> {
    let mut mirrored = positions.to_vec();
    for light in mirrored.chunks_mut(4) {
        if let Some(y) = light.get_mut(1) {
            *y = -*y;
        }
    }
    mirrored
}

/// One draw with a compiled program.
pub struct MaterialDraw {
    pub program: Arc<ShaderProgram>,
    /// Texture per `g_Texture<N>` slot; `None` binds a placeholder.
    pub textures: Vec<Option<TexRef>>,
    /// Static uniform values (defaults, material constants, layer values).
    pub constants: HashMap<String, Vec<f32>>,
    pub model: Mat4,
    pub mesh: Mesh,
    pub blend: BlendMode,
    pub camera: Camera,
    pub sprites: Vec<SpriteAnimation>,
    /// Depth testing/writing and face culling.
    pub raster: RasterState,
}

pub(crate) struct PreparedMaterialDraw {
    pub(crate) draw: MaterialDraw,
    /// Uniform values replaced every frame (animations, scripts).
    pub(crate) overrides: HashMap<String, Vec<f32>>,
    /// Sprite-sheet time instead of the scene's (scripts' texture animation).
    pub(crate) sprite_time: Option<f32>,
    block: UniformBlock,
    uniforms: Option<wgpu::Buffer>,
    geometry: Option<Geometry>,
    /// The mesh changed and must be re-uploaded.
    mesh_dirty: bool,
    /// Bumped whenever the mesh is replaced (shadow casters re-upload it).
    pub(crate) mesh_version: u64,
    /// Rebuilt when render targets are reallocated.
    bind_group: Option<(u64, wgpu::BindGroup)>,
    /// The same draw seen by the reflected camera, when mirrored.
    mirror: Option<Mirror>,
}

/// A material draw's uniforms and bindings for its mirror draw: the same
/// geometry and textures with the reflected camera and mirrored lights.
struct Mirror {
    block: UniformBlock,
    uniforms: wgpu::Buffer,
    bind_group: Option<(u64, wgpu::BindGroup)>,
}

/// Placeholder textures bound to unused or missing slots.
pub(crate) struct Placeholders {
    d2: GpuTexture,
    d3: GpuTexture,
    /// A 1×1 depth texture at the far plane, for missing depth buffers.
    depth: wgpu::TextureView,
    /// For textures read without filtering.
    nearest: wgpu::Sampler,
    /// Depth comparison passing when the reference is nearer or equal.
    pub(crate) comparison: wgpu::Sampler,
}

/// What material bindings are resolved against.
pub(crate) struct Bindables<'a> {
    pub placeholders: &'a Placeholders,
    pub targets: &'a Targets,
    pub depth: &'a DepthBuffers,
    pub shadow_atlas: Option<&'a wgpu::TextureView>,
    /// Changes whenever any texture above is (re)allocated.
    pub generation: u64,
}

impl Placeholders {
    pub fn new(gpu: &Gpu) -> Self {
        let make = |dim: wgpu::TextureDimension| {
            let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("placeholder"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: dim,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            gpu.queue.write_texture(
                texture.as_image_copy(),
                &[255; 4],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            GpuTexture {
                view: texture.create_view(&Default::default()),
                texture,
                sampler: gpu.device.create_sampler(&Default::default()),
                width: 1,
                height: 1,
                map_width: 1,
                map_height: 1,
            }
        };
        let depth = gpu
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("placeholder depth"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default());
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("placeholder depth"),
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        gpu.queue.submit([encoder.finish()]);
        Self {
            d2: make(wgpu::TextureDimension::D2),
            d3: make(wgpu::TextureDimension::D3),
            depth,
            nearest: gpu.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("nearest"),
                ..Default::default()
            }),
            comparison: gpu.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("comparison"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                compare: Some(wgpu::CompareFunction::LessEqual),
                ..Default::default()
            }),
        }
    }
}

fn slot_index(name: &str) -> Option<usize> {
    name.strip_prefix("g_Texture")?.parse().ok()
}

/// `{texture w, texture h, picture w, picture h}` as WE's `g_Texture<N>Resolution`.
fn resolution(t: &GpuTexture) -> [f32; 4] {
    let padded = t.width * t.height > t.map_width * t.map_height;
    let (w, h) = if padded {
        (t.width, t.height)
    } else {
        (t.map_width, t.map_height)
    };
    [w as f32, h as f32, t.map_width as f32, t.map_height as f32]
}

impl PreparedMaterialDraw {
    pub fn new(gpu: &Gpu, draw: MaterialDraw) -> Self {
        let r = &draw.program.reflection;
        let mut block = UniformBlock::new(r.uniform_size);
        let mut constants = draw.constants.clone();
        for slot in 0..draw.textures.len().max(16) {
            if let Some(Some(TexRef::Asset(t))) = draw.textures.get(slot) {
                constants
                    .entry(format!("g_Texture{slot}Resolution"))
                    .or_insert_with(|| resolution(t).to_vec());
            }
            constants
                .entry(format!("g_Texture{slot}Rotation"))
                .or_insert_with(|| vec![1.0, 0.0, 0.0, 1.0]);
            constants
                .entry(format!("g_Texture{slot}Translation"))
                .or_insert_with(|| vec![0.0, 0.0]);
        }
        for (name, values) in &constants {
            if let Some(member) = r.uniforms.get(name) {
                block.write(member, values);
            }
        }

        let uniforms = (r.uniform_size > 0).then(|| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("material uniforms"),
                size: r.uniform_size as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let geometry = Geometry::upload(gpu, &draw.program, &draw.mesh, None);
        Self {
            draw,
            overrides: HashMap::new(),
            sprite_time: None,
            block,
            uniforms,
            geometry,
            mesh_dirty: false,
            mesh_version: 0,
            bind_group: None,
            mirror: None,
        }
    }

    /// Also keep uniforms for a mirror draw ([`Self::record_mirror`]).
    pub fn enable_mirror(&mut self, gpu: &Gpu) {
        if self.mirror.is_some() || self.uniforms.is_none() {
            return;
        }
        let size = self.block.data.len();
        self.mirror = Some(Mirror {
            block: UniformBlock {
                data: vec![0; size],
            },
            uniforms: gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mirror uniforms"),
                size: size as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            bind_group: None,
        });
    }

    fn bind(&mut self, gpu: &Gpu, res: &Bindables) {
        let current = |group: &Option<(u64, wgpu::BindGroup)>| {
            group
                .as_ref()
                .is_some_and(|(generation, _)| *generation == res.generation)
        };
        if !current(&self.bind_group) {
            let group = self.bind_group_with(gpu, self.uniforms.as_ref(), res);
            self.bind_group = Some((res.generation, group));
        }
        if let Some(mirror) = &self.mirror
            && !current(&mirror.bind_group)
        {
            let group = self.bind_group_with(gpu, Some(&mirror.uniforms), res);
            if let Some(mirror) = &mut self.mirror {
                mirror.bind_group = Some((res.generation, group));
            }
        }
    }

    fn bind_group_with(
        &self,
        gpu: &Gpu,
        uniforms: Option<&wgpu::Buffer>,
        res: &Bindables,
    ) -> wgpu::BindGroup {
        let (placeholders, targets) = (res.placeholders, res.targets);
        let r = &self.draw.program.reflection;
        let mut entries = Vec::new();
        if let Some(buf) = uniforms {
            entries.push(wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            });
        }
        for (name, binding) in &r.textures {
            let wants_3d = binding.dim == TextureDim::D3;
            let bound = slot_index(name)
                .and_then(|i| self.draw.textures.get(i))
                .and_then(Option::as_ref);
            let depth_of = |bound: Option<&TexRef>| match bound {
                Some(TexRef::Depth(id)) => res.depth.view(*id),
                Some(TexRef::ShadowAtlas) => res.shadow_atlas,
                _ => None,
            };
            let (view, sampler) = match (binding.kind, bound) {
                (TextureKind::Comparison, _) => (
                    depth_of(bound).unwrap_or(&placeholders.depth),
                    &placeholders.comparison,
                ),
                (_, Some(TexRef::Asset(t)))
                    if (t.texture.dimension() == wgpu::TextureDimension::D3) == wants_3d =>
                {
                    (&t.view, &t.sampler)
                }
                (_, Some(TexRef::Target(id))) if !wants_3d => {
                    (&targets.get(*id).view, &targets.sampler)
                }
                (TextureKind::Unfilterable, Some(TexRef::Depth(_) | TexRef::ShadowAtlas))
                    if !wants_3d =>
                {
                    (
                        depth_of(bound).unwrap_or(&placeholders.depth),
                        &placeholders.nearest,
                    )
                }
                _ => {
                    let p = if wants_3d {
                        &placeholders.d3
                    } else {
                        &placeholders.d2
                    };
                    (&p.view, &p.sampler)
                }
            };
            let sampler = match binding.kind {
                TextureKind::Unfilterable => &placeholders.nearest,
                _ => sampler,
            };
            entries.push(wgpu::BindGroupEntry {
                binding: binding.texture,
                resource: wgpu::BindingResource::TextureView(view),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: binding.sampler,
                resource: wgpu::BindingResource::Sampler(sampler),
            });
        }
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("material"),
            layout: &self.draw.program.layout,
            entries: &entries,
        })
    }

    /// Refresh per-frame uniforms, upload the block and (re)bind textures.
    pub fn update(
        &mut self,
        gpu: &Gpu,
        globals: &FrameGlobals,
        shared: &HashMap<String, Vec<f32>>,
        res: &Bindables,
    ) {
        let targets = res.targets;
        for anim in &self.draw.sprites {
            let Some(frame) = anim.frame_at(self.sprite_time.unwrap_or(globals.time)) else {
                continue;
            };
            let Some(page) = anim.pages.get(frame.page) else {
                continue;
            };
            let bound = self.draw.textures.get(anim.slot).and_then(Option::as_ref);
            if !matches!(bound, Some(TexRef::Asset(t)) if Arc::ptr_eq(t, page)) {
                if self.draw.textures.len() <= anim.slot {
                    self.draw.textures.resize(anim.slot + 1, None);
                }
                self.draw.textures[anim.slot] = Some(TexRef::Asset(page.clone()));
                self.bind_group = None;
            }
            let r = &self.draw.program.reflection;
            for (name, values) in [
                (
                    format!("g_Texture{}Rotation", anim.slot),
                    &frame.rotation[..],
                ),
                (
                    format!("g_Texture{}Translation", anim.slot),
                    &frame.translation[..],
                ),
            ] {
                if let Some(member) = r.uniforms.get(&name) {
                    self.block.write(member, values);
                }
            }
        }
        if self.mesh_dirty {
            self.mesh_dirty = false;
            let previous = self.geometry.take();
            self.geometry = Geometry::upload(gpu, &self.draw.program, &self.draw.mesh, previous);
        }
        self.bind(gpu, res);
        let Some(buffer) = &self.uniforms else { return };
        let r = &self.draw.program.reflection;
        let model = self.draw.model;
        let perspective = matches!(self.draw.camera, Camera::Perspective);
        write_camera(
            &mut self.block,
            r,
            model,
            perspective,
            &self.draw.camera.resolve(globals),
        );
        // 2D layers keep the plain inverse transpose they always had.
        let normal = if perspective {
            normal_matrix(model)
        } else {
            Mat3::from_mat4(model.inverse().transpose())
        };
        let mat4 = |m: Mat4| m.to_cols_array().to_vec();
        let values: [(&str, Vec<f32>); 8] = [
            ("g_NormalModelMatrix", normal.to_cols_array().to_vec()),
            ("g_Time", vec![globals.time]),
            ("g_DayTime", vec![globals.daytime]),
            ("g_PointerPosition", globals.pointer.to_array().to_vec()),
            ("g_ParallaxPosition", globals.parallax.to_array().to_vec()),
            (
                "g_TexelSize",
                (Vec2::ONE / globals.screen).to_array().to_vec(),
            ),
            (
                "g_TexelSizeHalf",
                (Vec2::splat(0.5) / globals.screen).to_array().to_vec(),
            ),
            (
                "g_Screen",
                vec![
                    globals.screen.x,
                    globals.screen.y,
                    globals.screen.x / globals.screen.y,
                ],
            ),
        ];
        for (name, v) in values {
            if let Some(member) = r.uniforms.get(name) {
                self.block.write(member, &v);
            }
        }
        for name in [
            "g_EffectTextureProjectionMatrix",
            "g_EffectTextureProjectionMatrixInverse",
        ] {
            if let Some(member) = r.uniforms.get(name)
                && !self.draw.constants.contains_key(name)
            {
                self.block.write(member, &mat4(Mat4::IDENTITY));
            }
        }
        for (name, v) in shared.iter().chain(&self.overrides) {
            if let Some(member) = r.uniforms.get(name) {
                self.block.write(member, v);
            }
        }
        for (slot, tex) in self.draw.textures.iter().enumerate() {
            if let Some(TexRef::Target(id) | TexRef::Depth(id)) = tex
                && let Some(member) = r.uniforms.get(&format!("g_Texture{slot}Resolution"))
            {
                let (w, h) = targets.get(*id).size;
                let (w, h) = (w as f32, h as f32);
                self.block.write(member, &[w, h, w, h]);
            }
        }
        gpu.queue.write_buffer(buffer, 0, &self.block.data);
        if let Some(mirror) = &mut self.mirror {
            // The same values seen from below the floor, lit by the lights'
            // mirror images.
            mirror.block.data.copy_from_slice(&self.block.data);
            write_camera(
                &mut mirror.block,
                r,
                model,
                true,
                &Camera::reflected(globals),
            );
            if let (Some(member), Some(lights)) = (
                r.uniforms.get("g_LightsPosition"),
                shared.get("g_LightsPosition"),
            ) {
                mirror.block.write(member, &mirror_lights(lights));
            }
            gpu.queue
                .write_buffer(&mirror.uniforms, 0, &mirror.block.data);
        }
    }

    /// Draw into `pass`, whose colour attachment is `format`;
    /// `depth_attached` when it has a depth attachment too.
    pub fn record(
        &self,
        device: &wgpu::Device,
        pass: &mut wgpu::RenderPass,
        format: wgpu::TextureFormat,
        depth_attached: bool,
    ) {
        self.record_with(
            device,
            pass,
            format,
            depth_attached,
            self.bind_group.as_ref(),
        );
    }

    /// [`Self::record`] with the reflected camera ([`Self::enable_mirror`]).
    /// Both the view's and the projection's y flips reverse the winding, so
    /// culling stays as it is.
    pub fn record_mirror(
        &self,
        device: &wgpu::Device,
        pass: &mut wgpu::RenderPass,
        format: wgpu::TextureFormat,
        depth_attached: bool,
    ) {
        let group = self.mirror.as_ref().and_then(|m| m.bind_group.as_ref());
        self.record_with(device, pass, format, depth_attached, group);
    }

    fn record_with(
        &self,
        device: &wgpu::Device,
        pass: &mut wgpu::RenderPass,
        format: wgpu::TextureFormat,
        depth_attached: bool,
        group: Option<&(u64, wgpu::BindGroup)>,
    ) {
        let Some((_, group)) = group else {
            return;
        };
        let Some(geometry) = &self.geometry else {
            return;
        };
        let key = PipelineKey {
            blend: self.draw.blend,
            format,
            topology: self.draw.mesh.topology(),
            cull: self.draw.raster.cull,
            depth: PassDepth::new(self.draw.raster, depth_attached),
        };
        let Some(pipeline) = self.draw.program.pipeline(device, key) else {
            return;
        };
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, group, &[]);
        geometry.draw(pass);
    }

    /// Bind `texture` to slot `slot` from the next update.
    pub fn set_texture(&mut self, slot: usize, texture: Arc<GpuTexture>) {
        if self.draw.textures.len() <= slot {
            self.draw.textures.resize(slot + 1, None);
        }
        let resolution = resolution(&texture);
        self.draw.textures[slot] = Some(TexRef::Asset(texture));
        self.overrides
            .insert(format!("g_Texture{slot}Resolution"), resolution.to_vec());
        self.bind_group = None;
    }

    /// Replace the mesh; it is uploaded on the next update.
    pub fn set_mesh(&mut self, mesh: Mesh) {
        self.draw.mesh = mesh;
        self.mesh_dirty = true;
        self.mesh_version += 1;
    }
}

/// Vertex (and index) buffers holding a mesh in a program's input layout.
struct Geometry {
    vertices: wgpu::Buffer,
    indices: Option<wgpu::Buffer>,
    /// Vertices, or indices when indexed.
    count: u32,
}

impl Geometry {
    /// Upload `mesh`, reusing `previous` buffers when they are large enough.
    /// `None` for an empty mesh.
    fn upload(
        gpu: &Gpu,
        program: &ShaderProgram,
        mesh: &Mesh,
        previous: Option<Geometry>,
    ) -> Option<Self> {
        let data = mesh.data();
        let bytes = vertex_bytes(program, &data);
        if bytes.is_empty() {
            return None;
        }
        let buffer = |old: Option<wgpu::Buffer>, size: usize, usage, label| match old
            .filter(|b| b.size() >= size as u64)
        {
            Some(b) => b,
            None => gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (size as u64).next_power_of_two().max(64),
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
        };
        let (old_vertices, old_indices) = match previous {
            Some(g) => (Some(g.vertices), g.indices),
            None => (None, None),
        };
        let vertices = buffer(
            old_vertices,
            bytes.len(),
            wgpu::BufferUsages::VERTEX,
            "mesh vertices",
        );
        gpu.queue.write_buffer(&vertices, 0, &bytes);
        let (indices, count) = match &data.indices {
            Some(idx) if !idx.is_empty() => {
                let idx_bytes: &[u8] = bytemuck::cast_slice(idx);
                let b = buffer(
                    old_indices,
                    idx_bytes.len(),
                    wgpu::BufferUsages::INDEX,
                    "mesh indices",
                );
                gpu.queue.write_buffer(&b, 0, idx_bytes);
                (Some(b), idx.len() as u32)
            }
            Some(_) => return None,
            None => (None, data.vertex_count),
        };
        Some(Self {
            vertices,
            indices,
            count,
        })
    }

    fn draw(&self, pass: &mut wgpu::RenderPass) {
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        match &self.indices {
            Some(indices) => {
                pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..self.count, 0, 0..1);
            }
            None => pass.draw(0..self.count, 0..1),
        }
    }
}

/// Interleave `mesh`'s streams in `program`'s vertex layout. Inputs the mesh
/// lacks get zero, except `a_Color` which defaults to opaque white.
fn vertex_bytes(program: &ShaderProgram, mesh: &MeshData) -> Vec<u8> {
    let inputs = &program.reflection.vertex_inputs;
    let stride = program.vertex_stride.max(4) as usize;
    let count = mesh.vertex_count as usize;
    let mut data = vec![0u8; stride * count];
    for (input, attr) in inputs.iter().zip(&program.vertex_attributes) {
        let stream = mesh
            .attributes
            .iter()
            .find(|(name, ..)| *name == input.name);
        for v in 0..count {
            let at = v * stride + attr.offset as usize;
            for c in 0..input.components as usize {
                let value = match stream {
                    Some((_, n, values)) if c < *n as usize => {
                        values.get(v * *n as usize + c).copied().unwrap_or(0.0)
                    }
                    Some(_) if input.name == "a_Position" && c == 3 => 1.0,
                    None if input.name == "a_Color" => 1.0,
                    _ => 0.0,
                };
                let bytes = match input.scalar {
                    Scalar::Float | Scalar::Bool => value.to_le_bytes(),
                    Scalar::Int => (value as i32).to_le_bytes(),
                    Scalar::Uint => (value as u32).to_le_bytes(),
                };
                data[at + c * 4..at + c * 4 + 4].copy_from_slice(&bytes);
            }
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use wpe_shader::reflect::UniformType;

    fn member(offset: u32, rows: u8, columns: u8, count: u32, array_stride: u32) -> UniformMember {
        UniformMember {
            offset,
            size: 0,
            ty: UniformType {
                scalar: Scalar::Float,
                rows,
                columns,
                count,
                array_stride,
            },
        }
    }

    fn floats(block: &UniformBlock) -> Vec<f32> {
        block
            .data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn writes_std140_layouts() {
        let mut b = UniformBlock::new(64);
        b.write(&member(4, 3, 1, 1, 0), &[1.0, 2.0, 3.0, 9.0]);
        assert_eq!(&floats(&b)[..5], &[0.0, 1.0, 2.0, 3.0, 0.0]);

        let mut b = UniformBlock::new(48);
        b.write(
            &member(0, 3, 3, 1, 0),
            &[1., 2., 3., 4., 5., 6., 7., 8., 9.],
        );
        assert_eq!(floats(&b), [1., 2., 3., 0., 4., 5., 6., 0., 7., 8., 9., 0.]);

        let mut b = UniformBlock::new(32);
        b.write(&member(0, 1, 1, 2, 16), &[5.0, 6.0]);
        assert_eq!(floats(&b), [5., 0., 0., 0., 6., 0., 0., 0.]);

        // `vec3[2]` given as `vec4`s: the fourth values are skipped.
        let mut b = UniformBlock::new(32);
        b.write(&member(0, 3, 1, 2, 16), &[1., 2., 3., 9., 4., 5., 6., 9.]);
        assert_eq!(floats(&b), [1., 2., 3., 0., 4., 5., 6., 0.]);
    }

    #[test]
    fn parses_cull_modes() {
        assert_eq!(CullMode::parse("nocull"), CullMode::None);
        assert_eq!(CullMode::parse("normal"), CullMode::Back);
        assert_eq!(CullMode::parse("back"), CullMode::Back);
        assert_eq!(CullMode::parse("front"), CullMode::Front);
        assert_eq!(CullMode::parse("weird"), CullMode::None);
    }

    #[test]
    fn pass_depth_keys_and_states() {
        let flat = RasterState::default();
        let model = RasterState {
            depth_test: true,
            depth_write: true,
            cull: CullMode::Back,
        };
        assert!(!flat.uses_depth() && model.uses_depth());

        // Without an attachment nothing declares depth, as before.
        assert_eq!(PassDepth::new(model, false), PassDepth::None);
        assert!(PassDepth::None.state().is_none());

        // In a depth pass a 2D draw passes always and writes nothing.
        let state = PassDepth::new(flat, true).state().unwrap();
        assert_eq!(state.format, DEPTH_FORMAT);
        assert_eq!(state.depth_compare, Some(wgpu::CompareFunction::Always));
        assert_eq!(state.depth_write_enabled, Some(false));
        let state = PassDepth::new(model, true).state().unwrap();
        assert_eq!(state.depth_compare, Some(wgpu::CompareFunction::LessEqual));
        assert_eq!(state.depth_write_enabled, Some(true));

        let key = |cull, depth| PipelineKey {
            blend: BlendMode::Normal,
            format: wgpu::TextureFormat::Rgba8Unorm,
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull,
            depth,
        };
        let base = key(CullMode::None, PassDepth::None);
        assert_ne!(base, key(CullMode::Back, PassDepth::None));
        assert_ne!(base, key(CullMode::None, PassDepth::new(flat, true)));
        assert_ne!(
            key(CullMode::None, PassDepth::new(flat, true)),
            key(CullMode::None, PassDepth::new(model, true))
        );
    }

    #[test]
    fn perspective_camera_resolves_from_globals() {
        let view = Mat4::from_translation(Vec3::new(0.0, 0.0, -5.0));
        let proj = Mat4::from_scale(Vec3::new(1.5, 1.0, -0.5));
        let globals = FrameGlobals {
            view_proj: Mat4::from_scale(Vec3::splat(2.0)),
            eye: Vec3::new(1.0, 2.0, 3.0),
            perspective: proj * view,
            perspective_view: view,
            perspective_proj: proj,
            perspective_eye: Vec3::new(0.0, 0.0, 5.0),
            ..Default::default()
        };
        let v = Camera::Perspective.resolve(&globals);
        assert_eq!(v.view_proj, proj * view);
        assert_eq!((v.view, v.proj), (view, proj));
        assert_eq!(v.eye, globals.perspective_eye);

        let v = Camera::Scene.resolve(&globals);
        assert_eq!((v.view, v.proj), (Mat4::IDENTITY, globals.view_proj));
        assert_eq!(v.eye, globals.eye);
    }

    #[test]
    fn mirrored_lights_flip_height_only() {
        assert_eq!(
            mirror_lights(&[1.0, 2.0, 3.0, 0.5, -4.0, -5.0, 6.0, 1.0]),
            [1.0, -2.0, 3.0, 0.5, -4.0, 5.0, 6.0, 1.0]
        );
    }

    #[test]
    fn normal_matrix_has_unit_determinant() {
        let model = Mat4::from_scale(Vec3::new(2.0, 4.0, 8.0));
        let n = normal_matrix(model);
        assert!((n.determinant() - 1.0).abs() < 1e-5);
        // Shape of the inverse scale: x stretched most.
        assert!((n.x_axis.x / n.z_axis.z - 4.0).abs() < 1e-5);
        // A flattened model stays finite.
        let flat = normal_matrix(Mat4::from_scale(Vec3::new(1.0, 1.0, 0.0)));
        assert!(flat.to_cols_array().iter().all(|v| v.is_finite()));
    }
}
