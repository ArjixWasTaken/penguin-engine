//! SceneScript: the scene state scripts read and write, and applying their
//! writes to the renderer.

mod audit;
mod edit;
mod host;

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Vec2, Vec3};
use serde_json::Value;
use wpe_assets::scene::SceneObject;
use wpe_assets::value::Dynamic;
use wpe_render::{CardMesh, Gpu, GpuTexture, SceneRenderer};
use wpe_scene::{ImageLayer, Motion};
use wpe_script::{CursorEvent, CursorEventKind, CursorState, ScriptRuntime, ScriptValue};
use wpe_vfs::Vfs;

pub use edit::{SceneEdit, apply as apply_edits, resolve_paths};
use host::{LayerState, ScriptHost, vec3};

use crate::frame::LayerSteps;
use crate::sound::{SoundCommand, SoundState};
use crate::video::VideoStream;

/// A built frame as scripts see it: the scene document and the layers,
/// draws and videos built from it.
pub struct SceneView<'a> {
    pub doc: &'a wpe_assets::Scene,
    /// The document's objects as JSON, for their configurations.
    pub objects: &'a [Value],
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

/// A scripted property: name, script source, scriptproperties, initial value.
type Scripted<'a> = (
    &'static str,
    Option<&'a str>,
    Option<&'a Value>,
    ScriptValue,
);

/// Every scripted value of an object.
fn scripted(obj: &SceneObject) -> Vec<Scripted<'_>> {
    fn entry<'a, T>(
        property: &'static str,
        d: &'a Dynamic<T>,
        value: ScriptValue,
    ) -> Option<Scripted<'a>> {
        let script = d.script.as_deref()?;
        Some((property, Some(script), d.script_properties.as_ref(), value))
    }
    let Some(c) = obj.common() else {
        return Vec::new();
    };
    let v3 = |v: &wpe_assets::value::Vec3| vec3(Vec3::from_array(**v));
    let mut out = vec![
        entry("visible", &c.visible, ScriptValue::Bool(c.visible.value)),
        entry("origin", &c.origin, v3(&c.origin.value)),
        entry("scale", &c.scale, v3(&c.scale.value)),
        entry("angles", &c.angles, v3(&c.angles.value)),
    ];
    match obj {
        SceneObject::Image(i) => {
            out.push(entry(
                "alpha",
                &i.alpha,
                ScriptValue::Number(i.alpha.value as f64),
            ));
            out.push(entry("color", &i.color, v3(&i.color.value)));
        }
        SceneObject::Text(t) => {
            out.push(entry(
                "text",
                &t.text,
                ScriptValue::String(t.text.value.clone()),
            ));
            out.push(entry(
                "alpha",
                &t.alpha,
                ScriptValue::Number(t.alpha.value as f64),
            ));
            out.push(entry("color", &t.color, v3(&t.color.value)));
        }
        _ => {}
    }
    out.into_iter().flatten().collect()
}

impl Scripts {
    /// Load every script of the scene, or `None` when it has none. Scripts'
    /// `init` runs here and may already queue [`Self::take_edits`].
    pub fn load(
        assets: &Vfs,
        project: &wpe_assets::Project,
        view: &SceneView,
        canvas: Vec2,
    ) -> Option<Self> {
        let scripts: Vec<_> = view.doc.objects.iter().map(scripted).collect();
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

        let by_layer: HashMap<i32, &ImageLayer> = view.layers.iter().map(|l| (l.id, l)).collect();
        let mut host = ScriptHost::new(canvas, view.videos.clone());
        host.sounds = view.sounds.clone();
        for obj in &view.doc.objects {
            let Some(c) = obj.common() else { continue };
            let steps = view.steps.get(&c.id).cloned().unwrap_or_default();
            let layer = by_layer.get(&c.id).copied();
            if let Some(state) = LayerState::new(obj, view.config(c.id), layer, steps) {
                host.push(state);
            }
        }

        for (obj, entries) in view.doc.objects.iter().zip(scripts) {
            let Some(c) = obj.common() else { continue };
            for (property, source, props, initial) in entries {
                let Some(source) = source else { continue };
                if let Err(e) =
                    runtime.add_property_script(&mut host, c.id, property, source, props, initial)
                {
                    tracing::warn!("script on {:?}.{property}: {e}", c.name);
                }
            }
        }
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
    pub fn update(&mut self, time: f32, pointer: Vec2, spectrum: Option<wpe_audio::AudioSpectrum>) {
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
            .collect();
        for id in ended {
            tracing::trace!("layer {id} video ended");
            self.runtime.video_ended(&mut self.host, id);
        }
        let cursor = self.cursor_state(pointer);
        self.runtime.update(&mut self.host, time, dt, Some(cursor));
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
        let local = layer_motion(l)
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
                let model = layer_motion(l).transform_at(0.0);
                for &step in &l.steps.model {
                    renderer.set_model(step, model);
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
            l.dirty_visible = false;
            l.dirty_transform = false;
            l.dirty_color = false;
            l.dirty_text = false;
        }
    }
}

/// The layer's transform with script-set origin, scale and angles.
fn layer_motion(l: &LayerState) -> Motion {
    Motion {
        origin: l.origin,
        scale: l.scale,
        angles: l.angles,
        origin_animation: None,
        scale_animation: None,
        angles_animation: None,
        ..l.motion.clone()
    }
}

fn upload_rgba(gpu: &Gpu, width: u32, height: u32, pixels: Vec<u8>) -> GpuTexture {
    wpe_render::upload_texture(
        gpu,
        &crate::rgba_texture(width, height, pixels),
        "script text",
    )
}
