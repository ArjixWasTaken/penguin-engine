//! Wallpaper Engine SceneScript runtime.
//!
//! Scripts attached to scene values (`{"value": ..., "script": "export function
//! update(value) {...}"}`) run as ES modules in an embedded QuickJS engine.
//! They see WE's globals (`engine`, `thisScene`, `thisLayer`, `thisObject`,
//! `input`, `shared`, `console`, `localStorage`, `Vec2/3/4`, `Mat3/4`,
//! `createScriptProperties`) and the `WEMath`/`WEVector`/`WEColor` modules.
//! All scene access goes through a [`SceneHost`] the renderer implements.
//!
//! Per frame: [`ScriptRuntime::update`] (fires due timers, refreshes
//! `engine`/`input`/audio buffers, then calls every `update(value)` and writes
//! changed results back through [`SceneHost::set`]). Pointer input goes through
//! [`ScriptRuntime::cursor_event`], user-property edits through
//! [`ScriptRuntime::apply_user_properties`].

mod host;
mod value;

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use rquickjs::loader::{BuiltinLoader, BuiltinResolver};
use rquickjs::{Array, Context, Ctx, Function, Module, Object, Runtime, Value};
use serde_json::Map;

pub use host::{
    CursorEvent, CursorEventKind, CursorState, LayerId, LayerInfo, LayerKind, LogLevel, SceneHost,
};
pub use value::{ScriptValue, ValueKind};

const BASE_CLASSES: &str = include_str!("js/baseclasses.js");
const RUNTIME: &str = include_str!("js/runtime.js");
const WEMATH: &str = include_str!("js/wemath.js");
const WEVECTOR: &str = include_str!("js/wevector.js");
const WECOLOR: &str = include_str!("js/wecolor.js");
const MODULE_GLOBALS: &str = "import * as WEMath from 'WEMath';\n\
     import * as WEVector from 'WEVector';\n\
     import * as WEColor from 'WEColor';\n\
     globalThis.WEMath = WEMath; globalThis.WEVector = WEVector; globalThis.WEColor = WEColor;\n";

/// Errors creating the runtime or loading a script.
#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    #[error("failed to set up the script engine: {0}")]
    Engine(String),
    #[error("script failed to compile: {0}")]
    Compile(String),
    #[error("script threw while loading: {0}")]
    Load(String),
    #[error("script exceeded its time budget while loading")]
    Timeout,
}

/// Handle of a registered script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScriptId(pub usize);

/// A script function that threw (or ran out of time) after loading.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptFailure {
    pub script: ScriptId,
    /// Exported function that failed (`init`, `update`, `cursorClick`, ...).
    pub phase: &'static str,
    /// `Name: message` of the exception.
    pub message: String,
    /// First stack frame, e.g. `update (layer 3/visible:12:9)`.
    pub location: Option<String>,
    /// Whether the time budget ran out.
    pub timeout: bool,
}

/// Tuning knobs for [`ScriptRuntime::with_options`].
#[derive(Debug, Clone)]
pub struct ScriptOptions {
    /// Source of Wallpaper Engine's `assets/scripts/jsclasses/baseclasses.js`.
    /// When absent a compatible built-in implementation is used.
    pub base_classes: Option<String>,
    /// Wall-clock budget for one script function call per frame. It only
    /// stops runaway loops, so it allows for a busy machine.
    pub call_budget: Duration,
    /// Budget for evaluating a module and running its `init`.
    pub load_budget: Duration,
    /// QuickJS heap limit in bytes (0 = unlimited).
    pub memory_limit: usize,
}

impl Default for ScriptOptions {
    fn default() -> Self {
        Self {
            base_classes: None,
            call_budget: Duration::from_millis(250),
            load_budget: Duration::from_secs(1),
            memory_limit: 256 << 20,
        }
    }
}

const HANDLERS: [&str; 17] = [
    "init",
    "update",
    "destroy",
    "applyUserProperties",
    "cursorClick",
    "cursorDown",
    "cursorUp",
    "cursorMove",
    "cursorEnter",
    "cursorLeave",
    "resizeScreen",
    "mediaStatusChanged",
    "mediaPlaybackChanged",
    "mediaPropertiesChanged",
    "mediaThumbnailChanged",
    "mediaTimelineChanged",
    "applyGeneralSettings",
];

fn handler_bit(name: &str) -> u32 {
    HANDLERS
        .iter()
        .position(|h| *h == name)
        .map_or(0, |i| 1 << i)
}

const CURSOR_HANDLERS: [&str; 6] = [
    "cursorClick",
    "cursorDown",
    "cursorUp",
    "cursorMove",
    "cursorEnter",
    "cursorLeave",
];

