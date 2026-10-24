use std::collections::HashMap;

use glam::{Mat4, Vec2, Vec3};
use wpe_assets::material::{Material, MaterialPass, PassOverride};
use wpe_assets::scene::{
    ImageEffect, ImageObject, ObjectCommon, ParticleObject, SceneObject, TextObject,
};
use wpe_assets::{AssetError, ImageModel, load_json};

use crate::AnimationTime;
use wpe_vfs::Vfs;

use crate::{Animation, OrthoCamera, UserProperties, alignment_offset, local_transform};

const DEFAULT_ORTHO: Vec2 = Vec2::new(1920.0, 1080.0);

/// A scene resolved from its JSON: camera, clear colour and drawable layers.
#[derive(Debug, Clone)]
pub struct Scene {
    pub camera: OrthoCamera,
    pub clear_color: Vec3,
    /// Scene-wide bloom post-processing, when enabled.
    pub bloom: Option<Bloom>,
    /// Mouse/position parallax, when enabled.
    pub parallax: Option<Parallax>,
    /// Camera shake, when enabled.
    pub shake: Option<Shake>,
    /// Image layers in draw order (back to front), including hidden ones
    /// other layers may sample.
    pub layers: Vec<ImageLayer>,
}

/// Wallpaper Engine's legacy (LDR) bloom settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bloom {
    pub strength: f32,
    pub threshold: f32,
    pub tint: Vec3,
}

/// `general.camerashake*` settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shake {
    pub amplitude: f32,
    pub speed: f32,
    pub roughness: f32,
}

impl Shake {
    /// Camera offset at `time`, in units of 1% of the scene size.
    pub fn offset(&self, time: f32) -> Vec2 {
        let (t, r) = (time * self.speed, self.roughness);
        let x = t.sin() + (t * 2.3 + 1.7).sin() * r + (t * 4.7 + 3.1).sin() * r * r;
        let y = (t * 1.3 + 0.5).cos() + (t * 2.7 + 2.1).cos() * r + (t * 5.1 + 0.9).cos() * r * r;
        Vec2::new(x, y) * (self.amplitude / (1.0 + r + r * r))
    }
}

/// `general.cameraparallax*` settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Parallax {
    pub amount: f32,
    /// Smoothing time of the mouse response, in seconds.
    pub delay: f32,
    pub mouse_influence: f32,
}

/// A textured card resolved from an image object, its model and material.
#[derive(Debug, Clone)]
pub struct ImageLayer {
    pub id: i32,
    pub name: String,
    /// The layer and all its ancestors are visible.
    pub visible: bool,
    /// The layer's own flag, ancestors aside.
    pub self_visible: bool,
    /// No ancestor group is hidden, so the layer shows whenever it is made
    /// visible itself.
    pub ancestors_visible: bool,
    /// World transform of the card centre (alignment already applied).
    pub transform: Mat4,
    /// Card size in scene pixels.
    pub size: Vec2,
    pub color: Vec3,
    pub alpha: f32,
    pub brightness: f32,
    /// First material pass with the object's `instance` override applied.
    pub material: MaterialPass,
    /// What `transform`, `color` and `alpha` are made of, for animation.
    pub motion: Motion,
    /// How strongly camera parallax moves this layer.
    pub parallax_depth: Vec2,
    /// The model is a solid colour layer with no texture.
    pub solid: bool,
    /// A passthrough compose layer: its image is what is already on screen.
    pub compose: bool,
    /// Covers the whole output rather than a card.
    pub fullscreen: bool,
    /// `colorBlendMode`; 0 is plain alpha blending.
    pub color_blend_mode: i32,
    pub effects: Vec<ImageEffect>,
    /// Index of each of `effects` in the object's `effects` (hidden ones are
    /// left out of `effects`).
    pub effect_indices: Vec<usize>,
    /// For text layers, the object whose rendered text is the layer's image
    /// (see [`text_texture_name`]); its size and anchoring are known once rendered.
    pub text: Option<Box<TextObject>>,
    /// For particle layers, the particle definition and instance overrides.
    pub particle: Option<ParticleSource>,
    /// Puppet `.mdl` deforming the image, and the object's animation layers.
    pub puppet: Option<String>,
    pub animation_layers: Vec<serde_json::Value>,
}

