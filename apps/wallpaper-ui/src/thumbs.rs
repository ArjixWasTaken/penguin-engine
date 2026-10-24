//! Tile thumbnails: the preview's first frame, scaled down once and kept on
//! disk, decoded a few at a time off the UI thread.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::UNIX_EPOCH;

use iced::widget::image::Handle;
use tokio::sync::Semaphore;

/// Thumbnails are this size (16:9, covering), twice a default tile for
/// sharpness on scaled outputs.
pub const WIDTH: u32 = 400;
pub const HEIGHT: u32 = 225;

/// Handles kept in memory; the least recently shown go first.
const KEEP: usize = 500;

static WORKERS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(4)));

pub fn cache_dir() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("wallpaper-engine-rs/thumbs"))
}

/// The thumbnail of `preview`, from the disk cache or made now.
pub async fn load(preview: PathBuf) -> Option<Handle> {
    let _permit = WORKERS.clone().acquire_owned().await.ok()?;
    tokio::task::spawn_blocking(move || make(&preview))
        .await
        .ok()
        .flatten()
}

fn make(preview: &Path) -> Option<Handle> {
    let cached = cache_dir().map(|dir| dir.join(format!("{}.png", key(preview))));
    if let Some(image) = cached.as_ref().and_then(|c| image::open(c).ok()) {
        return Some(handle(image.into_rgba8()));
    }
    // `image::open` decodes the first frame of a GIF.
    let image = match image::open(preview) {
        Ok(image) => image,
        Err(e) => {
            tracing::debug!("{}: {e}", preview.display());
            return None;
        }
    };
    let thumb = image
        .resize_to_fill(WIDTH, HEIGHT, image::imageops::FilterType::Triangle)
        .into_rgba8();
    if let Some(cached) = cached {
        let _ = std::fs::create_dir_all(cached.parent().unwrap());
        if let Err(e) = thumb.save(&cached) {
            tracing::debug!("caching {}: {e}", cached.display());
        }
    }
    Some(handle(thumb))
}

fn handle(rgba: image::RgbaImage) -> Handle {
    Handle::from_rgba(rgba.width(), rgba.height(), rgba.into_raw())
}

/// Names the cached file: the preview's path, size and modification time.
fn key(preview: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    preview.hash(&mut hasher);
    if let Ok(meta) = std::fs::metadata(preview) {
        meta.len().hash(&mut hasher);
        if let Ok(time) = meta.modified() {
            time.duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .hash(&mut hasher);
        }
    }
    (WIDTH, HEIGHT).hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// What thumbnails are loaded or loading, by item id.
#[derive(Default)]
pub struct Thumbs {
    handles: HashMap<String, (Handle, u64)>,
    pending: HashMap<String, u64>,
    /// Ids that have no thumbnail (no preview, or it didn't decode).
    missing: std::collections::HashSet<String>,
    clock: u64,
}

impl Thumbs {
    pub fn get(&self, id: &str) -> Option<&Handle> {
        self.handles.get(id).map(|(h, _)| h)
    }

    /// The tile `id` came into view: true when its thumbnail should be
    /// loaded now.
    pub fn shown(&mut self, id: &str) -> bool {
        self.clock += 1;
        if let Some((_, used)) = self.handles.get_mut(id) {
            *used = self.clock;
            return false;
        }
        if self.missing.contains(id) || self.pending.contains_key(id) {
            return false;
        }
        self.pending.insert(id.to_owned(), self.clock);
        true
    }

    pub fn loaded(&mut self, id: String, handle: Option<Handle>) {
        self.pending.remove(&id);
        let Some(handle) = handle else {
            self.missing.insert(id);
            return;
        };
        self.clock += 1;
        self.handles.insert(id, (handle, self.clock));
        if self.handles.len() > KEEP {
            let mut by_age: Vec<_> = self.handles.iter().map(|(k, (_, t))| (*t, k.clone())).collect();
            by_age.sort_unstable();
            for (_, id) in by_age.into_iter().take(self.handles.len() - KEEP) {
                self.handles.remove(&id);
            }
        }
    }
}