struct Script {
    layer: LayerId,
    property: String,
    kind: ValueKind,
    value: ScriptValue,
    /// `(scriptProperty, userProperty)` pairs from `{"user": .., "value": ..}` bindings.
    bindings: Vec<(String, String)>,
    handlers: u32,
    initialized: bool,
    disabled: bool,
}

impl Script {
    fn has(&self, handler: &str) -> bool {
        self.handlers & handler_bit(handler) != 0
    }
}

type HostPtr = *mut (dyn SceneHost + 'static);

/// The host borrowed for the duration of one public call.
#[derive(Default)]
struct HostSlot(Cell<Option<HostPtr>>);

impl HostSlot {
    fn with<R>(&self, f: impl FnOnce(&mut dyn SceneHost) -> R) -> Option<R> {
        let ptr = self.0.take()?;
        // SAFETY: `ptr` was created by `enter` from a `&mut dyn SceneHost` that
        // stays exclusively borrowed until the returned guard drops, and taking
        // it out of the cell means no second reference can exist meanwhile.
        let r = f(unsafe { &mut *ptr });
        self.0.set(Some(ptr));
        Some(r)
    }

    fn enter<'a>(&'a self, host: &'a mut dyn SceneHost) -> HostGuard<'a> {
        let ptr: *mut (dyn SceneHost + 'a) = host;
        // SAFETY: only the trait-object lifetime is erased; the guard clears the
        // slot before `'a` ends, so the pointer is never used past the borrow.
        let ptr: HostPtr = unsafe { std::mem::transmute(ptr) };
        self.0.set(Some(ptr));
        HostGuard(self)
    }
}

struct HostGuard<'a>(&'a HostSlot);

impl Drop for HostGuard<'_> {
    fn drop(&mut self) {
        self.0.0.set(None);
    }
}

/// Runs every SceneScript of one scene.
///
/// Not `Send`: keep it on the thread that owns the scene.
pub struct ScriptRuntime {
    context: Context,
    runtime: Runtime,
    host: Rc<HostSlot>,
    deadline: Rc<Cell<Option<Instant>>>,
    options: ScriptOptions,
    scripts: Vec<Script>,
    user_properties: Map<String, serde_json::Value>,
    failures: Vec<ScriptFailure>,
    pressed: Vec<LayerId>,
    hovered: Vec<LayerId>,
}

impl ScriptRuntime {
    /// A runtime with the built-in base classes and default budgets.
    pub fn new() -> Result<Self, ScriptError> {
        Self::with_options(ScriptOptions::default())
    }

