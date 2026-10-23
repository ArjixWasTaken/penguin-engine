use std::collections::HashMap;

use glam::{Mat4, Vec2, Vec3};
use wpe_assets::material::{Material, MaterialPass, PassOverride};
use wpe_assets::scene::{ImageEffect, ImageObject, ObjectCommon, SceneObject};
use wpe_assets::{AssetError, ImageModel, load_json};
use wpe_vfs::Vfs;

use crate::{OrthoCamera, alignment_offset, local_transform};

const DEFAULT_ORTHO: Vec2 = Vec2::new(1920.0, 1080.0);

/// A scene resolved from its JSON: camera, clear colour and drawable layers.
#[derive(Debug, Clone)]
pub struct Scene {
    pub camera: OrthoCamera,
    pub clear_color: Vec3,
    /// Image layers in draw order (back to front).
    pub layers: Vec<ImageLayer>,
}

/// A textured card resolved from an image object, its model and material.
#[derive(Debug, Clone)]
pub struct ImageLayer {
    pub id: i32,
    pub name: String,
    /// World transform of the card centre (alignment already applied).
    pub transform: Mat4,
    /// Card size in scene pixels.
    pub size: Vec2,
    pub color: Vec3,
    pub alpha: f32,
    pub brightness: f32,
    /// First material pass with the object's `instance` override applied.
    pub material: MaterialPass,
    /// The model is a solid colour layer with no texture.
    pub solid: bool,
    pub effects: Vec<ImageEffect>,
}

impl ImageLayer {
    /// Texture bound to `g_Texture<slot>`, if any.
    pub fn texture(&self, slot: usize) -> Option<&str> {
        self.material
            .textures
            .get(slot)
            .and_then(Option::as_deref)
            .filter(|t| !t.is_empty())
    }
}

/// Transform-only view of every object, used to resolve parent chains.
struct Node<'a> {
    common: &'a ObjectCommon,
}

fn vec2(v: [f32; 2]) -> Vec2 {
    Vec2::from_array(v)
}

fn vec3(v: [f32; 3]) -> Vec3 {
    Vec3::from_array(v)
}

