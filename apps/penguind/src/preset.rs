//! Preset items: workshop items whose `project.json` has no wallpaper of
//! its own but names another one (`dependency`, a workshop id) and the
//! property values to show it with (`preset`). Setting one plays the
//! dependency with those values under the user's own overrides.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wpe_runtime::Project;

/// A preset item, resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    /// The preset item's own title and workshop id.
    pub title: Option<String>,
    pub workshop_id: Option<String>,
    /// The wallpaper it shows.
    pub dependency: PathBuf,
    /// Its property values (file paths made absolute).
    pub values: BTreeMap<String, Value>,
}

/// What loading the item at `path` means.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// The wallpaper to load: `path`, or a preset's dependency.
    pub load: PathBuf,
    pub preset: Option<Preset>,
    /// The values of the wallpaper's properties before any override: its
    /// `project.json`'s, under a preset's.
    pub defaults: BTreeMap<String, Value>,
}

impl Item {
    /// The properties to load with: the defaults, then `overrides`.
    pub fn properties(&self, overrides: &BTreeMap<String, Value>) -> Vec<(String, Value)> {
        let mut values = self
            .preset
            .as_ref()
            .map(|p| p.values.clone())
            .unwrap_or_default();
        values.extend(overrides.iter().map(|(k, v)| (k.clone(), v.clone())));
        values.into_iter().collect()
    }
}

/// `project.json` next to (or in) the wallpaper at `path`, as JSON.
fn project_json(path: &Path) -> Option<Value> {
    let dir = if path.is_dir() { path } else { path.parent()? };
    let text = std::fs::read_to_string(dir.join("project.json")).ok()?;
    serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(&text)).ok()
}

fn non_empty(v: &Value) -> Option<String> {
    let s = match v {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!s.is_empty()).then_some(s)
}

/// Find workshop item `id`: next to the preset item, else where workshop
/// items are installed.
fn find_dependency(item: &Path, id: &str) -> Result<PathBuf, String> {
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return Err(format!("invalid dependency id: {id:?}"));
    }
    if let Some(sibling) = item.parent().map(|dir| dir.join(id))
        && sibling.join("project.json").is_file()
    {
        return Ok(sibling);
    }
    if !wpe_runtime::steam::is_workshop_id(id) {
        return Err(format!("preset item depends on {id:?}, not a workshop id"));
    }
    wpe_runtime::steam::resolve_wallpaper(id)
        .map_err(|e| format!("the wallpaper this preset is for: {e}"))
}

/// What loading `path` means: itself, or the wallpaper a preset item is for.
pub fn resolve(path: &Path) -> Result<Item, String> {
    let json = project_json(path);
    let dependency = json.as_ref().and_then(|j| {
        let has_file = j.get("file").and_then(non_empty).is_some();
        (!has_file).then(|| j.get("dependency").and_then(non_empty))?
    });
    let Some(dependency) = dependency else {
        return Ok(Item {
            load: path.to_owned(),
            preset: None,
            defaults: defaults(json.as_ref()),
        });
    };
    let json = json.unwrap_or_default();
    let load = find_dependency(path, &dependency)?;
    let base = project_json(&load);
    let mut defaults = defaults(base.as_ref());
    let kinds: BTreeMap<String, String> = base
        .and_then(|b| serde_json::from_value::<Project>(b).ok())
        .map(|p| {
            p.general
                .properties
                .into_iter()
                .filter_map(|(name, p)| Some((name, p.kind?)))
                .collect()
        })
        .unwrap_or_default();
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let mut values = BTreeMap::new();
    if let Some(preset) = json.get("preset").and_then(Value::as_object) {
        for (name, value) in preset {
            // Group headers and labels have no value.
            if value.is_null() {
                continue;
            }
            let mut value = value.clone();
            // A file shipped with the preset is relative to it.
            if matches!(
                kinds.get(name).map(String::as_str),
                Some("file" | "directory")
            ) && let Some(rel) = value.as_str().filter(|s| !s.is_empty())
                && !Path::new(rel).is_absolute()
            {
                let candidate = dir.join(rel);
                if let (Ok(can_cand), Ok(can_dir)) = (candidate.canonicalize(), dir.canonicalize())
                    && can_cand.starts_with(&can_dir)
                {
                    value = Value::String(can_cand.display().to_string());
                }
            }
            values.insert(name.clone(), value);
        }
    }
    defaults.extend(values.iter().map(|(k, v)| (k.clone(), v.clone())));
    Ok(Item {
        load: load.clone(),
        preset: Some(Preset {
            title: json.get("title").and_then(non_empty),
            workshop_id: json
                .get("workshopid")
                .and_then(non_empty)
                .or_else(|| wpe_runtime::steam::workshop_id(path)),
            dependency: load,
            values,
        }),
        defaults,
    })
}

