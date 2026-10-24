//! Lenient readers for particle JSON values.
//!
//! Wallpaper Engine writes vectors as `"1 2 3"` strings, scalars as numbers
//! or numeric strings, and wraps user-bound values as `{"user": .., "value": ..}`.

use glam::{Vec2, Vec3, Vec4};
use serde_json::{Map, Value};

/// Strips a `{"user": .., "value": ..}` binding down to its value.
pub(crate) fn unwrap(v: &Value) -> &Value {
    match v {
        Value::Object(m) if m.contains_key("value") => &m["value"],
        _ => v,
    }
}

/// Every float in `v`: a number, a whitespace/comma separated string or an array.
pub(crate) fn floats(v: &Value) -> Option<Vec<f32>> {
    match unwrap(v) {
        Value::Number(n) => n.as_f64().map(|f| vec![f as f32]),
        Value::Bool(b) => Some(vec![if *b { 1.0 } else { 0.0 }]),
        Value::String(s) => {
            let out: Option<Vec<f32>> = s
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|t| !t.is_empty())
                .map(|t| t.parse::<f32>().ok())
                .collect();
            out.filter(|v| !v.is_empty())
        }
        Value::Array(a) => a
            .iter()
            .map(|x| floats(x).and_then(|f| f.first().copied()))
            .collect::<Option<Vec<f32>>>()
            .filter(|v| !v.is_empty()),
        _ => None,
    }
}

/// Read-only view of a JSON object with typed, defaulting accessors.
#[derive(Clone, Copy)]
pub(crate) struct Obj<'a>(pub &'a Map<String, Value>);

static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);

impl<'a> Obj<'a> {
    /// View `v` as an object; non-objects read as empty.
    pub fn of(v: &'a Value) -> Self {
        match unwrap(v) {
            Value::Object(m) => Self(m),
            _ => Self(&EMPTY),
        }
    }

    /// Whether `key` is present and not null.
    pub fn has(&self, key: &str) -> bool {
        self.0.get(key).is_some_and(|v| !unwrap(v).is_null())
    }

    pub fn get(&self, key: &str) -> Option<&'a Value> {
        self.0.get(key).map(unwrap).filter(|v| !v.is_null())
    }

    pub fn f32_opt(&self, key: &str) -> Option<f32> {
        self.get(key)
            .and_then(floats)
            .and_then(|v| v.first().copied())
            .filter(|f| f.is_finite())
    }

    pub fn f32(&self, key: &str, default: f32) -> f32 {
        self.f32_opt(key).unwrap_or(default)
    }

    pub fn u32(&self, key: &str, default: u32) -> u32 {
        self.f32_opt(key)
            .map(|f| f.max(0.0).min(u32::MAX as f32) as u32)
            .unwrap_or(default)
    }

    pub fn i32(&self, key: &str, default: i32) -> i32 {
        self.f32_opt(key).map(|f| f as i32).unwrap_or(default)
    }

    pub fn bool(&self, key: &str, default: bool) -> bool {
        self.f32_opt(key).map(|f| f != 0.0).unwrap_or(default)
    }

    pub fn str(&self, key: &str) -> Option<&'a str> {
        self.get(key).and_then(Value::as_str)
    }

    /// Components of `key`; a single number broadcasts to every component.
    pub fn vec_n<const N: usize>(&self, key: &str) -> Option<[f32; N]> {
        let f = self.get(key).and_then(floats)?;
        let mut out = [0.0; N];
        for (i, o) in out.iter_mut().enumerate() {
            *o = if f.len() == 1 {
                f[0]
            } else {
                f.get(i).copied().unwrap_or(0.0)
            };
        }
        out.iter().all(|x| x.is_finite()).then_some(out)
    }

    pub fn vec2(&self, key: &str, default: Vec2) -> Vec2 {
        self.vec_n::<2>(key).map(Vec2::from).unwrap_or(default)
    }

    pub fn vec3_opt(&self, key: &str) -> Option<Vec3> {
        self.vec_n::<3>(key).map(Vec3::from)
    }

    pub fn vec3(&self, key: &str, default: Vec3) -> Vec3 {
        self.vec3_opt(key).unwrap_or(default)
    }

    pub fn vec4(&self, key: &str, default: Vec4) -> Vec4 {
        self.vec_n::<4>(key).map(Vec4::from).unwrap_or(default)
    }

    /// Elements of the array at `key`; anything else reads as empty.
    pub fn array(&self, key: &str) -> &'a [Value] {
        match self.get(key) {
            Some(Value::Array(a)) => a,
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_we_value_spellings() {
        let v = json!({
            "a": "1 2 3", "b": 4, "c": "5", "d": {"user": "x", "value": "0.5 0.25 1"},
            "e": [1, 2], "f": null, "g": true
        });
        let o = Obj::of(&v);
        assert_eq!(o.vec3("a", Vec3::ZERO), Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(o.vec3("b", Vec3::ZERO), Vec3::splat(4.0));
        assert_eq!(o.f32("c", 0.0), 5.0);
        assert_eq!(o.f32("a", 0.0), 1.0);
        assert_eq!(o.vec3("d", Vec3::ZERO), Vec3::new(0.5, 0.25, 1.0));
        assert_eq!(o.vec3("e", Vec3::ONE), Vec3::new(1.0, 2.0, 0.0));
        assert_eq!(o.f32("f", 7.0), 7.0);
        assert!(!o.has("f"));
        assert!(o.bool("g", false));
        assert_eq!(o.u32("missing", 9), 9);
    }
}
