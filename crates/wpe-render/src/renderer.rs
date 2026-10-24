use std::sync::Arc;

use glam::{Mat4, Vec2, Vec4};

use crate::image_pipeline::{ImagePipelines, ImageUniforms};
use crate::material::{
    FrameGlobals, MaterialDraw, PassDepth, Placeholders, PreparedMaterialDraw, RasterState,
};
use crate::targets::{DEFAULT_FORMAT, DepthBuffers, TargetDesc, TargetId, Targets, depth_view};
use crate::{BlendMode, Gpu, GpuTexture};

/// One textured card drawn with the built-in stand-in shader.
#[derive(Clone)]
pub struct ImageDraw {
    /// World transform of the card's centre.
    pub model: Mat4,
    /// Card size in scene pixels.
    pub size: Vec2,
    /// Multiplied with the sampled texel.
    pub color: Vec4,
    pub blend: BlendMode,
    pub texture: Arc<GpuTexture>,
}

#[allow(clippy::large_enum_variant)]
pub enum Draw {
    /// The built-in stand-in image shader (scene camera).
    Image(ImageDraw),
    /// A compiled Wallpaper Engine material.
    Material(MaterialDraw),
}

/// One step of a frame, executed in order.
#[allow(clippy::large_enum_variant)]
pub enum Step {
    Draw {
        target: TargetId,
        draw: Draw,
    },
    /// Copy one target into another (the overlapping region).
    Copy {
        from: TargetId,
        to: TargetId,
    },
}

struct PreparedImageDraw {
    draw: ImageDraw,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

#[allow(clippy::large_enum_variant)]
enum PreparedStep {
    Image(TargetId, PreparedImageDraw),
    Material(TargetId, PreparedMaterialDraw),
    Copy { from: TargetId, to: TargetId },
}

impl PreparedStep {
    fn draw_target(&self) -> Option<TargetId> {
        match self {
            Self::Image(t, _) | Self::Material(t, _) => Some(*t),
            Self::Copy { .. } => None,
        }
    }

