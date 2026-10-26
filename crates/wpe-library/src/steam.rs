//! Finding Wallpaper Engine's files in a Steam install: the stock assets and
//! the workshop wallpapers.

use std::path::{Path, PathBuf};

/// Wallpaper Engine's Steam app id; workshop items live under this.
pub const APP_ID: &str = "431960";

/// Steam library roots of the common Linux installs (native, Debian's
/// symlink, Flatpak), relative to the home directory.
pub const STEAM_ROOTS: [&str; 3] = [
    ".local/share/Steam",
    ".steam/steam",
    ".var/app/com.valvesoftware.Steam/.local/share/Steam",
];

/// A discovered Steam library folder with metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamLibrary {
    pub path: PathBuf,
    pub has_app: bool,
}

/// Expand leading `~` or `~/` to the user's home directory.
pub fn expand_home(path: PathBuf) -> PathBuf {
    if let Ok(stripped) = path.strip_prefix("~")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(stripped);
    }
    path
}

/// Verify and normalize `path` as a Wallpaper Engine stock assets directory,
/// or a Wallpaper Engine root directory containing `assets/`.
/// Expands leading `~` if present.
pub fn resolve_assets_dir(path: PathBuf) -> Option<PathBuf> {
    let path = expand_home(path);
    if path.join("effects").is_dir() {
        Some(path)
    } else if path.join("assets/effects").is_dir() {
        Some(path.join("assets"))
    } else if path.is_dir() {
        Some(path)
    } else {
        None
    }
}

/// Parse Steam library paths and app metadata from `libraryfolders.vdf` text.
/// Supports both modern block format (`"1" { "path" "..." "apps" { ... } }`)
/// and legacy format (`"1" "/..."`).
pub fn parse_library_folders(text: &str) -> Vec<SteamLibrary> {
    let doc = crate::acf::parse(text);
    let Some(root) = doc
        .get("libraryfolders")
        .or_else(|| doc.get("LibraryFolders"))
    else {
        return Vec::new();
    };
    let mut libs = Vec::new();
    for (key, val) in root.entries() {
        match val {
            crate::acf::Vdf::Block(_) => {
                if let Some(path_str) = val.get("path").and_then(|v| v.as_str()) {
                    let has_app = val.get("apps").and_then(|a| a.get(APP_ID)).is_some();
                    libs.push(SteamLibrary {
                        path: PathBuf::from(path_str),
                        has_app,
                    });
                }
            }
            crate::acf::Vdf::Str(path_str) => {
                if !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()) {
                    libs.push(SteamLibrary {
                        path: PathBuf::from(path_str),
                        has_app: false,
                    });
                }
            }
        }
    }
    libs
}

/// All detected Steam library directories on this machine.
///
/// Starts with the default Steam roots under `$HOME` (`.local/share/Steam`,
/// `.steam/steam`, Flatpak), and discovers additional library locations from
/// `steamapps/libraryfolders.vdf`. Libraries where Wallpaper Engine (`431960`)
/// is known to be installed are listed first.
pub fn steam_libraries() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };

    let mut app_libs = Vec::new();
    let mut other_libs = Vec::new();
    let mut seen = Vec::new();

    let mut add_lib = |path: PathBuf, has_app: bool| {
        if path.is_dir() {
            let real = path.canonicalize().unwrap_or_else(|_| path.clone());
            if !seen.contains(&real) {
                seen.push(real);
                if has_app {
                    app_libs.push(path);
                } else {
                    other_libs.push(path);
                }
            }
        }
    };

    for root_rel in STEAM_ROOTS {
        let root = home.join(root_rel);
        if !root.is_dir() {
            continue;
        }

        let vdf_path = root.join("steamapps/libraryfolders.vdf");
        if let Ok(content) = std::fs::read_to_string(&vdf_path) {
            for lib in parse_library_folders(&content) {
                add_lib(lib.path, lib.has_app);
            }
        }

        add_lib(root, false);
    }

    app_libs.extend(other_libs);
    app_libs
}