/// A particle object's definition path and `instanceoverride`.
#[derive(Debug, Clone)]
pub struct ParticleSource {
    pub path: String,
    pub instance_override: Option<serde_json::Value>,
}

/// A layer's transform inputs and keyframe animations.
#[derive(Debug, Clone, Default)]
pub struct Motion {
    /// World transform of the parent chain.
    pub parent: Mat4,
    pub origin: Vec3,
    pub scale: Vec3,
    pub angles: Vec3,
    /// Alignment offset added to the origin.
    pub offset: Vec3,
    /// Offset of the card in the layer's own (scaled, rotated) space.
    pub local_offset: Vec3,
    pub origin_animation: Option<Animation>,
    pub scale_animation: Option<Animation>,
    pub angles_animation: Option<Animation>,
    pub color_animation: Option<Animation>,
    pub alpha_animation: Option<Animation>,
}

fn animate3(base: Vec3, anim: &Option<Animation>, time: impl AnimationTime) -> Vec3 {
    match anim {
        Some(a) => {
            let v = a.sample(time, &base.to_array());
            Vec3::new(v[0], v[1], v[2])
        }
        None => base,
    }
}

impl Motion {
    /// Layer position before parallax: origin plus alignment offset.
    pub fn position(&self) -> Vec3 {
        self.origin + self.offset
    }

    pub fn is_transform_animated(&self) -> bool {
        self.origin_animation.is_some()
            || self.scale_animation.is_some()
            || self.angles_animation.is_some()
    }

    /// Every animation of the motion, for registering clocks.
    pub fn animations(&self) -> impl Iterator<Item = &Animation> {
        [
            &self.origin_animation,
            &self.scale_animation,
            &self.angles_animation,
            &self.color_animation,
            &self.alpha_animation,
        ]
        .into_iter()
        .flatten()
    }

    pub fn transform_at(&self, time: impl AnimationTime) -> Mat4 {
        let origin = animate3(self.origin, &self.origin_animation, time) + self.offset;
        let scale = animate3(self.scale, &self.scale_animation, time);
        let angles = animate3(self.angles, &self.angles_animation, time);
        self.parent
            * local_transform(origin, scale, angles)
            * Mat4::from_translation(self.local_offset)
    }

    pub fn color_at(&self, base: Vec3, time: impl AnimationTime) -> Vec3 {
        animate3(base, &self.color_animation, time)
    }

    pub fn alpha_at(&self, base: f32, time: impl AnimationTime) -> f32 {
        self.alpha_animation
            .as_ref()
            .map_or(base, |a| a.sample(time, &[base])[0])
    }
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
    /// A bare group, whose visibility applies to its children.
    group: bool,
}

fn vec2(v: [f32; 2]) -> Vec2 {
    Vec2::from_array(v)
}

fn vec3(v: [f32; 3]) -> Vec3 {
    Vec3::from_array(v)
}

