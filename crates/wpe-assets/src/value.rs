//! Wallpaper Engine's polymorphic JSON value encodings.
//!
//! Almost any scene or material field can be written in several shapes:
//!
//! ```text
//! 0.5                                   literal
//! "1 0.5 0"                             vector as a space-separated string
//! {"value": 0.5, "user": "speed"}       bound to a project user property
//! {"value": true, "user": {"name": "mode", "condition": "2"}}
//! {"value": "0 0 0", "script": "...", "scriptproperties": {...}}
//! {"value": 1, "animation": {"c0": [...], "options": {...}}}
//! ```
//!
//! [`Dynamic`] captures the literal value plus whichever bindings are present;
//! [`VecN`] and [`FloatVec`] parse the vector encodings.

use std::fmt;
use std::ops::{Deref, DerefMut};

use serde::de::{self, DeserializeOwned, Deserializer, SeqAccess, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};

/// Reference from a value to a project user property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserBinding {
    /// `"user": "propname"`: the property's value replaces the literal.
    Name(String),
    /// `"user": {"name": ..., "condition": ...}`: true when the property equals `condition`.
    Condition { name: String, condition: String },
}

impl UserBinding {
    /// Name of the bound user property.
    pub fn name(&self) -> &str {
        match self {
            Self::Name(n) | Self::Condition { name: n, .. } => n,
        }
    }
}

impl<'de> Deserialize<'de> for UserBinding {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Cond {
            name: String,
            #[serde(default, deserialize_with = "lenient_string")]
            condition: String,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Name(String),
            Cond(Cond),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Name(n) => Self::Name(n),
            Raw::Cond(c) => Self::Condition {
                name: c.name,
                condition: c.condition,
            },
        })
    }
}

/// A value that may be bound to a user property, a script or a keyframe animation.
///
/// `value` holds the literal (or the binding's authored default). Script and
/// animation payloads are kept as raw JSON for later interpretation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dynamic<T> {
    pub value: T,
    pub user: Option<UserBinding>,
    /// JavaScript source of a SceneScript driving this value.
    pub script: Option<String>,
    pub script_properties: Option<Value>,
    pub animation: Option<Value>,
}

const BINDING_KEYS: [&str; 4] = ["value", "user", "script", "animation"];

impl<T> Dynamic<T> {
    /// A plain literal with no bindings.
    pub fn new(value: T) -> Self {
        Self {
            value,
            user: None,
            script: None,
            script_properties: None,
            animation: None,
        }
    }

    /// Whether anything other than the literal drives this value.
    pub fn is_bound(&self) -> bool {
        self.user.is_some() || self.script.is_some() || self.animation.is_some()
    }
}

impl<T> From<T> for Dynamic<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: DeserializeOwned + Default> Dynamic<T> {
    /// Interpret an already-parsed JSON value. `null` yields `T::default()`.
    pub fn from_value(v: Value) -> Result<Self, serde_json::Error> {
        let mut map = match v {
            Value::Null => return Ok(Self::new(T::default())),
            Value::Object(map) if BINDING_KEYS.iter().any(|k| map.contains_key(*k)) => map,
            other => return T::deserialize(other).map(Self::new),
        };
        let value = match map.remove("value") {
            None | Some(Value::Null) => T::default(),
            Some(v) => T::deserialize(v)?,
        };
        let user = match map.remove("user") {
            None | Some(Value::Null) => None,
            Some(v) => Some(UserBinding::deserialize(v)?),
        };
        let script = match map.remove("script") {
            Some(Value::String(s)) => Some(s),
            _ => None,
        };
        Ok(Self {
            value,
            user,
            script,
            script_properties: map.remove("scriptproperties"),
            animation: map.remove("animation").filter(|a| !a.is_null()),
        })
    }
}

impl<'de, T: DeserializeOwned + Default> Deserialize<'de> for Dynamic<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::from_value(Value::deserialize(d)?).map_err(de::Error::custom)
    }
}

impl<T: Serialize> Serialize for Dynamic<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if !self.is_bound() {
            return self.value.serialize(s);
        }
        let mut map = Map::new();
        let to_json = |v: &T| serde_json::to_value(v).map_err(serde::ser::Error::custom);
        map.insert("value".into(), to_json(&self.value)?);
        if let Some(u) = &self.user {
            let u = match u {
                UserBinding::Name(n) => Value::String(n.clone()),
                UserBinding::Condition { name, condition } => {
                    serde_json::json!({ "name": name, "condition": condition })
                }
            };
            map.insert("user".into(), u);
        }
        if let Some(sc) = &self.script {
            map.insert("script".into(), Value::String(sc.clone()));
        }
        if let Some(p) = &self.script_properties {
            map.insert("scriptproperties".into(), p.clone());
        }
        if let Some(a) = &self.animation {
            map.insert("animation".into(), a.clone());
        }
        map.serialize(s)
    }
}