/// `relative` under each discovered Steam library, where it exists.
fn steam_paths(relative: &str) -> Vec<PathBuf> {
    steam_libraries()
        .into_iter()
        .map(|lib| lib.join(relative))
        .filter(|p| p.exists())
        .collect()
}

fn steam_dirs(relative: &str) -> Vec<PathBuf> {
    let mut dirs = steam_paths(relative);
    dirs.retain(|p| p.is_dir());
    dirs
}

fn configured_settings() -> Option<wpe_ipc::Settings> {
    let config_dir = crate::config_dir()?;
    let path = config_dir.join("config.toml");
    wpe_ipc::Settings::load(&path).ok()
}

/// Wallpaper Engine's install directory (`steamapps/common/wallpaper_engine`).
pub fn we_dir() -> Option<PathBuf> {
    if let Some(assets) = default_assets_dir()
        && let Some(parent) = assets.parent()
        && parent.join("projects").is_dir()
    {
        return Some(parent.to_owned());
    }
    steam_dirs("steamapps/common/wallpaper_engine")
        .into_iter()
        .next()
}

/// Wallpaper Engine's stock assets directory, if Steam installed it in a
/// usual place, or as specified in `WPE_ASSETS` / `config.toml`.
pub fn default_assets_dir() -> Option<PathBuf> {
    if let Some(env_val) = std::env::var_os("WPE_ASSETS").map(PathBuf::from)
        && let Some(dir) = resolve_assets_dir(env_val)
    {
        return Some(dir);
    }

    if let Some(settings) = configured_settings()
        && let Some(assets) = settings.assets
        && let Some(dir) = resolve_assets_dir(assets)
    {
        return Some(dir);
    }

    steam_dirs("steamapps/common/wallpaper_engine/assets")
        .into_iter()
        .next()
}

/// Steam's workshop metadata for Wallpaper Engine
/// (`steamapps/workshop/appworkshop_431960.acf`).
pub fn acf_path() -> Option<PathBuf> {
    if let Some(acf) = steam_paths(&format!("steamapps/workshop/appworkshop_{APP_ID}.acf"))
        .into_iter()
        .find(|p| p.is_file())
    {
        return Some(acf);
    }
    for dir in workshop_dirs() {
        if let Some(acf) = acf_for(&dir) {
            return Some(acf);
        }
    }
    None
}

/// The `.acf` that describes the items of the workshop content directory
/// `content` (`.../workshop/content/431960` → `.../workshop/appworkshop_431960.acf`).
pub fn acf_for(content: &Path) -> Option<PathBuf> {
    let workshop = content.parent()?.parent()?;
    let acf = workshop.join(format!("appworkshop_{APP_ID}.acf"));
    acf.is_file().then_some(acf)
}

/// The workshop content directories holding subscribed wallpapers, one per
/// wallpaper id.
pub fn workshop_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(custom) = std::env::var_os("WPE_WORKSHOP") {
        for part in std::env::split_paths(&custom) {
            let part = expand_home(part);
            if part.is_dir() {
                dirs.push(part);
            }
        }
    }

    if let Some(settings) = configured_settings() {
        for dir in settings.workshop {
            let dir = expand_home(dir);
            if dir.is_dir() {
                dirs.push(dir);
            }
        }
    }

    dirs.extend(steam_dirs(&format!("steamapps/workshop/content/{APP_ID}")));

    let mut seen = Vec::new();
    dirs.retain(|d| {
        let real = d.canonicalize().unwrap_or_else(|_| d.clone());
        let new = !seen.contains(&real);
        seen.push(real);
        new
    });
    dirs
}

