use std::sync::Arc;

use glam::{Mat4, Vec2, Vec4};

use crate::image_pipeline::{ImagePipelines, ImageUniforms};
use crate::material::{
    Bindables, FrameGlobals, MaterialDraw, PassDepth, Placeholders, PreparedMaterialDraw,
    RasterState, TexRef,
};
use crate::shadow::{ShadowLights, ShadowMaps};
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
    /// Draw the material draw at step `source` again into `target`, seen by
    /// the reflected camera ([`FrameGlobals::reflected`]) and lit by mirrored
    /// lights: a planar reflection. It shows and hides with its source.
    ///
    /// The first mirror step and every step after it run before the
    /// frame's other steps, so the reflection is ready when they sample it.
    Mirror {
        target: TargetId,
        source: usize,
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
    Copy {
        from: TargetId,
        to: TargetId,
    },
    /// [`Step::Mirror`], with its source's depth and culling.
    Mirror {
        target: TargetId,
        source: usize,
        raster: RasterState,
    },
}

impl PreparedStep {
    fn draw_target(&self) -> Option<TargetId> {
        match self {
            Self::Image(t, _) | Self::Material(t, _) => Some(*t),
            Self::Mirror { target, .. } => Some(*target),
            Self::Copy { .. } => None,
        }
    }