    /// A runtime with explicit options.
    pub fn with_options(options: ScriptOptions) -> Result<Self, ScriptError> {
        let engine = |e: rquickjs::Error| ScriptError::Engine(e.to_string());
        let runtime = Runtime::new().map_err(engine)?;
        runtime.set_memory_limit(options.memory_limit);
        runtime.set_max_stack_size(1 << 20);
        let resolver = BuiltinResolver::default()
            .with_module("WEMath")
            .with_module("WEVector")
            .with_module("WEColor");
        let loader = BuiltinLoader::default()
            .with_module("WEMath", WEMATH)
            .with_module("WEVector", WEVECTOR)
            .with_module("WEColor", WECOLOR);
        runtime.set_loader(resolver, loader);
        let deadline: Rc<Cell<Option<Instant>>> = Rc::default();
        let d = deadline.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || {
            d.get().is_some_and(|t| Instant::now() >= t)
        })));
        let context = Context::full(&runtime).map_err(engine)?;
        let host: Rc<HostSlot> = Rc::default();

        let base = options.base_classes.as_deref().unwrap_or(BASE_CLASSES);
        context.with(|ctx| -> Result<(), ScriptError> {
            let fail =
                |ctx: &Ctx, e: rquickjs::Error| ScriptError::Engine(describe_error(ctx, e).message);
            let mut opts = rquickjs::context::EvalOptions::default();
            opts.strict = false;
            ctx.eval_with_options::<(), _>(base, opts)
                .map_err(|e| fail(&ctx, e))?;
            install_natives(&ctx, host.clone()).map_err(|e| fail(&ctx, e))?;
            ctx.eval::<(), _>(RUNTIME).map_err(|e| fail(&ctx, e))?;
            let (_, promise) = Module::declare(ctx.clone(), "wpe:globals", MODULE_GLOBALS)
                .and_then(|m| m.eval())
                .map_err(|e| fail(&ctx, e))?;
            promise.finish::<()>().map_err(|e| fail(&ctx, e))?;
            Ok(())
        })?;

        Ok(Self {
            context,
            runtime,
            host,
            deadline,
            options,
            scripts: Vec::new(),
            user_properties: Map::new(),
            failures: Vec::new(),
            pressed: Vec::new(),
            hovered: Vec::new(),
        })
    }

    /// Set the project's user properties before adding scripts.
    ///
    /// `properties` maps names to either project.json entries
    /// (`{"type": "color", "value": "1 0 0", ...}`) or plain values. Colours
    /// become `Vec3` in `engine.userProperties`.
    pub fn set_user_properties(&mut self, properties: &serde_json::Value) {
        let Some(map) = properties.as_object() else {
            return;
        };
        for (k, v) in map {
            self.user_properties.insert(k.clone(), v.clone());
        }
        let json = properties.to_string();
        let _ = self.run_js(None, "applyUserProperties", |_, w| {
            w.get::<_, Function>("setUserProperties")?
                .call::<_, ()>((json.as_str(),))
        });
    }

    /// Restore `localStorage` saved earlier (as given to
    /// [`SceneHost::storage_changed`]); call before loading scripts.
    pub fn set_local_storage(&mut self, data: &serde_json::Value) {
        let json = data.to_string();
        let _ = self.run_js(None, "localStorage", |_, w| {
            w.get::<_, Function>("setStorage")?
                .call::<_, ()>((json.as_str(),))
        });
    }

    /// Load the script attached to `layer`'s `property`.
    ///
    /// `property` is the scene-JSON path of the value relative to the object
    /// (`visible`, `origin`, `text`, `effects/0/visible`,
    /// `effects/0/passes/0/constantshadervalues/strength`, ...); it is handed
    /// back verbatim to [`SceneHost::get`]/[`SceneHost::set`]. `initial` fixes
    /// the value's type (see [`ScriptValue::from_scene_json`]). The module's
    /// top level runs now; `init` runs at [`Self::init`] or the first update.
    pub fn add_property_script(
        &mut self,
        host: &mut dyn SceneHost,
        layer: LayerId,
        property: &str,
        source: &str,
        script_properties: Option<&serde_json::Value>,
        initial: ScriptValue,
    ) -> Result<ScriptId, ScriptError> {
        let id = ScriptId(self.scripts.len());
        let sid = id.0 as f64;
        let name = format!("layer {layer}/{property}#{}", id.0);
        let src = format!(
            "const thisLayer = __wpe.scopeLayer({sid}), thisObject = __wpe.scopeObject({sid});{}",
            source.replace('\u{a0}', " ")
        );
        let (overrides, bindings) = self.resolve_script_properties(script_properties);
        let kind = initial.kind();
        let kind_code = kind_code(kind);
        let host_slot = self.host.clone();
        let _guard = host_slot.enter(host);
        self.arm(self.options.load_budget);
        let result = self.context.with(|ctx| -> Result<u32, ScriptError> {
            let w = wpe(&ctx).map_err(|e| ScriptError::Engine(e.to_string()))?;
            let js = |e: rquickjs::Error| describe_error(&ctx, e);
            w.get::<_, Function>("register")
                .and_then(|f| f.call::<_, ()>((sid, layer, property, kind_code)))
                .map_err(|e| ScriptError::Engine(js(e).message))?;
            let module = Module::declare(ctx.clone(), name, src).map_err(|e| {
                let f = js(e);
                ScriptError::Compile(f.with_location())
            })?;
            let (module, promise) = module.eval().map_err(|e| load_error(js(e)))?;
            promise.finish::<()>().map_err(|e| load_error(js(e)))?;
            let ns = module.namespace().map_err(|e| load_error(js(e)))?;
            let mask = w
                .get::<_, Function>("attach")
                .and_then(|f| f.call::<_, u32>((sid, ns)))
                .map_err(|e| load_error(js(e)))?;
            if let Some(o) = &overrides {
                w.get::<_, Function>("applyScriptProperties")
                    .and_then(|f| f.call::<_, ()>((sid, o.to_string())))
                    .map_err(|e| load_error(js(e)))?;
            }
            Ok(mask)
        });
        let timed_out = self.disarm();
        let handlers = match result {
            Ok(mask) => mask,
            Err(_) if timed_out => return Err(ScriptError::Timeout),
            Err(e) => return Err(e),
        };
        self.scripts.push(Script {
            layer,
            property: property.to_owned(),
            kind,
            value: initial,
            bindings,
            handlers,
            initialized: false,
            disabled: false,
        });
        Ok(id)
    }

    /// Run `init(value)` (and an initial `applyUserProperties`) of every
    /// script not initialized yet. Called implicitly by [`Self::update`].
    pub fn init(&mut self, host: &mut dyn SceneHost) {
        let slot = self.host.clone();
        let _guard = slot.enter(host);
        self.init_pending();
    }

    fn init_pending(&mut self) {
        for i in 0..self.scripts.len() {
            if self.scripts[i].initialized {
                continue;
            }
            self.scripts[i].initialized = true;
            if self.scripts[i].disabled {
                continue;
            }
            if self.scripts[i].has("init") {
                self.call_value(i, "init", self.options.load_budget);
            }
            if self.scripts[i].has("applyUserProperties") && !self.user_properties.is_empty() {
                self.call_changed(i, true);
            }
        }
        self.drain_jobs();
    }

    /// Advance one frame. `time` is the scene runtime in seconds
    /// (`engine.runtime`, also the timer clock), `frame_time` the delta
    /// (`engine.frametime`).
    pub fn update(
        &mut self,
        host: &mut dyn SceneHost,
        time: f64,
        frame_time: f64,
        cursor: Option<CursorState>,
    ) {
        let slot = self.host.clone();
        let _guard = slot.enter(host);
        let (canvas, screen) = self
            .host
            .with(|h| (h.canvas_size(), h.screen_resolution()))
            .unwrap_or(([1920.0, 1080.0], [1920.0, 1080.0]));
        let budget = self.options.call_budget;
        let world = cursor.map(|c| c.world.map(f64::from).to_vec());
        let screen_pos = cursor.map(|c| c.screen.map(f64::from).to_vec());
        let down = cursor.is_some_and(|c| c.left_down);
        let _ = self.run_js(None, "frame", |_, w| {
            w.get::<_, Function>("frame")?.call::<_, ()>((
                time,
                frame_time,
                world,
                screen_pos,
                down,
                canvas.map(f64::from).to_vec(),
                screen.map(f64::from).to_vec(),
            ))?;
            w.get::<_, Function>("invalidateLayers")?.call::<_, ()>(())
        });
        self.init_pending();
        self.arm(budget);
        let _ = self.run_js(None, "timer", |_, w| {
            w.get::<_, Function>("runTimers")?.call::<_, ()>(())
        });
        self.disarm();
        for i in 0..self.scripts.len() {
            if !self.scripts[i].disabled && self.scripts[i].has("update") {
                self.call_value(i, "update", budget);
            }
        }
        self.drain_jobs();
    }

    /// Deliver a pointer event. `hits` are the layers under the cursor
    /// (topmost first); enter/leave and click are derived here:
    ///
    /// * `Move`: `cursorEnter`/`cursorLeave` on hover changes, `cursorMove` on
    ///   hit layers and on layers held since `Down`;
    /// * `Down`: `cursorDown` on every hit layer with a handler (or on all
    ///   `cursorDown` scripts when nothing interactive was hit);
    /// * `Up`: `cursorUp` on pressed and hit layers, then `cursorClick` on the
    ///   topmost hit layer that was also pressed.
    pub fn cursor_event(&mut self, host: &mut dyn SceneHost, event: CursorEvent) {
        let slot = self.host.clone();
        let _guard = slot.enter(host);
        let interactive: HashSet<LayerId> = self.interactive_layers().into_iter().collect();
        let hits: Vec<LayerId> = event
            .hits
            .iter()
            .copied()
            .filter(|l| interactive.contains(l))
            .collect();
        let pos = (event.world, event.screen);
        match event.kind {
            CursorEventKind::Move => {
                let left: Vec<LayerId> = self
                    .hovered
                    .iter()
                    .copied()
                    .filter(|l| !hits.contains(l))
                    .collect();
                let entered: Vec<LayerId> = hits
                    .iter()
                    .copied()
                    .filter(|l| !self.hovered.contains(l))
                    .collect();
                for l in left {
                    self.fire_cursor(l, "cursorLeave", pos);
                }
                for l in entered {
                    self.fire_cursor(l, "cursorEnter", pos);
                }
                self.hovered = hits.clone();
                let mut targets = hits;
                for l in &self.pressed {
                    if !targets.contains(l) {
                        targets.push(*l);
                    }
                }
                for l in targets {
                    self.fire_cursor(l, "cursorMove", pos);
                }
            }
            CursorEventKind::Down => {
                if hits.is_empty() {
                    let all: Vec<LayerId> = self.layers_with("cursorDown");
                    for l in all {
                        self.fire_cursor(l, "cursorDown", pos);
                    }
                } else {
                    for &l in &hits {
                        self.fire_cursor(l, "cursorDown", pos);
                    }
                }
                self.pressed = hits;
            }
            CursorEventKind::Up => {
                let pressed = std::mem::take(&mut self.pressed);
                let mut targets = pressed.clone();
                for &l in &hits {
                    if !targets.contains(&l) {
                        targets.push(l);
                    }
                }
                for &l in &targets {
                    self.fire_cursor(l, "cursorUp", pos);
                }
                let click = hits.iter().copied().find(|l| {
                    pressed.contains(l) && self.layer_scripts(*l).any(|s| s.has("cursorClick"))
                });
                if let Some(l) = click {
                    self.fire_cursor(l, "cursorClick", pos);
                }
            }
        }
        self.drain_jobs();
    }

    /// Layers with at least one cursor handler; only these need hit testing.
    pub fn interactive_layers(&self) -> Vec<LayerId> {
        let mut out = Vec::new();
        for s in &self.scripts {
            if !s.disabled && CURSOR_HANDLERS.iter().any(|h| s.has(h)) && !out.contains(&s.layer) {
                out.push(s.layer);
            }
        }
        out
    }

    /// Apply edited user properties (same encoding as
    /// [`Self::set_user_properties`]): refreshes `engine.userProperties`,
    /// re-binds script properties bound to them and calls
    /// `applyUserProperties(changed)`.
    pub fn apply_user_properties(&mut self, host: &mut dyn SceneHost, changed: &serde_json::Value) {
        let Some(map) = changed.as_object() else {
            return;
        };
        let slot = self.host.clone();
        let _guard = slot.enter(host);
        // Scripts see their `init` before any change.
        self.init_pending();
        self.set_user_properties(changed);
        for i in 0..self.scripts.len() {
            if self.scripts[i].disabled {
                continue;
            }
            let rebound: Map<String, serde_json::Value> = self.scripts[i]
                .bindings
                .iter()
                .filter_map(|(key, user)| {
                    map.get(user).map(|v| (key.clone(), user_value(v).clone()))
                })
                .collect();
            if !rebound.is_empty() {
                let json = serde_json::Value::Object(rebound).to_string();
                let sid = i as f64;
                let _ = self.run_js(Some(i), "applyUserProperties", |_, w| {
                    w.get::<_, Function>("applyScriptProperties")?
                        .call::<_, ()>((sid, json.as_str()))
                });
            }
            if self.scripts[i].has("applyUserProperties") {
                self.call_changed(i, false);
            }
        }
        self.drain_jobs();
    }

    /// Fire the `addEndedCallback` callbacks registered on `layer`'s video
    /// texture or animation layers.
    pub fn video_ended(&mut self, host: &mut dyn SceneHost, layer: LayerId) {
        let slot = self.host.clone();
        let _guard = slot.enter(host);
        let _ = self.run_js(None, "videoEnded", |_, w| {
            w.get::<_, Function>("videoEnded")?.call::<_, ()>((layer,))
        });
    }

    /// Call `handler(arg)` on every script exporting it: `resizeScreen`
    /// (`arg` a `[w, h]` array becomes a `Vec2`), `mediaPlaybackChanged`,
    /// `mediaPropertiesChanged`, `mediaThumbnailChanged`,
    /// `mediaTimelineChanged`, `mediaStatusChanged`, `applyGeneralSettings`.
    pub fn emit(
        &mut self,
        host: &mut dyn SceneHost,
        handler: &'static str,
        arg: &serde_json::Value,
    ) {
        let slot = self.host.clone();
        let _guard = slot.enter(host);
        let value = match (handler, arg.as_array().map(Vec::as_slice)) {
            ("resizeScreen", Some([w, h])) => {
                ScriptValue::Vec2([w.as_f64().unwrap_or(0.0), h.as_f64().unwrap_or(0.0)])
            }
            _ => ScriptValue::Json(arg.clone()),
        };
        for i in 0..self.scripts.len() {
            if !self.scripts[i].disabled && self.scripts[i].has(handler) {
                self.call_plain(i, handler, &value);
            }
        }
        self.drain_jobs();
    }

    /// Call `destroy()` on every script and stop running them.
    pub fn destroy(&mut self, host: &mut dyn SceneHost) {
        let slot = self.host.clone();
        let _guard = slot.enter(host);
        for i in 0..self.scripts.len() {
            if !self.scripts[i].disabled && self.scripts[i].has("destroy") {
                self.call_plain(i, "destroy", &ScriptValue::Null);
            }
            self.scripts[i].disabled = true;
        }
    }

    /// Failures since the last call.
    pub fn take_failures(&mut self) -> Vec<ScriptFailure> {
        std::mem::take(&mut self.failures)
    }

    /// Number of registered scripts.
    pub fn script_count(&self) -> usize {
        self.scripts.len()
    }

    /// Whether the script was disabled after an error in `update`.
    pub fn is_disabled(&self, id: ScriptId) -> bool {
        self.scripts.get(id.0).is_none_or(|s| s.disabled)
    }

    /// Last value the script produced for its property.
    pub fn value(&self, id: ScriptId) -> Option<&ScriptValue> {
        self.scripts.get(id.0).map(|s| &s.value)
    }

    /// Exported handlers of a script (`init`, `update`, `cursorClick`, ...).
    pub fn handlers(&self, id: ScriptId) -> Vec<&'static str> {
        self.scripts.get(id.0).map_or_else(Vec::new, |s| {
            HANDLERS.iter().copied().filter(|h| s.has(h)).collect()
        })
    }

    fn layers_with(&self, handler: &str) -> Vec<LayerId> {
        let mut out = Vec::new();
        for s in &self.scripts {
            if !s.disabled && s.has(handler) && !out.contains(&s.layer) {
                out.push(s.layer);
            }
        }
        out
    }

    fn layer_scripts(&self, layer: LayerId) -> impl Iterator<Item = &Script> {
        self.scripts
            .iter()
            .filter(move |s| s.layer == layer && !s.disabled)
    }

    fn fire_cursor(&mut self, layer: LayerId, handler: &'static str, pos: ([f32; 2], [f32; 2])) {
        let ([wx, wy], [sx, sy]) = pos;
        let budget = self.options.call_budget;
        for i in 0..self.scripts.len() {
            let s = &self.scripts[i];
            if s.layer != layer || s.disabled || !s.has(handler) {
                continue;
            }
            let sid = i as f64;
            self.arm(budget);
            let _ = self.run_js(Some(i), handler, |_, w| {
                w.get::<_, Function>("cursorEvent")?
                    .call::<_, ()>((sid, handler, wx as f64, wy as f64, sx as f64, sy as f64))
            });
            self.disarm();
        }
    }

    fn call_plain(&mut self, i: usize, handler: &'static str, arg: &ScriptValue) {
        let sid = i as f64;
        self.arm(self.options.call_budget);
        let _ = self.run_js(Some(i), handler, |ctx, w| {
            let a = arg.to_js(ctx, w)?;
            w.get::<_, Function>("call")?
                .call::<_, ()>((sid, handler, a))
        });
        self.disarm();
    }

    fn call_changed(&mut self, i: usize, all: bool) {
        let sid = i as f64;
        self.arm(self.options.call_budget);
        let _ = self.run_js(Some(i), "applyUserProperties", |_, w| {
            w.get::<_, Function>("callChanged")?
                .call::<_, ()>((sid, all))
        });
        self.disarm();
    }

    /// Call `init`/`update` with the property's current value and write the
    /// result back to the host when it changed.
    fn call_value(&mut self, i: usize, handler: &'static str, budget: Duration) {
        let (layer, kind) = (self.scripts[i].layer, self.scripts[i].kind);
        let property = self.scripts[i].property.clone();
        let current = self
            .host
            .with(|h| h.get(layer, &property))
            .flatten()
            .and_then(|v| v.coerce(kind, Some(&self.scripts[i].value)))
            .unwrap_or_else(|| self.scripts[i].value.clone());
        let sid = i as f64;
        self.arm(budget);
        let result = self.run_js(Some(i), handler, |ctx, w| {
            let arg = current.to_js(ctx, w)?;
            let r: Value = w
                .get::<_, Function>("callValue")?
                .call((sid, handler, arg))?;
            Ok(ScriptValue::from_js(ctx, &r))
        });
        self.disarm();
        match result {
            Ok(ScriptValue::Null) => {}
            Ok(v) => {
                if let Some(v) = v.coerce(kind, Some(&current)) {
                    if v != current {
                        self.host.with(|h| h.set(layer, &property, v.clone()));
                    }
                    self.scripts[i].value = v;
                }
            }
            Err(()) if handler == "update" => self.scripts[i].disabled = true,
            Err(()) => {}
        }
    }

    fn drain_jobs(&mut self) {
        self.arm(self.options.call_budget);
        while self.runtime.is_job_pending() {
            match self.runtime.execute_pending_job() {
                Ok(true) => {}
                Ok(false) => break,
                Err(_) => {
                    if self.deadline_passed() {
                        break;
                    }
                }
            }
        }
        self.disarm();
    }

    fn arm(&self, budget: Duration) {
        self.deadline.set(Some(Instant::now() + budget));
    }

    fn deadline_passed(&self) -> bool {
        self.deadline.get().is_some_and(|t| Instant::now() >= t)
    }

    /// Clear the deadline; true when it had expired.
    fn disarm(&self) -> bool {
        let passed = self.deadline_passed();
        self.deadline.set(None);
        passed
    }

    /// Run JS against `__wpe`, recording a failure attributed to `script`.
    fn run_js<R>(
        &mut self,
        script: Option<usize>,
        phase: &'static str,
        f: impl for<'js> FnOnce(&Ctx<'js>, &Object<'js>) -> rquickjs::Result<R>,
    ) -> Result<R, ()> {
        let outcome = self.context.with(|ctx| {
            let res = wpe(&ctx).and_then(|w| f(&ctx, &w));
            res.map_err(|e| describe_error(&ctx, e))
        });
        match outcome {
            Ok(r) => Ok(r),
            Err(mut failure) => {
                failure.timeout = self.deadline_passed();
                if failure.timeout {
                    failure.message = "script exceeded its time budget".into();
                }
                let id = ScriptId(script.unwrap_or(usize::MAX));
                let label = script.and_then(|i| self.scripts.get(i)).map_or_else(
                    || "runtime".to_owned(),
                    |s| format!("layer {} {}", s.layer, s.property),
                );
                let msg = format!("{label}: {phase}: {}", failure.with_location());
                let _ = self.host.with(|h| h.log(LogLevel::Error, &msg));
                self.failures.push(ScriptFailure {
                    script: id,
                    phase,
                    message: failure.message,
                    location: failure.location,
                    timeout: failure.timeout,
                });
                Err(())
            }
        }
    }

    fn resolve_script_properties(
        &self,
        sp: Option<&serde_json::Value>,
    ) -> (Option<serde_json::Value>, Vec<(String, String)>) {
        let Some(map) = sp.and_then(|v| v.as_object()) else {
            return (None, Vec::new());
        };
        let mut out = Map::new();
        let mut bindings = Vec::new();
        for (key, v) in map {
            let resolved = match v.as_object() {
                Some(o) if o.contains_key("user") => {
                    let user = match &o["user"] {
                        serde_json::Value::String(s) => Some(s.clone()),
                        serde_json::Value::Object(u) => {
                            u.get("name").and_then(|n| n.as_str()).map(str::to_owned)
                        }
                        _ => None,
                    };
                    let bound = user
                        .as_ref()
                        .and_then(|u| self.user_properties.get(u))
                        .map(user_value);
                    if let Some(u) = user {
                        bindings.push((key.clone(), u));
                    }
                    bound
                        .or_else(|| o.get("value"))
                        .cloned()
                        .unwrap_or(serde_json::Value::Null)
                }
                Some(o) if o.contains_key("value") => o["value"].clone(),
                _ => v.clone(),
            };
            out.insert(key.clone(), resolved);
        }
        (Some(serde_json::Value::Object(out)), bindings)
    }
}

