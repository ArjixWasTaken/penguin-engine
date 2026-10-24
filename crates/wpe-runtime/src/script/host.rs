//! The [`SceneHost`] scripts run against: what they can read and change of
//! each layer and of the scene's structure.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{EulerRot, Mat4, Quat, Vec2, Vec3};
use serde_json::Value;
use wpe_assets::scene::{SceneObject, TextObject};
use wpe_scene::{ImageLayer, Motion};
use wpe_script::{LayerInfo, LayerKind, LogLevel, SceneHost, ScriptValue};

use super::audit::Audit;
use super::edit::SceneEdit;
use crate::bones::SharedBones;
use crate::clock::{self, SharedClocks};
use crate::frame::LayerSteps;
use crate::sound::{SoundCommand, SoundState};
use crate::video::VideoStream;

/// Which of a layer's values scripts have set; these survive rebuilds of the
/// frame, the rest follow the scene.
#[derive(Debug, Default, Clone, Copy)]
pub struct Touched {
    pub visible: bool,
    pub transform: bool,
    pub color: bool,
    pub text: bool,
}

/// What scripts can see and change of one scene object.
pub struct LayerState {
    pub info: LayerInfo,
    /// The object's scene configuration, as edited by scripts.
    pub config: Value,
    /// The configuration it was loaded or created with.
    pub initial_config: Value,
    pub steps: LayerSteps,
    pub motion: Motion,
    pub size: Vec2,
    /// Takes cursor clicks (`solid`); scripts turn it off to let clicks
    /// through to what is underneath.
    pub solid: bool,
    pub visible: bool,
    pub origin: Vec3,
    pub scale: Vec3,
    pub angles: Vec3,
    pub alpha: f32,
    pub color: Vec3,
    pub text: Option<String>,
    pub text_object: Option<Box<TextObject>>,
    /// No ancestor group is hidden, so the layer shows when `visible`.
    pub ancestors_visible: bool,
    pub touched: Touched,
    /// Material constants scripts set: (effect index or `None` for the
    /// layer's material, pass index or `None` for all, constant name).
    pub constants: HashMap<(Option<usize>, Option<usize>, String), Vec<f32>>,
    pub dirty_constants: bool,
    pub dirty_visible: bool,
    pub dirty_transform: bool,
    pub dirty_color: bool,
    pub dirty_text: bool,
}

pub fn vec3(v: Vec3) -> ScriptValue {
    ScriptValue::Vec3([v.x as f64, v.y as f64, v.z as f64])
}

pub fn to_vec3(v: &ScriptValue) -> Option<Vec3> {
    let c = v.components()?;
    match c.len() {
        0 => None,
        1 => Some(Vec3::splat(c[0] as f32)),
        2 => Some(Vec3::new(c[0] as f32, c[1] as f32, 0.0)),
        _ => Some(Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32)),
    }
}

pub fn kind(obj: &SceneObject) -> LayerKind {
    match obj {
        SceneObject::Image(_) => LayerKind::Image,
        SceneObject::Text(_) => LayerKind::Text,
        SceneObject::Particle(_) => LayerKind::Particle,
        SceneObject::Sound(_) => LayerKind::Sound,
        SceneObject::Light(_) => LayerKind::Light,
        SceneObject::Model(_) => LayerKind::Model,
        SceneObject::Group(_) => LayerKind::Group,
        SceneObject::Unknown(_) => LayerKind::Other,
    }
}

impl LayerState {
    /// State for `obj`, built as `layer` (when it draws) with `steps`.
    pub fn new(
        obj: &SceneObject,
        config: Value,
        layer: Option<&ImageLayer>,
        steps: LayerSteps,
    ) -> Option<Self> {
        let c = obj.common()?;
        let solid = config.get("solid").and_then(Value::as_bool).unwrap_or(true);
        let motion = layer.map(|l| l.motion.clone()).unwrap_or_else(|| Motion {
            origin: Vec3::from_array(*c.origin.value),
            scale: Vec3::from_array(*c.scale.value),
            angles: Vec3::from_array(*c.angles.value),
            ..Default::default()
        });
        Some(Self {
            info: LayerInfo {
                id: c.id,
                name: c.name.clone(),
                kind: kind(obj),
                parent: c.parent,
                effects: obj.effects().iter().map(|e| e.name.clone()).collect(),
            },
            initial_config: config.clone(),
            config,
            steps,
            size: layer.map_or(Vec2::ONE, |l| l.size),
            solid,
            visible: layer.map_or(c.visible.value, |l| l.self_visible),
            ancestors_visible: layer.is_none_or(|l| l.ancestors_visible),
            origin: motion.origin,
            scale: motion.scale,
            angles: motion.angles,
            alpha: layer.map_or(1.0, |l| l.alpha),
            color: layer.map_or(Vec3::ONE, |l| l.color),
            text: match obj {
                SceneObject::Text(t) => Some(t.text.value.clone()),
                _ => None,
            },
            text_object: layer.and_then(|l| l.text.clone()),
            motion,
            touched: Touched::default(),
            constants: HashMap::new(),
            dirty_constants: false,
            dirty_visible: false,
            dirty_transform: false,
            dirty_color: false,
            dirty_text: false,
        })
    }

    /// The layer's motion with script-set origin, scale and angles.
    pub fn motion_now(&self) -> Motion {
        Motion {
            origin: self.origin,
            scale: self.scale,
            angles: self.angles,
            origin_animation: None,
            scale_animation: None,
            angles_animation: None,
            ..self.motion.clone()
        }
    }

    /// Follow a rebuilt frame: take the scene's values for whatever scripts
    /// haven't set, and mark what they have for re-applying.
    pub fn rebind(&mut self, layer: Option<&ImageLayer>, steps: LayerSteps) {
        self.steps = steps;
        let Some(layer) = layer else {
            return;
        };
        self.size = layer.size;
        self.ancestors_visible = layer.ancestors_visible;
        if layer.text.is_some() {
            self.text_object = layer.text.clone();
        }
        let motion = layer.motion.clone();
        if !self.touched.transform {
            (self.origin, self.scale, self.angles) = (motion.origin, motion.scale, motion.angles);
        }
        self.motion = motion;
        if !self.touched.visible {
            self.visible = layer.self_visible;
        }
        if !self.touched.color {
            (self.alpha, self.color) = (layer.alpha, layer.color);
        }
        self.dirty_constants = !self.constants.is_empty();
        self.dirty_visible = self.touched.visible;
        self.dirty_transform = self.touched.transform;
        self.dirty_color = self.touched.color;
        self.dirty_text = self.touched.text;
    }
}

/// A material or effect property path scripts write.
#[derive(Debug, Clone, PartialEq)]
enum MaterialPath {
    /// `effects/<i>/visible`.
    EffectVisible(usize),
    /// `material/<name>`, `effects/<i>/material/<name>` or
    /// `effects/<i>/passes/<j>/constantshadervalues/<name>`.
    Constant {
        effect: Option<usize>,
        pass: Option<usize>,
        name: String,
    },
}

fn material_path(path: &str) -> Option<MaterialPath> {
    if let Some(name) = path.strip_prefix("material/") {
        return Some(MaterialPath::Constant {
            effect: None,
            pass: None,
            name: name.to_owned(),
        });
    }
    let rest = path.strip_prefix("effects/")?;
    let (index, rest) = rest.split_once('/')?;
    let effect = index.parse().ok()?;
    if rest == "visible" {
        return Some(MaterialPath::EffectVisible(effect));
    }
    if let Some(name) = rest.strip_prefix("material/") {
        return Some(MaterialPath::Constant {
            effect: Some(effect),
            pass: None,
            name: name.to_owned(),
        });
    }
    let (pass, name) = rest.strip_prefix("passes/")?.split_once('/')?;
    Some(MaterialPath::Constant {
        effect: Some(effect),
        pass: Some(pass.parse().ok()?),
        name: name.strip_prefix("constantshadervalues/")?.to_owned(),
    })
}

