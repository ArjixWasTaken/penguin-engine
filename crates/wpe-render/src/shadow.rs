//! Shadow maps of shadow-casting lights, packed into one depth atlas
//! (`_rt_shadowAtlas`) that Wallpaper Engine's lit shaders sample.
//!
//! What the stock shaders expect (`common_pbr_2.h`, the `LightingV1`
//! lighting code and `volumetricsfront`):
//!
//! * the atlas is a depth texture read with depth comparison
//!   (`texSample2DCompare`), passing where the receiver's depth is nearer;
//!   `g_Texture<N>Texel` is `(1/width, 1/height, width, height)`;
//! * a point light's tile holds its six cube faces as a 2 × 3 grid
//!   (`CalculateProjectedCoordsPoint`): +X, -X in the first row, ±Y in the
//!   second, ±Z in the third, each seen by a fixed view matrix and the
//!   projection `g_LFeature_ShadowPointProjection` = `(z scale, z offset,
//!   w scale, w offset)`, its 90° field of view widened so its edges leave a
//!   border for filtering; `g_LFeature_ShadowPointProjectionTransform` is the
//!   tile's offset and size in atlas UVs;
//! * spot lights and directional cascades have a view-projection
//!   (`g_LFeature_ShadowProjection`) and a tile
//!   (`g_LFeature_ShadowProjectionTransform`); a directional light has three
//!   nested cascades reaching its `cascadedistance0..2` from the camera.
//!
//! Atlas size, tile resolution, the near planes and the depth bias are not
//! fixed by the shaders; they are this module's choices.
//!
//! The scene's cookie spot light projects its texture through the same kind
//! of view-projection (the generated lighting code samples `COOKIE_SAMPLER`
//! at `CalculateProjectedCoords(worldPos, g_LFeature_ShadowProjection[i])`);
//! unshadowed, it has no tile and its projection follows the shadowed
//! spots'.

use std::collections::HashMap;

use glam::{Mat4, Vec3, Vec4};

use crate::material::{FrameGlobals, Mesh, PreparedMaterialDraw};
use crate::{Gpu, GpuTexture};

/// Format of the shadow atlas.
pub const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Largest atlas side (wgpu's default texture size limit).
const MAX_ATLAS: u32 = 8192;

/// A shadow-casting point light.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointShadow {
    pub origin: Vec3,
    pub radius: f32,
}

/// A shadow-casting spot light.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpotShadow {
    pub origin: Vec3,
    /// Unit direction it shines in.
    pub direction: Vec3,
    /// Half-angle of the outer cone, degrees.
    pub outer_cone: f32,
    pub radius: f32,
}

/// A shadow-casting directional light.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DirectionalShadow {
    /// Unit direction it shines in.
    pub direction: Vec3,
    /// How far from the camera each cascade reaches.
    pub cascades: [f32; 3],
}

/// The scene's shadow-casting lights, each kind in the order the lit
/// shaders index them (`LIGHTS_POINT_SHADOW`, ...).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShadowLights {
    pub points: Vec<PointShadow>,
    pub spots: Vec<SpotShadow>,
    pub directionals: Vec<DirectionalShadow>,
    /// The unshadowed cookie spot light (`LIGHTS_SPOT_COOKIE`), which needs
    /// a projection but no shadow map.
    pub cookie: Option<SpotShadow>,
    /// `LIGHTS_SHADOW_MAPPING_QUALITY`, 1 to 4.
    pub quality: u32,
    /// Cascades follow the perspective camera (3D scenes) rather than the
    /// scene's orthographic one.
    pub perspective: bool,
}

impl ShadowLights {
    pub fn is_empty(&self) -> bool {
        self.points.is_empty() && self.spots.is_empty() && self.directionals.is_empty()
    }

    /// Where the lights' maps go in the atlas (`None` without lights).
    pub fn layout(&self) -> Option<ShadowLayout> {
        let blocks = self.points.len() + self.spots.len() + self.directionals.len();
        (!self.is_empty()).then(|| ShadowLayout::new(blocks, self.quality))
    }
}

/// How far point light tiles' cube faces reach into their cell, from the
/// quality (`viewportPointCompensation` in `CalculateProjectedCoordsPoint`).
pub fn point_compensation(quality: u32) -> f32 {
    match quality {
        0..=2 => 0.47,
        3 => 0.48,
        _ => 0.49,
    }
}

/// Near plane of a light's shadow projection (a guess: a small fraction of
/// its reach).
fn near_plane(radius: f32) -> f32 {
    (radius * 0.005).max(0.01)
}

/// `g_LFeature_ShadowPointProjection` of a point light: the projection a
/// cube face uses, as `(z scale, z offset, w scale, w offset)` of
/// right-handed view space with depth 0 at the near plane and 1 at `radius`.
pub fn point_projection_info(radius: f32) -> [f32; 4] {
    let (n, f) = (near_plane(radius), radius.max(near_plane(radius) * 2.0));
    [f / (n - f), n * f / (n - f), -1.0, 0.0]
}