/// The value of a project user-property entry (`{"type", "value"}` or plain).
fn user_value(v: &serde_json::Value) -> &serde_json::Value {
    match v.as_object().and_then(|o| o.get("value")) {
        Some(inner) => inner,
        None => v,
    }
}

fn kind_code(kind: ValueKind) -> &'static str {
    match kind {
        ValueKind::Bool => "b",
        ValueKind::Number => "n",
        ValueKind::String => "s",
        ValueKind::Vec2 => "v2",
        ValueKind::Vec3 => "v3",
        ValueKind::Vec4 => "v4",
        ValueKind::Any => "any",
    }
}

fn wpe<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    ctx.globals().get("__wpe")
}

fn load_error(f: Failure) -> ScriptError {
    ScriptError::Load(f.with_location())
}

struct Failure {
    message: String,
    location: Option<String>,
    timeout: bool,
}

impl Failure {
    fn with_location(&self) -> String {
        match &self.location {
            Some(l) => format!("{} (at {l})", self.message),
            None => self.message.clone(),
        }
    }
}

fn describe_error(ctx: &Ctx, e: rquickjs::Error) -> Failure {
    if !e.is_exception() {
        return Failure {
            message: e.to_string(),
            location: None,
            timeout: false,
        };
    }
    let v = ctx.catch();
    let Some(obj) = v.as_object() else {
        return Failure {
            message: format!("uncaught {}", ScriptValue::from_js(ctx, &v).to_json()),
            location: None,
            timeout: false,
        };
    };
    let text = |k: &str| obj.get::<_, Option<String>>(k).ok().flatten();
    let message = match (text("name"), text("message")) {
        (Some(n), Some(m)) => format!("{n}: {m}"),
        (None, Some(m)) => m,
        _ => ScriptValue::from_js(ctx, &v).to_json().to_string(),
    };
    let location = text("stack").and_then(|s| {
        s.lines()
            .map(str::trim)
            .find(|l| l.starts_with("at "))
            .map(|l| l.trim_start_matches("at ").to_owned())
    });
    Failure {
        message,
        location,
        timeout: false,
    }
}