/// A script value as uniform floats.
fn floats(v: &ScriptValue) -> Option<Vec<f32>> {
    Some(match v {
        ScriptValue::Number(n) => vec![*n as f32],
        ScriptValue::Bool(b) => vec![*b as u8 as f32],
        ScriptValue::Vec2(c) => c.iter().map(|&x| x as f32).collect(),
        ScriptValue::Vec3(c) => c.iter().map(|&x| x as f32).collect(),
        ScriptValue::Vec4(c) => c.iter().map(|&x| x as f32).collect(),
        ScriptValue::String(s) => {
            let n: Vec<f32> = s
                .split_whitespace()
                .filter_map(|x| x.parse().ok())
                .collect();
            (!n.is_empty()).then_some(n)?
        }
        _ => return None,
    })
}

/// The JSON value at `path` (`effects/0/visible`) inside `obj`.
fn json_at<'a>(obj: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('/').try_fold(obj, |node, key| match node {
        Value::Object(map) => map.get(key),
        Value::Array(items) => items.get(key.parse::<usize>().ok()?),
        _ => None,
    })
}

fn json_at_mut<'a>(obj: &'a mut Value, path: &str) -> Option<&'a mut Value> {
    path.split('/').try_fold(obj, |node, key| match node {
        Value::Object(map) => map.get_mut(key),
        Value::Array(items) => items.get_mut(key.parse::<usize>().ok()?),
        _ => None,
    })
}

/// The scene value under `key` in an object configuration, looking through
/// `{"value": ...}` wrappers.
fn config_value<'a>(config: &'a Value, key: &str) -> Option<&'a Value> {
    let v = config.get(key)?;
    Some(v.get("value").unwrap_or(v))
}

/// Numbers in a scene vector (`"1 2 3"`, a number or an array).
fn config_numbers(v: &Value) -> Vec<f64> {
    match v {
        Value::String(s) => s
            .split_whitespace()
            .filter_map(|n| n.parse().ok())
            .collect(),
        Value::Number(n) => n.as_f64().into_iter().collect(),
        Value::Array(a) => a.iter().filter_map(Value::as_f64).collect(),
        _ => Vec::new(),
    }
}

/// Layer angles (degrees, applied Z then Y then X as scene transforms do)
/// of a rotation.
pub fn euler_degrees(rotation: Quat) -> Vec3 {
    let (z, y, x) = rotation.to_euler(EulerRot::ZYX);
    Vec3::new(x, y, z) * (180.0 / std::f32::consts::PI)
}

/// The rotation of layer angles in degrees.
pub fn rotation_of(angles: Vec3) -> Quat {
    let a = angles * (std::f32::consts::PI / 180.0);
    Quat::from_euler(EulerRot::ZYX, a.z, a.y, a.x)
}

/// The first keyframe animation named `name` (any, when empty) anywhere
/// in `value`: a scene value's `"animation"` binding.
fn find_animation(value: &Value, name: &str) -> Option<wpe_scene::Animation> {
    match value {
        Value::Object(map) => {
            if let Some(animation) = map
                .get("animation")
                .and_then(wpe_scene::Animation::parse)
                .filter(|a| name.is_empty() || a.name == name)
            {
                return Some(animation);
            }
            map.values().find_map(|v| find_animation(v, name))
        }
        Value::Array(items) => items.iter().find_map(|v| find_animation(v, name)),
        _ => None,
    }
}

/// A script value as it's written in scene JSON (vectors as `"x y z"`).
fn scene_json(v: &ScriptValue) -> Value {
    match v {
        ScriptValue::Vec2(c) => format!("{} {}", c[0], c[1]).into(),
        ScriptValue::Vec3(c) => format!("{} {} {}", c[0], c[1], c[2]).into(),
        ScriptValue::Vec4(c) => format!("{} {} {} {}", c[0], c[1], c[2], c[3]).into(),
        other => other.to_json(),
    }
}

/// Layer properties that live in the object's configuration and take effect
/// when the frame is rebuilt, with the value scripts see when it's unset.
fn layout_default(property: &str) -> Option<ScriptValue> {
    Some(match property {
        "alignment" => ScriptValue::String("center".into()),
        "perspective" | "rootmotion" => ScriptValue::Bool(false),
        "fov" => ScriptValue::Number(50.0),
        "parallaxDepth" => ScriptValue::Vec2([0.0, 0.0]),
        "horizontalalign" | "verticalalign" => ScriptValue::String("center".into()),
        "anchor" => ScriptValue::String("none".into()),
        "font" => ScriptValue::String(String::new()),
        "pointsize" | "padding" | "maxwidth" | "maxrows" => ScriptValue::Number(0.0),
        "limitrows" | "limitwidth" | "opaquebackground" => ScriptValue::Bool(false),
        "backgroundcolor" => ScriptValue::Vec3([0.0; 3]),
        _ => return None,
    })
}

fn layout_get(config: &Value, property: &str) -> Option<ScriptValue> {
    let default = layout_default(property)?;
    let Some(v) = config_value(config, property) else {
        return Some(default);
    };
    Some(match default {
        ScriptValue::Vec2(_) => match config_numbers(v)[..] {
            [x, y, ..] => ScriptValue::Vec2([x, y]),
            [x] => ScriptValue::Vec2([x, x]),
            _ => default,
        },
        ScriptValue::Vec3(_) => match config_numbers(v)[..] {
            [x, y, z, ..] => ScriptValue::Vec3([x, y, z]),
            _ => default,
        },
        ScriptValue::Number(_) => config_numbers(v)
            .first()
            .map_or(default, |&n| ScriptValue::Number(n)),
        ScriptValue::Bool(_) => v.as_bool().map_or(default, ScriptValue::Bool),
        _ => v
            .as_str()
            .map_or(default, |s| ScriptValue::String(s.to_owned())),
    })
}

/// A `createLayer` argument as an object configuration without an id: a
/// model, particle or text path, or a configuration object.
fn new_layer_config(config: &Value) -> Option<Value> {
    match config {
        Value::String(path) => {
            let key = if path.starts_with("particles/") {
                "particle"
            } else if path.ends_with(".mdl") || path.starts_with(crate::model_data::PREFIX) {
                "model"
            } else {
                "image"
            };
            Some(serde_json::json!({ key: path, "name": path }))
        }
        Value::Object(map) if map.contains_key("__path") => {
            let mut config = new_layer_config(map.get("__path")?)?;
            if let Some(workshop) = map.get("__workshop") {
                config["__workshop"] = workshop.clone();
            }
            Some(config)
        }
        Value::Object(map) => {
            let mut map = map.clone();
            map.remove("id");
            Some(Value::Object(map))
        }
        _ => None,
    }
}

/// What scripts do to a particle system.
#[derive(Debug, Clone, PartialEq)]
pub enum ParticleCommand {
    /// The whole `instanceoverride` object to use from now on.
    Override(Value),
    Emit(u32),
    /// Pause or resume the simulation.
    Playing(bool),
    /// Clear the system and pause it.
    Stop,
}

