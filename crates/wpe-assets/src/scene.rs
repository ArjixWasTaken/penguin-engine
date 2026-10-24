//! Scene JSON (usually `scene.json` inside `scene.pkg`): camera, global
//! settings and the object list.

use serde::Deserialize;
use serde::de::{self, Deserializer};
use serde_json::{Map, Value};

use crate::material::PassOverride;
use crate::value::{
    Dynamic, Vec2, Vec3, VecN, lenient_bool, lenient_i32, lenient_opt_string, lenient_string,
    null_default,
};

fn d<T>(v: T) -> Dynamic<T> {
    Dynamic::new(v)
}

/// A parsed scene file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Scene {
    #[serde(deserialize_with = "null_default")]
    pub camera: Camera,
    #[serde(deserialize_with = "null_default")]
    pub general: SceneGeneral,
    #[serde(deserialize_with = "null_default")]
    pub objects: Vec<SceneObject>,
    pub version: Option<i64>,
}

/// Scene camera placement.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Camera {
    pub center: Vec3,
    pub eye: Vec3,
    pub up: Vec3,
    /// Camera path definitions (kept raw).
    #[serde(deserialize_with = "null_default")]
    pub paths: Vec<Value>,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            center: VecN([0.0, 0.0, 0.0]),
            eye: VecN([0.0, 0.0, 1.0]),
            up: VecN([0.0, 1.0, 0.0]),
            paths: Vec::new(),
        }
    }
}

/// Orthographic projection size; `auto` sizes it from the scene's image layers.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OrthogonalProjection {
    #[serde(deserialize_with = "lenient_i32")]
    pub width: i32,
    #[serde(deserialize_with = "lenient_i32")]
    pub height: i32,
    #[serde(deserialize_with = "lenient_bool")]
    pub auto: bool,
    /// Editor bloom/compose preset: `low`, `medium`, `ultra` or `displayhdr`.
    #[serde(deserialize_with = "lenient_opt_string")]
    pub postprocessing: Option<String>,
}

/// The scene's `general` block. Defaults follow Wallpaper Engine's.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SceneGeneral {
    /// `None` (absent or `null`) means a perspective camera.
    pub orthogonalprojection: Option<OrthogonalProjection>,
    pub clearcolor: Dynamic<Vec3>,
    pub clearenabled: Dynamic<bool>,
    pub ambientcolor: Dynamic<Vec3>,
    pub skylightcolor: Dynamic<Vec3>,
    pub zoom: Dynamic<f32>,
    pub fov: Dynamic<f32>,
    pub perspectiveoverridefov: Option<Dynamic<f32>>,
    pub nearz: Dynamic<f32>,
    pub farz: Dynamic<f32>,
    pub cameraparallax: Dynamic<bool>,
    pub cameraparallaxamount: Dynamic<f32>,
    pub cameraparallaxdelay: Dynamic<f32>,
    pub cameraparallaxmouseinfluence: Dynamic<f32>,
    pub camerashake: Dynamic<bool>,
    pub camerashakeamplitude: Dynamic<f32>,
    pub camerashakespeed: Dynamic<f32>,
    pub camerashakeroughness: Dynamic<f32>,
    pub camerafade: Dynamic<bool>,
    pub camerapreview: Dynamic<bool>,
    pub hdr: Dynamic<bool>,
    pub bloom: Dynamic<bool>,
    pub bloomstrength: Dynamic<f32>,
    pub bloomthreshold: Dynamic<f32>,
    pub bloomtint: Dynamic<Vec3>,
    pub bloomhdrstrength: Dynamic<f32>,
    pub bloomhdrthreshold: Dynamic<f32>,
    pub bloomhdriterations: Dynamic<f32>,
    pub bloomhdrscatter: Dynamic<f32>,
    pub bloomhdrfeather: Dynamic<f32>,
    pub gravitydirection: Dynamic<Vec3>,
    pub gravitystrength: Dynamic<f32>,
    pub winddirection: Dynamic<Vec3>,
    pub windenabled: Dynamic<bool>,
    pub windstrength: Dynamic<f32>,
    /// Light quality settings (kept raw).
    pub lightconfig: Option<Value>,
    /// Distance fog (the editor's scene options "Distance Fog": enabled,
    /// start, end, color, start density, end density).
    pub fogdistance: Dynamic<bool>,
    pub fogdistancestart: Dynamic<f32>,
    pub fogdistanceend: Dynamic<f32>,
    pub fogdistancecolor: Dynamic<Vec3>,
    pub fogdistancestartdensity: Dynamic<f32>,
    pub fogdistanceenddensity: Dynamic<f32>,
    /// Height fog, the same settings along the world Y axis.
    pub fogheight: Dynamic<bool>,
    pub fogheightstart: Dynamic<f32>,
    pub fogheightend: Dynamic<f32>,
    pub fogheightcolor: Dynamic<Vec3>,
    pub fogheightstartdensity: Dynamic<f32>,
    pub fogheightenddensity: Dynamic<f32>,
}