/// The six cube face view matrices of a point light at `o`, in the order of
/// their cells (+X, -X, +Y, -Y, +Z, -Z), as `CalculateProjectedCoordsPoint`
/// builds them.
fn point_face_views(o: Vec3) -> [Mat4; 6] {
    let m = |c: [f32; 16]| Mat4::from_cols_array(&c);
    [
        m([
            0., 0., -1., 0., 0., 1., 0., 0., 1., 0., 0., 0., -o.z, -o.y, o.x, 1.,
        ]),
        m([
            0., 0., 1., 0., 0., 1., 0., 0., -1., 0., 0., 0., o.z, -o.y, -o.x, 1.,
        ]),
        m([
            1., 0., 0., 0., 0., 0., -1., 0., 0., 1., 0., 0., -o.x, -o.z, o.y, 1.,
        ]),
        m([
            1., 0., 0., 0., 0., 0., 1., 0., 0., -1., 0., 0., -o.x, o.z, -o.y, 1.,
        ]),
        m([
            -1., 0., 0., 0., 0., 1., 0., 0., 0., 0., -1., 0., o.x, -o.y, o.z, 1.,
        ]),
        m([
            1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., -o.x, -o.y, -o.z, 1.,
        ]),
    ]
}

/// The cell (column, row) of each cube face within a point light's tile.
const FACE_CELLS: [(f32, f32); 6] = [(0., 0.), (1., 0.), (0., 1.), (1., 1.), (0., 2.), (1., 2.)];
/// `viewportScale`: a cell's share of its tile.
const CELL_SCALE: (f32, f32) = (0.5, 0.3333);

/// Where each light's maps sit in the atlas: every light gets a block of
/// 2 × 3 cells. A point light fills it with its cube faces, a spot light
/// uses a 2 × 2 cell square, a directional light one cell per cascade.
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowLayout {
    /// Atlas size in texels.
    pub size: (u32, u32),
    /// Side of one cell in texels.
    pub cell: u32,
    /// Blocks per atlas row.
    columns: u32,
}

impl ShadowLayout {
    /// Lay out `lights` blocks at `quality` (cells of 256 to 1024 texels,
    /// shrunk until the atlas fits).
    pub fn new(lights: usize, quality: u32) -> Self {
        let lights = lights.max(1) as u32;
        let mut cell: u32 = match quality {
            0 | 1 => 256,
            2 | 3 => 512,
            _ => 1024,
        };
        let columns = lights.min(4);
        let rows = lights.div_ceil(columns);
        while cell > 64 && (2 * cell * columns > MAX_ATLAS || 3 * cell * rows > MAX_ATLAS) {
            cell /= 2;
        }
        Self {
            size: (2 * cell * columns, 3 * cell * rows),
            cell,
            columns,
        }
    }

    /// Texel rectangle `(x, y, w, h)` of block `i`.
    fn block(&self, i: usize) -> Vec4 {
        let i = i as u32;
        let (col, row) = (i % self.columns, i / self.columns);
        let c = self.cell as f32;
        Vec4::new(col as f32 * 2.0 * c, row as f32 * 3.0 * c, 2.0 * c, 3.0 * c)
    }

    /// A texel rectangle as an atlas UV transform `(x, y, w, h)`.
    fn uv(&self, rect: Vec4) -> [f32; 4] {
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        [rect.x / w, rect.y / h, rect.z / w, rect.w / h]
    }

    /// Texel rectangle of the point light tile `block`.
    pub fn point_tile(&self, block: usize) -> Vec4 {
        self.block(block)
    }

    /// Texel rectangle of the point light tile `block`'s cube face `face`,
    /// as `CalculateProjectedCoordsPoint` addresses it.
    fn point_cell(&self, block: usize, face: usize) -> Vec4 {
        let tile = self.block(block);
        let (cx, cy) = FACE_CELLS[face];
        let (w, h) = (tile.z * CELL_SCALE.0, tile.w * CELL_SCALE.1);
        Vec4::new(tile.x + cx * w, tile.y + cy * h, w, h)
    }

    /// The spot light tile of `block`: a square of 2 × 2 cells.
    fn spot_tile(&self, block: usize) -> Vec4 {
        let b = self.block(block);
        Vec4::new(b.x, b.y, b.z, b.z)
    }

