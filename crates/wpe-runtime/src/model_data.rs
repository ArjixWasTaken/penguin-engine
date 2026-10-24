//! Custom geometry scripts make with `thisScene.createModelData`, drawn by
//! model layers created with `createLayer({model: data})`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::Value;
use wpe_render::MeshData;

/// What a `createLayer` `model` value names when it is script geometry.
pub const PREFIX: &str = "modeldata:";

/// The id of the script geometry `model` names, if it does.
pub fn id_of(model: &str) -> Option<i32> {
    model.strip_prefix(PREFIX)?.parse().ok()
}

/// Vertex components in buffer order, with their size and shader attribute.
const COMPONENTS: [(&str, usize, &str); 5] = [
    ("position", 3, "a_Position"),
    ("normal", 3, "a_Normal"),
    ("tangentSigned", 4, "a_Tangent4"),
    ("uv", 2, "a_TexCoord"),
    ("color", 4, "a_Color"),
];

/// One shape: interleaved vertices in `format` order, optional triangle
/// indices and the material drawing it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelShape {
    pub format: Vec<String>,
    pub vertices: Vec<f32>,
    pub indices: Option<Vec<u32>>,
    pub material: Option<String>,
}

impl ModelShape {
    /// Floats per vertex.
    pub fn stride(&self) -> usize {
        COMPONENTS
            .iter()
            .filter(|(name, ..)| self.format.iter().any(|f| f == name))
            .map(|&(_, n, _)| n)
            .sum()
    }

    /// Merge the members `config` gives (`vertexFormat`, `vertexBuffer`,
    /// `indexBuffer` (`null` removes it), `material`) into this shape.
    fn merge(&mut self, config: &Value) {
        let floats = |v: &Value| -> Vec<f32> {
            v.as_array()
                .map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect())
                .unwrap_or_default()
        };
        if let Some(format) = config.get("vertexFormat").and_then(Value::as_array) {
            self.format = format
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
        }
        if let Some(v) = config.get("vertexBuffer") {
            self.vertices = floats(v);
        }
        match config.get("indexBuffer") {
            Some(Value::Null) => self.indices = None,
            Some(v) => {
                self.indices = Some(
                    v.as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_u64)
                                .map(|i| i as u32)
                                .collect()
                        })
                        .unwrap_or_default(),
                )
            }
            None => {}
        }
        if let Some(m) = config.get("material").and_then(Value::as_str) {
            self.material = Some(m.to_owned()).filter(|m| !m.is_empty());
        }
    }

    /// The shape as a triangle mesh for the renderer. Missing normals face
    /// the viewer, missing colours are white; indices out of range drop
    /// their triangle.
    pub fn mesh(&self) -> MeshData {
        let stride = self.stride().max(1);
        let count = self.vertices.len() / stride;
        let mut attributes = Vec::new();
        let mut offset = 0;
        for &(name, n, attribute) in &COMPONENTS {
            if !self.format.iter().any(|f| f == name) {
                continue;
            }
            let values = self
                .vertices
                .chunks_exact(stride)
                .flat_map(|v| v[offset..offset + n].iter().copied())
                .collect();
            attributes.push((attribute.to_owned(), n as u8, values));
            offset += n;
        }
        let has = |f: &str| self.format.iter().any(|x| x == f);
        if !has("normal") {
            attributes.push(("a_Normal".to_owned(), 3, [0.0, 0.0, 1.0].repeat(count)));
        }
        if !has("color") {
            attributes.push(("a_Color".to_owned(), 4, vec![1.0; count * 4]));
        }
        let indices = self.indices.as_ref().map(|indices| {
            indices
                .chunks_exact(3)
                .filter(|t| t.iter().all(|&i| (i as usize) < count))
                .flatten()
                .copied()
                .collect()
        });
        MeshData {
            vertex_count: count as u32,
            attributes,
            indices,
        }
    }
}

