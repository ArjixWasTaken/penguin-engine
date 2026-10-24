//! Drawing with compiled Wallpaper Engine shaders.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use glam::{Mat3, Mat4, Vec2, Vec3};
use wpe_shader::reflect::{Scalar, TextureDim, UniformMember};
use wpe_shader::{CompiledShader, Reflection};

use crate::targets::{TargetId, Targets};
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
    pipelines: Mutex<HashMap<(BlendMode, wgpu::TextureFormat), Option<wgpu::RenderPipeline>>>,
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
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: t.texture,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension,
                    multisampled: false,
                },
                count: None,
            });
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: t.sampler,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
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

    fn pipeline(
        &self,
        device: &wgpu::Device,
        blend: BlendMode,
        format: wgpu::TextureFormat,
    ) -> Option<wgpu::RenderPipeline> {
        let mut pipelines = self.pipelines.lock().unwrap_or_else(|e| e.into_inner());
        pipelines
            .entry((blend, format))
            .or_insert_with(|| {
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
                        topology: wgpu::PrimitiveTopology::TriangleStrip,
                        ..Default::default()
                    },
                    depth_stencil: None,
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
        for (i, v) in values.iter().enumerate() {
            let elem = i / per_element.max(1);
            if elem >= ty.count as usize {
                break;
            }
            let within = i % per_element.max(1);
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
#[derive(Debug, Clone, Copy)]
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
}

/// Where a texture slot's image comes from.
#[derive(Clone)]
pub enum TexRef {
    Asset(Arc<GpuTexture>),
    Target(TargetId),
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
}

/// One draw with a compiled program.
pub struct MaterialDraw {
    pub program: Arc<ShaderProgram>,
    /// Texture per `g_Texture<N>` slot; `None` binds a placeholder.
    pub textures: Vec<Option<TexRef>>,
    /// Static uniform values (defaults, material constants, layer values).
    pub constants: HashMap<String, Vec<f32>>,
    pub model: Mat4,
    pub mesh: CardMesh,
    pub blend: BlendMode,
    pub camera: Camera,
    pub sprites: Vec<SpriteAnimation>,
}

pub(crate) struct PreparedMaterialDraw {
    draw: MaterialDraw,
    block: UniformBlock,
    uniforms: Option<wgpu::Buffer>,
    vertices: wgpu::Buffer,
    /// Rebuilt when render targets are reallocated.
    bind_group: Option<(u64, wgpu::BindGroup)>,
}

/// Placeholder textures bound to unused or missing slots.
pub(crate) struct Placeholders {
    d2: GpuTexture,
    d3: GpuTexture,
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
        Self {
            d2: make(wgpu::TextureDimension::D2),
            d3: make(wgpu::TextureDimension::D3),
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
        let vertices = vertex_buffer(gpu, &draw.program, &draw.mesh);
        Self {
            draw,
            block,
            uniforms,
            vertices,
            bind_group: None,
        }
    }

    fn bind(&mut self, gpu: &Gpu, placeholders: &Placeholders, targets: &Targets) {
        if self
            .bind_group
            .as_ref()
            .is_some_and(|(generation, _)| *generation == targets.generation)
        {
            return;
        }
        let r = &self.draw.program.reflection;
        let mut entries = Vec::new();
        if let Some(buf) = &self.uniforms {
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
            let (view, sampler) = match bound {
                Some(TexRef::Asset(t))
                    if (t.texture.dimension() == wgpu::TextureDimension::D3) == wants_3d =>
                {
                    (&t.view, &t.sampler)
                }
                Some(TexRef::Target(id)) if !wants_3d => (&targets.get(*id).view, &targets.sampler),
                _ => {
                    let p = if wants_3d {
                        &placeholders.d3
                    } else {
                        &placeholders.d2
                    };
                    (&p.view, &p.sampler)
                }
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
        let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("material"),
            layout: &self.draw.program.layout,
            entries: &entries,
        });
        self.bind_group = Some((targets.generation, group));
    }

    /// Refresh per-frame uniforms, upload the block and (re)bind textures.
    pub fn update(
        &mut self,
        gpu: &Gpu,
        globals: &FrameGlobals,
        placeholders: &Placeholders,
        targets: &Targets,
    ) {
        for anim in &self.draw.sprites {
            let Some(frame) = anim.frame_at(globals.time) else {
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
        self.bind(gpu, placeholders, targets);
        let Some(buffer) = &self.uniforms else { return };
        let r = &self.draw.program.reflection;
        let model = self.draw.model;
        let view_proj = match self.draw.camera {
            Camera::Scene => globals.view_proj,
            Camera::Fixed(m) => m,
        };
        let mvp = view_proj * model;
        let mat4 = |m: Mat4| m.to_cols_array().to_vec();
        let mat3 = |m: Mat3| m.to_cols_array().to_vec();
        let values: [(&str, Vec<f32>); 16] = [
            ("g_ModelViewProjectionMatrix", mat4(mvp)),
            ("g_ModelViewProjectionMatrixInverse", mat4(mvp.inverse())),
            ("g_ModelMatrix", mat4(model)),
            ("g_ModelMatrixInverse", mat4(model.inverse())),
            ("g_AltModelMatrix", mat4(model)),
            ("g_ViewProjectionMatrix", mat4(view_proj)),
            ("g_AltViewProjectionMatrix", mat4(view_proj)),
            (
                "g_NormalModelMatrix",
                mat3(Mat3::from_mat4(model.inverse().transpose())),
            ),
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
            ("g_EyePosition", globals.eye.to_array().to_vec()),
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
        for (slot, tex) in self.draw.textures.iter().enumerate() {
            if let Some(TexRef::Target(id)) = tex
                && let Some(member) = r.uniforms.get(&format!("g_Texture{slot}Resolution"))
            {
                let (w, h) = targets.get(*id).size;
                let (w, h) = (w as f32, h as f32);
                self.block.write(member, &[w, h, w, h]);
            }
        }
        gpu.queue.write_buffer(buffer, 0, &self.block.data);
    }

    pub fn record(
        &self,
        device: &wgpu::Device,
        pass: &mut wgpu::RenderPass,
        format: wgpu::TextureFormat,
    ) {
        let Some((_, group)) = &self.bind_group else {
            return;
        };
        let Some(pipeline) = self.draw.program.pipeline(device, self.draw.blend, format) else {
            return;
        };
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, group, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..4, 0..1);
    }
}

fn vertex_buffer(gpu: &Gpu, program: &ShaderProgram, mesh: &CardMesh) -> wgpu::Buffer {
    let inputs = &program.reflection.vertex_inputs;
    let stride = program.vertex_stride.max(4) as usize;
    let mut data = vec![0u8; stride * 4];
    for (v, (pos, uv)) in mesh.corners().iter().enumerate() {
        for (input, attr) in inputs.iter().zip(&program.vertex_attributes) {
            let at = v * stride + attr.offset as usize;
            let mut values = [0.0f32; 4];
            match input.name.as_str() {
                "a_Position" => {
                    values[..3].copy_from_slice(pos);
                    values[3] = 1.0;
                }
                "a_TexCoord" => values[..2].copy_from_slice(uv),
                "a_Color" => values = [1.0; 4],
                _ => {}
            }
            for (c, value) in values.iter().take(input.components as usize).enumerate() {
                let bytes = match input.scalar {
                    Scalar::Float | Scalar::Bool => value.to_le_bytes(),
                    Scalar::Int => (*value as i32).to_le_bytes(),
                    Scalar::Uint => (*value as u32).to_le_bytes(),
                };
                data[at + c * 4..at + c * 4 + 4].copy_from_slice(&bytes);
            }
        }
    }
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("card"),
        size: data.len() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    gpu.queue.write_buffer(&buffer, 0, &data);
    buffer
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
    }
}