    /// Cascade `cascade` of the directional light in `block`: one cell.
    fn cascade_tile(&self, block: usize, cascade: usize) -> Vec4 {
        let b = self.block(block);
        let c = self.cell as f32;
        let (cx, cy) = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)][cascade.min(2)];
        Vec4::new(b.x + cx * c, b.y + cy * c, c, c)
    }

    /// `g_LFeature_ShadowPointProjectionTransform` of point light `k`.
    pub fn point_transform(&self, k: usize) -> [f32; 4] {
        self.uv(self.point_tile(k))
    }

    /// `g_LFeature_ShadowProjectionTransform` of the spot light in `block`
    /// (shadowed spots follow the shadowed point lights' blocks).
    pub fn spot_transform(&self, block: usize) -> [f32; 4] {
        self.uv(self.spot_tile(block))
    }

    /// `g_Texture<N>Texel` of the atlas.
    pub fn texel(&self) -> [f32; 4] {
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        [1.0 / w, 1.0 / h, w, h]
    }
}

/// One rendering of the casters into the atlas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ShadowView {
    /// Texel rectangle `(x, y, w, h)`.
    pub rect: Vec4,
    pub view_proj: Mat4,
}

/// A spot light's view-projection: its outer cone fills a square
/// (`g_LFeature_ShadowProjection`, and the cookie's projection).
pub fn spot_view_proj(s: &SpotShadow) -> Mat4 {
    let dir = s.direction.normalize_or(-Vec3::Z);
    let up = if dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let view = glam::camera::rh::view::look_to_mat4(s.origin, dir, up);
    let fov = (2.0 * s.outer_cone).clamp(1.0, 170.0).to_radians();
    let (n, f) = (
        near_plane(s.radius),
        s.radius.max(near_plane(s.radius) * 2.0),
    );
    glam::camera::rh::proj::directx::perspective(fov, 1.0, n, f) * view
}

/// The orthographic view-projections of a directional light's cascades,
/// each enclosing the camera's view from its near plane out to the
/// cascade's distance.
fn cascade_view_projs(
    d: &DirectionalShadow,
    camera_view_proj: Mat4,
    camera_proj: Mat4,
) -> [Mat4; 3] {
    let inverse = camera_view_proj.inverse();
    // Depth of view-space distance `dist` in front of the camera.
    let ndc_depth = |dist: f32| {
        camera_proj
            .project_point3(Vec3::new(0.0, 0.0, -dist))
            .z
            .clamp(0.0, 1.0)
    };
    let dir = d.direction.normalize_or(-Vec3::Y);
    let up = if dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    d.cascades.map(|reach| {
        let far = ndc_depth(reach.max(1e-3));
        let mut corners = Vec::with_capacity(8);
        for z in [0.0, far] {
            for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                corners.push(inverse.project_point3(Vec3::new(x, y, z)));
            }
        }
        let center = corners.iter().copied().sum::<Vec3>() / corners.len() as f32;
        let radius = corners
            .iter()
            .map(|c| c.distance(center))
            .fold(1.0_f32, f32::max);
        // Casters up to four radii behind the cascade still shadow it;
        // receivers must stay below depth 0.99 (beyond counts as outside).
        let back = 4.0 * radius;
        let view = glam::camera::rh::view::look_to_mat4(center - dir * back, dir, up);
        let proj = glam::camera::rh::proj::directx::orthographic(
            -radius,
            radius,
            -radius,
            radius,
            0.0,
            (back + radius) / 0.95,
        );
        proj * view
    })
}

/// Per-frame shadow uniforms and atlas renderings of `lights`.
pub(crate) fn frame_views(
    lights: &ShadowLights,
    layout: &ShadowLayout,
    globals: &FrameGlobals,
) -> (Vec<(String, Vec<f32>)>, Vec<ShadowView>) {
    let mut views = Vec::new();
    let mut point_projection = Vec::new();
    let mut point_transform = Vec::new();
    let mut projection = Vec::new();
    let mut transform = Vec::new();
    let mut block = 0;

    let comp = 2.0 * point_compensation(lights.quality);
    let widen = Mat4::from_scale(Vec3::new(comp, comp, 1.0));
    for p in &lights.points {
        let info = point_projection_info(p.radius);
        let proj = Mat4::from_cols(
            Vec4::X,
            Vec4::Y,
            Vec4::new(0.0, 0.0, info[0], info[2]),
            Vec4::new(0.0, 0.0, info[1], info[3]),
        );
        for (face, view) in point_face_views(p.origin).into_iter().enumerate() {
            views.push(ShadowView {
                rect: layout.point_cell(block, face),
                view_proj: widen * proj * view,
            });
        }
        point_projection.extend(info);
        point_transform.extend(layout.point_transform(block));
        block += 1;
    }
    for s in &lights.spots {
        let view_proj = spot_view_proj(s);
        let rect = layout.spot_tile(block);
        views.push(ShadowView { rect, view_proj });
        projection.extend(view_proj.to_cols_array());
        transform.extend(layout.uv(rect));
        block += 1;
    }
    cookie_projection(lights, &mut projection, &mut transform);
    let (camera, camera_proj) = if lights.perspective {
        (globals.perspective, globals.perspective_proj)
    } else {
        (globals.view_proj, globals.view_proj)
    };
    for d in &lights.directionals {
        for (c, view_proj) in cascade_view_projs(d, camera, camera_proj)
            .into_iter()
            .enumerate()
        {
            let rect = layout.cascade_tile(block, c);
            views.push(ShadowView { rect, view_proj });
            projection.extend(view_proj.to_cols_array());
            transform.extend(layout.uv(rect));
        }
        block += 1;
    }

    let mut uniforms = Vec::new();
    if !point_projection.is_empty() {
        uniforms.push((
            "g_LFeature_ShadowPointProjection".to_owned(),
            point_projection,
        ));
        uniforms.push((
            "g_LFeature_ShadowPointProjectionTransform".to_owned(),
            point_transform,
        ));
    }
    if !projection.is_empty() {
        uniforms.push(("g_LFeature_ShadowProjection".to_owned(), projection));
        uniforms.push(("g_LFeature_ShadowProjectionTransform".to_owned(), transform));
    }
    (uniforms, views)
}