/// One `IModelData`: its shapes and how far it has changed.
#[derive(Debug, Clone, Default)]
pub struct ModelData {
    pub shapes: Vec<ModelShape>,
    /// Bumped by every change of buffer contents (`applyData`).
    pub generation: u64,
    /// Bumped when the layout or materials changed (`replaceData`): layers
    /// using it must be rebuilt.
    pub layout: u64,
}

impl ModelData {
    /// Apply `{shapes: [...]}` (shapes as from the script), merging each
    /// shape config into the shape at its index; `null` removes a shape when
    /// `replace`.
    fn update(&mut self, data: &Value, replace: bool) {
        let empty = Vec::new();
        let shapes = data
            .get("shapes")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        for (i, config) in shapes.iter().enumerate() {
            if config.is_null() {
                if replace && i < self.shapes.len() {
                    self.shapes[i] = ModelShape::default();
                }
                continue;
            }
            if i >= self.shapes.len() {
                if !replace && !self.shapes.is_empty() {
                    continue;
                }
                self.shapes.resize_with(i + 1, ModelShape::default);
            }
            self.shapes[i].merge(config);
        }
        if replace {
            self.shapes
                .retain(|s| !s.format.is_empty() || !s.vertices.is_empty());
            self.layout += 1;
        }
        self.generation += 1;
    }
}

/// Every script geometry, by id.
#[derive(Debug, Default)]
pub struct ModelDatas {
    pub data: HashMap<i32, ModelData>,
    next: i32,
}

/// Shared by the script host (which changes it) and the frame (which draws it).
pub type SharedModelDatas = Rc<RefCell<ModelDatas>>;

impl ModelDatas {
    /// Run a script's `op` (see `SceneHost::model_data`).
    pub fn call(&mut self, op: &str, id: i32, data: &Value) -> Option<i32> {
        match op {
            "create" => {
                let id = self.next;
                self.next += 1;
                let mut model = ModelData::default();
                model.update(data, true);
                self.data.insert(id, model);
                Some(id)
            }
            "apply" => {
                self.data.get_mut(&id)?.update(data, false);
                Some(id)
            }
            "replace" => {
                self.data.get_mut(&id)?.update(data, true);
                Some(id)
            }
            "destroy" => {
                self.data.remove(&id);
                Some(id)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn shapes_become_meshes_and_apply_changes_contents() {
        let mut all = ModelDatas::default();
        let id = all
            .call(
                "create",
                -1,
                &json!({ "shapes": [{
                    "vertexFormat": ["position", "uv"],
                    "vertexBuffer": [0, 0, 0, 0, 0, 10, 0, 0, 1, 0, 0, 10, 0, 0, 1],
                    "indexBuffer": [0, 1, 2, 0, 1, 9],
                    "material": "materials/x.json",
                }] }),
            )
            .unwrap();
        let mesh = all.data[&id].shapes[0].mesh();
        assert_eq!(mesh.vertex_count, 3);
        assert_eq!(
            mesh.indices,
            Some(vec![0, 1, 2]),
            "out-of-range triangle dropped"
        );
        let attr = |m: &MeshData, a: &str| {
            m.attributes
                .iter()
                .find(|(n, ..)| n == a)
                .map(|(_, _, v)| v.clone())
        };
        assert_eq!(
            attr(&mesh, "a_TexCoord").unwrap(),
            [0.0, 0.0, 1.0, 0.0, 0.0, 1.0]
        );
        assert_eq!(attr(&mesh, "a_Normal").unwrap().len(), 9);
        let before = all.data[&id].layout;
        all.call(
            "apply",
            id,
            &json!({ "shapes": [{ "vertexBuffer": [5, 0, 0, 0, 0, 10, 0, 0, 1, 0, 0, 10, 0, 0, 1] }] }),
        );
        let model = &all.data[&id];
        assert_eq!(model.shapes[0].vertices[0], 5.0);
        assert_eq!(
            model.shapes[0].material.as_deref(),
            Some("materials/x.json")
        );
        assert_eq!(model.layout, before, "apply keeps the layout");
        assert_eq!(model.generation, 2);
        assert_eq!(id_of("modeldata:3"), Some(3));
        assert_eq!(id_of("models/x.mdl"), None);
    }
}
