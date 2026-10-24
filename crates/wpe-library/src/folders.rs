//! The user's folders of wallpapers (`folders.json` in the config
//! directory), and importing the ones made in Wallpaper Engine.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A named folder of wallpapers, possibly with subfolders.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub name: String,
    /// [`crate::Item::id`]s.
    #[serde(default)]
    pub items: Vec<String>,
    #[serde(default)]
    pub folders: Vec<Folder>,
}

/// `$XDG_CONFIG_HOME/penguin-engine/folders.json`.
pub fn default_path() -> Option<PathBuf> {
    crate::config_dir().map(|d| d.join("folders.json"))
}

/// The folders saved at `path`; none if the file doesn't exist.
pub fn load(path: &Path) -> io::Result<Vec<Folder>> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Save `folders` to `path` (atomically), creating its directory.
pub fn save(path: &Path, folders: &[Folder]) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(folders).map_err(io::Error::other)?;
    crate::write_atomic(path, &json)
}

/// The folders in Wallpaper Engine's `config.json`
/// (`steamuser.general.browser.folders`). Items are kept as Wallpaper Engine
/// names them: workshop ids for workshop items.
pub fn import_we_folders(config_json: &Path) -> io::Result<Vec<Folder>> {
    let text = std::fs::read_to_string(config_json)?;
    let config: Value = wpe_assets::parse_json(&text).map_err(io::Error::other)?;
    let Some(folders) = config.pointer("/steamuser/general/browser/folders") else {
        return Ok(Vec::new());
    };
    Ok(we_folders(folders))
}

fn we_folders(list: &Value) -> Vec<Folder> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter(|f| {
            f.get("type")
                .and_then(Value::as_str)
                .is_none_or(|t| t == "folder")
        })
        .map(|f| Folder {
            name: f
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            items: match f.get("items") {
                Some(Value::Object(items)) => items.keys().cloned().collect(),
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(|i| match i {
                        Value::String(s) => Some(s.clone()),
                        Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            },
            folders: f.get("subfolders").map(we_folders).unwrap_or_default(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/folders.json");
        assert!(load(&path).unwrap().is_empty());
        let folders = vec![Folder {
            name: "Space".into(),
            items: vec!["1".into(), "abc".into()],
            folders: vec![Folder {
                name: "Nested".into(),
                ..Default::default()
            }],
        }];
        save(&path, &folders).unwrap();
        assert_eq!(load(&path).unwrap(), folders);
    }

    #[test]
    fn imports_wallpaper_engine_folders() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"?installdirectory": "S:/x", "steamuser": {"general": {"browser": {"folders": [
                {"items": {"111": 1, "222": 1}, "title": "Calm", "type": "folder",
                 "subfolders": [{"items": {"333": 1}, "title": "Inner", "type": "folder"}]},
                {"items": {}, "title": "Empty", "type": "folder"},
                {"title": "Not a folder", "type": "other"}
            ]}}}}"#,
        )
        .unwrap();
        let folders = import_we_folders(&path).unwrap();
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[0].name, "Calm");
        assert_eq!(folders[0].items, ["111", "222"]);
        assert_eq!(folders[0].folders[0].items, ["333"]);
        assert!(folders[1].items.is_empty());

        std::fs::write(&path, "{}").unwrap();
        assert!(import_we_folders(&path).unwrap().is_empty());
    }
}