fn apply_override(pass: &mut MaterialPass, over: &PassOverride) {
    for (i, tex) in over.textures.iter().enumerate() {
        let Some(tex) = tex.as_ref().filter(|t| !t.is_empty()) else {
            continue;
        };
        if pass.textures.len() <= i {
            pass.textures.resize(i + 1, None);
        }
        pass.textures[i] = Some(tex.clone());
    }
    pass.combos
        .extend(over.combos.iter().map(|(k, v)| (k.clone(), *v)));
    pass.constantshadervalues.extend(
        over.constantshadervalues
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
}

impl Scene {
    /// Resolve `scene` against `vfs`. `texture_size` reports a texture's
    /// picture (or first sprite frame) size, for `autosize` models and
    /// automatic ortho sizing. Layers that fail to load are skipped with a
    /// warning.
    pub fn build(
        vfs: &Vfs,
        scene: &wpe_assets::Scene,
        mut texture_size: impl FnMut(&str) -> Option<Vec2>,
    ) -> Self {
        let general = &scene.general;
        let nodes: HashMap<i32, Node> = scene
            .objects
            .iter()
            .filter_map(SceneObject::common)
            .map(|common| (common.id, Node { common }))
            .collect();

        let mut images = Vec::new();
        for obj in &scene.objects {
            let SceneObject::Image(img) = obj else {
                continue;
            };
            match load_image(vfs, img, &mut texture_size) {
                Ok(Some(loaded)) => images.push((img, loaded)),
                Ok(None) => {}
                Err(e) => tracing::warn!("skipping layer {:?}: {e}", img.common.name),
            }
        }

        let ortho = match &general.orthogonalprojection {
            Some(p) if !p.auto && p.width > 0 && p.height > 0 => {
                Vec2::new(p.width as f32, p.height as f32)
            }
            Some(_) => images
                .iter()
                .filter_map(|(_, l)| l.size)
                .max_by(|a, b| (a.x * a.y).total_cmp(&(b.x * b.y)))
                .unwrap_or(DEFAULT_ORTHO),
            None => {
                tracing::warn!("perspective scenes are not supported yet; using a flat camera");
                DEFAULT_ORTHO
            }
        };

        let layers = images
            .into_iter()
            .filter(|(img, _)| is_visible(&nodes, img.common.id))
            .map(|(img, loaded)| {
                let (size, transform) = if loaded.fullscreen {
                    (ortho, Mat4::from_translation((ortho / 2.0).extend(0.0)))
                } else {
                    let common = &img.common;
                    let size = loaded
                        .size
                        .unwrap_or_else(|| vec3(*common.origin.value).truncate() * 2.0);
                    let origin = vec3(*common.origin.value)
                        + alignment_offset(&img.alignment, size).extend(0.0);
                    let local = local_transform(
                        origin,
                        vec3(*common.scale.value),
                        vec3(*common.angles.value),
                    );
                    (size, parent_transform(&nodes, common.parent, 0) * local)
                };
                ImageLayer {
                    id: img.common.id,
                    name: img.common.name.clone(),
                    transform,
                    size,
                    color: vec3(*img.color.value),
                    alpha: img.alpha.value,
                    brightness: img.brightness.value,
                    material: loaded.material,
                    solid: loaded.solid,
                    effects: img.effects.clone(),
                }
            })
            .collect();

        Self {
            camera: OrthoCamera {
                scene_size: ortho,
                zoom: general.zoom.value,
            },
            clear_color: vec3(*general.clearcolor.value),
            layers,
        }
    }
}

struct LoadedImage {
    material: MaterialPass,
    size: Option<Vec2>,
    fullscreen: bool,
    solid: bool,
}

fn load_image(
    vfs: &Vfs,
    img: &ImageObject,
    texture_size: &mut impl FnMut(&str) -> Option<Vec2>,
) -> Result<Option<LoadedImage>, AssetError> {
    let Some(model_path) = img.image.as_deref() else {
        tracing::debug!("shape layer {:?} not supported yet", img.common.name);
        return Ok(None);
    };
    let model: ImageModel = load_json(vfs, model_path)?;
    let Some(material_path) = model.material.as_deref() else {
        return Ok(None);
    };
    let material: Material = load_json(vfs, material_path)?;
    let Some(mut pass) = material.passes.into_iter().next() else {
        return Ok(None);
    };
    if let Some(over) = &img.instance {
        apply_override(&mut pass, over);
    }

    let first_texture = pass.textures.first().cloned().flatten();
    let size = match (model.width, model.height) {
        (Some(w), Some(h)) => Some(Vec2::new(w as f32, h as f32)),
        _ => img.size.map(|s| vec2(*s)).or_else(|| {
            model
                .autosize
                .then(|| first_texture.as_deref().and_then(&mut *texture_size))
                .flatten()
        }),
    };
    Ok(Some(LoadedImage {
        material: pass,
        size,
        fullscreen: model.fullscreen,
        solid: model.solidlayer,
    }))
}

/// Parent chains deeper than this are treated as cycles.
const MAX_DEPTH: usize = 64;

fn parent_transform(nodes: &HashMap<i32, Node>, parent: Option<i32>, depth: usize) -> Mat4 {
    let Some(node) = parent.and_then(|id| nodes.get(&id)) else {
        return Mat4::IDENTITY;
    };
    if depth > MAX_DEPTH {
        tracing::warn!("parent chain of object {} is cyclic", node.common.id);
        return Mat4::IDENTITY;
    }
    let c = node.common;
    parent_transform(nodes, c.parent, depth + 1)
        * local_transform(
            vec3(*c.origin.value),
            vec3(*c.scale.value),
            vec3(*c.angles.value),
        )
}

fn is_visible(nodes: &HashMap<i32, Node>, id: i32) -> bool {
    let mut current = Some(id);
    for _ in 0..MAX_DEPTH {
        let Some(node) = current.and_then(|id| nodes.get(&id)) else {
            return true;
        };
        if !node.common.visible.value {
            return false;
        }
        current = node.common.parent;
    }
    true
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;
    use wpe_vfs::{FileSystem, VfsError};

    struct MemFs(HashMap<&'static str, &'static str>);

    impl FileSystem for MemFs {
        fn contains(&self, path: &str) -> bool {
            self.0.contains_key(path)
        }
        fn read(&self, path: &str) -> wpe_vfs::Result<Cow<'_, [u8]>> {
            self.0
                .get(path)
                .map(|s| Cow::Borrowed(s.as_bytes()))
                .ok_or_else(|| VfsError::NotFound(path.to_owned()))
        }
        fn files(&self) -> Box<dyn Iterator<Item = &str> + '_> {
            Box::new(self.0.keys().copied())
        }
    }

    fn vfs() -> Vfs {
        let mut vfs = Vfs::new();
        vfs.mount(
            "mem",
            MemFs(HashMap::from([
                ("models/bg.json", r#"{"material": "materials/bg.json"}"#),
                (
                    "models/auto.json",
                    r#"{"material": "materials/bg.json", "autosize": true}"#,
                ),
                (
                    "materials/bg.json",
                    r#"{"passes":[{"shader":"genericimage2","textures":["bg"]}]}"#,
                ),
            ])),
        );
        vfs
    }

    #[test]
    fn builds_layers_with_parents_and_visibility() {
        let json = r#"{
            "general": {"orthogonalprojection": {"width": 1000, "height": 500}, "clearcolor": "0.1 0.2 0.3"},
            "objects": [
                {"id": 1, "name": "group", "origin": "100 0 0"},
                {"id": 2, "name": "bg", "image": "models/bg.json", "origin": "500 250 0",
                 "size": "1000 500", "parent": 1, "instance": {"textures": ["other"]}},
                {"id": 3, "name": "hidden", "image": "models/bg.json", "visible": false},
                {"id": 4, "name": "auto", "image": "models/auto.json", "origin": "10 10 0", "alignment": "topleft"}
            ]
        }"#;
        let scene: wpe_assets::Scene = wpe_assets::parse_json(json).unwrap();
        let built = Scene::build(&vfs(), &scene, |name| {
            (name == "bg").then_some(Vec2::new(64.0, 32.0))
        });
        assert_eq!(built.camera.scene_size, Vec2::new(1000.0, 500.0));
        assert_eq!(built.clear_color, Vec3::new(0.1, 0.2, 0.3));
        assert_eq!(built.layers.len(), 2);

        let bg = &built.layers[0];
        assert_eq!(bg.texture(0), Some("other"));
        assert_eq!(bg.transform.w_axis.truncate(), Vec3::new(600.0, 250.0, 0.0));

        let auto = &built.layers[1];
        assert_eq!(auto.size, Vec2::new(64.0, 32.0));
        assert_eq!(auto.transform.w_axis.truncate(), Vec3::new(42.0, -6.0, 0.0));
    }
}
