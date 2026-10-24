//! SceneScript: the scene state scripts read and write, and applying their
//! writes to the renderer.

mod audit;
mod edit;
mod host;

use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec2;
use serde_json::Value;
use wpe_render::{CardMesh, Gpu, GpuTexture, SceneRenderer};
use wpe_scene::ImageLayer;
use wpe_script::{CursorEvent, CursorEventKind, CursorState, ScriptRuntime, ScriptValue};
use wpe_vfs::Vfs;

pub use edit::{SceneEdit, apply as apply_edits, resolve_paths};
pub use host::ParticleCommand;
use host::{LayerState, ScriptHost};

use crate::bones::SharedBones;
use crate::clock::SharedClocks;
use crate::frame::LayerSteps;
use crate::sound::{SoundCommand, SoundState};
use crate::video::VideoStream;

/// A built frame as scripts see it: the scene document and the layers,
/// draws and videos built from it.
pub struct SceneView<'a> {
    pub doc: &'a wpe_assets::Scene,
    /// The document's objects as JSON, for their configurations.
    pub objects: &'a [Value],
    /// The document's `general` settings as JSON.
    pub general: &'a Value,
    pub layers: &'a [ImageLayer],
    pub steps: &'a HashMap<i32, LayerSteps>,
    pub videos: &'a HashMap<i32, Arc<VideoStream>>,
    pub sounds: &'a HashMap<i32, SoundState>,
}

impl SceneView<'_> {
    fn config(&self, id: i32) -> Value {
        self.objects
            .iter()
            .find(|o| o.get("id").and_then(Value::as_i64) == Some(id as i64))
            .cloned()
            .unwrap_or(Value::Null)
    }
}

/// A wallpaper's scripts and the state they drive.
pub struct Scripts {
    runtime: ScriptRuntime,
    host: ScriptHost,
    user: serde_json::Map<String, Value>,
    last_time: f64,
    pressed: bool,
}

/// A scripted value in an object's configuration: its path (`visible`,
/// `instanceoverride/rate`, `effects/0/passes/0/constantshadervalues/alpha`,
/// ...), script, `scriptproperties` and initial value.
struct Scripted<'a> {
    path: String,
    source: &'a str,
    properties: Option<&'a Value>,
    initial: ScriptValue,
}

