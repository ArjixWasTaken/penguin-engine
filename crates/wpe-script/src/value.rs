use rquickjs::{Ctx, Object, Value};

/// A value crossing the script boundary.
///
/// Vectors are the WE `Vec2`/`Vec3`/`Vec4` classes on the JS side; anything
/// that is not a scalar or vector (arrays, plain objects) travels as JSON.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptValue {
    /// `undefined` / `null`.
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Vec2([f64; 2]),
    Vec3([f64; 3]),
    Vec4([f64; 4]),
    /// Arrays and plain objects (e.g. a `createLayer` configuration or a 16-float matrix).
    Json(serde_json::Value),
}

/// Shape of a scripted property, fixed by its initial value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Bool,
    Number,
    String,
    Vec2,
    Vec3,
    Vec4,
    /// No coercion: whatever the script returns is passed on.
    Any,
}

impl ScriptValue {
    /// The kind a property holding this value has.
    pub fn kind(&self) -> ValueKind {
        match self {
            Self::Bool(_) => ValueKind::Bool,
            Self::Number(_) => ValueKind::Number,
            Self::String(_) => ValueKind::String,
            Self::Vec2(_) => ValueKind::Vec2,
            Self::Vec3(_) => ValueKind::Vec3,
            Self::Vec4(_) => ValueKind::Vec4,
            Self::Null | Self::Json(_) => ValueKind::Any,
        }
    }

    /// Scalar view: numbers, booleans (0/1) and the first vector component.
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Self::Number(n) => Some(n),
            Self::Bool(b) => Some(if b { 1.0 } else { 0.0 }),
            Self::Vec2(v) => Some(v[0]),
            Self::Vec3(v) => Some(v[0]),
            Self::Vec4(v) => Some(v[0]),
            _ => None,
        }
    }

    /// Truthiness as JavaScript would see it.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            Self::Number(n) => Some(*n != 0.0 && !n.is_nan()),
            Self::String(s) => Some(!s.is_empty()),
            Self::Null => None,
            _ => Some(true),
        }
    }

    /// String contents of a [`ScriptValue::String`].
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// Components of a vector or a broadcast scalar, padded with zeros to four.
    pub fn components(&self) -> Option<[f64; 4]> {
        Some(match *self {
            Self::Number(n) => [n; 4],
            Self::Vec2([x, y]) => [x, y, 0.0, 0.0],
            Self::Vec3([x, y, z]) => [x, y, z, 0.0],
            Self::Vec4(v) => v,
            _ => return None,
        })
    }

    /// Interpret a scene-JSON literal (`"1 0.5 0"`, `0.5`, `true`, ...) as the
    /// initial value of the scripted `property`.
    ///
    /// `text` and the text-layer string properties stay strings; `origin`,
    /// `scale`, `angles` and colours become `Vec3`; `parallaxDepth` a `Vec2`.
    /// Otherwise numeric strings become vectors by component count.
    pub fn from_scene_json(property: &str, value: &serde_json::Value) -> Self {
        let leaf = property.rsplit('/').next().unwrap_or(property);
        let want = match leaf {
            "text" | "font" | "horizontalalign" | "verticalalign" | "alignment" | "anchor" => {
                Some(ValueKind::String)
            }
            "visible" => Some(ValueKind::Bool),
            "origin" | "scale" | "angles" | "color" | "backgroundcolor" => Some(ValueKind::Vec3),
            "parallaxDepth" => Some(ValueKind::Vec2),
            "alpha" | "pointsize" | "maxwidth" | "padding" => Some(ValueKind::Number),
            _ => None,
        };
        let raw = match value {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(b) => Self::Bool(*b),
            serde_json::Value::Number(n) => Self::Number(n.as_f64().unwrap_or(0.0)),
            serde_json::Value::String(s) if want == Some(ValueKind::String) => {
                Self::String(s.clone())
            }
            serde_json::Value::String(s) => {
                parse_floats(s).unwrap_or_else(|| Self::String(s.clone()))
            }
            other => Self::Json(other.clone()),
        };
        match want {
            Some(kind) => raw.coerce(kind, None).unwrap_or(raw),
            None => raw,
        }
    }

    /// Convert to `kind`; components a shorter vector lacks come from
    /// `current` (or zero). `None` when the value cannot be coerced.
    pub fn coerce(&self, kind: ValueKind, current: Option<&ScriptValue>) -> Option<Self> {
        let fill = current.and_then(Self::components).unwrap_or([0.0; 4]);
        let comps = || -> Option<[f64; 4]> {
            let mut out = fill;
            match *self {
                Self::Number(v) => out = [v; 4],
                Self::Bool(b) => out = [if b { 1.0 } else { 0.0 }; 4],
                Self::Vec2(v) => out[..2].copy_from_slice(&v),
                Self::Vec3(v) => out[..3].copy_from_slice(&v),
                Self::Vec4(v) => out = v,
                Self::String(ref s) => match parse_floats(s)? {
                    Self::Number(v) => out = [v; 4],
                    other => return other.coerce(kind, current).and_then(|c| c.components()),
                },
                _ => return None,
            }
            Some(out)
        };
        Some(match kind {
            ValueKind::Any => self.clone(),
            ValueKind::Bool => Self::Bool(match self {
                Self::Bool(b) => *b,
                Self::Number(n) => *n != 0.0,
                _ => return None,
            }),
            ValueKind::Number => Self::Number(match self {
                Self::String(s) => s.trim().parse().ok()?,
                other => other.as_f64()?,
            }),
            ValueKind::String => match self {
                Self::String(s) => Self::String(s.clone()),
                Self::Number(n) => Self::String(format_number(*n)),
                Self::Bool(b) => Self::String(b.to_string()),
                _ => return None,
            },
            ValueKind::Vec2 => {
                let c = comps()?;
                Self::Vec2([c[0], c[1]])
            }
            ValueKind::Vec3 => {
                let c = comps()?;
                Self::Vec3([c[0], c[1], c[2]])
            }
            ValueKind::Vec4 => Self::Vec4(comps()?),
        })
    }

    /// The value as JSON (vectors as `"x y z"` strings, WE's scene encoding).
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        let join = |c: &[f64]| {
            J::String(
                c.iter()
                    .map(|v| format_number(*v))
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        };
        match self {
            Self::Null => J::Null,
            Self::Bool(b) => J::Bool(*b),
            Self::Number(n) => serde_json::Number::from_f64(*n).map_or(J::Null, J::Number),
            Self::String(s) => J::String(s.clone()),
            Self::Vec2(v) => join(v),
            Self::Vec3(v) => join(v),
            Self::Vec4(v) => join(v),
            Self::Json(j) => j.clone(),
        }
    }

    pub(crate) fn to_js<'js>(
        &self,
        ctx: &Ctx<'js>,
        wpe: &Object<'js>,
    ) -> rquickjs::Result<Value<'js>> {
        use rquickjs::{Function, IntoJs};
        let vec = |n: i32, c: [f64; 4]| -> rquickjs::Result<Value<'js>> {
            let f: Function = wpe.get("vec")?;
            f.call((n, c[0], c[1], c[2], c[3]))
        };
        match self {
            Self::Null => Ok(Value::new_undefined(ctx.clone())),
            Self::Bool(b) => b.into_js(ctx),
            Self::Number(n) => n.into_js(ctx),
            Self::String(s) => s.as_str().into_js(ctx),
            Self::Vec2([x, y]) => vec(2, [*x, *y, 0.0, 0.0]),
            Self::Vec3([x, y, z]) => vec(3, [*x, *y, *z, 0.0]),
            Self::Vec4(v) => vec(4, *v),
            Self::Json(j) => ctx.json_parse(j.to_string()),
        }
    }

    pub(crate) fn from_js<'js>(ctx: &Ctx<'js>, v: &Value<'js>) -> Self {
        if v.is_undefined() || v.is_null() {
            return Self::Null;
        }
        if let Some(b) = v.as_bool() {
            return Self::Bool(b);
        }
        if let Some(n) = v.as_number() {
            return Self::Number(n);
        }
        if let Some(s) = v.as_string() {
            return Self::String(s.to_string().unwrap_or_default());
        }
        if let Some(obj) = v.as_object() {
            if !obj.is_array() {
                let num = |k: &str| obj.get::<_, Value>(k).ok().and_then(|v| v.as_number());
                if let (Some(x), Some(y)) = (num("x"), num("y")) {
                    return match (num("z"), num("w")) {
                        (Some(z), Some(w)) => Self::Vec4([x, y, z, w]),
                        (Some(z), None) => Self::Vec3([x, y, z]),
                        _ => Self::Vec2([x, y]),
                    };
                }
            }
            if obj.as_function().is_some() {
                return Self::Null;
            }
            let json = ctx
                .json_stringify(v.clone())
                .ok()
                .flatten()
                .and_then(|s| s.to_string().ok());
            if let Some(j) = json.and_then(|s| serde_json::from_str(&s).ok()) {
                return Self::Json(j);
            }
        }
        Self::Null
    }
}

