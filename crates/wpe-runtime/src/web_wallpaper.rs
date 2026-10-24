//! Web wallpapers (project type `web`): the page, rendered by Servo
//! (`wpe-web`), shown over the whole output. Like a video wallpaper it is a
//! made-up one-layer scene, whose texture takes the page's frames; the page
//! is laid out at the output's size.

#[cfg(feature = "web")]
pub use enabled::*;

/// Texture name the page paints into.
pub const TEXTURE: &str = "__web";
/// The page's size until the wallpaper is first rendered.
pub const DEFAULT_SIZE: (u32, u32) = (1920, 1080);

/// User properties as pages get them, `{name: {type, value}}`.
pub fn properties_json(project: &wpe_assets::Project) -> serde_json::Value {
    project
        .general
        .properties
        .iter()
        .map(|(name, p)| (name.clone(), property_json(p.kind.as_deref(), &p.value)))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

pub fn property_json(kind: Option<&str>, value: &serde_json::Value) -> serde_json::Value {
    let mut entry = serde_json::json!({ "value": value });
    if let Some(kind) = kind {
        entry["type"] = kind.into();
    }
    entry
}

#[cfg(feature = "web")]
mod enabled {
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use wpe_render::{Gpu, GpuTexture, wgpu};
    pub use wpe_web::{Button, NetworkPolicy, shutdown};

    use crate::LoadError;

    static NETWORK: Mutex<Option<NetworkPolicy>> = Mutex::new(None);

    /// Hosts web wallpapers may reach although they are local (see
    /// [`NetworkPolicy`]); applies to pages opened afterwards.
    pub fn set_network_policy(policy: NetworkPolicy) {
        *NETWORK.lock().unwrap() = Some(policy);
    }

    fn network_policy() -> NetworkPolicy {
        if let Some(policy) = NETWORK.lock().unwrap().clone() {
            return policy;
        }
        // `WPE_WEB_ALLOW_HOSTS=nas.lan,192.168.1.10` for tools without settings.
        NetworkPolicy {
            allowed_hosts: std::env::var("WPE_WEB_ALLOW_HOSTS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
        }
    }

    /// A running web wallpaper's page and the texture showing it.
    pub struct WebState {
        pub page: wpe_web::WebPage,
        pub target: Option<Arc<GpuTexture>>,
        /// Size of the frame the target shows.
        pub shown: Option<(u32, u32)>,
        /// The page's exported textures, imported.
        pub shared: Option<Shared>,
        /// When the audio spectrum was last sent.
        pub audio_sent: Option<std::time::Instant>,
    }

    /// Exported textures frames are copied from.
    pub struct Shared {
        textures: Vec<wgpu::Texture>,
        slots: Arc<Mutex<wpe_web::Slots>>,
        size: (u32, u32),
    }

    /// Whether pages may hand frames over as dma-bufs: the renderer must
    /// be able to import them.
    pub fn can_share(gpu: &Gpu) -> bool {
        std::env::var("WPE_WEB").as_deref() != Ok("cpu")
            && gpu
                .device
                .features()
                .contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF)
    }

    /// Open the page `file` of the wallpaper in `dir`.
    pub fn open(
        dir: &Path,
        file: &str,
        project: &wpe_assets::Project,
        size: (u32, u32),
        export_frames: bool,
    ) -> Result<wpe_web::WebPage, LoadError> {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_owned());
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "page".to_owned());
        let options = wpe_web::PageOptions {
            root: dir,
            file: file.to_owned(),
            name,
            size,
            scale: 1.0,
            properties: super::properties_json(project),
            general: serde_json::json!({ "fps": 60 }),
            network: network_policy(),
            profile_dir: dirs::data_dir().map(|d| d.join("penguin-engine/web")),
            export_frames,
        };
        wpe_web::WebPage::open(options, Duration::from_secs(20))
            .map_err(|e| LoadError::Web(e.to_string()))
    }

    impl WebState {
        /// Show the page's newest frame, if it painted one.
        pub fn upload(&mut self, gpu: &Gpu) {
            let Some(target) = self.target.clone() else {
                return;
            };
            let size = target.texture.size();
            let size = (size.width, size.height);
            if let Some(frames) = self.page.take_gpu_frames() {
                match import(gpu, frames, target.texture.format()) {
                    Ok(shared) => {
                        tracing::debug!("page frames shared on the GPU at {:?}", shared.size);
                        self.shared = Some(shared);
                    }
                    Err(e) => {
                        tracing::warn!("page frames can't stay on the GPU, reading them back: {e}");
                        self.shared = None;
                        self.page.use_readback();
                    }
                }
            }
            if let Some(shared) = &self.shared
                && shared.size == size
            {
                let Some(slot) = wpe_web::Slots::lock(&shared.slots).take_ready() else {
                    return;
                };
                let source = &shared.textures[slot];
                let mut encoder = gpu.device.create_command_encoder(&Default::default());
                encoder.copy_texture_to_texture(
                    source.as_image_copy(),
                    target.texture.as_image_copy(),
                    source.size(),
                );
                gpu.queue.submit([encoder.finish()]);
                let slots = shared.slots.clone();
                gpu.queue.on_submitted_work_done(move || {
                    wpe_web::Slots::lock(&slots).release(slot);
                });
                self.shown = Some(size);
                return;
            }
            // Pixels, from a page painting in software or before a resize.
            if let Some(frame) = self.page.take_frame()
                && (frame.width, frame.height) == size
            {
                target.write_rgba(gpu, &frame.pixels);
                self.shown = Some(size);
            }
        }

        /// [`Self::upload`], first waiting up to `timeout` for a frame at
        /// the page's current size if none is shown yet (for screenshots).
        pub fn wait_frame(&mut self, gpu: &Gpu, timeout: Duration) {
            let deadline = std::time::Instant::now() + timeout;
            loop {
                self.page.tick();
                self.upload(gpu);
                if self.shown == Some(self.page.size()) {
                    return;
                }
                if std::time::Instant::now() >= deadline {
                    tracing::warn!("no frame from the page within {timeout:?}");
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        /// Show the newest frame and start the next.
        pub fn frame(&mut self, gpu: &Gpu) {
            self.upload(gpu);
            self.page.tick();
        }

        /// The output pointer (0..1, Y down) in page pixels.
        pub fn page_point(&self, pointer: glam::Vec2) -> (f32, f32) {
            let (w, h) = self.page.size();
            (pointer.x * w as f32, pointer.y * h as f32)
        }
    }

    fn import(
        gpu: &Gpu,
        frames: wpe_web::GpuFrames,
        format: wgpu::TextureFormat,
    ) -> Result<Shared, String> {
        if !matches!(
            format,
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb
        ) {
            return Err(format!("the layer texture is {format:?}"));
        }
        let size = wgpu::Extent3d {
            width: frames.width,
            height: frames.height,
            depth_or_array_layers: 1,
        };
        let textures = frames
            .buffers
            .into_iter()
            .map(|b| {
                crate::video::import_dmabuf(
                    gpu,
                    b.fd,
                    b.modifier,
                    b.stride,
                    b.offset,
                    size,
                    format,
                    "web frame",
                )
                .map_err(|e| e.to_string())
            })
            .collect::<Result<_, _>>()?;
        Ok(Shared {
            textures,
            slots: frames.slots,
            size: (frames.width, frames.height),
        })
    }
}
