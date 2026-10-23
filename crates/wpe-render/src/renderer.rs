use std::sync::Arc;

use glam::{Mat4, Vec2, Vec4};

use crate::image_pipeline::{ImagePipelines, ImageUniforms};
use crate::{BlendMode, Gpu, GpuTexture};

/// Format of `_rt_default`. Wallpaper Engine composites in non-linear
/// 8-bit space, so no sRGB conversion happens anywhere in the chain.
const RT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// One textured card drawn into `_rt_default`.
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

struct PreparedDraw {
    draw: ImageDraw,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

struct RenderTarget {
    size: (u32, u32),
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    blit_bind_group: wgpu::BindGroup,
}

/// Draws a list of image cards into an offscreen `_rt_default` and presents
/// it to an output view.
pub struct SceneRenderer {
    images: ImagePipelines,
    blit_pipeline: wgpu::RenderPipeline,
    blit_layout: wgpu::BindGroupLayout,
    blit_sampler: wgpu::Sampler,
    rt: Option<RenderTarget>,
    draws: Vec<PreparedDraw>,
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
        let blit_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            images: ImagePipelines::new(device, RT_FORMAT),
            blit_pipeline,
            blit_layout,
            blit_sampler,
            rt: None,
            draws: Vec::new(),
            clear_color: wgpu::Color::BLACK,
        }
    }

    /// Replace the draw list, in back-to-front order.
    pub fn set_draws(&mut self, gpu: &Gpu, draws: Vec<ImageDraw>) {
        self.draws = draws
            .into_iter()
            .map(|draw| {
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
                PreparedDraw {
                    draw,
                    uniforms,
                    bind_group,
                }
            })
            .collect();
    }

    fn ensure_rt(&mut self, gpu: &Gpu, size: (u32, u32)) -> &RenderTarget {
        if self.rt.as_ref().is_none_or(|rt| rt.size != size) {
            let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("_rt_default"),
                size: wgpu::Extent3d {
                    width: size.0.max(1),
                    height: size.1.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: RT_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let blit_bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("blit"),
                layout: &self.blit_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.blit_sampler),
                    },
                ],
            });
            self.rt = Some(RenderTarget {
                size,
                texture,
                view,
                blit_bind_group,
            });
        }
        self.rt.as_ref().expect("render target just created")
    }

    /// Record the frame: draws into `_rt_default` at `size`, then presents
    /// it onto `output`.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        view_proj: Mat4,
        output: &wgpu::TextureView,
        size: (u32, u32),
    ) {
        self.draw_scene(gpu, encoder, view_proj, size);
        let rt = self.rt.as_ref().expect("render target exists");
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
        pass.set_bind_group(0, &rt.blit_bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Render one frame offscreen and read `_rt_default` back as opaque RGBA8.
    pub fn capture(&mut self, gpu: &Gpu, view_proj: Mat4, size: (u32, u32)) -> Vec<u8> {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        self.draw_scene(gpu, &mut encoder, view_proj, size);
        let rt = self.rt.as_ref().expect("render target exists");

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
        view_proj: Mat4,
        size: (u32, u32),
    ) {
        for d in &self.draws {
            let uv = d.draw.texture.uv_scale();
            let uniforms = ImageUniforms {
                mvp: (view_proj * d.draw.model).to_cols_array_2d(),
                color: d.draw.color.to_array(),
                size_uv: [d.draw.size.x, d.draw.size.y, uv[0], uv[1]],
            };
            gpu.queue
                .write_buffer(&d.uniforms, 0, bytemuck::bytes_of(&uniforms));
        }
        for d in &self.draws {
            self.images.get(&gpu.device, d.draw.blend);
        }
        self.ensure_rt(gpu, size);
        let rt = self.rt.as_ref().expect("render target exists");

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("_rt_default"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &rt.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(self.clear_color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        for d in &self.draws {
            pass.set_pipeline(
                self.images
                    .cached(d.draw.blend)
                    .expect("pipelines created above"),
            );
            pass.set_bind_group(0, &d.bind_group, &[]);
            pass.draw(0..4, 0..1);
        }
    }
}
