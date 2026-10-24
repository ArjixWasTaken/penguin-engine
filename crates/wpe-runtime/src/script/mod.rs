//! SceneScript: the scene state scripts read and write, and applying their
//! writes to the renderer.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Vec2, Vec3};
use wpe_assets::scene::{SceneObject, TextObject};
use wpe_assets::value::Dynamic;
use wpe_render::{CardMesh, Gpu, GpuTexture, SceneRenderer};
use wpe_scene::{ImageLayer, Motion};
use wpe_script::{
    CursorEvent, CursorEventKind, CursorState, LayerInfo, LayerKind, LogLevel, SceneHost,
    ScriptRuntime, ScriptValue,
};
use wpe_vfs::Vfs;

use crate::frame::LayerSteps;
use crate::video::VideoStream;

/// What scripts can see and change of one scene object.
struct LayerState {
    info: LayerInfo,
    steps: LayerSteps,
    motion: Motion,
    size: Vec2,
    visible: bool,
    origin: Vec3,
    scale: Vec3,
    angles: Vec3,
    alpha: f32,
    color: Vec3,
    text: Option<String>,
    text_object: Option<Box<TextObject>>,
    /// No ancestor group is hidden, so the layer shows when `visible`.
    ancestors_visible: bool,
    dirty_visible: bool,
    dirty_transform: bool,
    dirty_color: bool,
    dirty_text: bool,
}

/// The [`SceneHost`] scripts run against.
pub struct ScriptHost {
    layers: Vec<LayerState>,
    by_id: HashMap<i32, usize>,
    canvas: [f32; 2],
    spectrum: Option<wpe_audio::AudioSpectrum>,
    /// Each layer's video, for `getVideoTexture()`.
    videos: HashMap<i32, Arc<VideoStream>>,
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

fn vec3(v: Vec3) -> ScriptValue {
    ScriptValue::Vec3([v.x as f64, v.y as f64, v.z as f64])
}

fn to_vec3(v: &ScriptValue) -> Option<Vec3> {
    let c = v.components()?;
    match c.len() {
        0 => None,
        1 => Some(Vec3::splat(c[0] as f32)),
        2 => Some(Vec3::new(c[0] as f32, c[1] as f32, 0.0)),
        _ => Some(Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32)),
    }
}

fn kind(obj: &SceneObject) -> LayerKind {
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

impl SceneHost for ScriptHost {
    fn layers(&self) -> Vec<LayerInfo> {
        self.layers.iter().map(|l| l.info.clone()).collect()
    }

    fn get(&self, layer: i32, property: &str) -> Option<ScriptValue> {
        let l = &self.layers[*self.by_id.get(&layer)?];
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
            _ => return None,
        })
    }

    fn set(&mut self, layer: i32, property: &str, value: ScriptValue) {
        let Some(&index) = self.by_id.get(&layer) else {
            return;
        };
        let l = &mut self.layers[index];
        match property {
            "visible" => {
                if let Some(v) = value.as_bool() {
                    l.dirty_visible |= v != l.visible;
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
                *slot = v;
            }
            "alpha" | "opacity" => {
                if let Some(v) = value.as_f64() {
                    l.dirty_color |= v as f32 != l.alpha;
                    l.alpha = v as f32;
                }
            }
            "color" => {
                if let Some(v) = to_vec3(&value) {
                    l.dirty_color |= v != l.color;
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
            _ => None,
        }
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
        match level {
            LogLevel::Error => tracing::warn!("script: {message}"),
            LogLevel::Warn => tracing::info!("script: {message}"),
            LogLevel::Info => tracing::debug!("script: {message}"),
        }
    }
}

/// A wallpaper's scripts and the state they drive.
pub struct Scripts {
    runtime: ScriptRuntime,
    host: ScriptHost,
    text: Option<wpe_text::TextRenderer>,
    user: serde_json::Map<String, serde_json::Value>,
    last_time: f64,
    pressed: bool,
}

/// A scripted property: name, script source, scriptproperties, initial value.
type Scripted<'a> = (
    &'static str,
    Option<&'a str>,
    Option<&'a serde_json::Value>,
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
    /// Load every script of `scene`, or `None` when it has none.
    #[allow(clippy::too_many_arguments)]
    pub fn load(
        assets: &Vfs,
        project: &wpe_assets::Project,
        scene: &wpe_assets::Scene,
        layers: &[ImageLayer],
        steps: &HashMap<i32, LayerSteps>,
        videos: HashMap<i32, Arc<VideoStream>>,
        canvas: Vec2,
        text: Option<wpe_text::TextRenderer>,
    ) -> Option<Self> {
        let scripts: Vec<_> = scene.objects.iter().map(scripted).collect();
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
        let user: serde_json::Map<String, serde_json::Value> = project
            .general
            .properties
            .iter()
            .map(|(k, p)| (k.clone(), p.value.clone()))
            .collect();
        runtime.set_user_properties(&serde_json::Value::Object(user.clone()));

        let by_layer: HashMap<i32, &ImageLayer> = layers.iter().map(|l| (l.id, l)).collect();
        let mut host = ScriptHost {
            layers: Vec::new(),
            by_id: HashMap::new(),
            canvas: canvas.to_array(),
            spectrum: None,
            videos,
        };
        for obj in &scene.objects {
            let Some(c) = obj.common() else { continue };
            let layer = by_layer.get(&c.id);
            let motion = layer.map(|l| l.motion.clone()).unwrap_or_default();
            host.by_id.insert(c.id, host.layers.len());
            host.layers.push(LayerState {
                info: LayerInfo {
                    id: c.id,
                    name: c.name.clone(),
                    kind: kind(obj),
                    parent: c.parent,
                    effects: obj.effects().iter().map(|e| e.name.clone()).collect(),
                },
                steps: steps.get(&c.id).cloned().unwrap_or_default(),
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
                dirty_visible: false,
                dirty_transform: false,
                dirty_color: false,
                dirty_text: false,
            });
        }

        for (obj, entries) in scene.objects.iter().zip(scripts) {
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
            text,
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

    /// Run the scripts' frame and push what they changed to the renderer.
    pub fn update(
        &mut self,
        gpu: &Gpu,
        vfs: &Vfs,
        renderer: &mut SceneRenderer,
        time: f32,
        pointer: Vec2,
        spectrum: Option<wpe_audio::AudioSpectrum>,
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
            .collect();
        for id in ended {
            tracing::trace!("layer {id} video ended");
            self.runtime.video_ended(&mut self.host, id);
        }
        let cursor = self.cursor_state(pointer);
        self.runtime.update(&mut self.host, time, dt, Some(cursor));
        self.apply(gpu, vfs, renderer);
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
        let Some(l) = self.host.by_id.get(&id).map(|&i| &self.host.layers[i]) else {
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

    fn apply(&mut self, gpu: &Gpu, vfs: &Vfs, renderer: &mut SceneRenderer) {
        let user = |name: &str| self.user.get(name).cloned();
        for l in &mut self.host.layers {
            if l.dirty_text
                && let (Some(text), Some(obj), Some(renderer_text)) = (
                    l.text.as_deref(),
                    l.text_object.as_deref(),
                    self.text.as_mut(),
                )
            {
                let style = wpe_text::TextStyle::from_object(obj, user);
                match renderer_text.render(vfs, &style, text) {
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
