//! The [`SceneHost`] scripts run against: what they can read and change of
//! each layer and of the scene's structure.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat4, Vec2, Vec3};
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
        self.dirty_visible = self.touched.visible;
        self.dirty_transform = self.touched.transform;
        self.dirty_color = self.touched.color;
        self.dirty_text = self.touched.text;
    }
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
        "perspective" => ScriptValue::Bool(false),
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
}

/// The [`SceneHost`] scripts run against.
pub struct ScriptHost {
    pub layers: Vec<LayerState>,
    pub by_id: HashMap<i32, usize>,
    pub canvas: [f32; 2],
    pub spectrum: Option<wpe_audio::AudioSpectrum>,
    /// Each layer's video, for `getVideoTexture()`.
    pub videos: HashMap<i32, Arc<VideoStream>>,
    /// Structural changes waiting for the next frame rebuild.
    pub edits: Vec<SceneEdit>,
    /// Id for the next created layer.
    pub next_id: i32,
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
    pub audit: Option<Audit>,
}

impl ScriptHost {
    pub fn new(canvas: Vec2, videos: HashMap<i32, Arc<VideoStream>>) -> Self {
        Self {
            layers: Vec::new(),
            by_id: HashMap::new(),
            canvas: canvas.to_array(),
            spectrum: None,
            videos,
            edits: Vec::new(),
            next_id: 1,
            sounds: HashMap::new(),
            sound_commands: Vec::new(),
            particle_overrides: HashMap::new(),
            particle_commands: Vec::new(),
            clocks: Default::default(),
            bones: Default::default(),
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
        let bone = || {
            let i = args.first()?.as_f64()?;
            (i >= 0.0 && (i as usize) < puppet.names.len()).then_some(i as usize)
        };
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
                    _ => matrix(args.get(1)?)?.w_axis.truncate(),
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
        let Some(clock) = key.and_then(|k| clocks.get_mut(layer, &k)) else {
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
                    l.dirty_visible |= v != l.visible;
                    l.touched.visible = true;
                    l.visible = v;
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
            "" if method == "emitParticles" && self.is_particles(layer) => {
                let count = args.first().and_then(ScriptValue::as_f64).unwrap_or(1.0);
                let count = count.clamp(0.0, 100_000.0) as u32;
                self.particle_commands
                    .push((layer, ParticleCommand::Emit(count)));
                None
            }
            "" if self.bones.borrow().puppets.contains_key(&layer) => {
                self.bone_call(layer, method, args)
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
                let name = &target[target.find("animation:").unwrap() + "animation:".len()..];
                self.clock_call(
                    layer,
                    Some(clock::animation_key(name)),
                    target,
                    method,
                    args,
                )
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

    fn set_parent(&mut self, layer: i32, parent: Option<i32>) {
        let Some(l) = self.layer_mut(layer) else {
            return;
        };
        if l.info.parent == parent {
            return;
        }
        l.info.parent = parent;
        if let Some(map) = l.config.as_object_mut() {
            match parent {
                Some(p) => map.insert("parent".to_owned(), p.into()),
                None => map.remove("parent"),
            };
        }
        self.edits.push(SceneEdit::Parent { id: layer, parent });
    }

    fn scene_get(&self, property: &str) -> Option<ScriptValue> {
        self.unhandled("scene_get", property);
        None
    }

    fn scene_set(&mut self, property: &str, _value: ScriptValue) {
        self.unhandled("scene_set", property);
    }

    fn canvas_size(&self) -> [f32; 2] {
        self.canvas
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