/// The [`SceneHost`] scripts run against.
pub struct ScriptHost {
    pub layers: Vec<LayerState>,
    pub by_id: HashMap<i32, usize>,
    pub canvas: [f32; 2],
    /// Output size in pixels, once known.
    pub screen: Option<[f32; 2]>,
    pub spectrum: Option<wpe_audio::AudioSpectrum>,
    /// Each layer's video, for `getVideoTexture()`.
    pub videos: HashMap<i32, Arc<VideoStream>>,
    /// Structural changes waiting for the next frame rebuild.
    pub edits: Vec<SceneEdit>,
    /// The scene's `general` settings, as scripts have changed them.
    pub general: Value,
    /// `localStorage` contents changed since the runtime last took them.
    pub storage: Option<Value>,
    /// `usershortcut` properties the user bound at runtime, by name.
    pub user_shortcuts: HashMap<String, Value>,
    /// Scene time of the current frame.
    pub time: f64,
    /// Particle layers paused by scripts.
    pub particles_paused: std::collections::HashSet<i32>,
    /// Animation layers created with `playSingleAnimation`, removed when
    /// they finish: layer and animation layer index.
    pub single_layers: Vec<(i32, usize)>,
    /// Camera centre and zoom scripts set (`setCameraTransforms`).
    pub camera: Option<(Vec2, f32)>,
    /// In 3D scenes, the perspective camera's eye, centre and up as last
    /// drawn, and the ones scripts set instead.
    pub view3d: Option<[Vec3; 3]>,
    pub camera3d: Option<[Vec3; 3]>,
    /// Render targets scripts asked to clear (effect functions).
    pub clears: Vec<wpe_render::TargetId>,
    /// Id for the next created layer.
    pub next_id: i32,
    /// A group's visibility changed: descendants must be re-evaluated.
    pub groups_dirty: bool,
    /// Sound layers' play state, and what scripts did to them.
    pub sounds: HashMap<i32, SoundState>,
    pub sound_commands: Vec<(i32, SoundCommand)>,
    /// Particle layers' instance overrides scripts changed, and what they
    /// asked of particle systems since the last frame.
    pub particle_overrides: HashMap<i32, Value>,
    pub particle_commands: Vec<(i32, ParticleCommand)>,
    /// Animation clocks and puppet bones, shared with the frame's bindings.
    pub clocks: SharedClocks,
    pub bones: SharedBones,
    /// Script geometry (`createModelData`), shared with the frame.
    pub model_data: crate::model_data::SharedModelDatas,
    pub audit: Option<Audit>,
}

impl ScriptHost {
    pub fn new(canvas: Vec2, videos: HashMap<i32, Arc<VideoStream>>) -> Self {
        Self {
            layers: Vec::new(),
            by_id: HashMap::new(),
            canvas: canvas.to_array(),
            screen: None,
            spectrum: None,
            videos,
            edits: Vec::new(),
            general: Value::Null,
            groups_dirty: false,
            storage: None,
            user_shortcuts: HashMap::new(),
            time: 0.0,
            particles_paused: Default::default(),
            single_layers: Vec::new(),
            camera: None,
            view3d: None,
            camera3d: None,
            clears: Vec::new(),
            next_id: 1,
            sounds: HashMap::new(),
            sound_commands: Vec::new(),
            particle_overrides: HashMap::new(),
            particle_commands: Vec::new(),
            clocks: Default::default(),
            bones: Default::default(),
            model_data: Default::default(),
            audit: Audit::from_env(),
        }
    }

    pub fn push(&mut self, state: LayerState) {
        self.next_id = self.next_id.max(state.info.id + 1);
        self.by_id.insert(state.info.id, self.layers.len());
        self.layers.push(state);
    }

    fn reindex(&mut self) {
        self.by_id = self
            .layers
            .iter()
            .enumerate()
            .map(|(i, l)| (l.info.id, i))
            .collect();
    }

    pub fn layer(&self, id: i32) -> Option<&LayerState> {
        self.by_id.get(&id).map(|&i| &self.layers[i])
    }

    fn layer_mut(&mut self, id: i32) -> Option<&mut LayerState> {
        self.by_id.get(&id).map(|&i| &mut self.layers[i])
    }

    /// Puppet bone queries and moves (`getBoneTransform`, `setBoneTransform`,
    /// `applyBonePhysicsImpulse`, ...), in scene coordinates.
    fn bone_call(&mut self, layer: i32, method: &str, args: &[ScriptValue]) -> Option<ScriptValue> {
        tracing::trace!("layer {layer} {method}({args:?})");
        let model = self.layer(layer)?.motion_now().transform_at(0.0);
        let mut bones = self.bones.borrow_mut();
        let puppet = bones.puppets.get_mut(&layer)?;
        // Bones and attachments go by name or index.
        let key = |v: Option<&ScriptValue>| match v? {
            ScriptValue::String(s) => Some(s.clone()),
            v => Some((v.as_f64()? as i64).to_string()),
        };
        let bone = || puppet.bone(&key(args.first())?);
        let attachment = || puppet.attachment(&key(args.first())?);
        // A `Mat4` arrives as its 16 numbers, column-major.
        let matrix = |v: &ScriptValue| -> Option<Mat4> {
            let ScriptValue::Json(Value::Array(a)) = v else {
                return None;
            };
            let c: Vec<f32> = a
                .iter()
                .filter_map(Value::as_f64)
                .map(|x| x as f32)
                .collect();
            (c.len() == 16).then(|| Mat4::from_cols_slice(&c))
        };
        let json = |m: Mat4| {
            let c: Vec<f64> = m.to_cols_array().iter().map(|&x| x as f64).collect();
            ScriptValue::Json(c.into())
        };
        let vector = |v: Option<&ScriptValue>| to_vec3(v?);
        Some(match method {
            "getBoneCount" => ScriptValue::Number(puppet.names.len() as f64),
            // Blend shapes go by name or index too (files store no names).
            "getBlendShapeIndex" => {
                let name = key(args.first())?;
                let i = puppet.blend_shape(&name);
                ScriptValue::Number(i.map_or(-1.0, |i| i as f64))
            }
            "getBlendShapeWeight" => {
                let i = puppet.blend_shape(&key(args.first())?)?;
                let weight = puppet
                    .current_weights
                    .get(i)
                    .or(puppet.blend_weights.get(i))
                    .copied()?;
                ScriptValue::Number(weight as f64)
            }
            "setBlendShapeWeight" => {
                let i = puppet.blend_shape(&key(args.first())?)?;
                let weight = args.get(1)?.as_f64()? as f32;
                puppet.blend_weights[i] = weight;
                // Read back right away, before the next frame draws it.
                if let Some(w) = puppet.current_weights.get_mut(i) {
                    *w = weight;
                }
                return None;
            }
            "getBoneIndex" => {
                let name = args.first().and_then(|v| match v {
                    ScriptValue::String(s) => Some(s.as_str()),
                    _ => None,
                })?;
                let i = puppet.names.iter().position(|n| n == name);
                ScriptValue::Number(i.map_or(-1.0, |i| i as f64))
            }
            "getBoneParentIndex" => {
                let parent = puppet.parents.get(bone()?).copied().flatten();
                ScriptValue::Number(parent.map_or(-1.0, |p| p as f64))
            }
            "getBoneTransform" => json(model * puppet.worlds[bone()?]),
            "getLocalBoneTransform" => {
                let i = bone()?;
                let parent = puppet.parents[i].map_or(Mat4::IDENTITY, |p| puppet.worlds[p]);
                json(parent.inverse() * puppet.worlds[i])
            }
            "getLocalBoneAngles" => {
                let i = bone()?;
                let parent = puppet.parents[i].map_or(Mat4::IDENTITY, |p| puppet.worlds[p]);
                let (_, rotation, _) =
                    (parent.inverse() * puppet.worlds[i]).to_scale_rotation_translation();
                vec3(euler_degrees(rotation))
            }
            "setLocalBoneAngles" => {
                let i = bone()?;
                let angles = vector(args.get(1))?;
                puppet.rotate(i, Some(rotation_of(angles)));
                return None;
            }
            "getAttachmentIndex" => {
                let i = attachment();
                ScriptValue::Number(i.map_or(-1.0, |i| i as f64))
            }
            "getAttachmentMatrix" => json(model * puppet.attachment_pose(attachment()?)?),
            "getAttachmentOrigin" => vec3(
                (model * puppet.attachment_pose(attachment()?)?)
                    .w_axis
                    .truncate(),
            ),
            "getAttachmentAngles" => {
                let m = model * puppet.attachment_pose(attachment()?)?;
                vec3(euler_degrees(m.to_scale_rotation_translation().1))
            }
            "getLocalBoneOrigin" => {
                let i = bone()?;
                let parent = puppet.parents[i].map_or(Mat4::IDENTITY, |p| puppet.worlds[p]);
                vec3((parent.inverse() * puppet.worlds[i]).w_axis.truncate())
            }
            "setBoneTransform" => {
                let (i, m) = (bone()?, matrix(args.get(1)?)?);
                let target = model.inverse().transform_point3(m.w_axis.truncate());
                puppet.place(i, target);
                return None;
            }
            "setLocalBoneTransform" | "setLocalBoneOrigin" => {
                let i = bone()?;
                let local = match method {
                    "setLocalBoneOrigin" => vector(args.get(1))?,
                    _ => {
                        let m = matrix(args.get(1)?)?;
                        puppet.rotate(i, Some(m.to_scale_rotation_translation().1));
                        m.w_axis.truncate()
                    }
                };
                let parent = puppet.parents[i].map_or(Mat4::IDENTITY, |p| puppet.worlds[p]);
                puppet.place(i, parent.transform_point3(local));
                return None;
            }
            "applyBonePhysicsImpulse" => {
                let i = bone()?;
                let direction = vector(args.get(1)).unwrap_or(Vec3::ZERO);
                puppet.impulse(i, model.inverse().transform_vector3(direction));
                return None;
            }
            "resetBonePhysicsSimulation" => {
                let i = args.first().and_then(ScriptValue::as_f64);
                puppet.reset(i.filter(|&i| i >= 0.0).map(|i| i as usize));
                return None;
            }
            _ => {
                drop(bones);
                self.unhandled("invoke", &format!("bones.{method}"));
                return None;
            }
        })
    }

