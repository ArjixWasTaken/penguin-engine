use std::borrow::Cow;
use std::collections::HashMap;

use crate::{FileSystem, Result, VfsError, normalize};

/// Files held in memory, for documents the runtime makes up (the scene a
/// video wallpaper is shown with).
#[derive(Debug, Clone, Default)]
pub struct MemoryFs {
    files: HashMap<String, Vec<u8>>,
}

impl MemoryFs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add (or replace) the file at `path`.
    pub fn insert(&mut self, path: &str, data: impl Into<Vec<u8>>) {
        self.files.insert(normalize(path), data.into());
    }
}

impl FileSystem for MemoryFs {
    fn contains(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    fn read(&self, path: &str) -> Result<Cow<'_, [u8]>> {
        self.files
            .get(path)
            .map(|d| Cow::Borrowed(d.as_slice()))
            .ok_or_else(|| VfsError::NotFound(path.to_owned()))
    }

    fn files(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(self.files.keys().map(String::as_str))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vfs;

    #[test]
    fn memory_files_mount_like_any_layer() {
        let mut mem = MemoryFs::new();
        mem.insert("Models/Video.json", b"{}".to_vec());
        let mut vfs = Vfs::new();
        vfs.mount("memory", mem);
        assert_eq!(vfs.read("models/video.json").unwrap().as_ref(), b"{}");
        assert!(!vfs.contains("models/other.json"));
    }
}