/// Append the unshadowed cookie light's projection (its tile transform is
/// unused).
fn cookie_projection(lights: &ShadowLights, projection: &mut Vec<f32>, transform: &mut Vec<f32>) {
    if let Some(c) = &lights.cookie {
        projection.extend(spot_view_proj(c).to_cols_array());
        transform.extend([0.0; 4]);
    }
}

/// The uniforms of a cookie light without any shadow maps.
fn cookie_uniforms(lights: &ShadowLights) -> Vec<(String, Vec<f32>)> {
    let (mut projection, mut transform) = (Vec::new(), Vec::new());
    cookie_projection(lights, &mut projection, &mut transform);
    if projection.is_empty() {
        return Vec::new();
    }
    vec![
        ("g_LFeature_ShadowProjection".to_owned(), projection),
        ("g_LFeature_ShadowProjectionTransform".to_owned(), transform),
    ]
}

const SHADER: &str = r#"
struct View { view_proj: mat4x4<f32> };
struct Caster { model: mat4x4<f32>, flags: vec4<u32> };

@group(0) @binding(0) var<uniform> view: View;
@group(1) @binding(0) var<uniform> caster: Caster;
@group(1) @binding(1) var<storage, read> bones: array<mat4x4<f32>>;
@group(1) @binding(2) var albedo: texture_2d<f32>;
@group(1) @binding(3) var albedo_sampler: sampler;

struct In {
    @location(0) position: vec3<f32>,
    @location(1) indices: vec4<u32>,
    @location(2) weights: vec4<f32>,
    @location(3) uv: vec2<f32>,
};

struct Out {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

fn bone(i: u32) -> mat4x4<f32> {
    return bones[min(i, arrayLength(&bones) - 1u)];
}

@vertex
fn vs_main(v: In) -> Out {
    var p = vec4<f32>(v.position, 1.0);
    if caster.flags.x != 0u {
        let skin = bone(v.indices.x) * v.weights.x + bone(v.indices.y) * v.weights.y
            + bone(v.indices.z) * v.weights.z + bone(v.indices.w) * v.weights.w;
        p = vec4<f32>((skin * p).xyz, 1.0);
    }
    var out: Out;
    out.clip = view.view_proj * (caster.model * p);
    out.uv = v.uv;
    return out;
}

@fragment
fn fs_main(v: Out) {
    if caster.flags.y != 0u && textureSample(albedo, albedo_sampler, v.uv).a < 0.5 {
        discard;
    }
}
"#;

/// Bytes per caster vertex: position, bone indices, bone weights, UV.
const VERTEX_SIZE: usize = 12 + 16 + 16 + 8;
/// Uniform buffer offset alignment of one view.
const VIEW_STRIDE: u64 = 256;

struct CasterPipeline {
    pipeline: wgpu::RenderPipeline,
    view_layout: wgpu::BindGroupLayout,
    caster_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl CasterPipeline {
    fn new(gpu: &Gpu) -> Self {
        let device = &gpu.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow caster"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let view_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow view"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let caster_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow caster"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow caster"),
            bind_group_layouts: &[Some(&view_layout), Some(&caster_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow caster"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: VERTEX_SIZE as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3,
                        1 => Uint32x4,
                        2 => Float32x4,
                        3 => Float32x2,
                    ],
                })],
            },
            // Both faces cast: models and cards need not be closed.
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: SHADOW_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                // Keeps lit surfaces from shadowing themselves (a guess,
                // the shaders add no bias of their own).
                bias: wgpu::DepthBiasState {
                    constant: 4,
                    slope_scale: 2.5,
                    clamp: 0.0,
                },
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            view_layout,
            caster_layout,
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("shadow caster"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
        }
    }
}

