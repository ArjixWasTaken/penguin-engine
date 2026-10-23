use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::{FileSystem, Result, VfsError, normalize};

/// A directory on disk, indexed up front so lookups are case-insensitive
/// regardless of the host filesystem.
pub struct PhysicalFs {
    root: PathBuf,
    index: HashMap<String, PathBuf>,
}

impl PhysicalFs {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let mut index = HashMap::new();
        for entry in walkdir::WalkDir::new(&root).follow_links(true) {
            let entry = entry.map_err(|e| VfsError::Io {
                path: root.display().to_string(),
                source: e.into(),
            })?;
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry.path().strip_prefix(&root).unwrap_or(entry.path());
            index.insert(normalize(&rel.to_string_lossy()), entry.into_path());
        }
        Ok(Self { root, index })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl FileSystem for PhysicalFs {
    fn contains(&self, path: &str) -> bool {
        self.index.contains_key(path)
    }

    fn read(&self, path: &str) -> Result<Cow<'_, [u8]>> {
        let real = self
            .index
            .get(path)
            .ok_or_else(|| VfsError::NotFound(path.to_owned()))?;
        std::fs::read(real).map(Cow::Owned).map_err(|source| VfsError::Io {
            path: real.display().to_string(),
            source,
        })
    }

    fn files(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(self.index.keys().map(String::as_str))
    }
}