impl Default for SceneGeneral {
    fn default() -> Self {
        Self {
            orthogonalprojection: None,
            clearcolor: d(VecN::splat(0.0)),
            clearenabled: d(true),
            ambientcolor: d(VecN::splat(0.2)),
            skylightcolor: d(VecN::splat(0.3)),
            zoom: d(1.0),
            fov: d(50.0),
            perspectiveoverridefov: None,
            nearz: d(0.01),
            farz: d(10000.0),
            cameraparallax: d(false),
            cameraparallaxamount: d(1.0),
            cameraparallaxdelay: d(0.5),
            cameraparallaxmouseinfluence: d(1.0),
            camerashake: d(false),
            camerashakeamplitude: d(0.5),
            camerashakespeed: d(1.0),
            camerashakeroughness: d(0.5),
            camerafade: d(false),
            camerapreview: d(false),
            hdr: d(false),
            bloom: d(false),
            bloomstrength: d(2.0),
            bloomthreshold: d(0.65),
            bloomtint: d(VecN::splat(1.0)),
            bloomhdrstrength: d(2.0),
            bloomhdrthreshold: d(1.0),
            bloomhdriterations: d(8.0),
            bloomhdrscatter: d(1.619),
            bloomhdrfeather: d(0.1),
            gravitydirection: d(VecN([0.0, -1.0, 0.0])),
            gravitystrength: d(1.0),
            winddirection: d(VecN([0.707, 0.707, 0.0])),
            windenabled: d(false),
            windstrength: d(1.0),
            lightconfig: None,
            // Fog defaults are guesses: off, fading in from nothing to
            // fully fogged over the range.
            fogdistance: d(false),
            fogdistancestart: d(0.0),
            fogdistanceend: d(1000.0),
            fogdistancecolor: d(VecN::splat(1.0)),
            fogdistancestartdensity: d(0.0),
            fogdistanceenddensity: d(1.0),
            fogheight: d(false),
            fogheightstart: d(0.0),
            fogheightend: d(100.0),
            fogheightcolor: d(VecN::splat(1.0)),
            fogheightstartdensity: d(1.0),
            fogheightenddensity: d(0.0),
        }
    }
}

/// An entry of the scene's `objects` array, classified the way Wallpaper
/// Engine does: by which content key is present and non-null, in the order
/// `image` (or `shape: "quad"`), `particle`, `sound`, `light`, `model`,
/// `text`; an object with only an `id` is a group node.
#[derive(Debug, Clone)]
pub enum SceneObject {
    Image(Box<ImageObject>),
    Particle(Box<ParticleObject>),
    Sound(Box<SoundObject>),
    Light(Box<LightObject>),
    Model(Box<ModelObject>),
    Text(Box<TextObject>),
    Group(Box<GroupObject>),
    /// No recognised content key and no `id`.
    Unknown(Value),
}

/// Discriminant of a [`SceneObject`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectKind {
    Image,
    Particle,
    Sound,
    Light,
    Model,
    Text,
    Group,
    Unknown,
}