/// A caster's geometry and per-frame values on the GPU.
struct CasterGpu {
    mesh_version: u64,
    vertices: wgpu::Buffer,
    indices: Option<wgpu::Buffer>,
    count: u32,
    uniforms: wgpu::Buffer,
    bones: wgpu::Buffer,
    group: wgpu::BindGroup,
    skinned: bool,
}

/// The vertices (and triangle-list indices) of a draw's mesh in the caster
/// layout. `None` for an empty mesh.
fn caster_vertices(mesh: &Mesh) -> Option<(Vec<u8>, Option<Vec<u32>>, u32)> {
    let data = mesh.data();
    let n = data.vertex_count as usize;
    if n == 0 {
        return None;
    }
    let stream = |name: &str| {
        data.attributes
            .iter()
            .find(|(a, ..)| a == name)
            .map(|(_, c, v)| (*c as usize, v.as_slice()))
    };
    let get = |s: Option<(usize, &[f32])>, v: usize, c: usize, default: f32| match s {
        Some((comps, values)) if c < comps => values.get(v * comps + c).copied().unwrap_or(0.0),
        _ => default,
    };
    let (position, indices, weights, uv) = (
        stream("a_Position"),
        stream("a_BlendIndices"),
        stream("a_BlendWeights"),
        stream("a_TexCoord"),
    );
    let mut bytes = Vec::with_capacity(n * VERTEX_SIZE);
    for v in 0..n {
        for c in 0..3 {
            bytes.extend(get(position, v, c, 0.0).to_le_bytes());
        }
        for c in 0..4 {
            bytes.extend((get(indices, v, c, 0.0) as u32).to_le_bytes());
        }
        for c in 0..4 {
            let w = get(weights, v, c, if c == 0 { 1.0 } else { 0.0 });
            bytes.extend(w.to_le_bytes());
        }
        for c in 0..2 {
            bytes.extend(get(uv, v, c, 0.0).to_le_bytes());
        }
    }
    let (index_list, count) = match (mesh, &data.indices) {
        // A card is a 4-vertex triangle strip.
        (Mesh::Card(_), _) => (Some(vec![0, 1, 2, 2, 1, 3]), 6),
        (_, Some(i)) => (Some(i.clone()), i.len() as u32),
        (_, None) => (None, n as u32),
    };
    Some((bytes, index_list, count))
}

/// The renderer's shadow state: lights, casters and the atlas.
#[derive(Default)]
pub(crate) struct ShadowMaps {
    pub lights: ShadowLights,
    /// Material steps whose geometry casts shadows.
    pub casters: Vec<usize>,
    layout: Option<ShadowLayout>,
    /// Allocated while there are casters.
    atlas: Option<(ShadowLayout, wgpu::Texture, wgpu::TextureView)>,
    /// Bumped when the atlas is reallocated.
    pub generation: u64,
    pipeline: Option<CasterPipeline>,
    gpu: HashMap<usize, CasterGpu>,
    views: Option<(wgpu::Buffer, wgpu::BindGroup, usize)>,
    frame_views: Vec<ShadowView>,
}

impl ShadowMaps {
    /// Replace the lights; the atlas follows their count and quality.
    pub fn set_lights(&mut self, lights: ShadowLights) {
        self.lights = lights;
    }

