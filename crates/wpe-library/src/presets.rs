//! Saved property values for a wallpaper:
//! `$XDG_CONFIG_HOME/penguin-engine/presets/<item id>/<name>.json`.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Named property values for one wallpaper.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    /// The [`crate::Item::id`] it is for.
    pub wallpaper: String,
    /// Property values by name.
    #[serde(default)]
    pub values: Map<String, Value>,
}

/// `$XDG_CONFIG_HOME/penguin-engine/presets`.
pub fn default_dir() -> Option<PathBuf> {
    crate::config_dir().map(|d| d.join("presets"))
}

/// `s` made safe as one path component across Linux and Windows/NTFS filesystems.
fn file_name(s: &str) -> String {
    let mut name: String = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | '\0' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    if name.is_empty() || name.starts_with('.') {
        name.insert(0, '_');
    }
    while name.len() > 240 {
        name.pop();
    }
    name
}

/// Where the preset `name` of `wallpaper` is stored under `dir`.
pub fn path(dir: &Path, wallpaper: &str, name: &str) -> PathBuf {
    dir.join(file_name(wallpaper))
        .join(format!("{}.json", file_name(name)))
}

/// The presets saved for `wallpaper`, by name.
pub fn list(dir: &Path, wallpaper: &str) -> Vec<Preset> {
    let Ok(entries) = std::fs::read_dir(dir.join(file_name(wallpaper))) else {
        return Vec::new();
    };
    let mut presets: Vec<Preset> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| {
            read(&p)
                .map_err(|e| tracing::debug!("{}: {e}", p.display()))
                .ok()
        })
        .map(|mut p| {
            p.wallpaper = wallpaper.to_owned();
            p
        })
        .collect();
    presets.sort_by_cached_key(|p| p.name.to_lowercase());
    presets
}

/// Save `preset` (replacing one of the same name); returns its file.
pub fn save(dir: &Path, preset: &Preset) -> io::Result<PathBuf> {
    let path = path(dir, &preset.wallpaper, &preset.name);
    let json = serde_json::to_vec_pretty(preset).map_err(io::Error::other)?;
    crate::write_atomic(&path, &json)?;
    Ok(path)
}

/// Delete the preset `name` of `wallpaper`; not an error if it is gone.
pub fn delete(dir: &Path, wallpaper: &str, name: &str) -> io::Result<()> {
    match std::fs::remove_file(path(dir, wallpaper, name)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Read a preset file.
pub fn read(file: &Path) -> io::Result<Preset> {
    let text = std::fs::read_to_string(file)?;
    serde_json::from_str(&text).map_err(io::Error::other)
}

/// Write `preset` to `file`, to share it.
pub fn export(preset: &Preset, file: &Path) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(preset).map_err(io::Error::other)?;
    crate::write_atomic(file, &json)
}

/// Read a shared preset from `file` and save it under `dir`. With
/// `wallpaper`, it is saved for that wallpaper whatever the file says.
pub fn import(dir: &Path, file: &Path, wallpaper: Option<&str>) -> io::Result<Preset> {
    let mut preset = read(file)?;
    if let Some(wallpaper) = wallpaper {
        preset.wallpaper = wallpaper.to_owned();
    }
    if preset.wallpaper.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the preset does not say which wallpaper it is for",
        ));
    }
    if preset.name.is_empty() {
        preset.name = file.file_stem().map_or_else(
            || "Imported".to_owned(),
            |s| s.to_string_lossy().into_owned(),
        );
    }
    save(dir, &preset)?;
    Ok(preset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn preset(name: &str) -> Preset {
        let Value::Object(values) = json!({"speed": 2.5, "schemecolor": "1 0 0", "on": true})
        else {
            unreachable!()
        };
        Preset {
            name: name.into(),
            wallpaper: "1234".into(),
            values,
        }
    }

    #[test]
    fn save_list_delete() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        assert!(list(dir, "1234").is_empty());
        save(dir, &preset("Night")).unwrap();
        let file = save(dir, &preset("a/b ..")).unwrap();
        assert!(file.starts_with(dir.join("1234")));
        assert_eq!(file.file_name().unwrap(), "a_b ...json");
        save(dir, &preset("Day")).unwrap();
        let names: Vec<String> = list(dir, "1234").into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["a/b ..", "Day", "Night"]);
        assert_eq!(list(dir, "1234")[2], preset("Night"));
        assert!(list(dir, "other").is_empty());

        delete(dir, "1234", "Day").unwrap();
        delete(dir, "1234", "Day").unwrap();
        assert_eq!(list(dir, "1234").len(), 2);
        // Unsafe names stay inside the store.
        assert!(path(dir, "../x", "..").starts_with(dir));
    }

    #[test]
    fn export_import() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("presets");
        let shared = dir.path().join("shared.json");
        export(&preset("Mine"), &shared).unwrap();

        let imported = import(&store, &shared, None).unwrap();
        assert_eq!(imported, preset("Mine"));
        assert_eq!(list(&store, "1234"), [preset("Mine")]);

        let other = import(&store, &shared, Some("abcd")).unwrap();
        assert_eq!(other.wallpaper, "abcd");
        assert_eq!(list(&store, "abcd").len(), 1);

        std::fs::write(&shared, r#"{"name": "", "wallpaper": "", "values": {}}"#).unwrap();
        assert!(import(&store, &shared, None).is_err());
        assert_eq!(import(&store, &shared, Some("x")).unwrap().name, "shared");
    }

    #[test]
    fn file_name_sanitizes_ntfs_and_truncates() {
        assert_eq!(file_name("Mood: Chill? *Night* <1>"), "Mood_ Chill_ _Night_ _1_");
        let long_name = "a".repeat(300);
        assert_eq!(file_name(&long_name).len(), 240);
    }
}