impl ObjectKind {
    /// Classify a raw object by key presence.
    pub fn classify(obj: &Map<String, Value>) -> Self {
        let has = |k: &str| obj.get(k).is_some_and(|v| !v.is_null());
        if has("image") || obj.get("shape").and_then(Value::as_str) == Some("quad") {
            Self::Image
        } else if has("particle") {
            Self::Particle
        } else if has("sound") {
            Self::Sound
        } else if has("light") {
            Self::Light
        } else if has("model") {
            Self::Model
        } else if has("text") {
            Self::Text
        } else if obj.contains_key("id") {
            Self::Group
        } else {
            Self::Unknown
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Particle => "particle",
            Self::Sound => "sound",
            Self::Light => "light",
            Self::Model => "model",
            Self::Text => "text",
            Self::Group => "group",
            Self::Unknown => "unknown",
        }
    }
}

impl SceneObject {
    pub fn kind(&self) -> ObjectKind {
        match self {
            Self::Image(_) => ObjectKind::Image,
            Self::Particle(_) => ObjectKind::Particle,
            Self::Sound(_) => ObjectKind::Sound,
            Self::Light(_) => ObjectKind::Light,
            Self::Model(_) => ObjectKind::Model,
            Self::Text(_) => ObjectKind::Text,
            Self::Group(_) => ObjectKind::Group,
            Self::Unknown(_) => ObjectKind::Unknown,
        }
    }

    /// Fields shared by every object kind; `None` for [`SceneObject::Unknown`].
    pub fn common(&self) -> Option<&ObjectCommon> {
        Some(match self {
            Self::Image(o) => &o.common,
            Self::Particle(o) => &o.common,
            Self::Sound(o) => &o.common,
            Self::Light(o) => &o.common,
            Self::Model(o) => &o.common,
            Self::Text(o) => &o.common,
            Self::Group(o) => &o.common,
            Self::Unknown(_) => return None,
        })
    }

    /// Effects applied to this object (images and text only).
    pub fn effects(&self) -> &[ImageEffect] {
        match self {
            Self::Image(o) => &o.effects,
            Self::Text(o) => &o.effects,
            _ => &[],
        }
    }
}

impl<'de> Deserialize<'de> for SceneObject {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        let Value::Object(obj) = &v else {
            return Ok(Self::Unknown(v));
        };
        let kind = ObjectKind::classify(obj);
        let label = format!(
            "{} object {}",
            kind.as_str(),
            obj.get("id")
                .map_or_else(|| "?".to_owned(), Value::to_string)
        );
        // Parsed separately rather than flattened so errors keep their field path.
        fn parse<T: ObjectBody>(v: Value) -> Result<Box<T>, crate::JsonError> {
            let common = serde_path_to_error::deserialize(&v)?;
            let mut body: Box<T> = serde_path_to_error::deserialize(v)?;
            body.set_common(common);
            Ok(body)
        }
        let parsed = match kind {
            ObjectKind::Image => parse(v).map(Self::Image),
            ObjectKind::Particle => parse(v).map(Self::Particle),
            ObjectKind::Sound => parse(v).map(Self::Sound),
            ObjectKind::Light => parse(v).map(Self::Light),
            ObjectKind::Model => parse(v).map(Self::Model),
            ObjectKind::Text => parse(v).map(Self::Text),
            ObjectKind::Group => parse(v).map(Self::Group),
            ObjectKind::Unknown => Ok(Self::Unknown(v)),
        };
        parsed.map_err(|e| de::Error::custom(format!("{label}: {e}")))
    }
}

/// Transform, identity and visibility fields every object carries.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ObjectCommon {
    #[serde(deserialize_with = "lenient_i32")]
    pub id: i32,
    #[serde(deserialize_with = "lenient_string")]
    pub name: String,
    /// Id of the parent object whose transform this one inherits.
    pub parent: Option<i32>,
    pub origin: Dynamic<Vec3>,
    pub scale: Dynamic<Vec3>,
    /// Euler angles in radians.
    pub angles: Dynamic<Vec3>,
    pub visible: Dynamic<bool>,
    #[serde(rename = "parallaxDepth")]
    pub parallax_depth: Dynamic<Vec2>,
    /// Do not inherit transform/visibility/alpha from the parent.
    #[serde(deserialize_with = "lenient_bool")]
    pub disablepropagation: bool,
    /// Ids of objects a compose layer samples via `_rt_imageLayerComposite_<id>`.
    #[serde(deserialize_with = "int_list")]
    pub dependencies: Vec<i32>,
}

