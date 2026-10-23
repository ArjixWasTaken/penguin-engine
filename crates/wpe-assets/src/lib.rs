//! Wallpaper Engine asset formats: TEXB textures and scene/material JSON models.

pub mod effect;
pub mod material;
pub mod model;
pub mod project;
pub mod scene;
pub mod tex;
pub mod value;

use serde::de::DeserializeOwned;
use wpe_vfs::{Vfs, VfsError};

pub use effect::Effect;
pub use material::Material;
pub use model::ImageModel;
pub use project::{Project, resolve_scene_file};
pub use scene::Scene;

/// A JSON document failed to deserialize; the message names the offending field.
pub type JsonError = serde_path_to_error::Error<serde_json::Error>;

#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error(transparent)]
    Vfs(#[from] VfsError),
    #[error("{path}: {source}")]
    Json {
        path: String,
        #[source]
        source: JsonError,
    },
}

/// Parse WE-flavoured JSON: a leading BOM, `//` and `/* */` comments and
/// trailing commas are tolerated, as Wallpaper Engine itself does.
pub fn parse_json<T: DeserializeOwned>(text: &str) -> Result<T, JsonError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut owned = text.to_owned();
    // Only fails on unterminated comments; serde_json then reports the real position.
    let _ = json_strip_comments::strip(&mut owned);
    let mut de = serde_json::Deserializer::from_str(&owned);
    let value = serde_path_to_error::deserialize(&mut de)?;
    de.end().map_err(|e| {
        serde_path_to_error::Error::new(serde_path_to_error::Track::new().path(), e)
    })?;
    Ok(value)
}

/// Read `path` from `vfs` and parse it with [`parse_json`].
pub fn load_json<T: DeserializeOwned>(vfs: &Vfs, path: &str) -> Result<T, AssetError> {
    let text = vfs.read_to_string(path)?;
    parse_json(&text).map_err(|source| AssetError::Json {
        path: path.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_is_lenient_about_syntax() {
        let v: serde_json::Value = parse_json(
            "\u{feff}{\n// comment\n\"a\": [1, 2,],\n/* block */ \"b\": {\"c\": 1,},\n}",
        )
        .unwrap();
        assert_eq!(v["a"][1], 2);
        assert_eq!(v["b"]["c"], 1);
    }

    #[test]
    fn parse_json_reports_field_path() {
        #[derive(Debug, serde::Deserialize)]
        #[allow(dead_code)]
        struct S {
            inner: Vec<Inner>,
        }
        #[derive(Debug, serde::Deserialize)]
        #[allow(dead_code)]
        struct Inner {
            n: u32,
        }
        let err = parse_json::<S>(r#"{"inner": [{"n": 1}, {"n": "x"}]}"#).unwrap_err();
        assert_eq!(err.path().to_string(), "inner[1].n");
    }

    #[test]
    fn load_json_names_missing_file() {
        let err = load_json::<serde_json::Value>(&Vfs::new(), "materials/x.json").unwrap_err();
        assert!(matches!(err, AssetError::Vfs(VfsError::NotFound(p)) if p == "materials/x.json"));
    }
}
