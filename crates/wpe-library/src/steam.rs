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

/// `relative` under each Steam root, where it exists.
fn steam_paths(relative: &str) -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    STEAM_ROOTS
        .into_iter()
        .map(|root| home.join(root).join(relative))
        .filter(|p| p.exists())
        .collect()
}

fn steam_dirs(relative: &str) -> Vec<PathBuf> {
    let mut dirs = steam_paths(relative);
    dirs.retain(|p| p.is_dir());
    dirs
}

/// Wallpaper Engine's install directory (`steamapps/common/wallpaper_engine`).
pub fn we_dir() -> Option<PathBuf> {
    steam_dirs("steamapps/common/wallpaper_engine")
        .into_iter()
        .next()
}

/// Wallpaper Engine's stock assets directory, if Steam installed it in a
/// usual place.
pub fn default_assets_dir() -> Option<PathBuf> {
    steam_dirs("steamapps/common/wallpaper_engine/assets")
        .into_iter()
        .next()
}

/// Steam's workshop metadata for Wallpaper Engine
/// (`steamapps/workshop/appworkshop_431960.acf`).
pub fn acf_path() -> Option<PathBuf> {
    steam_paths(&format!("steamapps/workshop/appworkshop_{APP_ID}.acf"))
        .into_iter()
        .find(|p| p.is_file())
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
    let mut dirs = steam_dirs(&format!("steamapps/workshop/content/{APP_ID}"));
    // `.steam/steam` is usually a symlink to `.local/share/Steam`.
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