/// Overlay a pass override (non-empty textures, combos, constants) onto `pass`.
pub fn apply_override(pass: &mut MaterialPass, over: &PassOverride) {
    for (i, tex) in over.textures.iter().enumerate() {
        let Some(tex) = tex.as_ref().filter(|t| !t.is_empty()) else {
            continue;
        };
        if pass.textures.len() <= i {
            pass.textures.resize(i + 1, None);
        }
        pass.textures[i] = Some(tex.clone());
    }
    for (i, tex) in over.usertextures.iter().enumerate() {
        let Some(tex) = tex else { continue };
        if pass.usertextures.len() <= i {
            pass.usertextures.resize(i + 1, None);
        }
        pass.usertextures[i] = Some(tex.clone());
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
        props: &UserProperties,
        mut texture_size: impl FnMut(&str) -> Option<Vec2>,
    ) -> Self {
        let general = &scene.general;
        let nodes: HashMap<i32, Node> = scene
            .objects
            .iter()
            .filter_map(|obj| {
                let common = obj.common()?;
                let group = matches!(obj, SceneObject::Group(_));
                Some((common.id, Node { common, group }))
            })
            .collect();

        let mut images = Vec::new();
        for (index, obj) in scene.objects.iter().enumerate() {
            let SceneObject::Image(img) = obj else {
                continue;
            };
            match load_image(vfs, img, &mut texture_size) {
                Ok(Some(loaded)) => images.push((index, img, loaded)),
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
                .filter_map(|(_, _, l)| l.size)
                .max_by(|a, b| (a.x * a.y).total_cmp(&(b.x * b.y)))
                .unwrap_or(DEFAULT_ORTHO),
            None => {
                tracing::warn!("perspective scenes are not supported yet; using a flat camera");
                DEFAULT_ORTHO
            }
        };

        let mut layers: Vec<(usize, ImageLayer)> = images
            .into_iter()
            .map(|(index, img, loaded)| {
                let common = &img.common;
                let anim = |a: &Option<serde_json::Value>| a.as_ref().and_then(Animation::parse);
                let (size, motion) = if loaded.fullscreen {
                    let motion = Motion {
                        parent: Mat4::IDENTITY,
                        origin: (ortho / 2.0).extend(0.0),
                        scale: Vec3::ONE,
                        angles: Vec3::ZERO,
                        offset: Vec3::ZERO,
                        ..Default::default()
                    };
                    (ortho, motion)
                } else {
                    let size = loaded
                        .size
                        .unwrap_or_else(|| vec3(*props.value(&common.origin)).truncate() * 2.0);
                    let motion = Motion {
                        parent: parent_transform(&nodes, props, common.parent, 0),
                        origin: vec3(*props.value(&common.origin)),
                        scale: vec3(*props.value(&common.scale)),
                        angles: vec3(*props.value(&common.angles)),
                        offset: alignment_offset(&img.alignment, size).extend(0.0),
                        local_offset: Vec3::ZERO,
                        origin_animation: anim(&common.origin.animation),
                        scale_animation: anim(&common.scale.animation),
                        angles_animation: anim(&common.angles.animation),
                        color_animation: anim(&img.color.animation),
                        alpha_animation: anim(&img.alpha.animation),
                    };
                    (size, motion)
                };
                let transform = motion.transform_at(0.0);
                let layer = ImageLayer {
                    id: img.common.id,
                    name: img.common.name.clone(),
                    visible: is_visible(&nodes, props, img.common.id),
                    self_visible: props.flag(&img.common.visible),
                    ancestors_visible: ancestors_visible(&nodes, props, img.common.id),
                    transform,
                    size,
                    color: vec3(*props.value(&img.color)),
                    alpha: props.value(&img.alpha),
                    brightness: props.value(&img.brightness),
                    material: loaded.material,
                    motion,
                    parallax_depth: vec2(*props.value(&common.parallax_depth)),
                    solid: loaded.solid,
                    compose: loaded.passthrough,
                    fullscreen: loaded.fullscreen,
                    color_blend_mode: img.color_blend_mode,
                    effects: img
                        .effects
                        .iter()
                        .filter(|fx| props.flag(&fx.visible))
                        .cloned()
                        .collect(),
                    effect_indices: img
                        .effects
                        .iter()
                        .enumerate()
                        .filter(|(_, fx)| props.flag(&fx.visible))
                        .map(|(i, _)| i)
                        .collect(),
                    text: None,
                    particle: None,
                    puppet: loaded.puppet,
                    animation_layers: img.animationlayers.clone(),
                };
                (index, layer)
            })
            .collect();
        for (index, obj) in scene.objects.iter().enumerate() {
            match obj {
                SceneObject::Text(text) => layers.push((index, text_layer(&nodes, props, text))),
                SceneObject::Particle(p) if !p.particle.is_empty() => {
                    layers.push((index, particle_layer(&nodes, props, p)))
                }
                _ => {}
            }
        }
        layers.sort_by_key(|(index, _)| *index);
        let layers = layers.into_iter().map(|(_, l)| l).collect();

        Self {
            camera: OrthoCamera {
                scene_size: ortho,
                zoom: props.value(&general.zoom),
            },
            clear_color: vec3(*props.value(&general.clearcolor)),
            parallax: props.flag(&general.cameraparallax).then(|| Parallax {
                amount: props.value(&general.cameraparallaxamount),
                delay: props.value(&general.cameraparallaxdelay),
                mouse_influence: props.value(&general.cameraparallaxmouseinfluence),
            }),
            shake: props.flag(&general.camerashake).then(|| Shake {
                amplitude: props.value(&general.camerashakeamplitude),
                speed: props.value(&general.camerashakespeed),
                roughness: props.value(&general.camerashakeroughness),
            }),
            bloom: props.flag(&general.bloom).then(|| Bloom {
                strength: props.value(&general.bloomstrength),
                threshold: props.value(&general.bloomthreshold),
                tint: vec3(*props.value(&general.bloomtint)),
            }),
            layers,
        }
    }
}

struct LoadedImage {
    material: MaterialPass,
    size: Option<Vec2>,
    fullscreen: bool,
    solid: bool,
    passthrough: bool,
    puppet: Option<String>,
}

/// Name of the texture slot a text layer's rendered image is bound to.
pub fn text_texture_name(id: i32) -> String {
    format!("__text_{id}")
}

fn text_layer(nodes: &HashMap<i32, Node>, props: &UserProperties, text: &TextObject) -> ImageLayer {
    let common = &text.common;
    let anim = |a: &Option<serde_json::Value>| a.as_ref().and_then(Animation::parse);
    let motion = Motion {
        parent: parent_transform(nodes, props, common.parent, 0),
        origin: vec3(*props.value(&common.origin)),
        scale: vec3(*props.value(&common.scale)),
        angles: vec3(*props.value(&common.angles)),
        origin_animation: anim(&common.origin.animation),
        scale_animation: anim(&common.scale.animation),
        angles_animation: anim(&common.angles.animation),
        color_animation: anim(&text.color.animation),
        alpha_animation: anim(&text.alpha.animation),
        ..Default::default()
    };
    ImageLayer {
        id: common.id,
        name: common.name.clone(),
        visible: is_visible(nodes, props, common.id),
        self_visible: props.flag(&common.visible),
        ancestors_visible: ancestors_visible(nodes, props, common.id),
        transform: motion.transform_at(0.0),
        size: Vec2::ONE,
        color: Vec3::ONE,
        alpha: props.value(&text.alpha),
        brightness: props.value(&text.brightness),
        material: MaterialPass {
            shader: "genericimage2".to_owned(),
            textures: vec![Some(text_texture_name(common.id))],
            ..Default::default()
        },
        motion,
        parallax_depth: vec2(*props.value(&common.parallax_depth)),
        solid: false,
        compose: false,
        fullscreen: false,
        color_blend_mode: 0,
        effects: text
            .effects
            .iter()
            .filter(|fx| props.flag(&fx.visible))
            .cloned()
            .collect(),
        effect_indices: text
            .effects
            .iter()
            .enumerate()
            .filter(|(_, fx)| props.flag(&fx.visible))
            .map(|(i, _)| i)
            .collect(),
        text: Some(Box::new(text.clone())),
        particle: None,
        puppet: None,
        animation_layers: Vec::new(),
    }
}

fn particle_layer(
    nodes: &HashMap<i32, Node>,
    props: &UserProperties,
    p: &ParticleObject,
) -> ImageLayer {
    let common = &p.common;
    let anim = |a: &Option<serde_json::Value>| a.as_ref().and_then(Animation::parse);
    let motion = Motion {
        parent: parent_transform(nodes, props, common.parent, 0),
        origin: vec3(*props.value(&common.origin)),
        scale: vec3(*props.value(&common.scale)),
        angles: vec3(*props.value(&common.angles)),
        origin_animation: anim(&common.origin.animation),
        scale_animation: anim(&common.scale.animation),
        angles_animation: anim(&common.angles.animation),
        ..Default::default()
    };
    ImageLayer {
        id: common.id,
        name: common.name.clone(),
        visible: is_visible(nodes, props, common.id),
        self_visible: props.flag(&common.visible),
        ancestors_visible: ancestors_visible(nodes, props, common.id),
        transform: motion.transform_at(0.0),
        size: Vec2::ONE,
        color: Vec3::ONE,
        alpha: 1.0,
        brightness: 1.0,
        material: MaterialPass::default(),
        motion,
        parallax_depth: vec2(*props.value(&common.parallax_depth)),
        solid: false,
        compose: false,
        fullscreen: false,
        color_blend_mode: 0,
        effects: Vec::new(),
        effect_indices: Vec::new(),
        text: None,
        particle: Some(ParticleSource {
            path: p.particle.clone(),
            instance_override: p.instanceoverride.clone(),
        }),
        puppet: None,
        animation_layers: Vec::new(),
    }
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
        passthrough: model.passthrough,
        puppet: model.puppet.filter(|p| !p.is_empty()),
    }))
}

