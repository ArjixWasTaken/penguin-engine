//! Material JSON (`materials/*.json`) and the pass overrides scenes and effects layer on top.
//!
//! ```json
//! {"passes": [{"shader": "genericimage2", "blending": "translucent",
//!              "textures": ["layer0", null], "combos": {"VERSION": 1},
//!              "constantshadervalues": {"alpha": 1, "color": "1 1 1"}}]}
//! ```

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::value::{Dynamic, FloatVec, LenientI32, lenient_i32, lenient_opt_string, null_default};

/// A material file: a list of render passes. Image materials use only the first.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Material {
    #[serde(deserialize_with = "null_default")]
    pub passes: Vec<MaterialPass>,
}

/// One pass of a material file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MaterialPass {
    /// Shader name relative to `shaders/`, without the `.vert`/`.frag` extension.
    pub shader: String,
    pub blending: Blending,
    #[serde(alias = "culling")]
    pub cullmode: CullMode,
    #[serde(alias = "depthtesting")]
    pub depthtest: Toggle,
    #[serde(alias = "depthwriting")]
    pub depthwrite: Toggle,
    pub alphawriting: Option<Toggle>,
    /// Texture per sampler slot (`g_Texture0`, ...): asset names or `_rt_*` render targets.
    #[serde(deserialize_with = "null_default")]
    pub textures: Vec<Option<String>>,
    /// User-selectable textures, index-aligned with `textures`.
    #[serde(deserialize_with = "null_default")]
    pub usertextures: Vec<Option<UserTexture>>,
    /// Shader preprocessor combos.
    #[serde(deserialize_with = "null_default")]
    pub combos: BTreeMap<String, LenientI32>,
    /// Uniform values keyed by the shader's material name for the constant.
    #[serde(deserialize_with = "null_default")]
    pub constantshadervalues: BTreeMap<String, Dynamic<FloatVec>>,
    /// User property name → shader constant it drives.
    #[serde(deserialize_with = "null_default")]
    pub usershadervalues: BTreeMap<String, String>,
}

/// A per-instance pass override: an effect pass in `scene.json`, an image's
/// `instance` block, or an effect file's material pass settings.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PassOverride {
    pub id: Option<i64>,
    /// Replacement textures; a `None` slot keeps the base material's texture.
    #[serde(deserialize_with = "null_default")]
    pub textures: Vec<Option<String>>,
    /// User-selectable textures, index-aligned with `textures`.
    #[serde(deserialize_with = "null_default")]
    pub usertextures: Vec<Option<UserTexture>>,
    /// Shader preprocessor combos.
    #[serde(deserialize_with = "null_default")]
    pub combos: BTreeMap<String, LenientI32>,
    /// Uniform values keyed by the shader's material name for the constant.
    #[serde(deserialize_with = "null_default")]
    pub constantshadervalues: BTreeMap<String, Dynamic<FloatVec>>,
    /// Render target this pass draws into; `None` means the effect's ping-pong output.
    #[serde(deserialize_with = "lenient_opt_string")]
    pub target: Option<String>,
    #[serde(deserialize_with = "null_default")]
    pub bind: Vec<Bind>,
}

/// Binds a named render target (or `previous`, the prior pass output) to a texture slot.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Bind {
    pub name: String,
    #[serde(deserialize_with = "lenient_i32")]
    pub index: i32,
}

/// A `usertextures` slot: either a bare reference name or `{"name", "type"}`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserTexture {
    pub name: String,
    /// `"system"` for engine-provided textures such as `$mediaThumbnail`.
    pub kind: Option<String>,
}

impl<'de> Deserialize<'de> for UserTexture {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::String(name) => Ok(Self { name, kind: None }),
            Value::Object(mut m) => {
                let mut take = |k: &str| match m.remove(k) {
                    Some(Value::String(s)) => Some(s),
                    _ => None,
                };
                Ok(Self {
                    name: take("name").unwrap_or_default(),
                    kind: take("type"),
                })
            }
            other => Err(de::Error::custom(format!(
                "expected a user texture name or object, got {other}"
            ))),
        }
    }
}