/// Every scripted value of an object configuration, in file order.
fn scripted(config: &Value) -> Vec<Scripted<'_>> {
    fn walk<'a>(v: &'a Value, path: &str, out: &mut Vec<Scripted<'a>>) {
        match v {
            Value::Object(map) => {
                if let Some(source) = map.get("script").and_then(Value::as_str) {
                    if !path.is_empty() {
                        let value = map.get("value").unwrap_or(&Value::Null);
                        out.push(Scripted {
                            path: path.to_owned(),
                            source,
                            properties: map.get("scriptproperties"),
                            initial: ScriptValue::from_scene_json(path, value),
                        });
                    }
                    return;
                }
                for (key, child) in map {
                    let child_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}/{key}")
                    };
                    walk(child, &child_path, out);
                }
            }
            Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    walk(child, &format!("{path}/{i}"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(config, "", &mut out);
    out
}

impl Scripts {
    /// Load every script of the scene, or `None` when it has none. Scripts'
    /// `init` runs here and may already queue [`Self::take_edits`].
    pub fn load(
        assets: &Vfs,
        project: &wpe_assets::Project,
        view: &SceneView,
        canvas: Vec2,
        clocks: SharedClocks,
        bones: SharedBones,
        storage: Option<Value>,
    ) -> Option<Self> {
        let scripts: Vec<_> = view.objects.iter().map(scripted).collect();
        if scripts.iter().all(Vec::is_empty) {
            return None;
        }
        let base_classes = assets
            .read_to_string("scripts/jsclasses/baseclasses.js")
            .ok();
        let mut runtime = ScriptRuntime::with_options(wpe_script::ScriptOptions {
            base_classes,
            ..Default::default()
        })
        .inspect_err(|e| tracing::warn!("scripts disabled: {e}"))
        .ok()?;
        let user: serde_json::Map<String, Value> = project
            .general
            .properties
            .iter()
            .map(|(k, p)| (k.clone(), p.value.clone()))
            .collect();
        runtime.set_user_properties(&Value::Object(user.clone()));
        if let Some(storage) = &storage {
            runtime.set_local_storage(storage);
        }

        let by_layer: HashMap<i32, &ImageLayer> = view.layers.iter().map(|l| (l.id, l)).collect();
        let mut host = ScriptHost::new(canvas, view.videos.clone());
        host.sounds = view.sounds.clone();
        host.general = view.general.clone();
        host.clocks = clocks;
        host.bones = bones;
        for obj in &view.doc.objects {
            let Some(c) = obj.common() else { continue };
            let steps = view.steps.get(&c.id).cloned().unwrap_or_default();
            let layer = by_layer.get(&c.id).copied();
            if let Some(state) = LayerState::new(obj, view.config(c.id), layer, steps) {
                host.push(state);
            }
        }

        for (config, entries) in view.objects.iter().zip(scripts) {
            let Some(id) = config.get("id").and_then(Value::as_i64) else {
                continue;
            };
            for s in entries {
                if let Err(e) = runtime.add_property_script(
                    &mut host,
                    id as i32,
                    &s.path,
                    s.source,
                    s.properties,
                    s.initial,
                ) {
                    tracing::warn!("script on layer {id} {}: {e}", s.path);
                }
            }
        }
        // As Wallpaper Engine does, `init` runs as the wallpaper loads.
        runtime.init(&mut host);
        Some(Self {
            runtime,
            host,
            user,
            last_time: 0.0,
            pressed: false,
        })
    }

    fn cursor_state(&self, pointer: Vec2) -> CursorState {
        let c = self.host.canvas;
        CursorState {
            world: [pointer.x * c[0], (1.0 - pointer.y) * c[1]],
            screen: [pointer.x * c[0], pointer.y * c[1]],
            left_down: self.pressed,
        }
    }

    /// Run the scripts' frame. Follow with [`Self::take_edits`] and
    /// [`Self::apply`].
    /// `ended_animations`: layers whose animations reached their end, for
    /// their `addEndedCallback` callbacks.
    pub fn update(
        &mut self,
        time: f32,
        pointer: Vec2,
        spectrum: Option<wpe_audio::AudioSpectrum>,
        ended_animations: &[i32],
    ) {
        let time = time as f64;
        let dt = (time - self.last_time).clamp(0.0, 0.25);
        self.last_time = time;
        self.host.spectrum = spectrum;
        let ended: Vec<i32> = self
            .host
            .videos
            .iter()
            .filter(|(_, v)| v.take_ended())
            .map(|(&id, _)| id)
            .chain(ended_animations.iter().copied())
            .collect();
        for id in ended {
            tracing::trace!("layer {id} video ended");
            self.runtime.video_ended(&mut self.host, id);
        }
        let cursor = self.cursor_state(pointer);
        self.runtime.update(&mut self.host, time, dt, Some(cursor));
    }

    /// A user property changed: scripts bound to it and their
    /// `applyUserProperties` see the new value.
    pub fn user_property_changed(&mut self, name: &str, value: Value) {
        self.user.insert(name.to_owned(), value.clone());
        let changed = serde_json::json!({ name: value });
        self.runtime.apply_user_properties(&mut self.host, &changed);
    }

    /// Call `handler(arg)` on every script exporting it (media events, ...).
    pub fn emit(&mut self, handler: &'static str, arg: &Value) {
        self.runtime.emit(&mut self.host, handler, arg);
    }

    /// Structural changes scripts made since the last call; the frame must be
    /// rebuilt from the edited scene and [`Self::rebind`] called.
    pub fn take_edits(&mut self) -> Vec<SceneEdit> {
        std::mem::take(&mut self.host.edits)
    }

    /// `localStorage` contents, when scripts changed them since the last call.
    pub fn take_storage(&mut self) -> Option<Value> {
        self.host.storage.take()
    }

    /// What scripts did to particle systems since the last call.
    pub fn take_particle_commands(&mut self) -> Vec<(i32, ParticleCommand)> {
        std::mem::take(&mut self.host.particle_commands)
    }

    /// What scripts did to sound layers since the last call.
    pub fn take_sound_commands(&mut self) -> Vec<(i32, SoundCommand)> {
        std::mem::take(&mut self.host.sound_commands)
    }

    /// Attach to a rebuilt frame, keeping what scripts have set.
    pub fn rebind(&mut self, view: &SceneView) {
        let by_layer: HashMap<i32, &ImageLayer> = view.layers.iter().map(|l| (l.id, l)).collect();
        for l in &mut self.host.layers {
            let id = l.info.id;
            let steps = view.steps.get(&id).cloned().unwrap_or_default();
            l.rebind(by_layer.get(&id).copied(), steps);
        }
        self.host.videos = view.videos.clone();
        // Rebuilt particle systems start from the scene's overrides.
        for (&layer, over) in &self.host.particle_overrides {
            self.host
                .particle_commands
                .push((layer, ParticleCommand::Override(over.clone())));
        }
    }

    /// Deliver a mouse button press or release at `pointer`.
    pub fn mouse_button(&mut self, pointer: Vec2, down: bool) {
        self.pressed = down;
        let state = self.cursor_state(pointer);
        let world = Vec2::from_array(state.world);
        let hits: Vec<i32> = self
            .runtime
            .interactive_layers()
            .into_iter()
            .rev()
            .filter(|id| self.hit(*id, world))
            .collect();
        let kind = if down {
            CursorEventKind::Down
        } else {
            CursorEventKind::Up
        };
        self.runtime.cursor_event(
            &mut self.host,
            CursorEvent::new(kind, state.world, state.screen, hits),
        );
    }

    fn hit(&self, id: i32, world: Vec2) -> bool {
        let Some(l) = self.host.layer(id) else {
            return false;
        };
        if !l.visible {
            return false;
        }
        let local = l
            .motion_now()
            .transform_at(0.0)
            .inverse()
            .transform_point3(world.extend(0.0));
        local.x.abs() <= l.size.x / 2.0 && local.y.abs() <= l.size.y / 2.0
    }

    /// Push what scripts changed to the renderer.
    pub fn apply(
        &mut self,
        gpu: &Gpu,
        vfs: &Vfs,
        renderer: &mut SceneRenderer,
        mut text: Option<&mut wpe_text::TextRenderer>,
    ) {
        let user = |name: &str| self.user.get(name).cloned();
        for l in &mut self.host.layers {
            if l.dirty_text
                && let (Some(string), Some(obj), Some(text)) = (
                    l.text.as_deref(),
                    l.text_object.as_deref(),
                    text.as_deref_mut(),
                )
            {
                let style = wpe_text::TextStyle::from_object(obj, user);
                match text.render(vfs, &style, string) {
                    Ok(image) => {
                        let texture =
                            Arc::new(upload_rgba(gpu, image.width, image.height, image.pixels));
                        l.size = Vec2::from_array(image.card_size);
                        l.motion.local_offset = Vec2::from_array(image.anchor_offset).extend(0.0);
                        for &step in &l.steps.all {
                            renderer.set_texture(step, 0, texture.clone());
                            renderer.set_mesh(
                                step,
                                CardMesh {
                                    size: l.size,
                                    uv_scale: Vec2::ONE,
                                }
                                .into(),
                            );
                        }
                        l.dirty_transform = true;
                    }
                    Err(e) => tracing::warn!("text {:?}: {e}", l.info.name),
                }
            }
            if l.dirty_visible {
                for &step in &l.steps.all {
                    renderer.set_visible(step, l.visible && l.ancestors_visible);
                }
            }
            if l.dirty_transform {
                let model = l.motion_now().transform_at(0.0);
                for &step in &l.steps.model {
                    renderer.set_model(step, model);
                }
            }
            if l.dirty_constants {
                for ((effect, pass, name), values) in &l.constants {
                    let passes = l
                        .steps
                        .passes
                        .iter()
                        .filter(|p| p.effect == *effect && pass.is_none_or(|j| p.pass == j));
                    for p in passes {
                        let Some(uniform) = p.info.uniform_for_constant(name) else {
                            continue;
                        };
                        let mut v = values.clone();
                        if let Some(default) = p.info.defaults.get(&uniform) {
                            let last = v.last().copied().unwrap_or(0.0);
                            v.resize(default.len().max(1), last);
                        }
                        renderer.set_uniform(p.step, &uniform, v);
                    }
                }
            }
            if l.dirty_color {
                let (c, a) = (l.color, l.alpha);
                for &(step, neutral) in &l.steps.color {
                    if !neutral {
                        renderer.set_uniform(step, "g_Color4", vec![c.x, c.y, c.z, a]);
                    }
                    renderer.set_uniform(step, "g_Color", c.to_array().to_vec());
                    renderer.set_uniform(step, "g_UserAlpha", vec![a]);
                    renderer.set_uniform(step, "g_Alpha", vec![a]);
                }
            }
            l.dirty_constants = false;
            l.dirty_visible = false;
            l.dirty_transform = false;
            l.dirty_color = false;
            l.dirty_text = false;
        }
    }
}

fn upload_rgba(gpu: &Gpu, width: u32, height: u32, pixels: Vec<u8>) -> GpuTexture {
    wpe_render::upload_texture(
        gpu,
        &crate::rgba_texture(width, height, pixels),
        "script text",
    )
}
