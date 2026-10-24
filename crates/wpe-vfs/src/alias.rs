use std::borrow::Cow;
use std::collections::HashMap;

use crate::{FileSystem, Result, VfsError};

/// Exposes files of an inner filesystem under additional names.
pub struct AliasFs<F> {
    inner: F,
    /// Alias path → path in `inner`, both normalized.
    aliases: HashMap<String, String>,
}

impl<F: FileSystem> AliasFs<F> {
    pub fn new(inner: F, aliases: HashMap<String, String>) -> Self {
        Self { inner, aliases }
    }
}

impl<F: FileSystem> FileSystem for AliasFs<F> {
    fn contains(&self, path: &str) -> bool {
        self.aliases.contains_key(path)
    }

    fn read(&self, path: &str) -> Result<Cow<'_, [u8]>> {
        let target = self
            .aliases
            .get(path)
            .ok_or_else(|| VfsError::NotFound(path.to_owned()))?;
        self.inner.read(target)
    }

    fn files(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(self.aliases.keys().map(String::as_str))
    }
}