    fn material_get(&self, layer: i32, property: &str, path: MaterialPath) -> Option<ScriptValue> {
        let l = self.layer(layer)?;
        if let MaterialPath::Constant {
            effect,
            pass,
            ref name,
        } = path
            && let Some(v) = l.constants.get(&(effect, pass, name.clone()))
        {
            return Some(match v[..] {
                [x] => ScriptValue::Number(x as f64),
                [x, y] => ScriptValue::Vec2([x as f64, y as f64]),
                [x, y, z] => ScriptValue::Vec3([x as f64, y as f64, z as f64]),
                _ => ScriptValue::Json(v.iter().map(|&x| x as f64).collect::<Vec<_>>().into()),
            });
        }
        let v = json_at(&l.config, property)?;
        let v = v.get("value").unwrap_or(v);
        Some(match path {
            MaterialPath::EffectVisible(_) => ScriptValue::Bool(v.as_bool().unwrap_or(true)),
            MaterialPath::Constant { .. } => ScriptValue::from_scene_json(property, v),
        })
    }

    fn material_set(
        &mut self,
        layer: i32,
        property: &str,
        path: MaterialPath,
        value: &ScriptValue,
    ) {
        let Some(l) = self.layer_mut(layer) else {
            return;
        };
        match path {
            MaterialPath::EffectVisible(_) => {
                let Some(visible) = value.as_bool() else {
                    return;
                };
                let current = json_at(&l.config, property).map(|v| v.get("value").unwrap_or(v));
                if current.and_then(Value::as_bool).unwrap_or(true) == visible {
                    return;
                }
                if let Some(slot) = property
                    .rsplit_once('/')
                    .and_then(|(parent, _)| json_at_mut(&mut l.config, parent))
                    .and_then(Value::as_object_mut)
                {
                    slot.insert("visible".to_owned(), visible.into());
                }
                self.edits.push(SceneEdit::Set {
                    id: layer,
                    key: property.to_owned(),
                    value: visible.into(),
                });
            }
            MaterialPath::Constant { effect, pass, name } => {
                let Some(values) = floats(value) else { return };
                let key = (effect, pass, name);
                if l.constants.get(&key) != Some(&values) {
                    l.constants.insert(key, values);
                    l.dirty_constants = true;
                }
            }
        }
    }

    /// `transformAttachmentToTexture(layer, attachment)` on `layer`: where an
    /// attachment of another (puppet) layer is in `layer`'s texture, as a 2D
    /// affine matrix from attachment space to UV.
    fn attachment_to_texture(&self, layer: i32, args: &[ScriptValue]) -> Option<ScriptValue> {
        let other = args.first()?.as_f64()? as i32;
        let name = match args.get(1)? {
            ScriptValue::String(s) => s.clone(),
            v => v.as_f64()?.to_string(),
        };
        let this = self.layer(layer)?;
        let pose = {
            let bones = self.bones.borrow();
            let puppet = bones.puppets.get(&other)?;
            puppet.attachment_pose(puppet.attachment(&name)?)?
        };
        let other_model = self.layer(other)?.motion_now().transform_at(0.0);
        let to_local = this.motion_now().transform_at(0.0).inverse() * other_model * pose;
        // Layer space (pixels, Y up, centred) to UV (0..1, Y down).
        let size = this.size.max(Vec2::ONE);
        let to_uv = glam::Mat3::from_cols(
            Vec3::new(1.0 / size.x, 0.0, 0.0),
            Vec3::new(0.0, -1.0 / size.y, 0.0),
            Vec3::new(0.5, 0.5, 1.0),
        );
        let affine = glam::Mat3::from_cols(
            to_local.x_axis.truncate().truncate().extend(0.0),
            to_local.y_axis.truncate().truncate().extend(0.0),
            to_local.w_axis.truncate().truncate().extend(1.0),
        );
        let m = (to_uv * affine).to_cols_array();
        Some(ScriptValue::Json(
            m.iter().map(|&v| v as f64).collect::<Vec<_>>().into(),
        ))
    }

    /// `rotateObjectSpace(angles)`, `lookAt(center, up)` and
    /// `lookAtYaw(center, up)`: change the layer's angles.
    fn orient(&mut self, layer: i32, method: &str, args: &[ScriptValue]) {
        let Some(l) = self.layer_mut(layer) else {
            return;
        };
        let Some(v) = args.first().and_then(to_vec3) else {
            return;
        };
        let angles = match method {
            "rotateObjectSpace" => euler_degrees(rotation_of(l.angles) * rotation_of(v)),
            _ => {
                let up = args.get(1).and_then(to_vec3).unwrap_or(Vec3::Y);
                let dir = v - l.origin;
                if dir.length_squared() < 1e-12 {
                    return;
                }
                if method == "lookAtYaw" {
                    let yaw = dir.x.atan2(dir.z).to_degrees();
                    Vec3::new(l.angles.x, yaw, l.angles.z)
                } else {
                    // The layer's +Z faces the target, its +Y as close to `up` as can be.
                    let z = dir.normalize();
                    let x = up.cross(z).try_normalize().unwrap_or(Vec3::X);
                    let y = z.cross(x);
                    euler_degrees(Quat::from_mat3(&glam::Mat3::from_cols(x, y, z)))
                }
            }
        };
        l.dirty_transform |= angles != l.angles;
        l.touched.transform = true;
        l.angles = angles;
    }

    /// A particle system's `play`, `pause`, `stop`, `isPlaying` and
    /// `emitParticles(count)`.
    fn particle_call(
        &mut self,
        layer: i32,
        method: &str,
        args: &[ScriptValue],
    ) -> Option<ScriptValue> {
        let command = match method {
            "isPlaying" => {
                return Some(ScriptValue::Bool(!self.particles_paused.contains(&layer)));
            }
            "emitParticles" => {
                let count = args.first().and_then(ScriptValue::as_f64).unwrap_or(1.0);
                ParticleCommand::Emit(count.clamp(0.0, 100_000.0) as u32)
            }
            "play" => {
                self.particles_paused.remove(&layer);
                ParticleCommand::Playing(true)
            }
            "pause" => {
                self.particles_paused.insert(layer);
                ParticleCommand::Playing(false)
            }
            "stop" => {
                self.particles_paused.insert(layer);
                ParticleCommand::Stop
            }
            _ => {
                self.unhandled("invoke", &format!("particles.{method}"));
                return None;
            }
        };
        self.particle_commands.push((layer, command));
        None
    }

    /// Remove animation layer `index` of puppet `layer`, as
    /// `destroyAnimationLayer` does.
    pub(super) fn destroy_animation_layer(&mut self, layer: i32, index: usize) {
        self.animation_layers_call(
            layer,
            "destroyAnimationLayer",
            &[ScriptValue::Number(index as f64)],
        );
    }