impl Default for ObjectCommon {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            parent: None,
            origin: d(VecN::splat(0.0)),
            scale: d(VecN::splat(1.0)),
            angles: d(VecN::splat(0.0)),
            visible: d(true),
            parallax_depth: d(VecN::splat(0.0)),
            disablepropagation: false,
            dependencies: Vec::new(),
        }
    }
}

const COMMON_KEYS: [&str; 10] = [
    "id",
    "name",
    "parent",
    "origin",
    "scale",
    "angles",
    "visible",
    "parallaxDepth",
    "disablepropagation",
    "dependencies",
];

/// A concrete object type whose [`ObjectCommon`] is parsed alongside it.
trait ObjectBody: serde::de::DeserializeOwned {
    fn set_common(&mut self, common: ObjectCommon);
}

macro_rules! object_body {
    ($($t:ty),* ; $($te:ty),*) => {
        $(impl ObjectBody for $t {
            fn set_common(&mut self, common: ObjectCommon) {
                self.common = common;
            }
        })*
        $(impl ObjectBody for $te {
            fn set_common(&mut self, common: ObjectCommon) {
                self.common = common;
                self.extra.retain(|k, _| !COMMON_KEYS.contains(&k.as_str()));
            }
        })*
    };
}

object_body!(ImageObject; ParticleObject, SoundObject, LightObject, ModelObject, TextObject, GroupObject);

/// Integers out of a JSON array; other entries are skipped.
fn int_list<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<i32>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_i64().map(|i| i as i32))
            .collect(),
        _ => Vec::new(),
    })
}

/// A textured quad (or puppet mesh), possibly with an effect chain.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ImageObject {
    #[serde(skip)]
    pub common: ObjectCommon,
    /// Image model JSON path, e.g. `models/layer.json`. `None` for shape quads.
    #[serde(deserialize_with = "lenient_opt_string")]
    pub image: Option<String>,
    /// `"quad"` for procedural effect quads without an image model.
    #[serde(deserialize_with = "lenient_opt_string")]
    pub shape: Option<String>,
    /// Quad size in pixels, when the model does not specify one.
    pub size: Option<Vec2>,
    pub color: Dynamic<Vec3>,
    pub alpha: Dynamic<f32>,
    pub brightness: Dynamic<f32>,
    #[serde(rename = "colorBlendMode", deserialize_with = "lenient_i32")]
    pub color_blend_mode: i32,
    /// Anchor of the quad relative to `origin` (`center`, `topleft`, ...).
    #[serde(deserialize_with = "lenient_string")]
    pub alignment: String,
    /// Render with the perspective camera instead of the flat one.
    #[serde(deserialize_with = "lenient_bool")]
    pub perspective: bool,
    /// A passthrough compose layer captures the framebuffer before its effects.
    #[serde(deserialize_with = "lenient_bool")]
    pub copybackground: bool,
    #[serde(deserialize_with = "null_default")]
    pub config: ImageConfig,
    #[serde(deserialize_with = "null_default")]
    pub effects: Vec<ImageEffect>,
    /// Per-instance material override (textures, user textures, combos).
    pub instance: Option<PassOverride>,
    /// Puppet animation layers (kept raw).
    #[serde(deserialize_with = "null_default")]
    pub animationlayers: Vec<Value>,
    /// Parent-puppet attachment point this image anchors to.
    #[serde(deserialize_with = "lenient_opt_string")]
    pub attachment: Option<String>,
}

impl Default for ImageObject {
    fn default() -> Self {
        Self {
            common: ObjectCommon::default(),
            image: None,
            shape: None,
            size: None,
            color: d(VecN::splat(1.0)),
            alpha: d(1.0),
            brightness: d(1.0),
            color_blend_mode: 0,
            alignment: "center".to_owned(),
            perspective: false,
            copybackground: true,
            config: ImageConfig::default(),
            effects: Vec::new(),
            instance: None,
            animationlayers: Vec::new(),
            attachment: None,
        }
    }
}

/// An image object's `config` block.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ImageConfig {
    #[serde(deserialize_with = "lenient_bool")]
    pub passthrough: bool,
}

