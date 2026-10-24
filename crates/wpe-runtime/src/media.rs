//! The desktop's media player state for wallpapers: SceneScript media events
//! and the album art behind the `$mediaThumbnail` texture.

use std::sync::Arc;

use wpe_media::{MediaEvent, MediaMonitor};
use wpe_render::{Gpu, GpuTexture, upload_texture};

/// Name of the system texture showing the current album art.
pub const THUMBNAIL: &str = "$mediaThumbnail";

pub struct Media {
    monitor: MediaMonitor,
    /// The album art texture (transparent without art) and its generation.
    texture: Arc<GpuTexture>,
    generation: u64,
}

fn transparent(gpu: &Gpu) -> Arc<GpuTexture> {
    Arc::new(upload_texture(
        gpu,
        &crate::rgba_texture(1, 1, vec![0; 4]),
        "no album art",
    ))
}

impl Media {
    /// Start watching media players; `None` without a session bus.
    pub fn start(gpu: &Gpu) -> Option<Self> {
        let monitor = MediaMonitor::start()
            .inspect_err(|e| tracing::info!("media integration unavailable: {e}"))
            .ok()?;
        Some(Self {
            monitor,
            texture: transparent(gpu),
            generation: 0,
        })
    }

    /// Events since the last call, oldest first.
    pub fn poll(&mut self) -> Vec<MediaEvent> {
        self.monitor.poll()
    }

    /// The album art texture; `true` alongside it when it changed since the
    /// last call.
    pub fn thumbnail(&mut self, gpu: &Gpu) -> (Arc<GpuTexture>, bool) {
        let generation = self.monitor.thumbnail_generation();
        if generation == self.generation {
            return (self.texture.clone(), false);
        }
        self.generation = generation;
        self.texture = match self.monitor.thumbnail() {
            Some(art) if generation != 0 => Arc::new(upload_texture(
                gpu,
                &crate::rgba_texture(art.width, art.height, art.rgba.clone()),
                "album art",
            )),
            _ => transparent(gpu),
        };
        (self.texture.clone(), true)
    }
}