    /// (Re)allocate the atlas for the lights' layout.
    /// Without casters nothing is drawn: the shaders sample the far-plane
    /// placeholder instead of an atlas.
    fn allocate(&mut self, gpu: &Gpu) {
        let layout = self.lights.layout();
        let wanted = layout.clone().filter(|_| !self.casters.is_empty());
        let allocated = self.atlas.as_ref().map(|(l, ..)| l);
        if wanted.as_ref() != allocated {
            self.atlas = wanted.map(|l| {
                let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("_rt_shadowAtlas"),
                    size: wgpu::Extent3d {
                        width: l.size.0,
                        height: l.size.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: SHADOW_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                let view = texture.create_view(&Default::default());
                (l, texture, view)
            });
            self.generation += 1;
        }
        self.layout = layout;
    }

    /// Replace the casters (a new frame's steps).
    pub fn set_casters(&mut self, casters: Vec<usize>) {
        self.casters = casters;
        self.gpu.clear();
    }

    pub fn atlas(&self) -> Option<&wgpu::TextureView> {
        self.atlas.as_ref().map(|(.., v)| v)
    }

    /// This frame's shadow uniforms (`g_LFeature_*`, `g_Texture6Texel`),
    /// computing where the casters are drawn from.
    pub fn prepare(&mut self, gpu: &Gpu, globals: &FrameGlobals) -> Vec<(String, Vec<f32>)> {
        self.allocate(gpu);
        let Some(layout) = &self.layout else {
            self.frame_views.clear();
            return cookie_uniforms(&self.lights);
        };
        let (mut uniforms, views) = frame_views(&self.lights, layout, globals);
        uniforms.push(("g_Texture6Texel".to_owned(), layout.texel().to_vec()));
        self.frame_views = views;
        uniforms
    }

    /// Render the visible `casters` (step index and draw) into the atlas.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        casters: &[(usize, &PreparedMaterialDraw)],
    ) {
        let Some((.., atlas)) = &self.atlas else {
            return;
        };
        let pipeline = self
            .pipeline
            .get_or_insert_with(|| CasterPipeline::new(gpu));

        // View matrices, one per 256 bytes.
        let count = self.frame_views.len().max(1);
        if self.views.as_ref().is_none_or(|(_, _, n)| *n < count) {
            let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("shadow views"),
                size: VIEW_STRIDE * count as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("shadow views"),
                layout: &pipeline.view_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &buffer,
                        offset: 0,
                        size: wgpu::BufferSize::new(64),
                    }),
                }],
            });
            self.views = Some((buffer, group, count));
        }
        let Some((view_buffer, view_group, _)) = &self.views else {
            return;
        };
        let mut bytes = vec![0u8; VIEW_STRIDE as usize * count];
        for (i, v) in self.frame_views.iter().enumerate() {
            let at = i * VIEW_STRIDE as usize;
            bytes[at..at + 64].copy_from_slice(bytemuck::cast_slice(&v.view_proj.to_cols_array()));
        }
        gpu.queue.write_buffer(view_buffer, 0, &bytes);

        for (step, draw) in casters {
            let current = self
                .gpu
                .get(step)
                .is_some_and(|c| c.mesh_version == draw.mesh_version);
            if !current {
                match upload_caster(gpu, pipeline, draw) {
                    Some(c) => self.gpu.insert(*step, c),
                    None => self.gpu.remove(step),
                };
            }
            let Some(c) = self.gpu.get(step) else {
                continue;
            };
            let bones = draw.overrides.get("g_Bones").filter(|_| c.skinned);
            let mut uniforms = [0u8; 80];
            uniforms[..64].copy_from_slice(bytemuck::cast_slice(&draw.draw.model.to_cols_array()));
            let alpha_test = !matches!(draw.draw.blend, crate::BlendMode::Normal)
                && caster_texture(draw).is_some();
            let flags = [bones.is_some() as u32, alpha_test as u32, 0, 0];
            uniforms[64..].copy_from_slice(bytemuck::cast_slice(&flags));
            gpu.queue.write_buffer(&c.uniforms, 0, &uniforms);
            if let Some(bones) = bones {
                let matrices: Vec<f32> = bones
                    .chunks_exact(12)
                    .flat_map(|b| {
                        [
                            b[0], b[1], b[2], 0.0, b[3], b[4], b[5], 0.0, b[6], b[7], b[8], 0.0,
                            b[9], b[10], b[11], 1.0,
                        ]
                    })
                    .collect();
                let bytes: &[u8] = bytemuck::cast_slice(&matrices);
                let size = (bytes.len() as u64).min(c.bones.size());
                gpu.queue.write_buffer(&c.bones, 0, &bytes[..size as usize]);
            }
        }

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shadow atlas"),
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: atlas,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        pass.set_pipeline(&pipeline.pipeline);
        for (i, v) in self.frame_views.iter().enumerate() {
            let r = v.rect;
            pass.set_viewport(r.x, r.y, r.z, r.w, 0.0, 1.0);
            let x0 = r.x.floor().max(0.0) as u32;
            let y0 = r.y.floor().max(0.0) as u32;
            let x1 = (r.x + r.z).ceil() as u32;
            let y1 = (r.y + r.w).ceil() as u32;
            pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0);
            pass.set_bind_group(0, view_group, &[(i as u64 * VIEW_STRIDE) as u32]);
            for (step, _) in casters {
                let Some(c) = self.gpu.get(step) else {
                    continue;
                };
                pass.set_bind_group(1, &c.group, &[]);
                pass.set_vertex_buffer(0, c.vertices.slice(..));
                match &c.indices {
                    Some(indices) => {
                        pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..c.count, 0, 0..1);
                    }
                    None => pass.draw(0..c.count, 0..1),
                }
            }
        }
    }
}

/// The texture an alpha-tested caster's shape comes from: its first slot.
fn caster_texture(draw: &PreparedMaterialDraw) -> Option<&GpuTexture> {
    match draw.draw.textures.first() {
        Some(Some(crate::TexRef::Asset(t)))
            if t.texture.dimension() == wgpu::TextureDimension::D2 =>
        {
            Some(t)
        }
        _ => None,
    }
}