/// Generates a string-backed enum with an `Unknown` fallback.
macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $variant:ident = $($s:literal)|+,)* } default $def:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Default)]
        pub enum $name {
            $($(#[$vm])* $variant,)*
            /// A spelling this crate does not know.
            Unknown(String),
        }

        impl $name {
            /// Parse a spelling case-insensitively.
            pub fn parse(s: &str) -> Self {
                match s.to_ascii_lowercase().as_str() {
                    $($($s)|+ => Self::$variant,)*
                    _ => Self::Unknown(s.to_owned()),
                }
            }

            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => [$($s),+][0],)*
                    Self::Unknown(s) => s,
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                Ok(match Value::deserialize(d)? {
                    Value::Null => Self::$def,
                    Value::String(s) => Self::parse(&s),
                    other => Self::parse(&other.to_string()),
                })
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }
    };
}

string_enum! {
    /// Framebuffer blend mode of a pass.
    Blending {
        /// Straight alpha: `src * a + dst * (1 - a)`.
        #[default]
        Translucent = "translucent",
        Additive = "additive",
        /// Opaque overwrite.
        Normal = "normal",
        /// Premultiplied alpha.
        TranslucentPa = "translucent_pa",
        Disabled = "disabled",
        AlphaToCoverage = "alphatocoverage",
    } default Translucent
}

string_enum! {
    /// Face culling mode.
    CullMode {
        #[default]
        NoCull = "nocull" | "none",
        /// Back-face culling.
        Normal = "normal" | "back",
    } default NoCull
}

string_enum! {
    /// An `"enabled"`/`"disabled"` switch (depth test, depth write, alpha write).
    Toggle {
        Enabled = "enabled" | "true" | "1",
        #[default]
        Disabled = "disabled" | "false" | "0",
    } default Disabled
}

impl Toggle {
    pub fn is_enabled(&self) -> bool {
        *self == Self::Enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_json;
    use crate::value::UserBinding;

    #[test]
    fn parses_material() {
        let m: Material = parse_json(
            r#"{"passes": [{
                "shader": "genericimage2",
                "blending": "Additive",
                "cullmode": "nocull",
                "depthtesting": "enabled",
                "depthwrite": "disabled",
                "textures": ["layer0", null, "_rt_FullFrameBuffer"],
                "usertextures": [null, "usertexture2", {"name": "$mediaThumbnail", "type": "system"}],
                "combos": {"VERSION": 1, "LIGHTING": 1.0},
                "constantshadervalues": {
                    "alpha": 0.5,
                    "color": "1 0 0",
                    "speed": {"user": "speed", "value": 2},
                    "flag": true
                },
                "usershadervalues": {"schemecolor": "color"}
            }]}"#,
        )
        .unwrap();
        let p = &m.passes[0];
        assert_eq!(p.shader, "genericimage2");
        assert_eq!(p.blending, Blending::Additive);
        assert!(p.depthtest.is_enabled() && !p.depthwrite.is_enabled());
        assert_eq!(p.textures[1], None);
        assert_eq!(p.usertextures[1].as_ref().unwrap().name, "usertexture2");
        assert_eq!(
            p.usertextures[2].as_ref().unwrap().kind.as_deref(),
            Some("system")
        );
        assert_eq!(p.combos["LIGHTING"].0, 1);
        let csv = &p.constantshadervalues;
        assert_eq!(csv["alpha"].value.0, [0.5]);
        assert_eq!(csv["color"].value.0, [1.0, 0.0, 0.0]);
        assert_eq!(csv["speed"].user, Some(UserBinding::Name("speed".into())));
        assert_eq!(csv["flag"].value.0, [1.0]);
        assert_eq!(p.usershadervalues["schemecolor"], "color");
    }

    #[test]
    fn defaults_and_unknown_modes() {
        let m: Material =
            parse_json(r#"{"passes": [{"shader": "x", "blending": "weird"}]}"#).unwrap();
        let p = &m.passes[0];
        assert_eq!(p.blending, Blending::Unknown("weird".into()));
        assert_eq!(p.cullmode, CullMode::NoCull);
        assert_eq!(p.depthtest, Toggle::Disabled);
        assert!(p.textures.is_empty());
        assert_eq!(Blending::default(), Blending::Translucent);
    }

    #[test]
    fn parses_override() {
        let o: PassOverride = parse_json(
            r#"{"id": 3, "textures": [null, "masks/m"], "combos": {"BLENDMODE": 2},
                "target": "_rt_A", "bind": [{"name": "previous", "index": 0}]}"#,
        )
        .unwrap();
        assert_eq!(o.id, Some(3));
        assert_eq!(o.textures, [None, Some("masks/m".into())]);
        assert_eq!(o.bind[0].name, "previous");
    }
}