    /// `getAnimationLayerCount`, `createAnimationLayer` (also
    /// `playSingleAnimation`) and `destroyAnimationLayer` on a puppet.
    fn animation_layers_call(
        &mut self,
        layer: i32,
        method: &str,
        args: &[ScriptValue],
    ) -> Option<ScriptValue> {
        let animations = {
            let bones = self.bones.borrow();
            bones
                .puppets
                .get(&layer)
                .map(|p| p.animations.clone())
                .unwrap_or_default()
        };
        let single = method == "playSingleAnimation";
        let l = self.layer_mut(layer)?;
        let list = l
            .config
            .as_object_mut()?
            .entry("animationlayers")
            .or_insert_with(|| Value::Array(Vec::new()));
        if !list.is_array() {
            *list = Value::Array(Vec::new());
        }
        let list = list.as_array_mut()?;
        let result = match method {
            "getAnimationLayerCount" => return Some(ScriptValue::Number(list.len() as f64)),
            "createAnimationLayer" | "playSingleAnimation" => {
                let (name, mut entry) = match args.first()? {
                    ScriptValue::String(name) => {
                        let id = animations
                            .iter()
                            .find(|(n, _)| n == name)
                            .map(|&(_, id)| id)?;
                        (
                            name.clone(),
                            serde_json::json!({ "animation": id, "name": name }),
                        )
                    }
                    ScriptValue::Json(config) => {
                        let name = config.get("name").and_then(Value::as_str);
                        (name.unwrap_or_default().to_owned(), config.clone())
                    }
                    other => {
                        let id = other.as_f64()? as i32;
                        (String::new(), serde_json::json!({ "animation": id }))
                    }
                };
                if let (Some(ScriptValue::Json(Value::Object(extra))), Some(map)) =
                    (args.get(1), entry.as_object_mut())
                {
                    for (k, v) in extra {
                        map.insert(k.clone(), v.clone());
                    }
                }
                let index = list.len();
                list.push(entry);
                if single {
                    self.single_layers.push((layer, index));
                }
                let key = if name.is_empty() {
                    index.to_string()
                } else {
                    name
                };
                ScriptValue::String(key)
            }
            "destroyAnimationLayer" => {
                let index = match args.first()? {
                    ScriptValue::String(key) => key.parse().ok().or_else(|| {
                        list.iter()
                            .position(|e| e.get("name").and_then(Value::as_str) == Some(key))
                    }),
                    v => v.as_f64().map(|i| i as usize),
                }?;
                if index >= list.len() {
                    return Some(ScriptValue::Bool(false));
                }
                list.remove(index);
                self.clocks.borrow_mut().remove_layer_clock(layer, index);
                self.single_layers
                    .retain(|&(l, i)| (l, i) != (layer, index));
                for (l, i) in &mut self.single_layers {
                    if *l == layer && *i > index {
                        *i -= 1;
                    }
                }
                ScriptValue::Bool(true)
            }
            _ => return None,
        };
        let value = self.layer(layer)?.config["animationlayers"].clone();
        self.edits.push(SceneEdit::Set {
            id: layer,
            key: "animationlayers".to_owned(),
            value,
        });
        Some(result)
    }

    /// An effect's `materialCount` and `executeMaterialFunction(name)`.
    fn effect_call(
        &mut self,
        layer: i32,
        effect: usize,
        method: &str,
        args: &[ScriptValue],
    ) -> Option<ScriptValue> {
        let l = self.layer(layer)?;
        match method {
            "materialCount" => {
                let count = l
                    .steps
                    .passes
                    .iter()
                    .filter(|p| p.effect == Some(effect))
                    .map(|p| p.pass + 1)
                    .max()
                    .unwrap_or(1);
                Some(ScriptValue::Number(count as f64))
            }
            "executeMaterialFunction" => {
                let ScriptValue::String(name) = args.first()? else {
                    return None;
                };
                let targets = l.steps.functions.get(&(effect, name.clone()))?.clone();
                self.clears.extend(targets);
                None
            }
            _ => {
                self.unhandled("invoke", &format!("effect.{method}"));
                None
            }
        }
    }

    fn is_particles(&self, layer: i32) -> bool {
        self.layer(layer)
            .is_some_and(|l| l.info.kind == LayerKind::Particle)
    }

    /// A particle layer's `instance.<key>` (`rate`, `count`, `colorn`,
    /// `controlpoint0`, ...); `None` leaves the script its default.
    fn particle_get(&self, layer: i32, key: &str) -> Option<ScriptValue> {
        if !self.is_particles(layer) {
            self.unhandled("get", &format!("instanceoverride/{key}"));
            return None;
        }
        let v = self
            .layer(layer)?
            .config
            .get("instanceoverride")?
            .get(key)?;
        let v = v.get("value").unwrap_or(v);
        Some(match config_numbers(v)[..] {
            [x] => ScriptValue::Number(x),
            [x, y, z, ..] => ScriptValue::Vec3([x, y, z]),
            _ => return None,
        })
    }

    fn particle_set(&mut self, layer: i32, key: &str, value: &ScriptValue) {
        if !self.is_particles(layer) {
            self.unhandled("set", &format!("instanceoverride/{key}"));
            return;
        }
        let json = scene_json(value);
        let Some(l) = self.layer_mut(layer) else {
            return;
        };
        let Some(config) = l.config.as_object_mut() else {
            return;
        };
        let over = config
            .entry("instanceoverride")
            .or_insert_with(|| Value::Object(Default::default()));
        if !over.is_object() {
            *over = Value::Object(Default::default());
        }
        if over.get(key) == Some(&json) {
            return;
        }
        over[key] = json;
        let over = over.clone();
        self.particle_overrides.insert(layer, over.clone());
        self.particle_commands
            .push((layer, ParticleCommand::Override(over)));
    }

    /// Index of a puppet animation layer named or numbered `key`.
    fn animation_layer_index(&self, layer: i32, key: &str) -> Option<usize> {
        if let Ok(index) = key.parse() {
            return Some(index);
        }
        self.layer(layer)?
            .config
            .get("animationlayers")?
            .as_array()?
            .iter()
            .position(|l| l.get("name").and_then(Value::as_str) == Some(key))
    }

    /// Make sure `layer` has a clock for its animation `name` defined under
    /// `scope` of its configuration (`effects/<i>/...`, `material/...`, or the
    /// whole object). Animations of parts that aren't drawn yet (an effect
    /// that starts hidden and a script is about to show) get their clock here;
    /// the rebuild keeps it. Returns the clock's key: without a `name`, the
    /// key of the animation found (named or not).
    fn ensure_clock(&mut self, layer: i32, scope: &str, name: &str) -> String {
        let key = clock::animation_key(name);
        if self.clocks.borrow().get(layer, &key).is_some() {
            return key;
        }
        let Some(config) = self.layer(layer).map(|l| &l.config) else {
            return key;
        };
        let mut root = config;
        let mut parts = scope.split('/').filter(|p| !p.is_empty()).peekable();
        // `effects/<i>` narrows to that effect; deeper parts are searched.
        if parts.peek() == Some(&"effects")
            && let (Some(_), Some(index)) = (parts.next(), parts.next())
            && let Some(effect) = index
                .parse::<usize>()
                .ok()
                .and_then(|i| config.get("effects")?.get(i))
        {
            root = effect;
        }
        let Some(animation) = find_animation(root, name) else {
            return key;
        };
        // The key the frame's binding uses for this animation.
        let key = clock::animation_key(&animation.name);
        self.clocks.borrow_mut().register(
            layer,
            key.clone(),
            clock::Clock::for_animation(&animation),
        );
        key
    }