fn parse_floats(s: &str) -> Option<ScriptValue> {
    let parts: Vec<f64> = s
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    Some(match parts[..] {
        [x] => ScriptValue::Number(x),
        [x, y] => ScriptValue::Vec2([x, y]),
        [x, y, z] => ScriptValue::Vec3([x, y, z]),
        [x, y, z, w, ..] => ScriptValue::Vec4([x, y, z, w]),
        [] => return None,
    })
}

/// Format like JavaScript's `Number.prototype.toString` for the common cases.
fn format_number(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        if n > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else if n == n.trunc() && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scene_literals() {
        assert_eq!(
            ScriptValue::from_scene_json("origin", &json!("1 2 3")),
            ScriptValue::Vec3([1.0, 2.0, 3.0])
        );
        assert_eq!(
            ScriptValue::from_scene_json("text", &json!("12 34")),
            ScriptValue::String("12 34".into())
        );
        assert_eq!(
            ScriptValue::from_scene_json("scale", &json!(2)),
            ScriptValue::Vec3([2.0; 3])
        );
        assert_eq!(
            ScriptValue::from_scene_json(
                "effects/0/passes/0/constantshadervalues/color",
                &json!("1 0 0")
            ),
            ScriptValue::Vec3([1.0, 0.0, 0.0])
        );
    }

    #[test]
    fn coercion_fills_missing_components() {
        let cur = ScriptValue::Vec3([0.0, 0.0, 5.0]);
        assert_eq!(
            ScriptValue::Vec2([1.0, 2.0]).coerce(ValueKind::Vec3, Some(&cur)),
            Some(ScriptValue::Vec3([1.0, 2.0, 5.0]))
        );
        assert_eq!(
            ScriptValue::Number(3.0).coerce(ValueKind::String, None),
            Some(ScriptValue::String("3".into()))
        );
    }
}