/// An effect instance on an image or text object.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ImageEffect {
    /// Path of the `effect.json`, e.g. `effects/blur/effect.json`.
    pub file: String,
    pub id: Option<i64>,
    #[serde(deserialize_with = "lenient_string")]
    pub name: String,
    pub visible: Dynamic<bool>,
    /// Overrides for the effect's material passes, index-aligned with them.
    #[serde(deserialize_with = "null_default")]
    pub passes: Vec<PassOverride>,
}

impl Default for ImageEffect {
    fn default() -> Self {
        Self {
            file: String::new(),
            id: None,
            name: String::new(),
            visible: d(true),
            passes: Vec::new(),
        }
    }
}

/// A particle system instance.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ParticleObject {
    #[serde(skip)]
    pub common: ObjectCommon,
    /// Particle definition JSON path.
    pub particle: String,
    /// Per-instance overrides of rate, count, colour, control points, ... (kept raw).
    pub instanceoverride: Option<Value>,
    /// Remaining keys, not yet modelled.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A sound player.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SoundObject {
    #[serde(skip)]
    pub common: ObjectCommon,
    /// Sound files; one is picked per playback depending on `playbackmode`.
    #[serde(deserialize_with = "string_list")]
    pub sound: Vec<String>,
    pub volume: Dynamic<f32>,
    /// `loop`, `random` or `single`.
    #[serde(deserialize_with = "lenient_opt_string")]
    pub playbackmode: Option<String>,
    pub mintime: Option<f32>,
    pub maxtime: Option<f32>,
    #[serde(deserialize_with = "lenient_bool")]
    pub startsilent: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for SoundObject {
    fn default() -> Self {
        Self {
            common: ObjectCommon::default(),
            sound: Vec::new(),
            volume: d(1.0),
            playbackmode: None,
            mintime: None,
            maxtime: None,
            startsilent: false,
            extra: Map::new(),
        }
    }
}

/// Non-empty strings out of a JSON array (or a single string).
fn string_list<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Array(a) => a
            .into_iter()
            .filter_map(|v| match v {
                Value::String(s) if !s.is_empty() => Some(s),
                _ => None,
            })
            .collect(),
        Value::String(s) => vec![s],
        _ => Vec::new(),
    })
}

/// A point/spot/tube/directional light.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LightObject {
    #[serde(skip)]
    pub common: ObjectCommon,
    /// Light type: `lpoint`, `lspot`, `ltube`, `ldirectional`, or the legacy `point`.
    pub light: String,
    pub color: Dynamic<Vec3>,
    pub radius: Dynamic<f32>,
    pub intensity: Dynamic<f32>,
    /// Falloff power: `pow(saturate(1 - distance / radius), exponent)`.
    pub exponent: Dynamic<f32>,
    /// Spot light cone half-angles, in degrees.
    #[serde(alias = "innerCone")]
    pub innercone: Option<Dynamic<f32>>,
    #[serde(alias = "outerCone")]
    pub outercone: Option<Dynamic<f32>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for LightObject {
    fn default() -> Self {
        Self {
            common: ObjectCommon::default(),
            light: String::new(),
            color: d(VecN::splat(1.0)),
            radius: d(1000.0),
            intensity: d(1.0),
            exponent: d(1.0),
            innercone: None,
            outercone: None,
            extra: Map::new(),
        }
    }
}

