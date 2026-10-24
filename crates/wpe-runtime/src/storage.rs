//! Scripts' `localStorage`, kept per wallpaper between runs in
//! `<data dir>/penguin-engine/storage/<wallpaper>.json`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Scripts may write every frame; the file is written at most this often.
const WRITE_INTERVAL: Duration = Duration::from_secs(2);

pub struct Storage {
    path: Option<PathBuf>,
    /// Data not written yet.
    pending: Option<Value>,
    last_write: Option<Instant>,
}

/// A file name for the wallpaper at `path`: its directory's name (the
/// workshop id) or, for a package or scene file, its parent's.
fn key(path: &Path) -> Option<String> {
    let path = path.canonicalize().ok()?;
    let dir = if path.is_dir() { &path } else { path.parent()? };
    Some(dir.file_name()?.to_string_lossy().into_owned())
}

impl Storage {
    pub fn for_wallpaper(path: &Path) -> Self {
        let path = dirs::data_dir().zip(key(path)).map(|(data, key)| {
            data.join("penguin-engine/storage")
                .join(format!("{key}.json"))
        });
        Self {
            path,
            pending: None,
            last_write: None,
        }
    }

    /// What scripts stored last time.
    pub fn load(&self) -> Option<Value> {
        let text = std::fs::read_to_string(self.path.as_ref()?).ok()?;
        serde_json::from_str(&text)
            .inspect_err(|e| tracing::warn!("script storage: {e}"))
            .ok()
    }

    /// Scripts changed their storage to `data`.
    pub fn changed(&mut self, data: Value) {
        self.pending = Some(data);
        if self
            .last_write
            .is_none_or(|t| t.elapsed() >= WRITE_INTERVAL)
        {
            self.flush();
        }
    }

    /// Write pending changes now.
    pub fn flush(&mut self) {
        let (Some(path), Some(data)) = (&self.path, self.pending.take()) else {
            return;
        };
        self.last_write = Some(Instant::now());
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(path.parent().expect("storage path has a parent"))?;
            std::fs::write(path, data.to_string())
        };
        if let Err(e) = write() {
            tracing::warn!("saving script storage to {}: {e}", path.display());
        }
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        self.flush();
    }
}
