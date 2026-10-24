use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde_json::Value;
use wpe_assets::Project;
use wpe_assets::value::{Dynamic, UserBinding};

/// Current values of a wallpaper's user properties (`project.json`
/// `general.properties`), against which `{"user": ...}` bindings resolve.
#[derive(Debug, Clone, Default)]
pub struct UserProperties {
    values: HashMap<String, Value>,
}

/// How WE compares a property against a binding condition: as text, with
/// whole numbers written without a fraction.
fn as_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
            _ => n.to_string(),
        },
        other => other.to_string(),
    }
}

fn truthy(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0),
        Value::String(s) => match s.trim() {
            "true" | "1" => Some(true),
            "false" | "0" | "" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

impl UserProperties {
    /// The defaults declared in `project.json`.
    pub fn from_project(project: &Project) -> Self {
        Self {
            values: project
                .general
                .properties
                .iter()
                .map(|(name, p)| (name.clone(), p.value.clone()))
                .collect(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.values.get(name)
    }

    pub fn set(&mut self, name: impl Into<String>, value: Value) {
        self.values.insert(name.into(), value);
    }

    fn condition_holds(&self, name: &str, condition: &str) -> Option<bool> {
        let current = as_text(self.values.get(name)?);
        Some(condition.split('|').any(|c| c.trim() == current))
    }

    /// `d`'s value with a `{"user": "name"}` binding resolved.
    pub fn value<T: DeserializeOwned + Default + Clone>(&self, d: &Dynamic<T>) -> T {
        match &d.user {
            Some(UserBinding::Name(name)) => self
                .values
                .get(name)
                .and_then(|v| Dynamic::<T>::from_value(v.clone()).ok())
                .map_or_else(|| d.value.clone(), |r| r.value),
            _ => d.value.clone(),
        }
    }

    /// A boolean (visibility) value with name and condition bindings resolved.
    pub fn flag(&self, d: &Dynamic<bool>) -> bool {
        match &d.user {
            Some(UserBinding::Name(name)) => {
                self.values.get(name).and_then(truthy).unwrap_or(d.value)
            }
            Some(UserBinding::Condition { name, condition }) => {
                self.condition_holds(name, condition).unwrap_or(d.value)
            }
            None => d.value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wpe_assets::value::Vec3;

    fn props() -> UserProperties {
        let mut p = UserProperties::default();
        p.set("pose", Value::from(1));
        p.set("show", Value::from(false));
        p.set("tint", Value::from("1 0 0"));
        p
    }

    fn dynamic<T: DeserializeOwned + Default>(json: &str) -> Dynamic<T> {
        Dynamic::from_value(serde_json::from_str(json).unwrap()).unwrap()
    }

    #[test]
    fn resolves_bindings() {
        let p = props();
        assert!(p.flag(&dynamic(
            r#"{"user":{"name":"pose","condition":"1"},"value":false}"#
        )));
        assert!(!p.flag(&dynamic(
            r#"{"user":{"name":"pose","condition":"2"},"value":true}"#
        )));
        assert!(p.flag(&dynamic(
            r#"{"user":{"name":"pose","condition":"0|1"},"value":false}"#
        )));
        assert!(!p.flag(&dynamic(r#"{"user":"show","value":true}"#)));
        assert!(p.flag(&dynamic(r#"{"user":"missing","value":true}"#)));
        let tint: Dynamic<Vec3> = dynamic(r#"{"user":"tint","value":"1 1 1"}"#);
        assert_eq!(*p.value(&tint), [1.0, 0.0, 0.0]);
    }
}