    /// An animation's playback members (`play`, `getFrame`, `rate`, ...) on
    /// the clock `key` of `layer`.
    fn clock_call(
        &mut self,
        layer: i32,
        key: Option<String>,
        target: &str,
        method: &str,
        args: &[ScriptValue],
    ) -> Option<ScriptValue> {
        let mut clocks = self.clocks.borrow_mut();
        // `thisScene.getAnimation(name)`: whichever layer has it.
        let layer = match (&key, layer) {
            (Some(k), -1) => clocks.find_layer(k).unwrap_or(-1),
            _ => layer,
        };
        let time = self.time as f32;
        let Some(clock) = key.as_deref().and_then(|k| clocks.get_mut(layer, k)) else {
            tracing::debug!(
                "layer {layer}: no clock {key:?}; it has {:?}",
                clocks.layer_keys(layer)
            );
            drop(clocks);
            self.unhandled("invoke", &format!("{target}.{method} (no such animation)"));
            return None;
        };
        let arg = || args.first().and_then(ScriptValue::as_f64).map(|v| v as f32);
        let number = |v: f32| Some(ScriptValue::Number(v as f64));
        match method {
            "play" => {
                if clock.single && clock.frames > 0.0 && clock.frame >= clock.frames {
                    clock.frame = 0.0;
                }
                clock.playing = true;
            }
            "pause" => clock.playing = false,
            "stop" => clock.stop(),
            // Back in step with the scene's time.
            "join" => {
                clock.frame = time * clock.fps;
                clock.playing = true;
            }
            "isPlaying" => return Some(ScriptValue::Bool(clock.playing)),
            "getFrame" => return number(clock.current_frame().floor()),
            "setFrame" => clock.frame = arg()?.max(0.0),
            "frameCount" => return number(clock.frames),
            "fps" => return number(clock.fps),
            "duration" => {
                return number(if clock.fps > 0.0 {
                    clock.frames / clock.fps
                } else {
                    0.0
                });
            }
            "rate" => return number(clock.rate),
            "setRate" => clock.rate = arg()?,
            "blend" => return number(clock.blend),
            "setBlend" => clock.blend = arg()?.clamp(0.0, 1.0),
            "visible" => return Some(ScriptValue::Bool(clock.visible)),
            "setVisible" => clock.visible = args.first()?.as_bool()?,
            _ => {
                drop(clocks);
                self.unhandled("invoke", &format!("{target}.{method}"));
            }
        }
        None
    }

    /// A sound layer's `play`, `pause`, `stop` or `isPlaying`.
    fn sound_call(&mut self, layer: i32, method: &str) -> Option<ScriptValue> {
        let sound = self.sounds.get_mut(&layer)?;
        let (playing, command) = match method {
            "isPlaying" => return Some(ScriptValue::Bool(sound.playing)),
            "play" => (true, SoundCommand::Play),
            "pause" => (false, SoundCommand::Pause),
            "stop" => (false, SoundCommand::Stop),
            _ => {
                self.unhandled("invoke", &format!("sound.{method}"));
                return None;
            }
        };
        sound.playing = playing;
        self.sound_commands.push((layer, command));
        None
    }

    /// Re-evaluate every layer's `ancestors_visible` from its groups'
    /// current visibility (hidden groups hide everything under them).
    pub fn update_ancestors(&mut self) {
        let visible: Vec<bool> = (0..self.layers.len())
            .map(|i| {
                let mut parent = self.layers[i].info.parent;
                for _ in 0..64 {
                    let Some(p) = parent.and_then(|p| self.layer(p)) else {
                        return true;
                    };
                    if p.info.kind == LayerKind::Group && !p.visible {
                        return false;
                    }
                    parent = p.info.parent;
                }
                true
            })
            .collect();
        for (l, visible) in self.layers.iter_mut().zip(visible) {
            if l.ancestors_visible != visible {
                l.ancestors_visible = visible;
                l.dirty_visible = true;
            }
        }
    }

    pub fn unhandled(&self, kind: &str, what: &str) {
        if let Some(audit) = &self.audit {
            audit.count(kind, what);
        }
    }

    /// Write a layout property into the layer's configuration, queueing a
    /// rebuild when it changes.
    fn layout_set(&mut self, id: i32, property: &str, value: &ScriptValue) {
        let json = scene_json(value);
        let Some(l) = self.layer_mut(id) else {
            return;
        };
        if config_value(&l.config, property) == Some(&json) {
            return;
        }
        if let Some(map) = l.config.as_object_mut() {
            map.insert(property.to_owned(), json.clone());
        }
        self.edits.push(SceneEdit::Set {
            id,
            key: property.to_owned(),
            value: json,
        });
    }
}

/// A `getVideoTexture()` member; `None` lets the script see its default.
fn video_call(video: &VideoStream, method: &str, args: &[ScriptValue]) -> Option<ScriptValue> {
    let arg = || args.first().and_then(ScriptValue::as_f64);
    Some(match method {
        "exists" => ScriptValue::Bool(true),
        "play" => {
            video.set_paused(false);
            return None;
        }
        "pause" => {
            video.set_paused(true);
            return None;
        }
        "stop" => {
            video.stop();
            return None;
        }
        "setCurrentTime" => {
            video.seek(arg()?);
            return None;
        }
        "setRate" => {
            video.set_rate(arg()?);
            return None;
        }
        "setLoop" => {
            video.set_looping(args.first()?.as_bool()?);
            return None;
        }
        "isPlaying" => ScriptValue::Bool(video.is_playing()),
        "getCurrentTime" => ScriptValue::Number(video.position()),
        "duration" => ScriptValue::Number(video.duration()),
        "rate" => ScriptValue::Number(video.rate()),
        "loop" => ScriptValue::Bool(video.looping()),
        _ => return None,
    })
}

impl SceneHost for ScriptHost {
    fn layers(&self) -> Vec<LayerInfo> {
        self.layers.iter().map(|l| l.info.clone()).collect()
    }

    fn get(&self, layer: i32, property: &str) -> Option<ScriptValue> {
        if let Some(path) = material_path(property) {
            return self.material_get(layer, property, path);
        }
        if let Some(key) = property.strip_prefix("instanceoverride/") {
            return self.particle_get(layer, key);
        }
        if property == "volume"
            && let Some(sound) = self.sounds.get(&layer)
        {
            return Some(ScriptValue::Number(sound.volume as f64));
        }
        let l = self.layer(layer)?;
        Some(match property {
            "visible" => ScriptValue::Bool(l.visible),
            "solid" => ScriptValue::Bool(l.solid),
            "origin" => vec3(l.origin),
            "scale" => vec3(l.scale),
            "angles" => vec3(l.angles),
            "alpha" | "opacity" => ScriptValue::Number(l.alpha as f64),
            "color" => vec3(l.color),
            "size" => ScriptValue::Vec2([l.size.x as f64, l.size.y as f64]),
            "name" => ScriptValue::String(l.info.name.clone()),
            "text" => ScriptValue::String(l.text.clone()?),
            "__config" => ScriptValue::Json(l.initial_config.clone()),
            "transform" => {
                let m = l.motion_now().transform_at(0.0).to_cols_array();
                ScriptValue::Json(m.iter().map(|&v| v as f64).collect::<Vec<_>>().into())
            }
            "worldOrigin" | "worldScale" => {
                let (scale, _, translation) = l
                    .motion_now()
                    .transform_at(0.0)
                    .to_scale_rotation_translation();
                vec3(if property == "worldOrigin" {
                    translation
                } else {
                    scale
                })
            }
            _ => match layout_get(&l.config, property) {
                Some(v) => v,
                None => {
                    self.unhandled("get", property);
                    return None;
                }
            },
        })
    }

