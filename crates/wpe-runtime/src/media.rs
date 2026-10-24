//! The desktop's media player state for wallpapers: SceneScript media events
//! and the album art behind the `$mediaThumbnail` texture.

use std::sync::Arc;

use wpe_media::{MediaEvent, MediaMonitor};
use wpe_render::{Gpu, GpuTexture, upload_texture};

/// Name of the system texture showing the current album art.
pub const THUMBNAIL: &str = "$mediaThumbnail";

pub struct Media {
    monitor: MediaMonitor,
    /// The album art texture, if any, and its generation.
    texture: Option<Arc<GpuTexture>>,
    generation: u64,
}

impl Media {
    /// Start watching media players; `None` without a session bus.
    pub fn start() -> Option<Self> {
        let monitor = MediaMonitor::start()
            .inspect_err(|e| tracing::info!("media integration unavailable: {e}"))
            .ok()?;
        Some(Self {
            monitor,
            texture: None,
            generation: 0,
        })
    }

    /// Events since the last call, oldest first.
    pub fn poll(&mut self) -> Vec<MediaEvent> {
        self.monitor.poll()
    }

    /// The album art texture (`None` without art); `true` alongside it when
    /// it changed since the last call.
    pub fn thumbnail(&mut self, gpu: &Gpu) -> (Option<Arc<GpuTexture>>, bool) {
        let generation = self.monitor.thumbnail_generation();
        if generation == self.generation {
            return (self.texture.clone(), false);
        }
        self.generation = generation;
        self.texture = match self.monitor.thumbnail() {
            Some(art) if generation != 0 => Some(Arc::new(upload_texture(
                gpu,
                &crate::rgba_texture(art.width, art.height, art.rgba.clone()),
                "album art",
            ))),
            _ => None,
        };
        (self.texture.clone(), true)
    }
}
