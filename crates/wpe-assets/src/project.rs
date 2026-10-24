//! `project.json`: wallpaper metadata and user-configurable properties.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use wpe_vfs::Vfs;

use crate::value::{lenient_opt_string, null_default};

/// Top-level `project.json`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Project {
    /// Entry file, e.g. `scene.json` (scenes) or `video.mp4`.
    #[serde(deserialize_with = "lenient_opt_string")]
    pub file: Option<String>,
    /// Wallpaper kind as authored (`"scene"`, `"Video"`, `"web"`, ...); see [`Project::is_scene`].
    #[serde(rename = "type", deserialize_with = "lenient_opt_string")]
    pub kind: Option<String>,
    #[serde(deserialize_with = "lenient_opt_string")]
    pub title: Option<String>,
    #[serde(deserialize_with = "lenient_opt_string")]
    pub description: Option<String>,
    #[serde(deserialize_with = "lenient_opt_string")]
    pub preview: Option<String>,
    #[serde(deserialize_with = "null_default")]
    pub tags: Vec<Value>,
    #[serde(deserialize_with = "lenient_opt_string")]
    pub workshopid: Option<String>,
    #[serde(deserialize_with = "lenient_opt_string")]
    pub contentrating: Option<String>,
    pub version: Option<Value>,
    #[serde(deserialize_with = "null_default")]
    pub general: ProjectGeneral,
}

impl Project {
    /// Whether this is a scene wallpaper (as opposed to video, web or application).
    pub fn is_scene(&self) -> bool {
        self.kind
            .as_deref()
            .is_some_and(|k| k.eq_ignore_ascii_case("scene"))
    }

    /// Whether this is a video wallpaper (its `file` is the video).
    pub fn is_video(&self) -> bool {
        self.kind
            .as_deref()
            .is_some_and(|k| k.eq_ignore_ascii_case("video"))
    }
}

/// The `general` block of `project.json`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProjectGeneral {
    /// User properties by name; scene values reference them via `"user": name`.
    #[serde(deserialize_with = "null_default")]
    pub properties: BTreeMap<String, UserProperty>,
    pub supportsaudioprocessing: Option<bool>,
    /// Translations of property labels, keyed by locale (kept raw).
    pub localization: Option<Value>,
}

/// One user-configurable property shown in Wallpaper Engine's settings panel.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UserProperty {
    /// Widget kind: `bool`, `slider`, `combo`, `color`, `textinput`, `file`, `text`, ...
    #[serde(rename = "type", deserialize_with = "lenient_opt_string")]
    pub kind: Option<String>,
    /// Current value: a bool, number, or string (colours are vector strings).
    pub value: Value,
    /// Display label (often a localization key or HTML).
    #[serde(deserialize_with = "lenient_opt_string")]
    pub text: Option<String>,
    pub order: Option<f64>,
    pub index: Option<f64>,
    #[serde(deserialize_with = "null_default")]
    pub options: Vec<PropertyOption>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub precision: Option<f64>,
    pub fraction: Option<bool>,
    /// Visibility condition in the settings panel (JavaScript-like expression).
    #[serde(deserialize_with = "lenient_opt_string")]
    pub condition: Option<String>,
}

/// One entry of a `combo` property's option list.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PropertyOption {
    #[serde(deserialize_with = "lenient_opt_string")]
    pub label: Option<String>,
    pub value: Value,
}

/// Name of the scene JSON inside the wallpaper's package.
///
/// Mirrors the reference loader: the project's `file` (file name only, with
/// the extension forced to `.json`) if it exists in `vfs`, then `scene.json`.
/// Falls back to `scene.json` when neither exists.
pub fn resolve_scene_file(project: &Project, vfs: &Vfs) -> String {
    let from_project = project.file.as_deref().and_then(|f| {
        let name = f.rsplit(['/', '\\']).next().unwrap_or(f);
        let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
        (!stem.is_empty()).then(|| format!("{stem}.json"))
    });
    from_project
        .into_iter()
        .chain(std::iter::once("scene.json".to_owned()))
        .find(|c| vfs.contains(c))
        .unwrap_or_else(|| "scene.json".to_owned())
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;
    use crate::parse_json;

    struct MemFs(Vec<&'static str>);

    impl wpe_vfs::FileSystem for MemFs {
        fn contains(&self, path: &str) -> bool {
            self.0.contains(&path)
        }
        fn read(&self, path: &str) -> wpe_vfs::Result<Cow<'_, [u8]>> {
            Err(wpe_vfs::VfsError::NotFound(path.to_owned()))
        }
        fn files(&self) -> Box<dyn Iterator<Item = &str> + '_> {
            Box::new(self.0.iter().copied())
        }
    }

    const PROJECT: &str = r#"{
        "file": "scene.json",
        "type": "scene",
        "title": "Test",
        "preview": "preview.jpg",
        "workshopid": 12345,
        "general": {
            "properties": {
                "schemecolor": {"order": 0, "text": "ui_browse_properties_scheme_color",
                                "type": "color", "value": "0.7 0.1 0.2"},
                "speed": {"type": "slider", "value": 2, "min": 0, "max": 10, "order": 101},
                "mode": {"type": "combo", "value": "1", "condition": "speed.value > 1",
                         "options": [{"label": "A", "value": "1"}, {"label": "B", "value": 2}]}
            }
        }
    }"#;

    #[test]
    fn parses_project() {
        let p: Project = parse_json(PROJECT).unwrap();
        assert!(p.is_scene());
        assert_eq!(p.workshopid.as_deref(), Some("12345"));
        let props = &p.general.properties;
        assert_eq!(props["schemecolor"].kind.as_deref(), Some("color"));
        assert_eq!(props["speed"].value, 2);
        assert_eq!(props["speed"].max, Some(10.0));
        assert_eq!(props["mode"].options.len(), 2);
        assert_eq!(props["mode"].options[1].value, 2);
    }

    #[test]
    fn missing_fields_default() {
        let p: Project = parse_json(r#"{"general": null}"#).unwrap();
        assert!(p.file.is_none() && !p.is_scene() && p.general.properties.is_empty());
    }

    #[test]
    fn scene_file_resolution() {
        let mut vfs = Vfs::new();
        vfs.mount("pkg", MemFs(vec!["scene.json", "other.json"]));
        let mut p = Project {
            file: Some("Other.pkg".into()),
            ..Default::default()
        };
        assert_eq!(resolve_scene_file(&p, &vfs), "Other.json");
        p.file = Some("missing.json".into());
        assert_eq!(resolve_scene_file(&p, &vfs), "scene.json");
        p.file = None;
        assert_eq!(resolve_scene_file(&p, &Vfs::new()), "scene.json");
    }
}
