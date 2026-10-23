//! Image model JSON: the file an image object's `image` key points at
//! (e.g. `models/layer.json`), naming its material and quad size.

use serde::Deserialize;

use crate::value::{Vec2, lenient_bool, lenient_opt_string};

/// Image model (`models/*.json`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ImageModel {
    /// Material JSON path, e.g. `materials/layer.json`.
    #[serde(deserialize_with = "lenient_opt_string")]
    pub material: Option<String>,
    /// Quad size in pixels; when absent the object's `size` (or `autosize`) applies.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// A signed unit quad rendered without model/view transforms.
    #[serde(deserialize_with = "lenient_bool")]
    pub fullscreen: bool,
    /// Size the quad from its first texture's dimensions.
    #[serde(deserialize_with = "lenient_bool")]
    pub autosize: bool,
    /// A flat-coloured quad with no texture.
    #[serde(deserialize_with = "lenient_bool")]
    pub solidlayer: bool,
    /// A compose layer that samples what is already drawn beneath it.
    #[serde(deserialize_with = "lenient_bool")]
    pub passthrough: bool,
    /// The whole rendered project as a layer (`models/util/projectlayer.json`).
    #[serde(deserialize_with = "lenient_bool")]
    pub projectlayer: bool,
    /// Do not pad the texture to a power of two.
    #[serde(deserialize_with = "lenient_bool")]
    pub nopadding: bool,
    #[serde(deserialize_with = "lenient_bool")]
    pub instanced: bool,
    #[serde(deserialize_with = "lenient_bool")]
    pub is_skybox: bool,
    /// Skeletal puppet mesh (`.mdl`) that replaces the quad.
    #[serde(deserialize_with = "lenient_opt_string")]
    pub puppet: Option<String>,
    pub cropoffset: Option<Vec2>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_json;

    #[test]
    fn parses_model() {
        let m: ImageModel = parse_json(
            r#"{"autosize": true, "material": "materials/a.json", "width": 1920,
                "height": 1080, "cropoffset": "0 4", "puppet": "models/a_puppet.mdl"}"#,
        )
        .unwrap();
        assert_eq!(m.material.as_deref(), Some("materials/a.json"));
        assert_eq!((m.width, m.height), (Some(1920), Some(1080)));
        assert!(m.autosize && !m.fullscreen);
        assert_eq!(m.cropoffset.unwrap().0, [0.0, 4.0]);
        assert!(m.puppet.is_some());
    }

    #[test]
    fn missing_fields_default() {
        let m: ImageModel = parse_json(r#"{"passthrough": 1}"#).unwrap();
        assert!(m.passthrough && m.material.is_none() && m.width.is_none());
    }
}
