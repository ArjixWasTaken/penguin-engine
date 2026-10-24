//! The wallpapers installed on this machine, for the settings app: workshop
//! items, Wallpaper Engine's own projects and folders the user added. No
//! GPU; scanning reads `project.json` files and Steam's metadata.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub mod acf;
pub mod condition;
mod details;
pub mod folders;
mod library;
pub mod locale;
pub mod presets;
pub mod steam;
pub mod thumbs;

pub use details::{localization, properties, property_infos, resolution};
pub use library::{Library, Sources, default_cache_path};
pub use locale::{Locale, plain};

/// What a wallpaper is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Scene,
    Video,
    Web,
    /// Discontinued by Wallpaper Engine; listed but not playable.
    Application,
    /// Property values for another wallpaper ([`Item::dependency`]).
    Preset,
    Unknown,
}

/// `project.json` `contentrating`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rating {
    Everyone,
    Questionable,
    Mature,
    Unknown,
}

/// Where an item was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// A subscribed workshop item.
    Workshop,
    /// Wallpaper Engine's `projects/defaultprojects` and `myprojects`.
    Projects,
    /// A folder the user added.
    User,
}

/// One wallpaper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// The workshop id, or a stable hash of the path for other items.
    pub id: String,
    /// The wallpaper's directory: what the daemon's `set` takes.
    pub path: PathBuf,
    pub title: String,
    pub description: Option<String>,
    pub kind: Kind,
    pub tags: Vec<String>,
    pub rating: Rating,
    /// The preview image (GIF, JPG or PNG), if it exists.
    pub preview: Option<PathBuf>,
    /// Bytes on disk.
    pub size: u64,
    /// Unix seconds: the item's last update (Steam's `timeupdated`, else
    /// the directory's mtime).
    pub updated: Option<u64>,
    /// Unix seconds it was installed or subscribed (Steam's `timetouched`).
    pub installed: Option<u64>,
    pub workshop_id: Option<String>,
    pub source: Source,
    /// `general.supportsaudioprocessing`.
    pub audio_reactive: bool,
    pub has_properties: bool,
    /// For presets: the workshop id of the wallpaper they configure.
    pub dependency: Option<String>,
}

/// `$XDG_CONFIG_HOME/wallpaper-engine-rs`.
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("wallpaper-engine-rs"))
}

/// Write `data` to `path` through a temporary file and a rename, creating
/// the directory.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// 64-bit FNV-1a: a hash that stays the same across runs and builds.
pub(crate) fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}
