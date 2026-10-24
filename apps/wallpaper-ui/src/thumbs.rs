//! Tile thumbnails ([`wpe_library::thumbs`]), decoded a few at a time off
//! the UI thread and kept in memory while they are being shown.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use iced::widget::image::Handle;
use tokio::sync::Semaphore;

/// Handles kept in memory; the least recently shown go first.
const KEEP: usize = 500;

static WORKERS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(4)));

/// The thumbnail of `preview`, from the disk cache or made now.
pub async fn load(preview: PathBuf) -> Option<Handle> {
    let _permit = WORKERS.clone().acquire_owned().await.ok()?;
    tokio::task::spawn_blocking(move || {
        let rgba = wpe_library::thumbs::rgba(&preview)?;
        Some(Handle::from_rgba(
            rgba.width(),
            rgba.height(),
            rgba.into_raw(),
        ))
    })
    .await
    .ok()
    .flatten()
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
            let mut by_age: Vec<_> = self
                .handles
                .iter()
                .map(|(k, (_, t))| (*t, k.clone()))
                .collect();
            by_age.sort_unstable();
            for (_, id) in by_age.into_iter().take(self.handles.len() - KEEP) {
                self.handles.remove(&id);
            }
        }
    }
}
