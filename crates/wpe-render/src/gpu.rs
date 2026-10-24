#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("no suitable GPU adapter: {0}")]
    Adapter(#[from] wgpu::RequestAdapterError),
    #[error("failed to create device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
}

/// Shared GPU handles.
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    /// Features used when the adapter offers them. BC compression covers the
    /// DXT/BC textures Wallpaper Engine ships; without it those are skipped.
    const OPTIONAL_FEATURES: wgpu::Features = wgpu::Features::TEXTURE_COMPRESSION_BC
        .union(wgpu::Features::TEXTURE_COMPRESSION_BC_SLICED_3D);

    pub async fn new(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self, GpuError> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface,
                ..Default::default()
            })
            .await?;
        let info = adapter.get_info();
        tracing::info!("using {} ({:?}, {})", info.name, info.backend, info.driver);

        let required_features = adapter.features() & Self::OPTIONAL_FEATURES;
        if !required_features.contains(wgpu::Features::TEXTURE_COMPRESSION_BC) {
            tracing::warn!(
                "adapter lacks BC texture compression; compressed textures will be blank"
            );
        }
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("wpe device"),
                required_features,
                required_limits: wgpu::Limits {
                    max_inter_stage_shader_variables: adapter
                        .limits()
                        .max_inter_stage_shader_variables,
                    ..wgpu::Limits::default().using_resolution(adapter.limits())
                },
                ..Default::default()
            })
            .await?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    pub fn supports_bc(&self) -> bool {
        self.device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    }
}