    /// Depth and culling the step draws with; only materials use depth.
    fn raster(&self) -> RasterState {
        match self {
            Self::Material(_, d) => d.draw.raster,
            _ => RasterState::default(),
        }
    }
}

/// Whether a pass over `steps` needs a depth attachment: any draw in it
/// that is shown uses depth.
fn needs_depth(steps: &[PreparedStep], hidden: &[bool]) -> bool {
    steps
        .iter()
        .zip(hidden)
        .any(|(step, hidden)| !hidden && step.raster().uses_depth())
}

/// Executes a scene's frame steps over its render targets and presents
/// `_rt_default` to an output view.
pub struct SceneRenderer {
    images: ImagePipelines,
    blit_pipeline: wgpu::RenderPipeline,
    blit_layout: wgpu::BindGroupLayout,
    blit_bind_group: Option<(u64, wgpu::BindGroup)>,
    targets: Targets,
    /// Depth buffers of targets that 3D draws render into.
    depth: DepthBuffers,
    steps: Vec<PreparedStep>,
    /// Steps skipped this frame.
    hidden: Vec<bool>,
    placeholders: Placeholders,
    /// Uniforms written into every material draw that declares them.
    globals: std::collections::HashMap<String, Vec<f32>>,
    /// Targets to zero before the next frame's passes ([`Self::clear_target`]).
    pending_clears: Vec<TargetId>,
    pub clear_color: wgpu::Color,
}

impl SceneRenderer {
    pub fn new(gpu: &Gpu, output_format: wgpu::TextureFormat) -> Self {
        let device = &gpu.device;
        let module = device.create_shader_module(wgpu::include_wgsl!("blit.wgsl"));
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[Some(&blit_layout)],
            immediate_size: 0,
        });
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(output_format.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            images: ImagePipelines::new(device, DEFAULT_FORMAT),
            blit_pipeline,
            blit_layout,
            blit_bind_group: None,
            targets: Targets::new(gpu),
            depth: DepthBuffers::default(),
            steps: Vec::new(),
            hidden: Vec::new(),
            placeholders: Placeholders::new(gpu),
            globals: Default::default(),
            pending_clears: Vec::new(),
            clear_color: wgpu::Color::BLACK,
        }
    }

    /// Replace the frame: extra render targets (ids from 1, in order; 0 is
    /// `_rt_default`) and the steps that draw into them.
    pub fn set_frame(&mut self, gpu: &Gpu, targets: Vec<TargetDesc>, steps: Vec<Step>) {
        self.targets.set(targets);
        self.steps = steps
            .into_iter()
            .map(|step| match step {
                Step::Draw {
                    target,
                    draw: Draw::Image(draw),
                } => PreparedStep::Image(target, self.prepare_image(gpu, draw)),
                Step::Draw {
                    target,
                    draw: Draw::Material(draw),
                } => PreparedStep::Material(target, PreparedMaterialDraw::new(gpu, draw)),
                Step::Copy { from, to } => PreparedStep::Copy { from, to },
            })
            .collect();
        self.hidden = vec![false; self.steps.len()];
    }

    /// Show or hide the draw at `step`.
    pub fn set_visible(&mut self, step: usize, visible: bool) {
        if let Some(hidden) = self.hidden.get_mut(step) {
            *hidden = !visible;
        }
    }

    /// Play the sprite sheets of the draw at `step` at `time` seconds instead
    /// of the scene's time; `None` returns them to the scene clock.
    pub fn set_sprite_time(&mut self, step: usize, time: Option<f32>) {
        if let Some(PreparedStep::Material(_, d)) = self.steps.get_mut(step) {
            d.sprite_time = time;
        }
    }

    /// Frame count and loop duration of the first sprite sheet of the draw
    /// at `step`.
    pub fn sprite_timing(&self, step: usize) -> Option<(usize, f32)> {
        let Some(PreparedStep::Material(_, d)) = self.steps.get(step) else {
            return None;
        };
        let anim = d.draw.sprites.first()?;
        Some((anim.frames.len(), anim.duration()))
    }

    /// Bind `texture` to `g_Texture<slot>` of the material draw at `step`.
    pub fn set_texture(&mut self, step: usize, slot: usize, texture: Arc<GpuTexture>) {
        if let Some(PreparedStep::Material(_, d)) = self.steps.get_mut(step) {
            d.set_texture(slot, texture);
        }
    }

    /// Replace the model matrix of the draw at `step` (its index in the
    /// list given to [`Self::set_frame`]).
    pub fn set_model(&mut self, step: usize, model: Mat4) {
        match self.steps.get_mut(step) {
            Some(PreparedStep::Material(_, d)) => d.draw.model = model,
            Some(PreparedStep::Image(_, d)) => d.draw.model = model,
            _ => {}
        }
    }

    /// Set a uniform for every material draw (e.g. the audio spectrum).
    /// Zero `target` before the next frame draws (effect `clear` functions
    /// on targets that carry over between frames).
    pub fn clear_target(&mut self, target: TargetId) {
        if !self.pending_clears.contains(&target) {
            self.pending_clears.push(target);
        }
    }

    pub fn set_global_uniform(&mut self, name: &str, values: &[f32]) {
        match self.globals.get_mut(name) {
            Some(v) => {
                v.clear();
                v.extend_from_slice(values);
            }
            None => {
                self.globals.insert(name.to_owned(), values.to_vec());
            }
        }
    }

    /// Replace the mesh of the material draw at `step` (e.g. particles each frame).
    pub fn set_mesh(&mut self, step: usize, mesh: crate::Mesh) {
        if let Some(PreparedStep::Material(_, d)) = self.steps.get_mut(step) {
            d.set_mesh(mesh);
        }
    }

    /// Override a uniform of the material draw at `step` from the next frame on.
    pub fn set_uniform(&mut self, step: usize, name: &str, values: Vec<f32>) {
        if let Some(PreparedStep::Material(_, d)) = self.steps.get_mut(step) {
            d.overrides.insert(name.to_owned(), values);
        }
    }

    fn prepare_image(&self, gpu: &Gpu, draw: ImageDraw) -> PreparedImageDraw {
        let uniforms = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image uniforms"),
            size: size_of::<ImageUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image"),
            layout: &self.images.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&draw.texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&draw.texture.sampler),
                },
            ],
        });
        PreparedImageDraw {
            draw,
            uniforms,
            bind_group,
        }
    }

    /// Record the frame at `size`, then present `_rt_default` onto `output`.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        globals: FrameGlobals,
        output: &wgpu::TextureView,
        size: (u32, u32),
    ) {
        self.draw_scene(gpu, encoder, globals, size);
        let generation = self.targets.generation;
        if self
            .blit_bind_group
            .as_ref()
            .is_none_or(|(g, _)| *g != generation)
        {
            let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("blit"),
                layout: &self.blit_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(
                            &self.targets.get(TargetId::DEFAULT).view,
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.targets.sampler),
                    },
                ],
            });
            self.blit_bind_group = Some((generation, group));
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("present"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: output,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.blit_pipeline);
        if let Some((_, group)) = &self.blit_bind_group {
            pass.set_bind_group(0, group, &[]);
        }
        pass.draw(0..3, 0..1);
    }

    /// Render one frame offscreen and read `_rt_default` back as opaque RGBA8.
    pub fn capture(&mut self, gpu: &Gpu, globals: FrameGlobals, size: (u32, u32)) -> Vec<u8> {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        self.draw_scene(gpu, &mut encoder, globals, size);
        let rt = self.targets.get(TargetId::DEFAULT);

        let row = size.0 * 4;
        let padded_row = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture"),
            size: padded_row as u64 * size.1 as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            rt.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(size.1),
                },
            },
            wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit([encoder.finish()]);

        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("map capture buffer"));
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll device");
        let data = slice.get_mapped_range().expect("capture buffer mapped");
        let mut out = Vec::with_capacity((row * size.1) as usize);
        for r in data.chunks_exact(padded_row as usize) {
            out.extend_from_slice(&r[..row as usize]);
        }
        for px in out.chunks_exact_mut(4) {
            px[3] = 255;
        }
        out
    }

    fn draw_scene(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        mut globals: FrameGlobals,
        size: (u32, u32),
    ) {
        let size = (size.0.max(1), size.1.max(1));
        globals.screen = Vec2::new(size.0 as f32, size.1 as f32);
        self.targets.prepare(gpu, size);

        for step in &mut self.steps {
            match step {
                PreparedStep::Image(_, d) => {
                    let uv = d.draw.texture.uv_scale();
                    let uniforms = ImageUniforms {
                        mvp: (globals.view_proj * d.draw.model).to_cols_array_2d(),
                        color: d.draw.color.to_array(),
                        size_uv: [d.draw.size.x, d.draw.size.y, uv[0], uv[1]],
                    };
                    gpu.queue
                        .write_buffer(&d.uniforms, 0, bytemuck::bytes_of(&uniforms));
                }
                PreparedStep::Material(_, d) => d.update(
                    gpu,
                    &globals,
                    &self.globals,
                    &self.placeholders,
                    &self.targets,
                ),
                PreparedStep::Copy { .. } => {}
            }
        }

        for target in std::mem::take(&mut self.pending_clears) {
            if target.0 >= self.targets.len() {
                continue;
            }
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets.get(target).view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        let mut written = vec![false; self.targets.len()];
        self.depth.begin_frame(self.targets.len());
        let mut i = 0;
        while i < self.steps.len() {
            if self.hidden[i] && matches!(self.steps[i], PreparedStep::Copy { .. }) {
                i += 1;
                continue;
            }
            if let PreparedStep::Copy { from, to } = self.steps[i] {
                let (src, dst) = (self.targets.get(from), self.targets.get(to));
                encoder.copy_texture_to_texture(
                    src.texture.as_image_copy(),
                    dst.texture.as_image_copy(),
                    wgpu::Extent3d {
                        width: src.size.0.min(dst.size.0),
                        height: src.size.1.min(dst.size.1),
                        depth_or_array_layers: 1,
                    },
                );
                written[to.0] = true;
                i += 1;
                continue;
            }
            let target = self.steps[i].draw_target().expect("not a copy");
            let end = (i..self.steps.len())
                .find(|&j| self.steps[j].draw_target() != Some(target))
                .unwrap_or(self.steps.len());
            // Hidden draws must not clear their target either.
            if self.hidden[i..end].iter().all(|&h| h) {
                i = end;
                continue;
            }
            let load = if written[target.0] {
                wgpu::LoadOp::Load
            } else if target == TargetId::DEFAULT {
                wgpu::LoadOp::Clear(self.clear_color)
            } else {
                wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
            };
            written[target.0] = true;
            let format = self.targets.format(target);
            let color = self.targets.get(target);
            let depth = if needs_depth(&self.steps[i..end], &self.hidden[i..end]) {
                let (view, first) = self
                    .depth
                    .get(target, color.size, |size| depth_view(gpu, size));
                let load = if first {
                    wgpu::LoadOp::Clear(1.0)
                } else {
                    wgpu::LoadOp::Load
                };
                Some(wgpu::RenderPassDepthStencilAttachment {
                    view,
                    depth_ops: Some(wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                })
            } else {
                None
            };
            let depth_attached = depth.is_some();
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: depth,
                ..Default::default()
            });
            for (step, hidden) in self.steps[i..end].iter().zip(&self.hidden[i..end]) {
                if *hidden {
                    continue;
                }
                match step {
                    PreparedStep::Image(_, d) if format == DEFAULT_FORMAT => {
                        let depth = PassDepth::new(RasterState::default(), depth_attached);
                        pass.set_pipeline(&self.images.get(&gpu.device, d.draw.blend, depth));
                        pass.set_bind_group(0, &d.bind_group, &[]);
                        pass.draw(0..4, 0..1);
                    }
                    PreparedStep::Material(_, d) => {
                        d.record(&gpu.device, &mut pass, format, depth_attached)
                    }
                    _ => {}
                }
            }
            i = end;
        }
        if !written[TargetId::DEFAULT.0] {
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets.get(TargetId::DEFAULT).view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use wpe_shader::glslang::{self, Stage};
    use wpe_shader::{CompiledShader, Reflection};

    use super::*;
    use crate::material::{Camera, CullMode, Mesh, MeshData, ShaderProgram};

    const BLOCK: &str = "layout(set = 0, binding = 0) uniform Globals {
        mat4 g_ModelViewProjectionMatrix; vec4 g_Color; };";

    fn flat_program(gpu: &Gpu) -> Arc<ShaderProgram> {
        let vert = format!(
            "#version 450\n{BLOCK}\nlayout(location = 0) in vec3 a_Position;\n\
             void main() {{ gl_Position = g_ModelViewProjectionMatrix * vec4(a_Position, 1.0); }}"
        );
        let frag = format!(
            "#version 450\n{BLOCK}\nlayout(location = 0) out vec4 o;\n\
             void main() {{ o = g_Color; }}"
        );
        let spirv = glslang::compile(&[(Stage::Vertex, &vert), (Stage::Fragment, &frag)])
            .expect("test shaders compile");
        let mut reflection = Reflection::default();
        for (stage, words) in [(Stage::Vertex, &spirv[0]), (Stage::Fragment, &spirv[1])] {
            let module = wpe_shader::reflect::parse_spirv(words).unwrap();
            reflection.add_stage(stage, &module).unwrap();
        }
        let shader = CompiledShader {
            info: Default::default(),
            combos: Default::default(),
            vertex: spirv[0].clone(),
            fragment: spirv[1].clone(),
            reflection,
        };
        Arc::new(ShaderProgram::new(gpu, "flat", &shader).unwrap())
    }

    /// A quad over `x0..x1` × `y0..y1` in clip space at depth `z`.
    fn quad(x0: f32, x1: f32, y0: f32, y1: f32, z: f32) -> Mesh {
        Mesh::Triangles(MeshData {
            vertex_count: 6,
            attributes: vec![(
                "a_Position".to_owned(),
                3,
                [[x0, y0], [x1, y0], [x1, y1], [x0, y0], [x1, y1], [x0, y1]]
                    .iter()
                    .flat_map(|[x, y]| [*x, *y, z])
                    .collect(),
            )],
            indices: None,
        })
    }

    fn draw(
        program: &Arc<ShaderProgram>,
        mesh: Mesh,
        color: [f32; 4],
        raster: RasterState,
    ) -> Step {
        Step::Draw {
            target: TargetId::DEFAULT,
            draw: Draw::Material(MaterialDraw {
                program: program.clone(),
                textures: Vec::new(),
                constants: HashMap::from([("g_Color".to_owned(), color.to_vec())]),
                model: Mat4::IDENTITY,
                mesh,
                blend: BlendMode::Normal,
                camera: Camera::Fixed(Mat4::IDENTITY),
                sprites: Vec::new(),
                raster,
            }),
        }
    }

    #[test]
    fn depth_tested_draws_share_a_pass_with_flat_ones() {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let Ok(gpu) = pollster::block_on(Gpu::new(instance, None)) else {
            eprintln!("no GPU; skipped");
            return;
        };
        let program = flat_program(&gpu);
        let model = RasterState {
            depth_test: true,
            depth_write: true,
            cull: CullMode::Back,
        };
        let behind = RasterState {
            depth_write: false,
            ..model
        };
        let mut renderer = SceneRenderer::new(&gpu, DEFAULT_FORMAT);
        renderer.set_frame(
            &gpu,
            Vec::new(),
            vec![
                // Near red on the left, then far green everywhere: green
                // only shows on the right.
                draw(
                    &program,
                    quad(-1.0, 0.0, -1.0, 1.0, 0.2),
                    [1., 0., 0., 1.],
                    model,
                ),
                draw(
                    &program,
                    quad(-1.0, 1.0, -1.0, 1.0, 0.6),
                    [0., 1., 0., 1.],
                    behind,
                ),
                // A flat draw ignores depth: blue over the bottom right.
                draw(
                    &program,
                    quad(0.0, 1.0, -1.0, 0.0, 0.9),
                    [0., 0., 1., 1.],
                    RasterState::default(),
                ),
                // Clockwise, so culled.
                draw(
                    &program,
                    quad(-1.0, 1.0, 1.0, -1.0, 0.0),
                    [1., 1., 1., 1.],
                    model,
                ),
            ],
        );
        let globals = FrameGlobals::default();
        for _ in 0..2 {
            let pixels = renderer.capture(&gpu, globals, (4, 4));
            let at = |x: usize, y: usize| &pixels[(y * 4 + x) * 4..][..3];
            assert_eq!(at(0, 0), [255, 0, 0]);
            assert_eq!(at(1, 3), [255, 0, 0]);
            assert_eq!(at(3, 0), [0, 255, 0]);
            assert_eq!(at(3, 3), [0, 0, 255]);
        }
    }
}
