use std::collections::HashMap;

use crate::material::PassDepth;

/// How a layer's output is combined with what is already in its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BlendMode {
    /// Overwrite.
    Normal,
    /// Straight-alpha blending.
    #[default]
    Translucent,
    Additive,
    /// Premultiplied-alpha blending.
    TranslucentPremultiplied,
}

impl BlendMode {
    pub(crate) fn state(self) -> Option<wgpu::BlendState> {
        use wgpu::{BlendComponent as C, BlendFactor as F, BlendOperation as Op};
        let comp = |src_factor, dst_factor| C {
            src_factor,
            dst_factor,
            operation: Op::Add,
        };
        match self {
            Self::Normal => None,
            Self::Translucent => Some(wgpu::BlendState {
                color: comp(F::SrcAlpha, F::OneMinusSrcAlpha),
                alpha: comp(F::One, F::OneMinusSrcAlpha),
            }),
            Self::Additive => Some(wgpu::BlendState {
                color: comp(F::SrcAlpha, F::One),
                alpha: comp(F::SrcAlpha, F::One),
            }),
            Self::TranslucentPremultiplied => Some(wgpu::BlendState {
                color: comp(F::One, F::OneMinusSrcAlpha),
                alpha: comp(F::One, F::OneMinusSrcAlpha),
            }),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ImageUniforms {
    pub mvp: [[f32; 4]; 4],
    pub color: [f32; 4],
    pub size_uv: [f32; 4],
}

/// Pipelines for the built-in image shader, one per blend mode, pass depth
/// state and target format.
pub(crate) struct ImagePipelines {
    pub layout: wgpu::BindGroupLayout,
    module: wgpu::ShaderModule,
    pipeline_layout: wgpu::PipelineLayout,
    pipelines: HashMap<(BlendMode, PassDepth, wgpu::TextureFormat), wgpu::RenderPipeline>,
}

impl ImagePipelines {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::include_wgsl!("image.wgsl"));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image"),
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
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("image"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        Self {
            layout,
            module,
            pipeline_layout,
            pipelines: HashMap::new(),
        }
    }

    /// The pipeline for `blend` in a pass with `depth` (images never test
    /// or write depth, but must match the pass's attachment) into `format`.
    pub fn get(
        &mut self,
        device: &wgpu::Device,
        blend: BlendMode,
        depth: PassDepth,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        let key = (blend, depth, format);
        let pipeline = self.pipelines.entry(key).or_insert_with(|| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("image"),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &self.module,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: depth.state(),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &self.module,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: blend.state(),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        });
        pipeline.clone()
    }
}