/// The property values `project.json` declares.
fn defaults(json: Option<&Value>) -> BTreeMap<String, Value> {
    let Some(props) = json
        .and_then(|j| j.get("general"))
        .and_then(|g| g.get("properties"))
        .and_then(Value::as_object)
    else {
        return BTreeMap::new();
    };
    props
        .iter()
        .filter_map(|(name, p)| Some((name.clone(), p.get("value")?.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wpe-preset-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn write(dir: &Path, project: Value) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("project.json"), project.to_string()).unwrap();
    }

    #[test]
    fn presets_resolve_to_their_dependency() {
        let root = temp_dir("resolve");
        let base = root.join("111");
        write(
            &base,
            json!({"title": "Base", "type": "web", "file": "index.html",
                   "general": {"properties": {
                       "speed": {"type": "slider", "value": 1},
                       "bg": {"type": "file", "value": ""},
                       "rain": {"type": "bool", "value": true}}}}),
        );
        let item = root.join("222");
        write(
            &item,
            json!({"title": "My look", "dependency": "111",
                   "preset": {"speed": 3, "bg": "files/a.jpg", "_group": null}}),
        );
        std::fs::create_dir_all(item.join("files")).unwrap();
        std::fs::write(item.join("files/a.jpg"), b"").unwrap();

        let resolved = resolve(&item).unwrap();
        assert_eq!(resolved.load, base);
        let preset = resolved.preset.as_ref().unwrap();
        assert_eq!(preset.title.as_deref(), Some("My look"));
        assert_eq!(preset.workshop_id.as_deref(), Some("222"));
        assert_eq!(preset.dependency, base);
        let bg = item.join("files/a.jpg").display().to_string();
        assert_eq!(preset.values["bg"], json!(bg));
        assert!(!preset.values.contains_key("_group"));
        // Defaults: the base's, under the preset's.
        assert_eq!(resolved.defaults["speed"], json!(3));
        assert_eq!(resolved.defaults["rain"], json!(true));
        // Overrides win over the preset.
        let overrides = BTreeMap::from([("speed".to_owned(), json!(5))]);
        let props: BTreeMap<_, _> = resolved.properties(&overrides).into_iter().collect();
        assert_eq!(props["speed"], json!(5));
        assert_eq!(props["bg"], json!(bg));

        // An ordinary wallpaper is itself.
        let plain = resolve(&base).unwrap();
        assert_eq!(plain.load, base);
        assert!(plain.preset.is_none());
        assert_eq!(plain.defaults["speed"], json!(1));
        assert_eq!(
            plain.properties(&overrides),
            vec![("speed".into(), json!(5))]
        );

        // A dependency that isn't installed.
        let lost = root.join("333");
        write(&lost, json!({"dependency": "999999999999", "preset": {}}));
        assert!(resolve(&lost).is_err());

        // Traversal dependency IDs are rejected.
        assert!(find_dependency(&root.join("111"), "../etc").is_err());
        assert!(find_dependency(&root.join("111"), "sub/dir").is_err());

        std::fs::remove_dir_all(&root).unwrap();
    }
}