/// A 3D model instance.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ModelObject {
    #[serde(skip)]
    pub common: ObjectCommon,
    /// Model path, usually the `.mdl` itself (see [`crate::model::model_mdl_path`]).
    pub model: String,
    /// Parent-puppet attachment point this model rigs to.
    #[serde(deserialize_with = "lenient_opt_string")]
    pub attachment: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A text layer.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TextObject {
    #[serde(skip)]
    pub common: ObjectCommon,
    /// The text, often produced by a script.
    pub text: Dynamic<String>,
    #[serde(deserialize_with = "lenient_opt_string")]
    pub font: Option<String>,
    pub pointsize: Dynamic<f32>,
    pub size: Option<Vec2>,
    pub color: Dynamic<Vec3>,
    pub alpha: Dynamic<f32>,
    pub brightness: Dynamic<f32>,
    #[serde(deserialize_with = "lenient_opt_string")]
    pub horizontalalign: Option<String>,
    #[serde(deserialize_with = "lenient_opt_string")]
    pub verticalalign: Option<String>,
    #[serde(deserialize_with = "lenient_opt_string")]
    pub anchor: Option<String>,
    pub padding: Dynamic<Vec2>,
    pub maxwidth: Option<Dynamic<f32>>,
    pub maxrows: Option<f32>,
    #[serde(deserialize_with = "null_default")]
    pub effects: Vec<ImageEffect>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for TextObject {
    fn default() -> Self {
        Self {
            common: ObjectCommon::default(),
            text: d(String::new()),
            font: None,
            pointsize: d(32.0),
            size: None,
            color: d(VecN::splat(1.0)),
            alpha: d(1.0),
            brightness: d(1.0),
            horizontalalign: None,
            verticalalign: None,
            anchor: None,
            padding: d(VecN::splat(0.0)),
            maxwidth: None,
            maxrows: None,
            effects: Vec::new(),
            extra: Map::new(),
        }
    }
}

/// A transform-only node that other objects parent to.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GroupObject {
    #[serde(skip)]
    pub common: ObjectCommon,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_json;
    use crate::value::UserBinding;

    const SCENE: &str = r#"{
        "camera": {"center": "0 0 -1", "eye": "0 0 0", "up": "0 1 0"},
        "general": {
            "orthogonalprojection": {"width": 1920, "height": 1080},
            "clearcolor": {"user": "schemecolor", "value": "0.1 0.2 0.3"},
            "bloom": true,
            "zoom": 1.5,
            "clearenabled": null
        },
        "objects": [
            {"id": 1, "name": "bg", "image": "models/bg.json", "origin": "960 540 0",
             "scale": 2, "size": "1920 1080", "alpha": {"user": "opacity", "value": 0.8},
             "visible": {"user": {"condition": "1", "name": "mode"}, "value": true},
             "effects": [{"file": "effects/blur/effect.json", "id": 5, "name": "",
                          "visible": {"user": "blur", "value": false},
                          "passes": [{"id": 6, "combos": {"KERNEL": 2},
                                      "constantshadervalues": {"scale": "2 2"},
                                      "textures": [null, "masks/m"]}]}],
             "instance": {"id": 7, "combos": {"version": 2}, "textures": ["util/white"]},
             "dependencies": [2, "x"]},
            {"id": 2, "shape": "quad", "origin": "0 0 0", "effects": []},
            {"id": 3, "particle": "particles/rain.json", "image": null, "parent": 10,
             "instanceoverride": {"count": 2}},
            {"id": 4, "sound": ["sounds/a.mp3", ""], "volume": {"user": "vol", "value": 0.5},
             "playbackmode": "loop"},
            {"id": 5, "light": "lpoint", "color": "1 0.5 0", "radius": 300, "intensity": 2},
            {"id": 6, "model": "models/x.mdl", "attachment": "hand", "origin": {"animation": {"c0": []}, "value": "1 2 3"}},
            {"id": 7, "text": {"script": "...", "value": "12:00"}, "font": "fonts/a.ttf",
             "pointsize": {"user": "size", "value": 40}, "padding": 32},
            {"id": 10, "name": "group", "origin": "1 1 1"},
            {"name": "orphan"}
        ]
    }"#;

    fn scene() -> Scene {
        parse_json(SCENE).unwrap()
    }

    #[test]
    fn camera_and_general() {
        let s = scene();
        assert_eq!(s.camera.center.0, [0.0, 0.0, -1.0]);
        let g = &s.general;
        let ortho = g.orthogonalprojection.as_ref().unwrap();
        assert_eq!((ortho.width, ortho.height, ortho.auto), (1920, 1080, false));
        assert_eq!(g.clearcolor.value.0, [0.1, 0.2, 0.3]);
        assert_eq!(g.clearcolor.user.as_ref().unwrap().name(), "schemecolor");
        assert!(g.bloom.value && g.clearenabled.value == bool::default());
        assert_eq!(g.zoom.value, 1.5);
        assert_eq!(g.fov.value, 50.0);
        assert_eq!(g.ambientcolor.value.0, [0.2; 3]);
    }

    #[test]
    fn perspective_and_auto_projection() {
        let s: Scene = parse_json(r#"{"general": {"orthogonalprojection": null}}"#).unwrap();
        assert!(s.general.orthogonalprojection.is_none());
        let s: Scene =
            parse_json(r#"{"general": {"orthogonalprojection": {"auto": true}}}"#).unwrap();
        assert!(s.general.orthogonalprojection.unwrap().auto);
        let s: Scene = parse_json("{}").unwrap();
        assert!(s.objects.is_empty() && s.general.orthogonalprojection.is_none());
    }

    #[test]
    fn classifies_objects() {
        let kinds: Vec<_> = scene().objects.iter().map(SceneObject::kind).collect();
        use ObjectKind::*;
        assert_eq!(
            kinds,
            [
                Image, Image, Particle, Sound, Light, Model, Text, Group, Unknown
            ]
        );
    }

    #[test]
    fn image_object() {
        let s = scene();
        let SceneObject::Image(img) = &s.objects[0] else {
            panic!()
        };
        assert_eq!(img.image.as_deref(), Some("models/bg.json"));
        assert_eq!(img.common.scale.value.0, [2.0; 3]);
        assert_eq!(img.common.origin.value.0, [960.0, 540.0, 0.0]);
        assert_eq!(img.size.unwrap().0, [1920.0, 1080.0]);
        assert_eq!(img.alpha.value, 0.8);
        assert_eq!(img.color.value.0, [1.0; 3]);
        assert!(matches!(
            img.common.visible.user,
            Some(UserBinding::Condition { .. })
        ));
        assert_eq!(img.common.dependencies, [2]);
        assert_eq!(img.alignment, "center");
        assert!(img.copybackground);
        let fx = &img.effects[0];
        assert!(!fx.visible.value && fx.visible.user.is_some());
        assert_eq!(fx.passes[0].combos["KERNEL"].0, 2);
        assert_eq!(
            fx.passes[0].constantshadervalues["scale"].value.0,
            [2.0, 2.0]
        );
        assert_eq!(img.instance.as_ref().unwrap().textures.len(), 1);

        let SceneObject::Image(quad) = &s.objects[1] else {
            panic!()
        };
        assert!(quad.image.is_none() && quad.shape.as_deref() == Some("quad"));
    }

    #[test]
    fn other_objects() {
        let s = scene();
        let SceneObject::Particle(p) = &s.objects[2] else {
            panic!()
        };
        assert_eq!(p.common.parent, Some(10));
        assert!(p.instanceoverride.is_some());
        let SceneObject::Sound(snd) = &s.objects[3] else {
            panic!()
        };
        assert_eq!(snd.sound, ["sounds/a.mp3"]);
        assert_eq!(snd.volume.value, 0.5);
        let SceneObject::Light(l) = &s.objects[4] else {
            panic!()
        };
        assert_eq!((l.radius.value, l.intensity.value), (300.0, 2.0));
        assert_eq!(l.exponent.value, 1.0);
        assert!(l.innercone.is_none());
        let SceneObject::Model(m) = &s.objects[5] else {
            panic!()
        };
        assert!(m.common.origin.animation.is_some());
        assert_eq!(m.attachment.as_deref(), Some("hand"));
        assert!(m.extra.is_empty());
        let SceneObject::Text(t) = &s.objects[6] else {
            panic!()
        };
        assert_eq!(t.text.value, "12:00");
        assert!(t.text.script.is_some());
        assert_eq!(t.pointsize.value, 40.0);
        assert_eq!(t.padding.value.0, [32.0; 2]);
        let SceneObject::Group(g) = &s.objects[7] else {
            panic!()
        };
        assert_eq!(g.common.name, "group");
        assert!(g.extra.is_empty());
    }

    #[test]
    fn object_errors_name_the_object() {
        let err = parse_json::<Scene>(r#"{"objects": [{"id": 9, "image": "m", "origin": "1 x"}]}"#)
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("image object 9") && msg.contains("origin"),
            "{msg}"
        );
    }
}