    /// Depth and culling the step draws with; only materials use depth.
    fn raster(&self) -> RasterState {
        match self {
            Self::Material(_, d) => d.draw.raster,
            Self::Mirror { raster, .. } => *raster,
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
    blit_module: wgpu::ShaderModule,
    blit_pipeline_layout: wgpu::PipelineLayout,
    /// Present pipelines by output format.
    blit_pipelines: std::collections::HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    output_format: wgpu::TextureFormat,
    blit_layout: wgpu::BindGroupLayout,
    blit_bind_group: Option<(u64, wgpu::BindGroup)>,
    targets: Targets,
    /// Depth buffers of targets that 3D draws render into.
    depth: DepthBuffers,
    steps: Vec<PreparedStep>,
    /// Steps skipped this frame.
    hidden: Vec<bool>,
    /// Steps from here on run first ([`Step::Mirror`]).
    prelude: usize,
    placeholders: Placeholders,
    /// Uniforms written into every material draw that declares them.
    globals: std::collections::HashMap<String, Vec<f32>>,
    /// Targets to zero before the next frame's passes ([`Self::clear_target`]).
    pending_clears: Vec<TargetId>,
    /// Uniforms in output texels ([`Self::set_texel_uniform`]).
    texel_uniforms: Vec<(usize, String, [f32; 4])>,
    /// Shadow-casting lights, their casters and the shadow atlas.
    shadows: ShadowMaps,
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
        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[Some(&blit_layout)],
            immediate_size: 0,
        });
        let mut renderer = Self {
            images: ImagePipelines::new(device),
            blit_module: module,
            blit_pipeline_layout,
            blit_pipelines: Default::default(),
            output_format,
            blit_layout,
            blit_bind_group: None,
            targets: Targets::new(gpu),
            depth: DepthBuffers::default(),
            steps: Vec::new(),
            hidden: Vec::new(),
            prelude: 0,
            placeholders: Placeholders::new(gpu),
            globals: Default::default(),
            pending_clears: Vec::new(),
            texel_uniforms: Vec::new(),
            shadows: ShadowMaps::default(),
            clear_color: wgpu::Color::BLACK,
        };
        renderer.blit_pipeline(device, output_format);
        renderer
    }

    /// The present pipeline writing into `format`.
    fn blit_pipeline(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        let (module, layout) = (&self.blit_module, &self.blit_pipeline_layout);
        self.blit_pipelines
            .entry(format)
            .or_insert_with(|| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("blit"),
                    layout: Some(layout),
                    vertex: wgpu::VertexState {
                        module,
                        entry_point: Some("vs_main"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: Default::default(),
                    depth_stencil: None,
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module,
                        entry_point: Some("fs_main"),
                        compilation_options: Default::default(),
                        targets: &[Some(format.into())],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            })
            .clone()
    }

    /// Format of `_rt_default`: [`DEFAULT_FORMAT`], or
    /// [`crate::HDR_FORMAT`] for HDR scenes. Every other target holding scene
    /// colour must match it, since copies need identical formats.
    pub fn set_scene_format(&mut self, format: wgpu::TextureFormat) {
        self.targets.set_default_format(format);
    }

    pub fn scene_format(&self) -> wgpu::TextureFormat {
        self.targets.format(TargetId::DEFAULT)
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
                Step::Mirror { target, source } => PreparedStep::Mirror {
                    target,
                    source,
                    raster: RasterState::default(),
                },
            })
            .collect();
        for i in 0..self.steps.len() {
            let PreparedStep::Mirror { source, .. } = self.steps[i] else {
                continue;
            };
            let raster = match self.steps.get_mut(source) {
                Some(PreparedStep::Material(_, d)) => {
                    d.enable_mirror(gpu);
                    d.draw.raster
                }
                _ => continue,
            };
            if let PreparedStep::Mirror { raster: r, .. } = &mut self.steps[i] {
                *r = raster;
            }
        }
        self.prelude = self
            .steps
            .iter()
            .position(|s| matches!(s, PreparedStep::Mirror { .. }))
            .unwrap_or(self.steps.len());
        self.hidden = vec![false; self.steps.len()];
        self.texel_uniforms.clear();
        self.shadows.set_casters(Vec::new());
    }

    /// The scene's shadow-casting lights (none: no shadow atlas). Set again
    /// whenever they move.
    pub fn set_shadow_lights(&mut self, lights: ShadowLights) {
        self.shadows.set_lights(lights);
    }

    /// The material draws (step indices) whose geometry casts shadows.
    pub fn set_shadow_casters(&mut self, steps: Vec<usize>) {
        self.shadows.set_casters(steps);
    }

    /// Set uniform `name` of the material draw at `step` every frame to
    /// `scale` times the output's texel size (1/width, 1/height, 1/width,
    /// 1/height), for sample offsets that follow the output resolution.
    pub fn set_texel_uniform(&mut self, step: usize, name: &str, scale: [f32; 4]) {
        self.texel_uniforms.push((step, name.to_owned(), scale));
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
        let format = self.output_format;
        self.present(gpu, encoder, output, format);
    }

    /// Draw `_rt_default` onto `output`, clamped to the displayable range
    /// (HDR scenes hold values above 1).
    fn present(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        format: wgpu::TextureFormat,
    ) {
        let pipeline = self.blit_pipeline(&gpu.device, format);
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
        pass.set_pipeline(&pipeline);
        if let Some((_, group)) = &self.blit_bind_group {
            pass.set_bind_group(0, group, &[]);
        }
        pass.draw(0..3, 0..1);
    }

    /// Render one frame offscreen and read `_rt_default` back as opaque RGBA8.
    pub fn capture(&mut self, gpu: &Gpu, globals: FrameGlobals, size: (u32, u32)) -> Vec<u8> {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        self.draw_scene(gpu, &mut encoder, globals, size);
        // An HDR `_rt_default` is read through the present pass, as shown.
        let presented = (self.scene_format() != DEFAULT_FORMAT).then(|| {
            let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("capture"),
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEFAULT_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            self.present(gpu, &mut encoder, &view, DEFAULT_FORMAT);
            texture
        });
        let source = match &presented {
            Some(texture) => texture,
            None => &self.targets.get(TargetId::DEFAULT).texture,
        };

        let row = size.0 * 4;
        let padded_row = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture"),
            size: padded_row as u64 * size.1 as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            source.as_image_copy(),
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

    /// Clear the depth buffers the shown draws in `start..end` (drawing into
    /// `target`) sample, if nothing drew depth into them yet this frame.
    fn clear_sampled_depth(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        start: usize,
        end: usize,
        target: TargetId,
    ) {
        for j in start..end {
            let (Some(PreparedStep::Material(_, d)), Some(false)) =
                (self.steps.get(j), self.hidden.get(j))
            else {
                continue;
            };
            for tex in d.draw.textures.iter().flatten() {
                let TexRef::Depth(t) = *tex else {
                    continue;
                };
                if t.0 >= self.targets.len() || t == target {
                    continue;
                }
                let size = self.targets.get(t).size;
                let (view, first) = self.depth.get(t, size, |size| depth_view(gpu, size));
                if first {
                    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("clear sampled depth"),
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view,
                            depth_ops: Some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(1.0),
                                store: wgpu::StoreOp::Store,
                            }),
                            stencil_ops: None,
                        }),
                        ..Default::default()
                    });
                }
            }
        }
    }

    /// Create the pipelines the current frame's draws use, so the first
    /// frame doesn't stall on them (scene loading calls this off the render
    /// thread). Draws are grouped into passes as [`Self::render`] does.
    pub fn warm_pipelines(&mut self, gpu: &Gpu) {
        let scene_format = self.scene_format();
        let len = self.steps.len().min(self.hidden.len());
        let mut i = 0;
        while i < len {
            let Some(target) = self.steps[i].draw_target() else {
                i += 1;
                continue;
            };
            let end = (i..len)
                .find(|&j| self.steps[j].draw_target() != Some(target))
                .unwrap_or(len);
            if target.0 >= self.targets.len() {
                i = end;
                continue;
            }
            let format = self.targets.format(target);
            let depth_attached = needs_depth(&self.steps[i..end], &self.hidden[i..end]);
            for step in &self.steps[i..end] {
                match step {
                    PreparedStep::Image(_, d) if format == scene_format => {
                        let depth = PassDepth::new(RasterState::default(), depth_attached);
                        self.images.get(&gpu.device, d.draw.blend, depth, format);
                    }
                    PreparedStep::Material(_, d) => d.warm(&gpu.device, format, depth_attached),
                    PreparedStep::Mirror { source, .. } => {
                        if let Some(PreparedStep::Material(_, d)) = self.steps.get(*source) {
                            d.warm(&gpu.device, format, depth_attached);
                        }
                    }
                    _ => {}
                }
            }
            i = end;
        }
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
        for (step, name, scale) in &self.texel_uniforms {
            if let Some(PreparedStep::Material(_, d)) = self.steps.get_mut(*step) {
                let texel = [globals.screen.x, globals.screen.y].map(|v| 1.0 / v);
                let values = (0..4).map(|i| scale[i] * texel[i % 2]).collect();
                d.overrides.insert(name.clone(), values);
            }
        }

        for (name, values) in self.shadows.prepare(gpu, &globals) {
            self.globals.insert(name, values);
        }
        // Depth buffers that draws sample exist before they are bound.
        for step in &self.steps {
            let PreparedStep::Material(_, d) = step else {
                continue;
            };
            for tex in d.draw.textures.iter().flatten() {
                if let TexRef::Depth(t) = *tex
                    && t.0 < self.targets.len()
                {
                    let size = self.targets.get(t).size;
                    self.depth.ensure(t, size, |size| depth_view(gpu, size));
                }
            }
        }
        let bindables = Bindables {
            placeholders: &self.placeholders,
            targets: &self.targets,
            depth: &self.depth,
            shadow_atlas: self.shadows.atlas(),
            generation: self
                .targets
                .generation
                .wrapping_add(self.depth.generation)
                .wrapping_add(self.shadows.generation),
        };
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
                PreparedStep::Material(_, d) => d.update(gpu, &globals, &self.globals, &bindables),
                PreparedStep::Copy { .. } | PreparedStep::Mirror { .. } => {}
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
        if self.shadows.atlas().is_some() {
            let casters: Vec<(usize, &PreparedMaterialDraw)> = self
                .shadows
                .casters
                .iter()
                .filter_map(|&i| match (self.steps.get(i), self.hidden.get(i)) {
                    (Some(PreparedStep::Material(_, d)), Some(false)) => Some((i, d)),
                    _ => None,
                })
                .collect();
            self.shadows.render(gpu, encoder, &casters);
        }
        let scene_format = self.scene_format();
        let mut written = vec![false; self.targets.len()];
        self.depth.begin_frame(self.targets.len());
        let (prelude, len) = (self.prelude.min(self.steps.len()), self.steps.len());
        for i in prelude..len {
            if let PreparedStep::Mirror { source, .. } = self.steps[i] {
                self.hidden[i] = self.hidden.get(source).copied().unwrap_or(true);
            }
        }
        // Mirror passes and the steps after them first, then the rest.
        for (start, stop) in [(prelude, len), (0, prelude)] {
            let mut i = start;
            while i < stop {
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
                let end = (i..stop)
                    .find(|&j| self.steps[j].draw_target() != Some(target))
                    .unwrap_or(stop);
                // Hidden draws must not clear their target either.
                if self.hidden[i..end].iter().all(|&h| h) {
                    i = end;
                    continue;
                }
                self.clear_sampled_depth(gpu, encoder, i, end, target);
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
                        PreparedStep::Image(_, d) if format == scene_format => {
                            let depth = PassDepth::new(RasterState::default(), depth_attached);
                            let pipeline =
                                self.images.get(&gpu.device, d.draw.blend, depth, format);
                            pass.set_pipeline(&pipeline);
                            pass.set_bind_group(0, &d.bind_group, &[]);
                            pass.draw(0..4, 0..1);
                        }
                        PreparedStep::Material(_, d) => {
                            d.record(&gpu.device, &mut pass, format, depth_attached)
                        }
                        PreparedStep::Mirror { source, .. } => {
                            if let Some(PreparedStep::Material(_, d)) = self.steps.get(*source) {
                                d.record_mirror(&gpu.device, &mut pass, format, depth_attached)
                            }
                        }
                        _ => {}
                    }
                }
                i = end;
            }
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

    #[test]
    fn hdr_scenes_keep_overbright_values_until_presented() {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let Ok(gpu) = pollster::block_on(Gpu::new(instance, None)) else {
            eprintln!("no GPU; skipped");
            return;
        };
        let program = flat_program(&gpu);
        let full = || quad(-1.0, 1.0, -1.0, 1.0, 0.5);
        let steps = || {
            let mut subtract = draw(
                &program,
                full(),
                [-1.0, -1.0, 0.0, 1.0],
                RasterState::default(),
            );
            if let Step::Draw {
                draw: Draw::Material(d),
                ..
            } = &mut subtract
            {
                d.blend = BlendMode::Additive;
            }
            vec![
                draw(
                    &program,
                    full(),
                    [1.5, 2.5, 0.25, 1.0],
                    RasterState::default(),
                ),
                subtract,
            ]
        };
        let mut renderer = SceneRenderer::new(&gpu, DEFAULT_FORMAT);
        renderer.set_frame(&gpu, Vec::new(), steps());
        // 8-bit targets clamp each draw's output.
        let pixels = renderer.capture(&gpu, FrameGlobals::default(), (2, 2));
        assert_eq!(&pixels[..3], [255, 255, 64]);

        renderer.set_scene_format(crate::HDR_FORMAT);
        renderer.set_frame(&gpu, Vec::new(), steps());
        let pixels = renderer.capture(&gpu, FrameGlobals::default(), (2, 2));
        // 1.5 - 1 and 2.5 - 1 (clamped when presented).
        assert_eq!(&pixels[..3], [128, 255, 64]);
    }

    #[test]
    fn mirror_draws_run_first_with_the_reflected_camera() {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let Ok(gpu) = pollster::block_on(Gpu::new(instance, None)) else {
            eprintln!("no GPU; skipped");
            return;
        };
        let program = flat_program(&gpu);
        let reflection = TargetId(1);
        let steps = || {
            // Red over the top half, seen by the perspective camera.
            let mut top = draw(
                &program,
                quad(-1.0, 1.0, 0.0, 1.0, 0.5),
                [1., 0., 0., 1.],
                RasterState::default(),
            );
            if let Step::Draw {
                draw: Draw::Material(d),
                ..
            } = &mut top
            {
                d.camera = Camera::Perspective;
            }
            vec![
                // Reads the reflection before the mirror step's index: only
                // works because mirror steps run first.
                Step::Copy {
                    from: reflection,
                    to: TargetId::DEFAULT,
                },
                top,
                Step::Mirror {
                    target: reflection,
                    source: 1,
                },
            ]
        };
        let targets = || {
            vec![TargetDesc {
                name: "_rt_Reflection".to_owned(),
                size: crate::TargetSize::Screen { divisor: 1.0 },
                format: DEFAULT_FORMAT,
            }]
        };
        let globals = FrameGlobals {
            perspective: Mat4::IDENTITY,
            reflected: Mat4::from_scale(glam::Vec3::new(1.0, -1.0, 1.0)),
            ..Default::default()
        };
        let mut renderer = SceneRenderer::new(&gpu, DEFAULT_FORMAT);
        renderer.set_frame(&gpu, targets(), steps());
        let pixels = renderer.capture(&gpu, globals, (4, 4));
        let at = |pixels: &[u8], y: usize| pixels[y * 16..][..3].to_vec();
        // The mirror image fills the bottom half.
        assert_eq!(at(&pixels, 0), [255, 0, 0]);
        assert_eq!(at(&pixels, 3), [255, 0, 0]);

        // A hidden source hides its mirror too.
        let mut renderer = SceneRenderer::new(&gpu, DEFAULT_FORMAT);
        renderer.set_frame(&gpu, targets(), steps());
        renderer.set_visible(1, false);
        let pixels = renderer.capture(&gpu, globals, (4, 4));
        assert_eq!(at(&pixels, 3), [0, 0, 0]);
    }
}