/// Fixed-size float vector, e.g. a position or colour.
///
/// Parsed from `"x y z"` strings, JSON arrays, or a single number broadcast to
/// every component. A string with a single component is broadcast too; extra
/// components are ignored and too few are an error.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VecN<const N: usize>(pub [f32; N]);

pub type Vec2 = VecN<2>;
pub type Vec3 = VecN<3>;
pub type Vec4 = VecN<4>;

impl<const N: usize> VecN<N> {
    /// Every component set to `v`.
    pub const fn splat(v: f32) -> Self {
        Self([v; N])
    }

    fn from_floats(v: &[f32]) -> Result<Self, String> {
        match v.len() {
            1 => Ok(Self::splat(v[0])),
            n if n >= N => Ok(Self(std::array::from_fn(|i| v[i]))),
            n => Err(format!("expected {N} components, got {n}")),
        }
    }
}

impl<const N: usize> Default for VecN<N> {
    fn default() -> Self {
        Self::splat(0.0)
    }
}

impl<const N: usize> From<[f32; N]> for VecN<N> {
    fn from(v: [f32; N]) -> Self {
        Self(v)
    }
}

impl<const N: usize> Deref for VecN<N> {
    type Target = [f32; N];
    fn deref(&self) -> &[f32; N] {
        &self.0
    }
}

impl<const N: usize> DerefMut for VecN<N> {
    fn deref_mut(&mut self) -> &mut [f32; N] {
        &mut self.0
    }
}

impl<'de, const N: usize> Deserialize<'de> for VecN<N> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let floats = FloatVec::deserialize(d)?;
        Self::from_floats(&floats).map_err(de::Error::custom)
    }
}

impl<const N: usize> Serialize for VecN<N> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        FloatVec(self.0.to_vec()).serialize(s)
    }
}

/// Variable-length float vector, used for shader constants.
///
/// Accepts a number, a bool (as 0/1), a space-separated string, or an array.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FloatVec(pub Vec<f32>);

impl Deref for FloatVec {
    type Target = Vec<f32>;
    fn deref(&self) -> &Vec<f32> {
        &self.0
    }
}

/// Parse a WE vector string. Components may be separated by spaces or commas.
pub fn parse_floats(s: &str) -> Result<Vec<f32>, String> {
    s.split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .map(|t| {
            t.parse::<f32>()
                .map_err(|_| format!("invalid vector component {t:?} in {s:?}"))
        })
        .collect()
}

impl<'de> Deserialize<'de> for FloatVec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = FloatVec;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a number, a vector string or an array of numbers")
            }
            fn visit_bool<E>(self, v: bool) -> Result<FloatVec, E> {
                Ok(FloatVec(vec![v as u8 as f32]))
            }
            fn visit_i64<E>(self, v: i64) -> Result<FloatVec, E> {
                Ok(FloatVec(vec![v as f32]))
            }
            fn visit_u64<E>(self, v: u64) -> Result<FloatVec, E> {
                Ok(FloatVec(vec![v as f32]))
            }
            fn visit_f64<E>(self, v: f64) -> Result<FloatVec, E> {
                Ok(FloatVec(vec![v as f32]))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<FloatVec, E> {
                parse_floats(v).map(FloatVec).map_err(E::custom)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<FloatVec, A::Error> {
                let mut out = Vec::new();
                while let Some(v) = seq.next_element::<f32>()? {
                    out.push(v);
                }
                Ok(FloatVec(out))
            }
        }
        d.deserialize_any(V)
    }
}

impl Serialize for FloatVec {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [v] => s.serialize_f32(*v),
            vs => {
                let parts: Vec<String> = vs.iter().map(f32::to_string).collect();
                s.serialize_str(&parts.join(" "))
            }
        }
    }
}

/// Deserialize a string, also accepting numbers and bools in their JSON spelling.
pub fn lenient_string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s,
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

/// [`lenient_string`] for optional fields; `null` becomes `None`.
pub fn lenient_opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Null => None,
        Value::String(s) => Some(s),
        other => Some(other.to_string()),
    })
}

/// Deserialize an integer, also accepting floats (truncated), bools and numeric strings.
pub fn lenient_i32<'de, D: Deserializer<'de>>(d: D) -> Result<i32, D::Error> {
    LenientI32::deserialize(d).map(|v| v.0)
}

/// An `i32` parsed with [`lenient_i32`] rules, for use as a map value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LenientI32(pub i32);

impl<'de> Deserialize<'de> for LenientI32 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        let n = match &v {
            Value::Bool(b) => Some(*b as i32),
            Value::Number(n) => n
                .as_i64()
                .map(|i| i as i32)
                .or_else(|| n.as_f64().map(|f| f as i32)),
            Value::String(s) => s.trim().parse::<f64>().ok().map(|f| f as i32),
            _ => None,
        };
        n.map(LenientI32)
            .ok_or_else(|| de::Error::custom(format!("expected an integer, got {v}")))
    }
}

