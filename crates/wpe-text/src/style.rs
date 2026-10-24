use serde::de::DeserializeOwned;
use serde_json::Value;
use wpe_assets::scene::TextObject;
use wpe_assets::value::{Dynamic, UserBinding, Vec2, Vec3};

/// Horizontal alignment of lines within the text block, and which card edge
/// sits on the object origin.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum HAlign {
    Left,
    #[default]
    Center,
    Right,
}

/// Vertical alignment: which card edge sits on the object origin.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum VAlign {
    Top,
    #[default]
    Center,
    Bottom,
}

impl HAlign {
    /// Parse a scene `horizontalalign` value; unknown values centre.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "left" => Self::Left,
            "right" => Self::Right,
            _ => Self::Center,
        }
    }

    /// Position of the aligned edge in `[0, 1]` (left = 0).
    pub fn factor(self) -> f32 {
        match self {
            Self::Left => 0.0,
            Self::Center => 0.5,
            Self::Right => 1.0,
        }
    }
}

impl VAlign {
    /// Parse a scene `verticalalign` value; unknown values centre.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "top" => Self::Top,
            "bottom" => Self::Bottom,
            _ => Self::Center,
        }
    }

    /// Position of the aligned edge in `[0, 1]` (top = 0).
    pub fn factor(self) -> f32 {
        match self {
            Self::Top => 0.0,
            Self::Center => 0.5,
            Self::Bottom => 1.0,
        }
    }
}

/// A blurred, offset copy of the glyphs drawn beneath them
/// (`dropshadow*` fields).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DropShadow {
    /// Linear RGB.
    pub color: [f32; 3],
    /// Offset in scene units; +x right, +y down.
    pub offset: [f32; 2],
    pub opacity: f32,
    /// Blur radius in scene units.
    pub size: f32,
}

/// Everything about a text layer that affects its rasterised image, with
/// user-property bindings already resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    /// Font path inside the VFS (`fonts/foo.ttf`) or a `systemfont_<name>` alias.
    pub font: String,
    pub pointsize: f32,
    /// Scene units per point; [`crate::POINT_TO_UNITS`] by default.
    pub point_scale: f32,
    /// Glyph colour baked into the image's RGB.
    pub color: [f32; 3],
    /// Layer opacity. Not baked: apply it as `g_Color4.a` / `g_UserAlpha`.
    pub alpha: f32,
    /// Not baked: apply it as a colour multiplier (`g_Brightness`).
    pub brightness: f32,
    pub halign: HAlign,
    pub valign: VAlign,
    /// The scene's `anchor` field, kept for the caller; it does not affect layout.
    pub anchor: Option<String>,
    /// Empty space around the text block on each side, in scene units (x, y).
    pub padding: [f32; 2],
    /// Extra letter spacing (x, scene units) and line spacing (y, scene units).
    pub spacing: [f32; 2],
    /// The editor-cached card size from the scene file; layout recomputes it.
    pub authored_size: Option<[f32; 2]>,
    /// Wrap lines wider than this many scene units (`limitwidth` + `maxwidth`).
    pub max_width: Option<f32>,
    /// Keep at most this many lines (`limitrows` + `maxrows`).
    pub max_rows: Option<u32>,
    /// Mark text cut by `max_width` / `max_rows` with an ellipsis.
    pub ellipsis: bool,
    /// Opaque background fill (linear RGB, brightness applied) baked under the text.
    pub background: Option<[f32; 3]>,
    pub drop_shadow: Option<DropShadow>,
    /// Texture pixels per scene unit. Raise it for text that is scaled up or
    /// viewed on a display denser than the scene's ortho projection.
    pub raster_scale: f32,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font: String::new(),
            pointsize: 32.0,
            point_scale: crate::POINT_TO_UNITS,
            color: [1.0; 3],
            alpha: 1.0,
            brightness: 1.0,
            halign: HAlign::Center,
            valign: VAlign::Center,
            anchor: None,
            padding: [0.0; 2],
            spacing: [0.0; 2],
            authored_size: None,
            max_width: None,
            max_rows: None,
            ellipsis: false,
            background: None,
            drop_shadow: None,
            raster_scale: 1.0,
        }
    }
}

/// Resolve `d` against the user properties exposed by `user`.
fn resolve<T: DeserializeOwned + Default + Clone>(
    d: &Dynamic<T>,
    user: &impl Fn(&str) -> Option<Value>,
) -> T {
    match &d.user {
        Some(UserBinding::Name(name)) => user(name)
            .and_then(|v| Dynamic::<T>::from_value(v).ok())
            .map_or_else(|| d.value.clone(), |r| r.value),
        _ => d.value.clone(),
    }
}

/// An unmodelled field from [`TextObject::extra`], resolved like a typed one.
fn extra<T: DeserializeOwned + Default + Clone>(
    obj: &TextObject,
    key: &str,
    user: &impl Fn(&str) -> Option<Value>,
) -> Option<T> {
    let d = Dynamic::<T>::from_value(obj.extra.get(key)?.clone()).ok()?;
    Some(resolve(&d, user))
}

fn flag(obj: &TextObject, key: &str, user: &impl Fn(&str) -> Option<Value>) -> bool {
    match obj.extra.get(key) {
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(_) => extra::<bool>(obj, key, user).unwrap_or(false),
        None => false,
    }
}

