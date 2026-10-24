use crate::ScriptValue;

/// Scene-side identifier of a layer (object).
pub type LayerId = i32;

/// What kind of object a layer is; scripts mostly treat them alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayerKind {
    #[default]
    Image,
    Text,
    Model,
    Particle,
    Sound,
    Light,
    Group,
    Other,
}

impl LayerKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Text => "text",
            Self::Model => "model",
            Self::Particle => "particle",
            Self::Sound => "sound",
            Self::Light => "light",
            Self::Group => "group",
            Self::Other => "other",
        }
    }
}

/// One layer as scripts see it through `thisScene`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LayerInfo {
    pub id: LayerId,
    pub name: String,
    pub kind: LayerKind,
    pub parent: Option<LayerId>,
    /// Effect names in order; `getEffect(name | index)` resolves against this.
    pub effects: Vec<String>,
}

impl LayerInfo {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "kind": self.kind.as_str(),
            "parent": self.parent,
            "effects": self.effects,
        })
    }
}

/// Severity of a script `console` message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// The renderer side of SceneScript.
///
/// Layer list order is the scene's draw order (`thisScene.getLayer(index)`,
/// `getLayerIndex`, `sortLayer`). Property names follow scene JSON:
///
/// * transforms and common state: `origin`, `scale`, `angles` (Vec3),
///   `visible` (Bool), `alpha` (Number), `color` (Vec3), `parallaxDepth` (Vec2),
///   `size` (Vec2, read-only), `perspective`, `solid`;
/// * text layers: `text`, `pointsize`, `font`, `horizontalalign`,
///   `verticalalign`, `alignment`, `anchor`, `padding`, `maxwidth`, `maxrows`,
///   `limitrows`, `limitwidth`, `opaquebackground`, `backgroundcolor`;
/// * sound layers: `volume`;
/// * effects: `effects/<i>/visible`; material uniforms
///   `material/<name>` and `effects/<i>/material/<name>`;
/// * particles: `instanceoverride/<name>` (`rate`, `count`, `controlpoint0`, ...);
/// * read-only extras: `worldOrigin`, `worldScale` (Vec3) and `transform`
///   (16-float JSON array, column-major).
///
/// Scripted property names passed to [`crate::ScriptRuntime::add_property_script`]
/// come back through [`SceneHost::get`] / [`SceneHost::set`] unchanged, so a
/// host can use the scene-JSON path (`effects/0/passes/0/constantshadervalues/strength`).
pub trait SceneHost {
    /// All layers in draw order.
    fn layers(&self) -> Vec<LayerInfo>;
    /// Current value of a layer property, `None` if unknown.
    fn get(&self, layer: LayerId, property: &str) -> Option<ScriptValue>;
    /// Apply a property write from a script.
    fn set(&mut self, layer: LayerId, property: &str, value: ScriptValue);

    /// Method calls on layer sub-objects. `target` is `""` for the layer
    /// itself (`play`, `stop`, `isPlaying`, bones, ...), `"video"`,
    /// `"textureAnimation"`, `"animation"`/`"animation:<name>"`,
    /// `"animationLayer:<key>"` or `"effect:<i>"`. Getter-style members
    /// (`frameCount`, `duration`, `isPlaying`, `getFrame`) are calls too.
    /// `None` makes the script see a neutral default.
    fn invoke(
        &mut self,
        layer: LayerId,
        target: &str,
        method: &str,
        args: &[ScriptValue],
    ) -> Option<ScriptValue> {
        let _ = (layer, target, method, args);
        None
    }
    /// `thisScene.createLayer`: a model/image path string or a configuration
    /// object (`{image, origin, scale, angles, alpha, visible, ...}`, vectors as
    /// `"x y z"` strings). Returns the new layer's id.
    fn create_layer(&mut self, config: &serde_json::Value) -> Option<LayerId> {
        let _ = config;
        None
    }
    fn destroy_layer(&mut self, layer: LayerId) {
        let _ = layer;
    }
    /// Move `layer` to draw-order `index`.
    fn sort_layer(&mut self, layer: LayerId, index: usize) {
        let _ = (layer, index);
    }
    /// Re-parent `layer`, optionally to a named `attachment` of the parent;
    /// with `adjust` it keeps its place in the world.
    fn set_parent(
        &mut self,
        layer: LayerId,
        parent: Option<LayerId>,
        attachment: Option<&str>,
        adjust: bool,
    ) {
        let _ = (layer, parent, attachment, adjust);
    }
    /// `engine.openUserShortcut(name)`: open what the user bound to the
    /// `usershortcut` property `name`. Returns whether something opened.
    fn open_user_shortcut(&mut self, name: &str) -> bool {
        let _ = name;
        false
    }
    /// Scene-wide properties (`camerashake`, `bloomstrength`, `clearcolor`,
    /// `cameratransforms`, ...).
    fn scene_get(&self, property: &str) -> Option<ScriptValue> {
        let _ = property;
        None
    }
    fn scene_set(&mut self, property: &str, value: ScriptValue) {
        let _ = (property, value);
    }
    /// `engine.canvasSize`: the scene's authored ortho size.
    fn canvas_size(&self) -> [f32; 2];
    /// `engine.screenResolution`: output size in pixels.
    fn screen_resolution(&self) -> [f32; 2] {
        self.canvas_size()
    }
    /// Fill `left`/`right` (both `resolution` long: 16, 32 or 64) with the
    /// current audio spectrum.
    fn audio_spectrum(&self, left: &mut [f32], right: &mut [f32]) {
        left.fill(0.0);
        right.fill(0.0);
    }
    /// `CursorEvent.hitBox` for a pointer at scene position `world` over
    /// puppet `layer`: the name (or index) of the bone it's on.
    fn hit_box(&self, layer: LayerId, world: [f32; 2]) -> Option<String> {
        let _ = (layer, world);
        None
    }
    /// `localStorage` changed: `{"global": {...}, "screen": {...}}`, to keep
    /// for [`crate::ScriptRuntime::set_local_storage`] next time.
    fn storage_changed(&mut self, data: &serde_json::Value) {
        let _ = data;
    }
    /// A `console.*` message.
    fn log(&mut self, level: LogLevel, message: &str) {
        match level {
            LogLevel::Info => tracing::info!(target: "scenescript", "{message}"),
            LogLevel::Warn => tracing::warn!(target: "scenescript", "{message}"),
            LogLevel::Error => tracing::error!(target: "scenescript", "{message}"),
        }
    }
}

/// Per-frame cursor state for `input`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CursorState {
    /// Scene coordinates (Y up), as layer origins.
    pub world: [f32; 2],
    /// Output pixels (Y down), as `engine.screenResolution`.
    pub screen: [f32; 2],
    pub left_down: bool,
}

/// What happened to the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorEventKind {
    Move,
    Down,
    Up,
}

/// A pointer event with the host's hit test.
#[derive(Debug, Clone, PartialEq)]
pub struct CursorEvent {
    pub kind: CursorEventKind,
    pub world: [f32; 2],
    pub screen: [f32; 2],
    /// Layers under the cursor, topmost first. Only layers in
    /// [`crate::ScriptRuntime::interactive_layers`] matter.
    pub hits: Vec<LayerId>,
}

impl CursorEvent {
    /// An event of `kind` at the given position.
    pub fn new(
        kind: CursorEventKind,
        world: [f32; 2],
        screen: [f32; 2],
        hits: Vec<LayerId>,
    ) -> Self {
        Self {
            kind,
            world,
            screen,
            hits,
        }
    }
}