fn install_natives<'js>(ctx: &Ctx<'js>, host: Rc<HostSlot>) -> rquickjs::Result<()> {
    let n = Object::new(ctx.clone())?;
    let h = host.clone();
    n.set(
        "layers",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>| -> rquickjs::Result<Value<'js>> {
                let list = h.with(|h| h.layers()).unwrap_or_default();
                let json: Vec<_> = list.iter().map(LayerInfo::to_json).collect();
                ctx.json_parse(serde_json::Value::Array(json).to_string())
            },
        )?,
    )?;
    let h = host.clone();
    n.set(
        "get",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, id: f64, prop: String| -> rquickjs::Result<Value<'js>> {
                match h.with(|h| h.get(id as LayerId, &prop)).flatten() {
                    Some(v) => v.to_js(&ctx, &wpe(&ctx)?),
                    None => Ok(Value::new_undefined(ctx)),
                }
            },
        )?,
    )?;
    let h = host.clone();
    n.set(
        "set",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, id: f64, prop: String, v: Value<'js>| {
                let v = ScriptValue::from_js(&ctx, &v);
                h.with(|h| h.set(id as LayerId, &prop, v));
            },
        )?,
    )?;
    let h = host.clone();
    n.set(
        "invoke",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>,
                  id: f64,
                  target: String,
                  method: String,
                  args: Array<'js>|
                  -> rquickjs::Result<Value<'js>> {
                let args: Vec<ScriptValue> = args
                    .iter::<Value>()
                    .map(|a| a.map(|a| ScriptValue::from_js(&ctx, &a)))
                    .collect::<rquickjs::Result<_>>()?;
                match h
                    .with(|h| h.invoke(id as LayerId, &target, &method, &args))
                    .flatten()
                {
                    Some(v) => v.to_js(&ctx, &wpe(&ctx)?),
                    None => Ok(Value::new_undefined(ctx)),
                }
            },
        )?,
    )?;
    let h = host.clone();
    n.set(
        "create",
        Function::new(ctx.clone(), move |json: String| -> f64 {
            let config = serde_json::from_str(&json).unwrap_or(serde_json::Value::Null);
            h.with(|h| h.create_layer(&config))
                .flatten()
                .map_or(-1.0, f64::from)
        })?,
    )?;
    let h = host.clone();
    n.set(
        "destroy",
        Function::new(ctx.clone(), move |id: f64| {
            h.with(|h| h.destroy_layer(id as LayerId));
        })?,
    )?;
    let h = host.clone();
    n.set(
        "sort",
        Function::new(ctx.clone(), move |id: f64, index: f64| {
            h.with(|h| h.sort_layer(id as LayerId, index.max(0.0) as usize));
        })?,
    )?;
    let h = host.clone();
    n.set(
        "parent",
        Function::new(ctx.clone(), move |id: f64, parent: f64| {
            let parent = (parent >= 0.0).then_some(parent as LayerId);
            h.with(|h| h.set_parent(id as LayerId, parent));
        })?,
    )?;
    let h = host.clone();
    n.set(
        "sceneGet",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, prop: String| -> rquickjs::Result<Value<'js>> {
                match h.with(|h| h.scene_get(&prop)).flatten() {
                    Some(v) => v.to_js(&ctx, &wpe(&ctx)?),
                    None => Ok(Value::new_undefined(ctx)),
                }
            },
        )?,
    )?;
    let h = host.clone();
    n.set(
        "sceneSet",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, prop: String, v: Value<'js>| {
                let v = ScriptValue::from_js(&ctx, &v);
                h.with(|h| h.scene_set(&prop, v));
            },
        )?,
    )?;
    let h = host.clone();
    n.set(
        "audio",
        Function::new(ctx.clone(), move |n: f64| -> Vec<f64> {
            let n = (n as usize).clamp(1, 64);
            let mut left = vec![0.0f32; n];
            let mut right = vec![0.0f32; n];
            h.with(|h| h.audio_spectrum(&mut left, &mut right));
            left.iter().chain(&right).map(|&v| f64::from(v)).collect()
        })?,
    )?;
    let h = host.clone();
    n.set(
        "storage",
        Function::new(ctx.clone(), move |json: String| {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&json) {
                h.with(|h| h.storage_changed(&data));
            }
        })?,
    )?;
    let h = host;
    n.set(
        "log",
        Function::new(ctx.clone(), move |level: f64, msg: String| {
            let level = match level as i32 {
                0 => LogLevel::Info,
                1 => LogLevel::Warn,
                _ => LogLevel::Error,
            };
            if h.with(|h| h.log(level, &msg)).is_none() {
                tracing::info!(target: "scenescript", "{msg}");
            }
        })?,
    )?;
    ctx.globals().set("__native", n)
}

#[cfg(test)]
mod tests;
