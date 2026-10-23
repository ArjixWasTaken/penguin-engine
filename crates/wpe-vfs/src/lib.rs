//! Virtual filesystem for Wallpaper Engine content.
//!
//! Wallpaper Engine resolves every asset path (`materials/foo.json`,
//! `shaders/genericimage2.frag`, ...) against a stack of sources: the stock
//! `assets/` directory shipped with the application, overlaid by the
//! wallpaper's own `scene.pkg` (or its unpacked directory). Lookups are
//! case-insensitive and the most recently mounted layer wins.

mod path;
mod physical;
mod pkg;

use std::borrow::Cow;

pub use path::normalize;
pub use physical::PhysicalFs;
pub use pkg::{PkgEntry, PkgFs};

#[derive(Debug, thiserror::Error)]
pub enum VfsError {
    #[error("file not found: {0}")]
    NotFound(String),
    #[error("i/o error reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("malformed pkg archive: {0}")]
    MalformedPkg(String),
}

pub type Result<T, E = VfsError> = std::result::Result<T, E>;

/// A read-only source of files addressed by normalized, lowercase paths.
pub trait FileSystem: Send + Sync {
    /// Whether `path` (already normalized) exists in this source.
    fn contains(&self, path: &str) -> bool;

    /// Read the whole file at `path` (already normalized).
    fn read(&self, path: &str) -> Result<Cow<'_, [u8]>>;

    /// All normalized file paths in this source, in no particular order.
    fn files(&self) -> Box<dyn Iterator<Item = &str> + '_>;
}

/// A stack of [`FileSystem`] layers; later mounts shadow earlier ones.
#[derive(Default)]
pub struct Vfs {
    layers: Vec<Layer>,
}

struct Layer {
    name: String,
    fs: Box<dyn FileSystem>,
}

impl Vfs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount `fs` on top of all existing layers.
    pub fn mount(&mut self, name: impl Into<String>, fs: impl FileSystem + 'static) {
        self.layers.push(Layer {
            name: name.into(),
            fs: Box::new(fs),
        });
    }

    pub fn contains(&self, path: &str) -> bool {
        let path = normalize(path);
        self.layers.iter().any(|l| l.fs.contains(&path))
    }

    pub fn read(&self, path: &str) -> Result<Cow<'_, [u8]>> {
        let norm = normalize(path);
        self.layers
            .iter()
            .rev()
            .find(|l| l.fs.contains(&norm))
            .ok_or_else(|| VfsError::NotFound(path.to_owned()))?
            .fs
            .read(&norm)
    }

    pub fn read_to_string(&self, path: &str) -> Result<String> {
        let bytes = self.read(path)?;
        // Some WE JSON files carry a UTF-8 BOM.
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    /// Name of the layer that would serve `path`, for diagnostics.
    pub fn layer_of(&self, path: &str) -> Option<&str> {
        let path = normalize(path);
        self.layers
            .iter()
            .rev()
            .find(|l| l.fs.contains(&path))
            .map(|l| l.name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn later_mount_wins() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::create_dir(a.path().join("Shaders")).unwrap();
        std::fs::write(a.path().join("Shaders/x.frag"), "a").unwrap();
        std::fs::write(a.path().join("only_a.txt"), "a").unwrap();
        std::fs::create_dir(b.path().join("shaders")).unwrap();
        std::fs::write(b.path().join("shaders/X.FRAG"), "b").unwrap();

        let mut vfs = Vfs::new();
        vfs.mount("a", PhysicalFs::new(a.path()).unwrap());
        vfs.mount("b", PhysicalFs::new(b.path()).unwrap());

        assert_eq!(&*vfs.read("shaders/x.frag").unwrap(), b"b");
        assert_eq!(&*vfs.read("/ONLY_A.txt").unwrap(), b"a");
        assert_eq!(vfs.layer_of("Shaders\\X.frag"), Some("b"));
        assert!(matches!(vfs.read("missing"), Err(VfsError::NotFound(_))));
    }
}
