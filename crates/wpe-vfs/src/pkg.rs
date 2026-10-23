//! Wallpaper Engine `.pkg` archives.
//!
//! Layout (little-endian, strings are `u32` length + bytes, no NUL):
//!
//! ```text
//! string  version            e.g. "PKGV0022"
//! u32     entry_count
//! entry_count × { string path, u32 offset, u32 length }
//! data blob                  offsets are relative to the end of the table
//! ```

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Cursor, Seek};
use std::path::Path;

use binrw::{BinRead, binread};
use memmap2::Mmap;

use crate::{FileSystem, Result, VfsError, normalize};

const MAX_PATH_LEN: u32 = 4096;
const MAX_ENTRIES: u32 = 1 << 20;

#[binread]
#[br(little)]
struct SizedString {
    #[br(temp, assert(len <= MAX_PATH_LEN, "string length {} too large", len))]
    len: u32,
    #[br(count = len, map = |b: Vec<u8>| String::from_utf8_lossy(&b).into_owned())]
    value: String,
}

#[binread]
#[br(little)]
struct RawEntry {
    path: SizedString,
    offset: u32,
    length: u32,
}

#[binread]
#[br(little)]
struct Header {
    version: SizedString,
    #[br(temp, assert(entry_count <= MAX_ENTRIES, "entry count {} too large", entry_count))]
    entry_count: u32,
    #[br(count = entry_count)]
    entries: Vec<RawEntry>,
}

/// One file inside a pkg archive.
#[derive(Debug, Clone)]
pub struct PkgEntry {
    /// Path as stored in the archive (original case).
    pub path: String,
    /// Absolute byte offset into the archive.
    pub offset: usize,
    pub length: usize,
}

/// A memory-mapped `.pkg` archive.
pub struct PkgFs {
    version: String,
    map: Mmap,
    entries: Vec<PkgEntry>,
    index: HashMap<String, usize>,
}

impl PkgFs {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let io_err = |source| VfsError::Io {
            path: path.display().to_string(),
            source,
        };
        let file = File::open(path).map_err(io_err)?;
        // SAFETY: the archive is opened read-only; Steam replacing it while
        // mapped is the only way it changes underneath us.
        let map = unsafe { Mmap::map(&file) }.map_err(io_err)?;
        Self::from_mmap(map)
    }

    fn from_mmap(map: Mmap) -> Result<Self> {
        let mut cursor = Cursor::new(&map[..]);
        let header =
            Header::read(&mut cursor).map_err(|e| VfsError::MalformedPkg(e.to_string()))?;
        let data_start = cursor.stream_position().expect("cursor position") as usize;

        let mut entries = Vec::with_capacity(header.entries.len());
        let mut index = HashMap::with_capacity(header.entries.len());
        for raw in header.entries {
            let offset = data_start + raw.offset as usize;
            let length = raw.length as usize;
            if offset.checked_add(length).is_none_or(|end| end > map.len()) {
                return Err(VfsError::MalformedPkg(format!(
                    "entry {:?} ({offset}+{length}) exceeds archive size {}",
                    raw.path.value,
                    map.len()
                )));
            }
            index.insert(normalize(&raw.path.value), entries.len());
            entries.push(PkgEntry {
                path: raw.path.value,
                offset,
                length,
            });
        }

        Ok(Self {
            version: header.version.value,
            map,
            entries,
            index,
        })
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// Entries in archive order.
    pub fn entries(&self) -> &[PkgEntry] {
        &self.entries
    }

    pub fn entry_bytes(&self, entry: &PkgEntry) -> &[u8] {
        &self.map[entry.offset..entry.offset + entry.length]
    }
}

impl FileSystem for PkgFs {
    fn contains(&self, path: &str) -> bool {
        self.index.contains_key(path)
    }

    fn read(&self, path: &str) -> Result<Cow<'_, [u8]>> {
        let &i = self
            .index
            .get(path)
            .ok_or_else(|| VfsError::NotFound(path.to_owned()))?;
        Ok(Cow::Borrowed(self.entry_bytes(&self.entries[i])))
    }

    fn files(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(self.index.keys().map(String::as_str))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn put_str(buf: &mut Vec<u8>, s: &str) {
        buf.extend((s.len() as u32).to_le_bytes());
        buf.extend(s.as_bytes());
    }

    fn build_pkg(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        put_str(&mut buf, "PKGV0001");
        buf.extend((files.len() as u32).to_le_bytes());
        let mut offset = 0u32;
        for (name, data) in files {
            put_str(&mut buf, name);
            buf.extend(offset.to_le_bytes());
            buf.extend((data.len() as u32).to_le_bytes());
            offset += data.len() as u32;
        }
        for (_, data) in files {
            buf.extend(*data);
        }
        buf
    }

    fn open_bytes(bytes: &[u8]) -> Result<PkgFs> {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        PkgFs::open(f.path())
    }

    #[test]
    fn reads_entries() {
        let pkg = open_bytes(&build_pkg(&[
            ("scene.json", b"{}"),
            ("Materials/Foo.json", b"hello"),
        ]))
        .unwrap();
        assert_eq!(pkg.version(), "PKGV0001");
        assert_eq!(pkg.entries().len(), 2);
        assert_eq!(&*pkg.read("materials/foo.json").unwrap(), b"hello");
        assert_eq!(&*pkg.read("scene.json").unwrap(), b"{}");
        assert!(!pkg.contains("nope"));
    }

    #[test]
    fn rejects_out_of_bounds_entry() {
        let mut bytes = build_pkg(&[("a", b"xyz")]);
        bytes.truncate(bytes.len() - 1);
        assert!(matches!(open_bytes(&bytes), Err(VfsError::MalformedPkg(_))));
    }

    #[test]
    fn rejects_hostile_lengths() {
        let mut bytes = Vec::new();
        bytes.extend(u32::MAX.to_le_bytes());
        assert!(matches!(open_bytes(&bytes), Err(VfsError::MalformedPkg(_))));
    }
}
