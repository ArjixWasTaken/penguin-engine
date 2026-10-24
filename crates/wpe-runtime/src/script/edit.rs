//! Changes scripts make to the scene's structure, applied to the scene JSON
//! before the frame is rebuilt.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum SceneEdit {
    /// Add an object (its configuration, with a fresh `id`) on top.
    Create {
        config: Value,
    },
    Destroy {
        id: i32,
    },
    /// Move an object before another, or to the top.
    Move {
        id: i32,
        before: Option<i32>,
    },
    Parent {
        id: i32,
        parent: Option<i32>,
    },
    /// Set a field of an object's configuration.
    Set {
        id: i32,
        key: String,
        value: Value,
    },
}

fn id_of(obj: &Value) -> Option<i32> {
    obj.get("id")?.as_i64().map(|id| id as i32)
}

/// Point created objects' files at the creating script's workshop folder
/// when they aren't found as given (`models/bar.json` from workshop item 26
/// is `models/workshop/26/bar.json`), and drop the `__workshop` marker.
pub fn resolve_paths(edits: &mut [SceneEdit], exists: impl Fn(&str) -> bool) {
    for edit in edits {
        let SceneEdit::Create { config } = edit else {
            continue;
        };
        let Some(map) = config.as_object_mut() else {
            continue;
        };
        let Some(workshop) = map.remove("__workshop") else {
            continue;
        };
        let Some(workshop) = workshop.as_str() else {
            continue;
        };
        for key in ["image", "particle"] {
            let Some(Value::String(path)) = map.get_mut(key) else {
                continue;
            };
            if exists(path) {
                continue;
            }
            if let Some((dir, rest)) = path.split_once('/') {
                let candidate = format!("{dir}/workshop/{workshop}/{rest}");
                if exists(&candidate) {
                    *path = candidate;
                }
            }
        }
    }
}

/// Apply `edits` in order to the `objects` of scene JSON `doc`.
pub fn apply(doc: &mut Value, edits: &[SceneEdit]) {
    let Some(objects) = doc.get_mut("objects").and_then(Value::as_array_mut) else {
        return;
    };
    let position =
        |objects: &Vec<Value>, id: i32| objects.iter().position(|o| id_of(o) == Some(id));
    for edit in edits {
        match edit {
            SceneEdit::Create { config } => objects.push(config.clone()),
            SceneEdit::Destroy { id } => {
                if let Some(at) = position(objects, *id) {
                    objects.remove(at);
                }
            }
            SceneEdit::Move { id, before } => {
                let Some(from) = position(objects, *id) else {
                    continue;
                };
                let obj = objects.remove(from);
                let to = before
                    .and_then(|b| position(objects, b))
                    .unwrap_or(objects.len());
                objects.insert(to, obj);
            }
            SceneEdit::Parent { id, parent } => {
                let Some(obj) = position(objects, *id).and_then(|at| objects[at].as_object_mut())
                else {
                    continue;
                };
                match parent {
                    Some(p) => obj.insert("parent".to_owned(), (*p).into()),
                    None => obj.remove("parent"),
                };
            }
            SceneEdit::Set { id, key, value } => {
                if let Some(obj) = position(objects, *id).and_then(|at| objects[at].as_object_mut())
                {
                    obj.insert(key.clone(), value.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn workshop_paths_resolve_when_missing() {
        let mut edits = [
            SceneEdit::Create {
                config: json!({"image": "models/bar.json", "__workshop": "26"}),
            },
            SceneEdit::Create {
                config: json!({"image": "models/own.json", "__workshop": "26"}),
            },
        ];
        resolve_paths(&mut edits, |p| {
            p == "models/workshop/26/bar.json" || p == "models/own.json"
        });
        assert_eq!(
            edits[0],
            SceneEdit::Create {
                config: json!({"image": "models/workshop/26/bar.json"})
            }
        );
        assert_eq!(
            edits[1],
            SceneEdit::Create {
                config: json!({"image": "models/own.json"})
            }
        );
    }

    #[test]
    fn edits_reshape_the_object_list() {
        let mut doc = json!({"objects": [{"id": 1}, {"id": 2}, {"id": 3}]});
        apply(
            &mut doc,
            &[
                SceneEdit::Create {
                    config: json!({"id": 4, "image": "models/bar.json"}),
                },
                SceneEdit::Move {
                    id: 4,
                    before: Some(2),
                },
                SceneEdit::Destroy { id: 1 },
                SceneEdit::Parent {
                    id: 3,
                    parent: Some(2),
                },
                SceneEdit::Set {
                    id: 4,
                    key: "alignment".into(),
                    value: json!("bottom"),
                },
            ],
        );
        assert_eq!(
            doc,
            json!({"objects": [
                {"id": 4, "image": "models/bar.json", "alignment": "bottom"},
                {"id": 2},
                {"id": 3, "parent": 2},
            ]})
        );
    }
}