/// Parent chains deeper than this are treated as cycles.
const MAX_DEPTH: usize = 64;

fn parent_transform(
    nodes: &HashMap<i32, Node>,
    props: &UserProperties,
    parent: Option<i32>,
    depth: usize,
) -> Mat4 {
    let Some(node) = parent.and_then(|id| nodes.get(&id)) else {
        return Mat4::IDENTITY;
    };
    if depth > MAX_DEPTH {
        tracing::warn!("parent chain of object {} is cyclic", node.common.id);
        return Mat4::IDENTITY;
    }
    let c = node.common;
    parent_transform(nodes, props, c.parent, depth + 1)
        * local_transform(
            vec3(*props.value(&c.origin)),
            vec3(*props.value(&c.scale)),
            vec3(*props.value(&c.angles)),
        )
}

/// A layer shows when it is visible itself and no ancestor group is hidden;
/// hidden image parents only anchor their children's transforms.
fn is_visible(nodes: &HashMap<i32, Node>, props: &UserProperties, id: i32) -> bool {
    nodes
        .get(&id)
        .is_none_or(|node| props.flag(&node.common.visible))
        && ancestors_visible(nodes, props, id)
}

/// No group above `id` is hidden.
fn ancestors_visible(nodes: &HashMap<i32, Node>, props: &UserProperties, id: i32) -> bool {
    let mut current = nodes.get(&id).and_then(|n| n.common.parent);
    for _ in 1..MAX_DEPTH {
        let Some(node) = current.and_then(|id| nodes.get(&id)) else {
            return true;
        };
        if node.group && !props.flag(&node.common.visible) {
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
        let built = Scene::build(&vfs(), &scene, &UserProperties::default(), |name| {
            (name == "bg").then_some(Vec2::new(64.0, 32.0))
        });
        assert_eq!(built.camera.scene_size, Vec2::new(1000.0, 500.0));
        assert_eq!(built.clear_color, Vec3::new(0.1, 0.2, 0.3));
        assert_eq!(built.layers.len(), 3);
        assert!(!built.layers[1].visible);

        let bg = &built.layers[0];
        assert_eq!(bg.texture(0), Some("other"));
        assert_eq!(bg.transform.w_axis.truncate(), Vec3::new(600.0, 250.0, 0.0));

        let auto = &built.layers[2];
        assert_eq!(auto.size, Vec2::new(64.0, 32.0));
        assert_eq!(auto.transform.w_axis.truncate(), Vec3::new(42.0, -6.0, 0.0));
    }
}
