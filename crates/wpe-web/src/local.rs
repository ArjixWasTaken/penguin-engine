//! Files on this machine a page may read through `file:` URLs: its own
//! directory, and the files and directories the user picked for its
//! `file` and `directory` properties. Everything else is refused.
//!
//! Wallpaper Engine hands pages Windows paths (`C:/Users/...`) that they
//! put behind `file:///`; paths here go out the same way, without their
//! leading slash.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct LocalFiles {
    root: Option<PathBuf>,
    /// Picked paths by property name; directories allow what is inside.
    picked: BTreeMap<String, PathBuf>,
}

impl LocalFiles {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.canonicalize().ok(),
            picked: BTreeMap::new(),
        }
    }

    /// Note the file and directory properties in `properties`
    /// (`{name: {type, value}}`, all of them or the changed ones).
    pub fn update(&mut self, properties: &Value) {
        let Some(map) = properties.as_object() else {
            return;
        };
        for (name, entry) in map {
            if !matches!(entry["type"].as_str(), Some("file" | "directory")) {
                continue;
            }
            match entry["value"].as_str().and_then(local_path) {
                Some(path) => self.picked.insert(name.clone(), path),
                None => self.picked.remove(name),
            };
        }
    }

    /// The file `url` names, if the page may read it.
    pub fn allowed(&self, url: &url::Url) -> Option<PathBuf> {
        let path = url.to_file_path().ok()?.canonicalize().ok()?;
        let inside = |dir: &Path| path.starts_with(dir);
        let ok = self.root.as_deref().is_some_and(inside)
            || self
                .picked
                .values()
                .any(|p| *p == path || (p.is_dir() && inside(p)));
        (ok && path.is_file()).then_some(path)
    }
}

/// A property value as a canonical local path: `/home/x`, `home/x` (as
/// pages get it) or `file:///home/x`.
fn local_path(value: &str) -> Option<PathBuf> {
    let value = value.strip_prefix("file://").unwrap_or(value);
    if value.is_empty() {
        return None;
    }
    let path = if value.starts_with('/') {
        PathBuf::from(value)
    } else {
        Path::new("/").join(value)
    };
    path.canonicalize().ok()
}

/// `properties` as the page gets them: file and directory paths without
/// their leading slash, and directories with `files`, what they hold.
pub fn for_page(properties: &Value) -> Value {
    let mut properties = properties.clone();
    let Some(map) = properties.as_object_mut() else {
        return properties;
    };
    for entry in map.values_mut() {
        let kind = entry["type"].as_str().unwrap_or_default().to_owned();
        if !matches!(kind.as_str(), "file" | "directory") {
            continue;
        }
        let path = entry["value"].as_str().and_then(local_path);
        if let Some(path) = &path {
            entry["value"] = page_path(path).into();
        }
        if kind == "directory" && path.is_some() {
            let files: Vec<Value> = path
                .and_then(|dir| std::fs::read_dir(dir).ok())
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .map(|p| page_path(&p).into())
                .collect();
            entry["files"] = files.into();
        }
    }
    properties
}

fn page_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    s.strip_prefix('/').unwrap_or(&s).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wpe-web-local-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("wallpaper")).unwrap();
        std::fs::create_dir_all(dir.join("pictures")).unwrap();
        std::fs::write(dir.join("wallpaper/index.html"), "").unwrap();
        std::fs::write(dir.join("pictures/a.png"), "").unwrap();
        std::fs::write(dir.join("pictures/b.png"), "").unwrap();
        std::fs::write(dir.join("secret.txt"), "").unwrap();
        dir.canonicalize().unwrap()
    }

    fn url(path: &Path) -> url::Url {
        url::Url::from_file_path(path).unwrap()
    }

    #[test]
    fn only_the_wallpaper_and_picked_files() {
        let d = dir();
        let mut files = LocalFiles::new(&d.join("wallpaper"));
        assert!(
            files
                .allowed(&url(&d.join("wallpaper/index.html")))
                .is_some()
        );
        assert!(files.allowed(&url(&d.join("secret.txt"))).is_none());
        assert!(files.allowed(&url(&d.join("pictures/a.png"))).is_none());
        // `..` doesn't get out.
        let sneaky = url::Url::parse(&format!("file://{}/wallpaper/../secret.txt", d.display()));
        assert!(files.allowed(&sneaky.unwrap()).is_none());

        let picked = d.join("pictures/a.png").display().to_string();
        files.update(&json!({"bg": {"type": "file", "value": picked.trim_start_matches('/')}}));
        assert!(files.allowed(&url(&d.join("pictures/a.png"))).is_some());
        assert!(files.allowed(&url(&d.join("pictures/b.png"))).is_none());

        let dir = d.join("pictures").display().to_string();
        files.update(&json!({"bg": {"type": "file", "value": ""}, "pics": {"type": "directory", "value": dir}}));
        assert!(files.allowed(&url(&d.join("pictures/b.png"))).is_some());
        assert!(files.allowed(&url(&d.join("secret.txt"))).is_none());
    }

    #[test]
    fn pages_get_windows_like_paths() {
        let d = dir();
        let dir = d.join("pictures").display().to_string();
        let page = for_page(&json!({
            "pics": {"type": "directory", "value": dir},
            "n": {"type": "slider", "value": 3},
        }));
        assert_eq!(page["pics"]["value"], dir.trim_start_matches('/'));
        let mut files: Vec<_> = page["pics"]["files"].as_array().unwrap().iter().collect();
        files.sort_by_key(|v| v.as_str().unwrap().to_owned());
        assert!(files[0].as_str().unwrap().ends_with("pictures/a.png"));
        assert!(!files[0].as_str().unwrap().starts_with('/'));
        assert_eq!(page["n"]["value"], 3);
    }
}