    fn set(&mut self, layer: i32, property: &str, value: ScriptValue) {
        if let Some(path) = material_path(property) {
            self.material_set(layer, property, path, &value);
            return;
        }
        if let Some(key) = property.strip_prefix("instanceoverride/") {
            self.particle_set(layer, key, &value);
            return;
        }
        if property == "volume"
            && let (Some(sound), Some(v)) = (self.sounds.get_mut(&layer), value.as_f64())
        {
            let v = v.clamp(0.0, 1.0) as f32;
            if sound.volume != v {
                sound.volume = v;
                self.sound_commands.push((layer, SoundCommand::Volume(v)));
            }
            return;
        }
        if property == "solid" {
            // Input only: no rebuild.
            if let (Some(l), ScriptValue::Bool(solid)) = (self.layer_mut(layer), &value) {
                l.solid = *solid;
            }
            return;
        }
        if layout_default(property).is_some() {
            self.layout_set(layer, property, &value);
            return;
        }
        if !matches!(
            property,
            "visible" | "origin" | "scale" | "angles" | "alpha" | "opacity" | "color" | "text"
        ) {
            self.unhandled("set", property);
            return;
        }
        let Some(l) = self.layer_mut(layer) else {
            return;
        };
        match property {
            "visible" => {
                if let Some(v) = value.as_bool() {
                    let changed = v != l.visible;
                    l.dirty_visible |= changed;
                    l.touched.visible = true;
                    l.visible = v;
                    if changed && l.info.kind == LayerKind::Group {
                        self.groups_dirty = true;
                    }
                }
            }
            "origin" | "scale" | "angles" => {
                let Some(v) = to_vec3(&value) else { return };
                let slot = match property {
                    "origin" => &mut l.origin,
                    "scale" => &mut l.scale,
                    _ => &mut l.angles,
                };
                l.dirty_transform |= *slot != v;
                l.touched.transform = true;
                *slot = v;
            }
            "alpha" | "opacity" => {
                if let Some(v) = value.as_f64() {
                    l.dirty_color |= v as f32 != l.alpha;
                    l.touched.color = true;
                    l.alpha = v as f32;
                }
            }
            "color" => {
                if let Some(v) = to_vec3(&value) {
                    l.dirty_color |= v != l.color;
                    l.touched.color = true;
                    l.color = v;
                }
            }
            "text" => {
                let text = match &value {
                    ScriptValue::String(s) => s.clone(),
                    ScriptValue::Number(n) => n.to_string(),
                    _ => return,
                };
                if l.text.as_deref() != Some(text.as_str()) {
                    l.text = Some(text);
                    l.touched.text = true;
                    l.dirty_text = true;
                }
            }
            _ => {}
        }
    }

    fn invoke(
        &mut self,
        layer: i32,
        target: &str,
        method: &str,
        args: &[ScriptValue],
    ) -> Option<ScriptValue> {
        match target {
            "video" => {
                tracing::trace!("layer {layer} video.{method}({args:?})");
                video_call(self.videos.get(&layer)?, method, args)
            }
            "" if self.sounds.contains_key(&layer) => self.sound_call(layer, method),
            "" if method == "transformAttachmentToTexture" => {
                self.attachment_to_texture(layer, args)
            }
            "" if matches!(method, "rotateObjectSpace" | "lookAt" | "lookAtYaw") => {
                self.orient(layer, method, args);
                None
            }
            "" if self.is_particles(layer) => self.particle_call(layer, method, args),
            "" if method.contains("AnimationLayer") => {
                self.animation_layers_call(layer, method, args)
            }
            "" if self.bones.borrow().puppets.contains_key(&layer) => {
                self.bone_call(layer, method, args)
            }
            _ if target.starts_with("effect:") => {
                let effect: usize = target["effect:".len()..].parse().ok()?;
                self.effect_call(layer, effect, method, args)
            }
            "textureAnimation" | "animation" => {
                let key = match target {
                    "animation" => self.clocks.borrow().first_animation(layer),
                    _ => Some(clock::TEXTURE_ANIMATION.to_owned()),
                };
                self.clock_call(layer, key, target, method, args)
            }
            _ if target.contains("animation:") => {
                // `animation:<name>`, also under `material/` or `effects/<i>/`.
                let at = target.find("animation:").unwrap();
                let name = &target[at + "animation:".len()..];
                let key = self.ensure_clock(layer, &target[..at], name);
                self.clock_call(layer, Some(key), target, method, args)
            }
            _ if target.starts_with("animationLayer:") => {
                let key = self.animation_layer_index(layer, &target["animationLayer:".len()..]);
                self.clock_call(layer, key.map(clock::layer_key), target, method, args)
            }
            _ => {
                self.unhandled("invoke", &format!("{target}.{method}"));
                None
            }
        }
    }

    fn create_layer(&mut self, config: &Value) -> Option<i32> {
        let mut config = new_layer_config(config)?;
        let id = self.next_id;
        config.as_object_mut()?.insert("id".to_owned(), id.into());
        let obj: SceneObject = serde_json::from_value(config.clone())
            .inspect_err(|e| tracing::warn!("createLayer {config}: {e}"))
            .ok()?;
        let state = LayerState::new(&obj, config.clone(), None, LayerSteps::default())?;
        self.push(state);
        self.edits.push(SceneEdit::Create { config });
        Some(id)
    }

    fn destroy_layer(&mut self, layer: i32) {
        let Some(&index) = self.by_id.get(&layer) else {
            return;
        };
        self.layers.remove(index);
        self.videos.remove(&layer);
        self.reindex();
        self.edits.push(SceneEdit::Destroy { id: layer });
    }

    fn sort_layer(&mut self, layer: i32, index: usize) {
        let Some(&from) = self.by_id.get(&layer) else {
            return;
        };
        let state = self.layers.remove(from);
        let to = index.min(self.layers.len());
        self.layers.insert(to, state);
        self.reindex();
        if from != to {
            let before = self.layers.get(to + 1).map(|l| l.info.id);
            self.edits.push(SceneEdit::Move { id: layer, before });
        }
    }

    fn set_parent(
        &mut self,
        layer: i32,
        parent: Option<i32>,
        attachment: Option<&str>,
        adjust: bool,
    ) {
        let new_parent_world = parent
            .and_then(|p| self.layer(p))
            .map_or(Mat4::IDENTITY, |p| p.motion_now().transform_at(0.0));
        let Some(l) = self.layer_mut(layer) else {
            return;
        };
        let current = l.config.get("attachment").and_then(Value::as_str);
        if l.info.parent == parent && current == attachment {
            return;
        }
        if adjust {
            // Keep the layer where it is in the world under its new parent.
            let world = l.motion_now().transform_at(0.0);
            let local = new_parent_world.inverse() * world;
            let (scale, rotation, origin) = local.to_scale_rotation_translation();
            (l.origin, l.scale, l.angles) = (origin, scale, euler_degrees(rotation));
            l.touched.transform = true;
            l.dirty_transform = true;
        }
        l.info.parent = parent;
        if let Some(map) = l.config.as_object_mut() {
            match parent {
                Some(p) => map.insert("parent".to_owned(), p.into()),
                None => map.remove("parent"),
            };
            match attachment {
                Some(a) => map.insert("attachment".to_owned(), a.into()),
                None => map.remove("attachment"),
            };
        }
        self.edits.push(SceneEdit::Parent { id: layer, parent });
        self.edits.push(SceneEdit::Set {
            id: layer,
            key: "attachment".to_owned(),
            value: attachment.map_or(Value::Null, Value::from),
        });
    }

    fn model_data(&mut self, op: &str, id: i32, data: &Value) -> Option<i32> {
        let result = self.model_data.borrow_mut().call(op, id, data);
        if op == "replace" {
            // New layouts or materials need the layers drawing it rebuilt.
            let model = Value::from(format!("{}{id}", crate::model_data::PREFIX));
            let users: Vec<i32> = self
                .layers
                .iter()
                .filter(|l| l.config.get("model") == Some(&model))
                .map(|l| l.info.id)
                .collect();
            for layer in users {
                self.edits.push(SceneEdit::Set {
                    id: layer,
                    key: "model".to_owned(),
                    value: model.clone(),
                });
            }
        }
        result
    }

    fn open_user_shortcut(&mut self, name: &str) -> bool {
        // Only shortcuts the user bound (through the settings) open; a
        // wallpaper's own defaults never launch anything.
        let Some(shortcut) = self.user_shortcuts.get(name) else {
            return false;
        };
        let target = shortcut
            .get("file")
            .and_then(Value::as_str)
            .or_else(|| shortcut.as_str())
            .filter(|f| !f.is_empty());
        let bound = shortcut
            .get("isbound")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let Some(target) = target.filter(|_| bound) else {
            return false;
        };
        std::process::Command::new("xdg-open")
            .arg(target)
            .spawn()
            .inspect_err(|e| tracing::warn!("opening {target}: {e}"))
            .is_ok()
    }