fn upload_caster(
    gpu: &Gpu,
    pipeline: &CasterPipeline,
    draw: &PreparedMaterialDraw,
) -> Option<CasterGpu> {
    use wgpu::util::DeviceExt;
    let (vertices, indices, count) = caster_vertices(&draw.draw.mesh)?;
    let device = &gpu.device;
    let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("shadow caster vertices"),
        contents: &vertices,
        usage: wgpu::BufferUsages::VERTEX,
    });
    let indices = indices.map(|i| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("shadow caster indices"),
            contents: bytemuck::cast_slice(&i),
            usage: wgpu::BufferUsages::INDEX,
        })
    });
    let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("shadow caster"),
        size: 80,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let reflection = &draw.draw.program.reflection;
    let bone_count = reflection
        .uniforms
        .get("g_Bones")
        .map_or(0, |m| m.ty.count as u64);
    let skinned = bone_count > 0
        && reflection
            .vertex_inputs
            .iter()
            .any(|i| i.name == "a_BlendIndices");
    let bones = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("shadow caster bones"),
        size: 64 * bone_count.max(1),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    if bone_count == 0 {
        gpu.queue.write_buffer(
            &bones,
            0,
            bytemuck::cast_slice(&Mat4::IDENTITY.to_cols_array()),
        );
    }
    let fallback;
    let texture = match caster_texture(draw) {
        Some(t) => &t.view,
        None => {
            fallback = device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("shadow caster albedo"),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default());
            &fallback
        }
    };
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("shadow caster"),
        layout: &pipeline.caster_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: bones.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(texture),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
            },
        ],
    });
    Some(CasterGpu {
        mesh_version: draw.mesh_version,
        vertices,
        indices,
        count,
        uniforms,
        bones,
        group,
        skinned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CalculateProjectedCoordsPoint` from `common_pbr_2.h`, transcribed.
    fn shader_point_coords(
        world: Vec3,
        origin: Vec3,
        info: [f32; 4],
        tile: [f32; 4],
        quality: u32,
    ) -> Vec4 {
        let d = world - origin;
        let a = d.abs();
        let views = point_face_views(origin);
        let face = if a.x >= a.y && a.x >= a.z {
            if d.x >= 0.0 { 0 } else { 1 }
        } else if a.y >= a.x && a.y >= a.z {
            if d.y >= 0.0 { 2 } else { 3 }
        } else if d.z >= 0.0 {
            4
        } else {
            5
        };
        let steps = (tile[2] * CELL_SCALE.0, tile[3] * CELL_SCALE.1);
        let (cx, cy) = FACE_CELLS[face];
        let project = Mat4::from_cols(
            Vec4::X,
            Vec4::Y,
            Vec4::new(0.0, 0.0, info[0], info[2]),
            Vec4::new(0.0, 0.0, info[1], info[3]),
        );
        let mut p = project * views[face] * world.extend(1.0);
        p = (p.truncate() / p.w).extend(p.w);
        let comp = point_compensation(quality);
        p.x = p.x * comp + 0.5;
        p.y = p.y * -comp + 0.5;
        p.x = p.x * steps.0 + tile[0] + cx * steps.0;
        p.y = p.y * steps.1 + tile[1] + cy * steps.1;
        p
    }

    #[test]
    fn point_faces_land_where_the_shader_looks() {
        let lights = ShadowLights {
            points: vec![
                PointShadow {
                    origin: Vec3::new(5.0, -2.0, 1.0),
                    radius: 50.0,
                },
                PointShadow {
                    origin: Vec3::new(0.0, 3.0, 0.0),
                    radius: 20.0,
                },
            ],
            quality: 3,
            ..Default::default()
        };
        let layout = ShadowLayout::new(2, 3);
        let (uniforms, views) = frame_views(&lights, &layout, &FrameGlobals::default());
        assert_eq!(views.len(), 12);
        let get = |name: &str| &uniforms.iter().find(|(n, _)| n == name).unwrap().1;
        let info = get("g_LFeature_ShadowPointProjection");
        let tiles = get("g_LFeature_ShadowPointProjectionTransform");
        let (w, h) = (layout.size.0 as f32, layout.size.1 as f32);
        for (k, light) in lights.points.iter().enumerate() {
            for dir in [
                Vec3::X,
                -Vec3::X,
                Vec3::Y,
                -Vec3::Y,
                Vec3::Z,
                -Vec3::Z,
                Vec3::new(0.7, 0.3, -0.2),
                Vec3::new(-0.2, -0.9, 0.4),
            ] {
                let world = light.origin + dir.normalize() * 7.0;
                let info: [f32; 4] = info[k * 4..k * 4 + 4].try_into().unwrap();
                let tile: [f32; 4] = tiles[k * 4..k * 4 + 4].try_into().unwrap();
                let expected = shader_point_coords(world, light.origin, info, tile, 3);
                // The view rendering that face puts the point at the same
                // atlas texel, at the same depth.
                let hit = views[k * 6..k * 6 + 6].iter().find_map(|v| {
                    let clip = v.view_proj * world.extend(1.0);
                    let ndc = clip.truncate() / clip.w;
                    (clip.w > 0.0 && ndc.x.abs() <= 1.0 && ndc.y.abs() <= 1.0).then(|| {
                        let x = v.rect.x + (ndc.x * 0.5 + 0.5) * v.rect.z;
                        let y = v.rect.y + (0.5 - ndc.y * 0.5) * v.rect.w;
                        Vec3::new(x / w, y / h, ndc.z)
                    })
                });
                let hit = hit.expect("some face sees the point");
                assert!(
                    (hit - expected.truncate()).abs().max_element() < 1e-4,
                    "{dir}: {hit} vs {expected}"
                );
                assert!(expected.z > 0.0 && expected.z < 1.0);
            }
        }
    }

    #[test]
    fn layout_fits_the_texture_limit() {
        let l = ShadowLayout::new(12, 4);
        assert!(l.size.0 <= MAX_ATLAS && l.size.1 <= MAX_ATLAS);
        let l = ShadowLayout::new(1, 4);
        assert_eq!((l.size, l.cell), ((2048, 3072), 1024));
        assert_eq!(l.texel(), [1.0 / 2048.0, 1.0 / 3072.0, 2048.0, 3072.0]);
    }

    #[test]
    fn spot_and_cascade_tiles_follow_their_matrices() {
        let lights = ShadowLights {
            spots: vec![SpotShadow {
                origin: Vec3::new(0.0, 10.0, 0.0),
                direction: -Vec3::Y,
                outer_cone: 30.0,
                radius: 40.0,
            }],
            directionals: vec![DirectionalShadow {
                direction: Vec3::new(0.3, -1.0, 0.2),
                cascades: [10.0, 30.0, 80.0],
            }],
            quality: 2,
            perspective: true,
            ..Default::default()
        };
        let layout = ShadowLayout::new(2, 2);
        let proj = glam::camera::rh::proj::directx::perspective(1.0, 1.5, 0.1, 500.0);
        let view =
            glam::camera::rh::view::look_at_mat4(Vec3::new(0.0, 5.0, 20.0), Vec3::ZERO, Vec3::Y);
        let globals = FrameGlobals {
            perspective: proj * view,
            perspective_proj: proj,
            perspective_view: view,
            ..Default::default()
        };
        let (uniforms, views) = frame_views(&lights, &layout, &globals);
        assert_eq!(views.len(), 4);
        // A cookie light's projection goes between the spots' and the
        // cascades'.
        let cookie = SpotShadow {
            origin: Vec3::new(4.0, 10.0, 0.0),
            ..lights.spots[0]
        };
        let with_cookie = ShadowLights {
            cookie: Some(cookie),
            ..lights.clone()
        };
        let (u2, views2) = frame_views(&with_cookie, &layout, &globals);
        assert_eq!(views2.len(), 4);
        let p2 = &u2
            .iter()
            .find(|(n, _)| n == "g_LFeature_ShadowProjection")
            .unwrap()
            .1;
        assert_eq!(p2.len(), 16 * 5);
        assert_eq!(p2[16..32], spot_view_proj(&cookie).to_cols_array());
        let alone = cookie_uniforms(&ShadowLights {
            cookie: Some(cookie),
            ..Default::default()
        });
        assert_eq!(alone[0].1, spot_view_proj(&cookie).to_cols_array());
        let get = |name: &str| &uniforms.iter().find(|(n, _)| n == name).unwrap().1;
        assert_eq!(get("g_LFeature_ShadowProjection").len(), 16 * 4);
        assert_eq!(get("g_LFeature_ShadowProjectionTransform").len(), 4 * 4);
        // A point below the spot light lands inside its tile.
        let spot = Mat4::from_cols_slice(&get("g_LFeature_ShadowProjection")[..16]);
        let p = spot.project_point3(Vec3::new(1.0, 0.0, 1.0));
        assert!(p.x.abs() < 1.0 && p.y.abs() < 1.0 && p.z > 0.0 && p.z < 1.0);
        // A point just ahead of the camera is inside every cascade, the
        // origin 20 units away in the outer two.
        let eye = Vec3::new(0.0, 5.0, 20.0);
        let near = eye - eye.normalize() * 5.0;
        let inside = |c: usize, p: Vec3| {
            let m = Mat4::from_cols_slice(&get("g_LFeature_ShadowProjection")[16 * (c + 1)..]);
            let p = m.project_point3(p);
            p.x.abs() < 0.99 && p.y.abs() < 0.99 && p.z < 0.99 && p.z > 0.0
        };
        for c in 0..3 {
            assert!(inside(c, near), "{c}");
        }
        assert!(inside(1, Vec3::ZERO) && inside(2, Vec3::ZERO));
    }
}