/// The text object's string with a user-property binding resolved. Script
/// driven text yields the script's authored default.
pub fn resolve_text(obj: &TextObject, user: impl Fn(&str) -> Option<Value>) -> String {
    match &obj.text.user {
        Some(UserBinding::Name(name)) => match user(name) {
            Some(Value::String(s)) => s,
            Some(Value::Null) | None => obj.text.value.clone(),
            Some(other) => other.to_string(),
        },
        _ => obj.text.value.clone(),
    }
}

impl TextStyle {
    /// Build the style of `obj`. `user` returns the current value of a project
    /// user property by name (`None` keeps the authored default).
    pub fn from_object(obj: &TextObject, user: impl Fn(&str) -> Option<Value>) -> Self {
        let max_width = obj
            .maxwidth
            .as_ref()
            .map(|d| resolve(d, &user))
            .filter(|w| *w > 0.0 && flag(obj, "limitwidth", &user));
        let max_rows = obj
            .maxrows
            .filter(|r| *r >= 1.0 && flag(obj, "limitrows", &user))
            .map(|r| r as u32);
        let background = flag(obj, "opaquebackground", &user).then(|| {
            let c: Vec3 = extra(obj, "backgroundcolor", &user).unwrap_or_default();
            let b: f32 = extra(obj, "backgroundbrightness", &user).unwrap_or(1.0);
            c.map(|v| v * b)
        });
        let drop_shadow = flag(obj, "dropshadow", &user).then(|| DropShadow {
            color: extra::<Vec3>(obj, "dropshadowcolor", &user)
                .unwrap_or_default()
                .0,
            offset: extra::<Vec2>(obj, "dropshadowoffset", &user)
                .unwrap_or(Vec2::splat(4.0))
                .0,
            opacity: extra(obj, "dropshadowopacity", &user).unwrap_or(1.0),
            size: extra(obj, "dropshadowsize", &user).unwrap_or(6.0),
        });
        Self {
            font: obj.font.clone().unwrap_or_default(),
            pointsize: resolve(&obj.pointsize, &user),
            color: resolve(&obj.color, &user).0,
            alpha: resolve(&obj.alpha, &user),
            brightness: resolve(&obj.brightness, &user),
            halign: obj
                .horizontalalign
                .as_deref()
                .map(HAlign::parse)
                .unwrap_or_default(),
            valign: obj
                .verticalalign
                .as_deref()
                .map(VAlign::parse)
                .unwrap_or_default(),
            anchor: obj.anchor.clone(),
            padding: resolve(&obj.padding, &user).0.map(|p| p.max(0.0)),
            spacing: extra::<Vec2>(obj, "spacing", &user).unwrap_or_default().0,
            authored_size: obj.size.map(|s| s.0),
            max_width,
            max_rows,
            ellipsis: flag(obj, "limituseellipsis", &user),
            background,
            drop_shadow,
            ..Self::default()
        }
    }

    /// Glyph em size in scene units.
    pub fn font_size(&self) -> f32 {
        self.pointsize * self.point_scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wpe_assets::scene::SceneObject;

    fn text_object(json: &str) -> TextObject {
        match serde_json::from_str::<SceneObject>(json).unwrap() {
            SceneObject::Text(t) => *t,
            other => panic!("not a text object: {other:?}"),
        }
    }

    #[test]
    fn parses_scene_fields() {
        let obj = text_object(
            r#"{"id": 1, "text": {"value": "Hi", "user": "label"}, "font": "fonts/a.ttf",
                "pointsize": {"user": "size", "value": 30.0}, "horizontalalign": "left",
                "verticalalign": "bottom", "padding": "32.00000 16.00000",
                "maxwidth": {"value": 400.0}, "limitwidth": true, "maxrows": 2,
                "limitrows": true, "limituseellipsis": true, "opaquebackground": true,
                "backgroundcolor": "0.5 0 1", "backgroundbrightness": 2.0,
                "dropshadow": true, "dropshadowoffset": "2 3", "size": "100 50"}"#,
        );
        let user = |name: &str| match name {
            "size" => Some(Value::from(10.0)),
            "label" => Some(Value::from("Bound")),
            _ => None,
        };
        let style = TextStyle::from_object(&obj, user);
        assert_eq!(style.pointsize, 10.0);
        assert_eq!(style.halign, HAlign::Left);
        assert_eq!(style.valign, VAlign::Bottom);
        assert_eq!(style.padding, [32.0, 16.0]);
        assert_eq!(style.max_width, Some(400.0));
        assert_eq!(style.max_rows, Some(2));
        assert!(style.ellipsis);
        assert_eq!(style.background, Some([1.0, 0.0, 2.0]));
        assert_eq!(style.drop_shadow.unwrap().offset, [2.0, 3.0]);
        assert_eq!(style.authored_size, Some([100.0, 50.0]));
        assert_eq!(resolve_text(&obj, user), "Bound");
    }

    #[test]
    fn limits_need_their_flags() {
        let obj = text_object(
            r#"{"id": 1, "text": "x", "padding": 32, "maxwidth": 500.0, "maxrows": 1,
                "limitwidth": false, "limitrows": false}"#,
        );
        let style = TextStyle::from_object(&obj, |_| None);
        assert_eq!(style.padding, [32.0, 32.0]);
        assert_eq!(style.max_width, None);
        assert_eq!(style.max_rows, None);
        assert_eq!(style.background, None);
    }
}