    fn hit_box(&self, layer: i32, world: [f32; 2]) -> Option<String> {
        let model = self.layer(layer)?.motion_now().transform_at(0.0);
        let bones = self.bones.borrow();
        let puppet = bones.puppets.get(&layer)?;
        let local = model
            .inverse()
            .transform_point3(Vec2::from_array(world).extend(0.0));
        let bone = puppet.nearest(local)?;
        Some(match puppet.names[bone].as_str() {
            "" => bone.to_string(),
            name => name.to_owned(),
        })
    }

    fn scene_get(&self, property: &str) -> Option<ScriptValue> {
        if property == "cameratransforms"
            && let Some(view) = self.view3d
        {
            let [eye, center, up] = self.camera3d.unwrap_or(view);
            return Some(ScriptValue::Json(serde_json::json!({
                "eye": eye.to_array(),
                "center": center.to_array(),
                "up": up.to_array(),
                "zoom": 1.0,
            })));
        }
        if property == "cameratransforms" {
            let half = Vec2::from_array(self.canvas) / 2.0;
            let (center, zoom) = self.camera.unwrap_or((half, 1.0));
            return Some(ScriptValue::Json(serde_json::json!({
                "eye": [center.x, center.y, 1.0],
                "center": [center.x, center.y, 0.0],
                "up": [0.0, 1.0, 0.0],
                "zoom": zoom,
            })));
        }
        let Some(v) = self.general.get(property) else {
            self.unhandled("scene_get", property);
            return None;
        };
        let v = v.get("value").unwrap_or(v);
        Some(ScriptValue::from_scene_json(property, v))
    }

    fn scene_set(&mut self, property: &str, value: ScriptValue) {
        if property == "cameratransforms"
            && let (Some(view), ScriptValue::Json(t)) = (self.view3d, &value)
        {
            // A 3D scene's camera: where it is, looks at and which way is up.
            let vector = |key: &str, default: Vec3| -> Vec3 {
                let Some(v) = t.get(key) else { return default };
                let n = |k: &str| v.get(k).and_then(Value::as_f64);
                match (n("x"), n("y"), n("z")) {
                    (Some(x), Some(y), Some(z)) => Vec3::new(x as f32, y as f32, z as f32),
                    _ => match config_numbers(v)[..] {
                        [x, y, z, ..] => Vec3::new(x as f32, y as f32, z as f32),
                        _ => default,
                    },
                }
            };
            self.camera3d = Some([
                vector("eye", view[0]),
                vector("center", view[1]),
                vector("up", view[2]),
            ]);
            return;
        }
        if property == "cameratransforms" {
            // A flat scene's camera: where it looks and how far it zooms in.
            let ScriptValue::Json(t) = &value else { return };
            let vector = |key: &str| -> Option<Vec2> {
                let v = t.get(key)?;
                let n = |k: &str| v.get(k).and_then(Value::as_f64);
                match (n("x"), n("y")) {
                    (Some(x), Some(y)) => Some(Vec2::new(x as f32, y as f32)),
                    _ => match config_numbers(v)[..] {
                        [x, y, ..] => Some(Vec2::new(x as f32, y as f32)),
                        _ => None,
                    },
                }
            };
            let half = Vec2::from_array(self.canvas) / 2.0;
            let center = vector("center").unwrap_or(half);
            let zoom = t.get("zoom").and_then(Value::as_f64).unwrap_or(1.0) as f32;
            self.camera = Some((center, zoom.max(1e-3)));
            return;
        }
        let json = scene_json(&value);
        let current = self
            .general
            .get(property)
            .map(|v| v.get("value").unwrap_or(v));
        if current == Some(&json) {
            return;
        }
        if let Some(general) = self.general.as_object_mut() {
            general.insert(property.to_owned(), json.clone());
        }
        self.edits.push(SceneEdit::General {
            key: property.to_owned(),
            value: json,
        });
    }

    fn storage_changed(&mut self, data: &Value) {
        self.storage = Some(data.clone());
    }

    fn canvas_size(&self) -> [f32; 2] {
        self.canvas
    }

    fn screen_resolution(&self) -> [f32; 2] {
        self.screen.unwrap_or(self.canvas)
    }

    fn audio_spectrum(&self, left: &mut [f32], right: &mut [f32]) {
        let Some(s) = &self.spectrum else { return };
        let (l, r): (&[f32], &[f32]) = match left.len() {
            16 => (&s.left16, &s.right16),
            32 => (&s.left32, &s.right32),
            _ => (&s.left64, &s.right64),
        };
        for (dst, src) in left.iter_mut().zip(l) {
            *dst = *src;
        }
        for (dst, src) in right.iter_mut().zip(r) {
            *dst = *src;
        }
    }

    fn log(&mut self, level: LogLevel, message: &str) {
        if level == LogLevel::Error {
            // "layer 12 text: update: TypeError: ..." -> the error itself.
            let error = message.find("Error").map_or(message, |at| {
                &message[message[..at].rfind(' ').map_or(0, |s| s + 1)..]
            });
            let error: String = error.chars().take(90).collect();
            self.unhandled("error", &error);
        }
        match level {
            LogLevel::Error => tracing::warn!("script: {message}"),
            LogLevel::Warn => tracing::info!("script: {message}"),
            LogLevel::Info => tracing::debug!("script: {message}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn material_paths_parse() {
        assert_eq!(
            material_path("effects/2/visible"),
            Some(MaterialPath::EffectVisible(2))
        );
        assert_eq!(
            material_path("effects/1/passes/0/constantshadervalues/alpha"),
            Some(MaterialPath::Constant {
                effect: Some(1),
                pass: Some(0),
                name: "alpha".into()
            })
        );
        assert_eq!(
            material_path("material/Brightness"),
            Some(MaterialPath::Constant {
                effect: None,
                pass: None,
                name: "Brightness".into()
            })
        );
        assert_eq!(material_path("origin"), None);
    }

    #[test]
    fn animations_of_unbuilt_parts_are_found_in_the_configuration() {
        let anim = |name: &str| {
            json!({"c0": [{"frame": 0, "value": 1}, {"frame": 15, "value": 0}],
                   "options": {"fps": 15, "length": 15, "mode": "single", "name": name}})
        };
        let effect = json!({"passes": [{"constantshadervalues": {
            "multiply": {"value": 0, "animation": anim("")},
            "fade": {"value": 1, "animation": anim("fade")},
        }}]});
        assert!(find_animation(&effect, "").is_some());
        assert_eq!(find_animation(&effect, "fade").unwrap().name, "fade");
        assert!(find_animation(&effect, "missing").is_none());
    }

    #[test]
    fn layout_values_read_from_configuration() {
        let config =
            json!({"alignment": "bottom", "parallaxDepth": {"user": "p", "value": "0.5 0.25"}});
        assert_eq!(
            layout_get(&config, "alignment"),
            Some(ScriptValue::String("bottom".into()))
        );
        assert_eq!(
            layout_get(&config, "parallaxDepth"),
            Some(ScriptValue::Vec2([0.5, 0.25]))
        );
        assert_eq!(
            layout_get(&config, "perspective"),
            Some(ScriptValue::Bool(false))
        );
        assert_eq!(layout_get(&config, "origin"), None);
    }

    #[test]
    fn created_layer_configs() {
        assert_eq!(
            new_layer_config(&json!("models/bar.json")),
            Some(json!({"image": "models/bar.json", "name": "models/bar.json"}))
        );
        assert_eq!(
            new_layer_config(&json!({"__path": "particles/rain.json", "__workshop": "7"})),
            Some(
                json!({"particle": "particles/rain.json", "name": "particles/rain.json", "__workshop": "7"})
            )
        );
        assert_eq!(
            new_layer_config(&json!({"id": 5, "image": "models/a.json"})),
            Some(json!({"image": "models/a.json"}))
        );
    }
}