/// Whether `s` looks like a workshop id.
pub fn is_workshop_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Resolve what a user named as a wallpaper: an existing path (directory,
/// `.pkg` or scene `.json`), or a workshop id looked up in `workshop`.
pub fn resolve_wallpaper_in(spec: &str, workshop: &[PathBuf]) -> Result<PathBuf, String> {
    let path = Path::new(spec);
    if path.exists() {
        return Ok(path.to_owned());
    }
    if is_workshop_id(spec) {
        return workshop
            .iter()
            .map(|dir| dir.join(spec))
            .find(|p| p.is_dir())
            .ok_or_else(|| format!("workshop wallpaper {spec} is not installed"));
    }
    Err(format!("{spec}: no such file or directory"))
}

/// [`resolve_wallpaper_in`] against [`workshop_dirs`].
pub fn resolve_wallpaper(spec: &str) -> Result<PathBuf, String> {
    resolve_wallpaper_in(spec, &workshop_dirs())
}

/// The workshop id of the wallpaper at `path`: its directory's name when
/// that is a number (workshop items are stored by id).
pub fn workshop_id(path: &Path) -> Option<String> {
    let dir = if path.is_dir() { path } else { path.parent()? };
    let name = dir.file_name()?.to_str()?;
    is_workshop_id(name).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODERN_VDF: &str = r#""libraryfolders"
{
	"0"
	{
		"path"		"/home/user/.local/share/Steam"
		"apps"
		{
			"228980"		"297648344"
		}
	}
	"1"
	{
		"path"		"/mnt/Drive/SteamLibrary"
		"apps"
		{
			"431960"		"826275581"
		}
	}
}
"#;

    const LEGACY_VDF: &str = r#""LibraryFolders"
{
	"TimeNextStatsReport"		"12345678"
	"1"		"/mnt/Legacy1/Steam"
	"2"		"/mnt/Legacy2/Steam"
}
"#;

    #[test]
    fn parses_modern_and_legacy_library_folders() {
        let modern = parse_library_folders(MODERN_VDF);
        assert_eq!(modern.len(), 2);
        assert_eq!(
            modern[0].path,
            PathBuf::from("/home/user/.local/share/Steam")
        );
        assert!(!modern[0].has_app);
        assert_eq!(modern[1].path, PathBuf::from("/mnt/Drive/SteamLibrary"));
        assert!(modern[1].has_app);

        let legacy = parse_library_folders(LEGACY_VDF);
        assert_eq!(legacy.len(), 2);
        assert_eq!(legacy[0].path, PathBuf::from("/mnt/Legacy1/Steam"));
        assert!(!legacy[0].has_app);
        assert_eq!(legacy[1].path, PathBuf::from("/mnt/Legacy2/Steam"));
        assert!(!legacy[1].has_app);
    }

    #[test]
    fn resolves_custom_assets_directories() {
        let temp = tempfile::tempdir().unwrap();
        let we_root = temp.path().join("wallpaper_engine");
        let assets_dir = we_root.join("assets");
        let effects_dir = assets_dir.join("effects");
        std::fs::create_dir_all(&effects_dir).unwrap();

        // Direct assets dir
        assert_eq!(
            resolve_assets_dir(assets_dir.clone()),
            Some(assets_dir.clone())
        );
        // Parent WE root dir containing assets/effects
        assert_eq!(resolve_assets_dir(we_root), Some(assets_dir));
        // Nonexistent dir
        assert_eq!(resolve_assets_dir(temp.path().join("nonexistent")), None);
    }

    #[test]
    fn resolves_paths_and_ids() {
        let root = std::env::temp_dir().join(format!("wpe-steam-{}", std::process::id()));
        let item = root.join("123456");
        std::fs::create_dir_all(&item).unwrap();
        let workshop = [root.clone()];

        assert_eq!(resolve_wallpaper_in("123456", &workshop).unwrap(), item);
        let path = item.to_str().unwrap();
        assert_eq!(resolve_wallpaper_in(path, &workshop).unwrap(), item);
        assert!(resolve_wallpaper_in("999", &workshop).is_err());
        assert!(resolve_wallpaper_in("/nonexistent/x", &workshop).is_err());
        assert_eq!(workshop_id(&item).as_deref(), Some("123456"));
        assert_eq!(
            workshop_id(&item.join("scene.pkg")).as_deref(),
            Some("123456")
        );
        assert_eq!(workshop_id(&root), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