/// Deserialize a bool, also accepting 0/1 numbers, `"true"`/`"false"` strings and `null` (false).
pub fn lenient_bool<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let v = Value::deserialize(d)?;
    match &v {
        Value::Null => Ok(false),
        Value::Bool(b) => Ok(*b),
        Value::Number(n) => Ok(n.as_f64().is_some_and(|f| f != 0.0)),
        Value::String(s) if s.eq_ignore_ascii_case("true") || s == "1" => Ok(true),
        Value::String(s) if s.eq_ignore_ascii_case("false") || s == "0" || s.is_empty() => {
            Ok(false)
        }
        _ => Err(de::Error::custom(format!("expected a bool, got {v}"))),
    }
}

/// Deserialize `T`, mapping `null` to `T::default()`.
pub fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dynamic<T: DeserializeOwned + Default>(json: &str) -> Dynamic<T> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn dynamic_literal() {
        let d: Dynamic<f32> = dynamic("0.5");
        assert_eq!(d.value, 0.5);
        assert!(!d.is_bound());
        assert!(dynamic::<bool>("true").value);
    }

    #[test]
    fn dynamic_user_binding() {
        let d: Dynamic<bool> = dynamic(r#"{"user": "showclock", "value": false}"#);
        assert!(!d.value);
        assert_eq!(d.user, Some(UserBinding::Name("showclock".into())));

        let d: Dynamic<bool> =
            dynamic(r#"{"user": {"condition": 2, "name": "mode"}, "value": true}"#);
        assert!(d.value);
        assert_eq!(
            d.user,
            Some(UserBinding::Condition {
                name: "mode".into(),
                condition: "2".into()
            })
        );
        assert_eq!(d.user.unwrap().name(), "mode");
    }

    #[test]
    fn dynamic_script_and_animation() {
        let d: Dynamic<Vec3> = dynamic(
            r#"{"script": "export function update(v) { return v; }",
                "scriptproperties": {"speed": 1}, "value": "1 2 3"}"#,
        );
        assert_eq!(d.value, VecN([1.0, 2.0, 3.0]));
        assert!(d.script.unwrap().contains("update"));
        assert_eq!(d.script_properties.unwrap()["speed"], 1);

        let d: Dynamic<f32> = dynamic(r#"{"animation": {"c0": []}}"#);
        assert_eq!(d.value, 0.0);
        assert!(d.animation.is_some());
    }

    #[test]
    fn dynamic_round_trips() {
        for json in [
            r#"0.5"#,
            r#"{"value":1.0,"user":"speed"}"#,
            r#"{"value":true,"user":{"name":"m","condition":"1"}}"#,
        ] {
            let d: Dynamic<Value> = dynamic(json);
            let back: Dynamic<Value> = dynamic(&serde_json::to_string(&d).unwrap());
            assert_eq!(d, back);
        }
    }

    #[test]
    fn vectors() {
        let v: Vec3 = serde_json::from_str(r#""1 0.5 0""#).unwrap();
        assert_eq!(v.0, [1.0, 0.5, 0.0]);
        let v: Vec3 = serde_json::from_str("2").unwrap();
        assert_eq!(v.0, [2.0; 3]);
        let v: Vec2 = serde_json::from_str(r#""3""#).unwrap();
        assert_eq!(v.0, [3.0; 2]);
        let v: Vec4 = serde_json::from_str("[1, 2, 3, 4]").unwrap();
        assert_eq!(v.0, [1.0, 2.0, 3.0, 4.0]);
        let v: Vec2 = serde_json::from_str(r#""1.5 2.5 0""#).unwrap();
        assert_eq!(v.0, [1.5, 2.5]);
        assert!(serde_json::from_str::<Vec3>(r#""1 2""#).is_err());
        assert!(serde_json::from_str::<Vec3>(r#""a b c""#).is_err());
    }

    #[test]
    fn float_vec() {
        let v: FloatVec = serde_json::from_str(r#""0.1 0.2 0.3 1""#).unwrap();
        assert_eq!(v.len(), 4);
        let v: FloatVec = serde_json::from_str("true").unwrap();
        assert_eq!(v.0, [1.0]);
        let d: Dynamic<FloatVec> = dynamic(r#"{"user": "color", "value": "1 0 0"}"#);
        assert_eq!(d.value.0, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn lenient_scalars() {
        #[derive(Deserialize)]
        struct S {
            #[serde(deserialize_with = "lenient_i32")]
            i: i32,
            #[serde(deserialize_with = "lenient_bool")]
            b: bool,
            #[serde(deserialize_with = "lenient_string")]
            s: String,
        }
        let s: S = serde_json::from_str(r#"{"i": 2.0, "b": 1, "s": 7}"#).unwrap();
        assert_eq!((s.i, s.b, s.s.as_str()), (2, true, "7"));
        let s: S = serde_json::from_str(r#"{"i": "3", "b": "false", "s": "x"}"#).unwrap();
        assert_eq!((s.i, s.b, s.s.as_str()), (3, false, "x"));
    }
}
