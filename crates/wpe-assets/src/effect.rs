//! Effect definitions (`effects/<name>/effect.json`): a chain of material
//! passes and copy/swap commands over named framebuffers.

use serde::Deserialize;
use serde::de::{self, Deserializer};
use serde_json::Value;

use crate::material::PassOverride;
use crate::value::{lenient_bool, lenient_opt_string, lenient_string, null_default};

/// An `effect.json` file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Effect {
    /// Display name (often a localization key such as `ui_editor_effect_blur_title`).
    #[serde(deserialize_with = "lenient_string")]
    pub name: String,
    #[serde(deserialize_with = "lenient_opt_string")]
    pub description: Option<String>,
    #[serde(deserialize_with = "lenient_opt_string")]
    pub group: Option<String>,
    pub version: Option<i64>,
    #[serde(deserialize_with = "null_default")]
    pub fbos: Vec<Fbo>,
    #[serde(deserialize_with = "null_default")]
    pub passes: Vec<EffectPass>,
    /// Files the effect ships with (materials, shaders, textures).
    #[serde(deserialize_with = "null_default")]
    pub dependencies: Vec<String>,
}

/// A framebuffer an effect allocates for its passes.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Fbo {
    pub name: String,
    /// Downscale divisor relative to the layer size (1 = full size).
    pub scale: u32,
    #[serde(deserialize_with = "lenient_opt_string")]
    pub format: Option<String>,
    pub fit: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    #[serde(deserialize_with = "lenient_bool")]
    pub unique: bool,
}

impl Default for Fbo {
    fn default() -> Self {
        Self {
            name: String::new(),
            scale: 1,
            format: None,
            fit: None,
            width: None,
            height: None,
            unique: false,
        }
    }
}

/// One step of an effect: a material pass or a framebuffer command.
#[derive(Debug, Clone)]
pub enum EffectPass {
    Material(EffectMaterialPass),
    Command(EffectCommand),
}

impl<'de> Deserialize<'de> for EffectPass {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        let has = |k: &str| v.get(k).is_some_and(|x| !x.is_null());
        if has("material") {
            #[derive(Deserialize)]
            struct Head {
                material: String,
                #[serde(default, deserialize_with = "lenient_bool")]
                compose: bool,
            }
            let head: Head = serde_path_to_error::deserialize(&v).map_err(de::Error::custom)?;
            let settings = serde_path_to_error::deserialize(v).map_err(de::Error::custom)?;
            Ok(Self::Material(EffectMaterialPass {
                material: head.material,
                settings,
                compose: head.compose,
            }))
        } else if has("command") {
            serde_path_to_error::deserialize(v).map(Self::Command)
        } else {
            return Err(de::Error::custom(
                "effect pass has neither `material` nor `command`",
            ));
        }
        .map_err(de::Error::custom)
    }
}

/// A pass that renders `material` with the effect's overrides.
#[derive(Debug, Clone, Default)]
pub struct EffectMaterialPass {
    pub material: String,
    /// Target, binds, textures, combos and constants for this pass.
    pub settings: PassOverride,
    /// Marks a two-pass compose effect that renders through `_rt_FullCompoBuffer1`.
    pub compose: bool,
}

/// A framebuffer operation between passes.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EffectCommand {
    pub command: CommandKind,
    pub source: String,
    pub target: String,
}

/// What an [`EffectCommand`] does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CommandKind {
    /// Copy `source` into `target`.
    #[default]
    Copy,
    /// Exchange `source` and `target`.
    Swap,
    Unknown(String),
}

impl<'de> Deserialize<'de> for CommandKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = lenient_string(d)?;
        Ok(match s.to_ascii_lowercase().as_str() {
            "copy" => Self::Copy,
            "swap" => Self::Swap,
            _ => Self::Unknown(s),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_json;

    #[test]
    fn parses_effect() {
        let e: Effect = parse_json(
            r#"{
                "name": "Blur",
                "passes": [
                    {"material": "materials/effects/blur_x.json", "target": "_rt_Q1",
                     "bind": [{"name": "previous", "index": 0}],
                     "combos": {"KERNEL": 1}, "constantshadervalues": {"scale": "1 1"}},
                    {"command": "copy", "source": "_rt_Q1", "target": "_rt_Q2"},
                    {"material": "materials/effects/blur_combine.json", "compose": true}
                ],
                "fbos": [{"name": "_rt_Q1", "scale": 4, "format": "rgba_backbuffer"},
                         {"name": "_rt_Q2"}],
                "dependencies": ["shaders/effects/blur.frag"]
            }"#,
        )
        .unwrap();
        assert_eq!(e.fbos[0].scale, 4);
        assert_eq!(e.fbos[1].scale, 1);
        let EffectPass::Material(p) = &e.passes[0] else {
            panic!("expected material pass")
        };
        assert_eq!(p.settings.target.as_deref(), Some("_rt_Q1"));
        assert_eq!(p.settings.bind[0].index, 0);
        assert_eq!(p.settings.combos["KERNEL"].0, 1);
        let EffectPass::Command(c) = &e.passes[1] else {
            panic!("expected command")
        };
        assert_eq!(
            (c.command.clone(), c.source.as_str()),
            (CommandKind::Copy, "_rt_Q1")
        );
        assert!(matches!(&e.passes[2], EffectPass::Material(p) if p.compose));
    }

    #[test]
    fn rejects_empty_pass() {
        assert!(parse_json::<Effect>(r#"{"passes": [{"target": "x"}]}"#).is_err());
    }
}
