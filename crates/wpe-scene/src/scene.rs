use std::collections::HashMap;

use glam::{Mat4, Vec2, Vec3};
use wpe_assets::material::{Material, MaterialPass, PassOverride};
use wpe_assets::scene::{
    GroupObject, ImageEffect, ImageObject, LightObject, ModelObject, ObjectCommon, ParticleObject,
    SceneObject, TextObject,
};
use wpe_assets::{AssetError, ImageModel, load_json};

use crate::AnimationTime;
use wpe_vfs::Vfs;

use crate::{
    Animation, LightKind, LightUniforms, OrthoCamera, PerspectiveCamera, SceneLight,
    UserProperties, alignment_offset, light_uniforms, local_transform,
};

const DEFAULT_ORTHO: Vec2 = Vec2::new(1920.0, 1080.0);

/// A scene resolved from its JSON: camera, clear colour and drawable layers.
#[derive(Debug, Clone)]
pub struct Scene {
    /// The flat camera image layers draw with. In 3D scenes it is the overlay
    /// for flat layers, sized to `orthogonalprojection` (1920×1080 when absent).
    pub camera: OrthoCamera,
    /// `general.orthogonalprojection` is null or absent: models and
    /// `perspective` layers draw with [`Scene::perspective`].
    pub is_3d: bool,
    /// 3D scenes' camera, or 2D scenes' `"global_perspective"` one for
    /// `perspective` layers (lined up with `camera` on the z = 0 plane).
    pub perspective: PerspectiveCamera,
    pub clear_color: Vec3,
    /// The scene renders in HDR: colour buffers hold unclamped floats and
    /// the output clamps them (see [`hdr_intent`]).
    pub hdr: bool,
    /// Scene-wide bloom post-processing, when enabled.
    pub bloom: Option<Bloom>,
    /// Mouse/position parallax, when enabled.
    pub parallax: Option<Parallax>,
    /// Camera shake, when enabled.
    pub shake: Option<Shake>,
    /// Image layers in draw order (back to front), including hidden ones
    /// other layers may sample.
    pub layers: Vec<ImageLayer>,
    /// Each group's own visibility (hidden groups hide their descendants).
    pub groups: HashMap<i32, bool>,
    /// 3D model objects, in object order.
    pub models: Vec<ModelLayer>,
    /// Image, text and particle layers and models interleaved in draw order
    /// (back to front): scene object order.
    pub draw_order: Vec<DrawItem>,
    /// Light objects, in object order (hidden ones included).
    pub lights: Vec<SceneLight>,
    /// `general.ambientcolor` and `general.skylightcolor`.
    pub ambient_color: Vec3,
    pub skylight_color: Vec3,
}

/// One entry of [`Scene::draw_order`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawItem {
    /// Index into [`Scene::layers`].
    Layer(usize),
    /// Index into [`Scene::models`].
    Model(usize),
}

/// A 3D model object.
#[derive(Debug, Clone)]
pub struct ModelLayer {
    pub id: i32,
    pub name: String,
    /// The object's `model` path as written.
    pub model: String,
    /// The `.mdl` it resolves to (see [`wpe_assets::model::model_mdl_path`]).
    pub mdl: Option<String>,
    /// The model and all its ancestors are visible.
    pub visible: bool,
    /// The model's own flag, ancestors aside.
    pub self_visible: bool,
    /// No ancestor group is hidden.
    pub ancestors_visible: bool,
    /// World transform: parent chain · translate · Rz · Ry · Rx · scale.
    pub transform: Mat4,
    /// What `transform` is made of, for animation.
    pub motion: Motion,
    /// Parent object, and the parent puppet's attachment point this model
    /// rigs to (`attachment`).
    pub parent: Option<i32>,
    pub attachment: Option<String>,
    pub parallax_depth: Vec2,
    /// Draw with the perspective camera (always in 3D scenes).
    pub perspective: bool,
    /// The object's `animationlayers`, for skinned models.
    pub animation_layers: Vec<serde_json::Value>,
    /// Position in the scene's object list, the draw-order key shared with
    /// [`ImageLayer::object_index`].
    pub object_index: usize,
}

/// Scene bloom settings. HDR scenes use the `bloomhdr*` strength and
/// threshold, others `bloomstrength` / `bloomthreshold`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bloom {
    pub strength: f32,
    pub threshold: f32,
    pub tint: Vec3,
    /// The HDR mip-chain bloom, used instead of the legacy LDR passes.
    pub mip_chain: Option<HdrBloom>,
}

/// Parameters of the HDR mip-chain bloom: HDR scenes whose
/// `orthogonalprojection.postprocessing` is `ultra` or `displayhdr`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HdrBloom {
    /// `bloomhdrfeather`: soft-knee width as a fraction of the threshold.
    pub feather: f32,
    /// `bloomhdriterations` and `bloomhdrscatter`, as authored. Wallpaper
    /// Engine's use of them isn't known: like the reference renderer the
    /// chain has four levels and upsamples with a neutral scatter (feeding
    /// scatter into every level compounds and blows the bloom out).
    pub iterations: u32,
    pub scatter: f32,
}

impl Bloom {
    fn from_general(
        general: &wpe_assets::scene::SceneGeneral,
        props: &UserProperties,
        hdr: bool,
    ) -> Self {
        let (strength, threshold) = if hdr {
            (&general.bloomhdrstrength, &general.bloomhdrthreshold)
        } else {
            (&general.bloomstrength, &general.bloomthreshold)
        };
        let preset = general
            .orthogonalprojection
            .as_ref()
            .and_then(|p| p.postprocessing.as_deref());
        // The mip chain needs the scene's own `hdr`, not the overbright
        // auto-enable of [`hdr_intent`].
        let mip_chain = (props.flag(&general.hdr)
            && matches!(preset, Some("ultra" | "displayhdr")))
        .then(|| HdrBloom {
            feather: props.value(&general.bloomhdrfeather),
            iterations: props.value(&general.bloomhdriterations).max(0.0) as u32,
            scatter: props.value(&general.bloomhdrscatter),
        });
        Self {
            strength: props.value(strength),
            threshold: props.value(threshold),
            tint: vec3(*props.value(&general.bloomtint)),
            mip_chain,
        }
    }
}

/// Whether `scene` renders in HDR: `general.hdr`, or a particle instance
/// override with `brightness` above 1, whose overbright colours would
/// otherwise clamp per channel and add up to white.
pub fn hdr_intent(scene: &wpe_assets::Scene, props: &UserProperties) -> bool {
    props.flag(&scene.general.hdr)
        || scene.objects.iter().any(|obj| match obj {
            SceneObject::Particle(p) => p
                .instanceoverride
                .as_ref()
                .and_then(|o| o.get("brightness"))
                .and_then(|b| b.as_f64())
                .is_some_and(|b| b > 1.0),
            _ => false,
        })
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
    /// Parent object, and the parent puppet's attachment point this layer
    /// follows (`attachment`).
    pub parent: Option<i32>,
    pub attachment: Option<String>,
    /// For text layers, the object whose rendered text is the layer's image
    /// (see [`text_texture_name`]); its size and anchoring are known once rendered.
    pub text: Option<Box<TextObject>>,
    /// For particle layers, the particle definition and instance overrides.
    pub particle: Option<ParticleSource>,
    /// Puppet `.mdl` deforming the image, and the object's animation layers.
    pub puppet: Option<String>,
    pub animation_layers: Vec<serde_json::Value>,
    /// Draw with [`Scene::perspective`] rather than the flat camera.
    pub perspective: bool,
    /// Position in the scene's object list, the draw-order key shared with
    /// [`ModelLayer::object_index`].
    pub object_index: usize,
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
            // 3D scene: flat layers keep WE's default projection size.
            None => DEFAULT_ORTHO,
        };
        let is_3d = general.orthogonalprojection.is_none();

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
                    parent: common.parent,
                    attachment: img.attachment.clone().filter(|a| !a.is_empty()),
                    text: None,
                    particle: None,
                    puppet: loaded.puppet,
                    animation_layers: img.animationlayers.clone(),
                    perspective: img.perspective,
                    object_index: index,
                };
                (index, layer)
            })
            .collect();
        let mut models = Vec::new();
        let mut lights = Vec::new();
        for (index, obj) in scene.objects.iter().enumerate() {
            match obj {
                SceneObject::Text(text) => layers.push((index, text_layer(&nodes, props, text))),
                SceneObject::Particle(p) if !p.particle.is_empty() => {
                    layers.push((index, particle_layer(&nodes, props, p)))
                }
                SceneObject::Model(m) => models.push(model_layer(vfs, &nodes, props, m, index)),
                SceneObject::Light(l) => lights.push(scene_light(&nodes, props, l)),
                _ => {}
            }
        }
        layers.sort_by_key(|(index, _)| *index);
        let layers: Vec<ImageLayer> = layers
            .into_iter()
            .map(|(index, mut l)| {
                l.object_index = index;
                l
            })
            .collect();
        let mut draw_order: Vec<(usize, DrawItem)> = layers
            .iter()
            .enumerate()
            .map(|(i, l)| (l.object_index, DrawItem::Layer(i)))
            .chain(
                models
                    .iter()
                    .enumerate()
                    .map(|(i, m): (usize, &ModelLayer)| (m.object_index, DrawItem::Model(i))),
            )
            .collect();
        draw_order.sort_by_key(|(index, _)| *index);

        let camera = OrthoCamera {
            scene_size: ortho,
            zoom: props.value(&general.zoom),
        };
        let (near, far) = (props.value(&general.nearz), props.value(&general.farz));
        let perspective = if is_3d {
            scene_camera(scene, &nodes, props, near, far)
        } else {
            PerspectiveCamera::global_perspective(camera, near, far)
        };

        let hdr = hdr_intent(scene, props);
        Self {
            camera,
            is_3d,
            perspective,
            clear_color: vec3(*props.value(&general.clearcolor)),
            hdr,
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
            bloom: props
                .flag(&general.bloom)
                .then(|| Bloom::from_general(general, props, hdr)),
            groups: nodes
                .iter()
                .filter(|(_, n)| n.group)
                .map(|(&id, n)| (id, props.flag(&n.common.visible)))
                .collect(),
            layers,
            models,
            draw_order: draw_order.into_iter().map(|(_, d)| d).collect(),
            lights,
            ambient_color: vec3(*props.value(&general.ambientcolor)),
            skylight_color: vec3(*props.value(&general.skylightcolor)),
        }
    }

    /// [`light_uniforms`] of the scene's lights and ambient colours.
    pub fn light_uniforms(&self) -> LightUniforms {
        light_uniforms(&self.lights, self.ambient_color, self.skylight_color)
    }
}

/// A camera object: a bare node carrying `"camera": "<name>"`. Wallpaper
/// Engine keeps the runtime camera there; the scene's `camera` block is the
/// editor's saved viewport and may sit elsewhere.
fn camera_object(scene: &wpe_assets::Scene) -> Option<&GroupObject> {
    scene.objects.iter().find_map(|obj| match obj {
        SceneObject::Group(g) if g.extra.get("camera").is_some_and(|v| v.is_string()) => Some(&**g),
        _ => None,
    })
}

/// A 3D scene's camera: the camera object's placement and fov when there is
/// one, else the scene's `camera` block with `perspectiveoverridefov` or `fov`.
fn scene_camera(
    scene: &wpe_assets::Scene,
    nodes: &HashMap<i32, Node>,
    props: &UserProperties,
    near: f32,
    far: f32,
) -> PerspectiveCamera {
    let general = &scene.general;
    let scene_fov = general
        .perspectiveoverridefov
        .as_ref()
        .map(|f| props.value(f))
        .filter(|&f| f > 0.0)
        .unwrap_or_else(|| props.value(&general.fov));
    let mut camera = PerspectiveCamera {
        eye: vec3(*scene.camera.eye),
        center: vec3(*scene.camera.center),
        up: vec3(*scene.camera.up),
        fov: scene_fov,
        near,
        far,
        fit: None,
    };
    if let Some(obj) = camera_object(scene) {
        let c = &obj.common;
        // Orientation only: the object's scale must not skew the basis.
        let world = parent_transform(nodes, props, c.parent, 0)
            * local_transform(
                vec3(*props.value(&c.origin)),
                Vec3::ONE,
                vec3(*props.value(&c.angles)),
            );
        camera.eye = world.transform_point3(Vec3::ZERO);
        camera.center = camera.eye + world.transform_vector3(-Vec3::Z).normalize_or(-Vec3::Z);
        camera.up = world.transform_vector3(Vec3::Y).normalize_or(Vec3::Y);
        if let Some(fov) = obj
            .extra
            .get("fov")
            .and_then(|v| v.as_f64())
            .filter(|&f| f > 0.0)
        {
            camera.fov = fov as f32;
        }
    }
    camera
}

fn model_layer(
    vfs: &Vfs,
    nodes: &HashMap<i32, Node>,
    props: &UserProperties,
    m: &ModelObject,
    object_index: usize,
) -> ModelLayer {
    let common = &m.common;
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
    // Script geometry (`createModelData`) has no file behind it.
    let scripted = m.model.starts_with("modeldata:");
    let mdl = (!scripted)
        .then(|| wpe_assets::model::model_mdl_path(&m.model, |p| vfs.read(p).ok().map(Vec::from)))
        .flatten();
    if mdl.is_none() && !scripted {
        tracing::warn!("model {:?}: no mesh for {:?}", common.name, m.model);
    }
    ModelLayer {
        id: common.id,
        name: common.name.clone(),
        model: m.model.clone(),
        mdl,
        visible: is_visible(nodes, props, common.id),
        self_visible: props.flag(&common.visible),
        ancestors_visible: ancestors_visible(nodes, props, common.id),
        transform: motion.transform_at(0.0),
        motion,
        parent: common.parent,
        attachment: m.attachment.clone().filter(|a| !a.is_empty()),
        parallax_depth: vec2(*props.value(&common.parallax_depth)),
        perspective: m.extra.get("perspective").is_some_and(|v| {
            v.as_bool()
                .or_else(|| v.get("value").and_then(serde_json::Value::as_bool))
                .unwrap_or(false)
        }),
        animation_layers: m
            .extra
            .get("animationlayers")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default(),
        object_index,
    }
}

fn scene_light(nodes: &HashMap<i32, Node>, props: &UserProperties, l: &LightObject) -> SceneLight {
    let common = &l.common;
    let anim = |a: &Option<serde_json::Value>| a.as_ref().and_then(Animation::parse);
    let motion = Motion {
        parent: parent_transform(nodes, props, common.parent, 0),
        origin: vec3(*props.value(&common.origin)),
        scale: vec3(*props.value(&common.scale)),
        angles: vec3(*props.value(&common.angles)),
        origin_animation: anim(&common.origin.animation),
        scale_animation: anim(&common.scale.animation),
        angles_animation: anim(&common.angles.animation),
        color_animation: anim(&l.color.animation),
        ..Default::default()
    };
    let cone = |c: &Option<wpe_assets::value::Dynamic<f32>>, default: f32| {
        c.as_ref().map_or(default, |c| props.value(c))
    };
    let mut light = SceneLight {
        id: common.id,
        name: common.name.clone(),
        kind: LightKind::parse(&l.light),
        visible: is_visible(nodes, props, common.id),
        transform: Mat4::IDENTITY,
        position: Vec3::ZERO,
        direction: -Vec3::Z,
        color: vec3(*props.value(&l.color)),
        radius: props.value(&l.radius),
        intensity: props.value(&l.intensity),
        exponent: props.value(&l.exponent),
        inner_cone: cone(&l.innercone, 30.0),
        outer_cone: cone(&l.outercone, 45.0),
        parent: common.parent,
        motion,
        cast_shadow: false,
        cast_volumetrics: false,
        density: 1.0,
        volumetrics_exponent: 1.0,
        cascades: crate::light::DEFAULT_CASCADES,
    };
    crate::light::apply_light_extras(&mut light, &l.extra, props);
    light.set_transform(light.motion.transform_at(0.0));
    light
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
        parent: common.parent,
        attachment: None,
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
        perspective: false,
        object_index: 0,
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
        parent: common.parent,
        attachment: None,
        text: None,
        particle: Some(ParticleSource {
            path: p.particle.clone(),
            instance_override: p.instanceoverride.clone(),
        }),
        puppet: None,
        animation_layers: Vec::new(),
        perspective: false,
        object_index: 0,
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
    use std::f32::consts::FRAC_PI_2;
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

        assert!(!built.is_3d && built.perspective.fit == Some(built.camera));
        assert_eq!(built.perspective.eye(), Vec3::new(500.0, 250.0, 1000.0));
        assert_eq!(built.draw_order.len(), 3);
    }

    fn build(json: &str) -> Scene {
        let scene: wpe_assets::Scene = wpe_assets::parse_json(json).unwrap();
        Scene::build(&vfs(), &scene, &UserProperties::default(), |_| None)
    }

    #[test]
    fn models_lights_and_draw_order() {
        let built = build(
            r#"{
            "general": {"orthogonalprojection": null, "ambientcolor": "0.1 0.1 0.1",
                        "skylightcolor": "0.4 0.5 0.6", "fov": 60, "nearz": 1, "farz": 500},
            "camera": {"eye": "0 0 10", "center": "0 0 0", "up": "0 1 0"},
            "objects": [
                {"id": 1, "name": "rig", "origin": "10 0 0", "angles": "0 0 1.5707964"},
                {"id": 2, "name": "bg", "image": "models/bg.json", "perspective": true},
                {"id": 3, "name": "rock", "model": "models/rock.mdl", "parent": 1,
                 "origin": "1 0 0", "angles": "0.5 0.25 0", "attachment": "hand"},
                {"id": 4, "name": "front", "image": "models/bg.json"},
                {"id": 5, "light": "lspot", "parent": 1, "origin": "0 2 0", "color": "1 0 0",
                 "radius": 50, "intensity": 2, "exponent": 0.5, "visible": false},
                {"id": 6, "light": "lpoint", "origin": "3 4 5"}
            ]
        }"#,
        );
        assert!(built.is_3d);
        assert_eq!(built.camera.scene_size, DEFAULT_ORTHO);
        let cam = built.perspective;
        assert_eq!(
            (cam.eye, cam.fov, cam.near, cam.far),
            (Vec3::new(0.0, 0.0, 10.0), 60.0, 1.0, 500.0)
        );
        assert!(cam.fit.is_none());

        assert!(built.layers[0].perspective && !built.layers[1].perspective);
        let rock = &built.models[0];
        assert_eq!(rock.mdl.as_deref(), Some("models/rock.mdl"));
        assert_eq!(rock.attachment.as_deref(), Some("hand"));
        assert!(rock.visible);
        let expected = local_transform(
            Vec3::new(10.0, 0.0, 0.0),
            Vec3::ONE,
            Vec3::new(0.0, 0.0, FRAC_PI_2),
        ) * local_transform(Vec3::X, Vec3::ONE, Vec3::new(0.5, 0.25, 0.0));
        assert!(rock.transform.abs_diff_eq(expected, 1e-5));
        assert!((rock.transform.w_axis.truncate() - Vec3::new(10.0, 1.0, 0.0)).length() < 1e-5);
        assert_eq!(
            built.draw_order,
            [DrawItem::Layer(0), DrawItem::Model(0), DrawItem::Layer(1)]
        );

        let [spot, point] = &built.lights[..] else {
            panic!()
        };
        assert_eq!((spot.kind, spot.visible), (LightKind::Spot, false));
        assert_eq!(
            (spot.radius, spot.intensity, spot.exponent),
            (50.0, 2.0, 0.5)
        );
        // The rig's quarter turn about Z carries the light to (8, 0, 0).
        assert!((spot.position - Vec3::new(8.0, 0.0, 0.0)).length() < 1e-5);
        assert_eq!((point.kind, point.radius), (LightKind::LPoint, 1000.0));
        assert_eq!(built.ambient_color, Vec3::splat(0.1));
        assert_eq!(built.skylight_color, Vec3::new(0.4, 0.5, 0.6));

        let u = built.light_uniforms();
        assert_eq!(
            &u.uniform("g_LightsPosition").unwrap()[..4],
            [3.0, 4.0, 5.0, 1.0]
        );
        assert_eq!(u.combo("LIGHTS_POINT"), Some(1));
        assert_eq!(u.combo("LIGHTS_SPOT"), Some(0));
        assert_eq!(u.uniform("g_LightSkylightColor").unwrap(), [0.4, 0.5, 0.6]);
    }

    #[test]
    fn camera_object_overrides_saved_viewport() {
        let built = build(
            r#"{
            "general": {"fov": 50, "perspectiveoverridefov": 40},
            "camera": {"eye": "0 0 10", "center": "0 0 0", "up": "0 1 0"},
            "objects": [{"id": 1, "camera": "default", "origin": "1 2 3",
                         "angles": "0 1.5707964 0", "fov": 70}]
        }"#,
        );
        let cam = built.perspective;
        assert_eq!((cam.eye, cam.fov), (Vec3::new(1.0, 2.0, 3.0), 70.0));
        // Yawed a quarter turn: -Z turns to -X.
        assert!((cam.center - Vec3::new(0.0, 2.0, 3.0)).length() < 1e-5);
        assert!((cam.up - Vec3::Y).length() < 1e-5);

        let built = build(r#"{"general": {"fov": 50, "perspectiveoverridefov": 40}}"#);
        assert_eq!(built.perspective.fov, 40.0);
    }

    #[test]
    fn hdr_intent_picks_bloom_parameters_and_path() {
        let general = |extra: &str| {
            format!(
                r#"{{"general": {{"bloom": true, "bloomstrength": 0.5, "bloomthreshold": 0.6,
                    "bloomhdrstrength": 3, "bloomhdrthreshold": 1.5, "bloomhdrfeather": 0.2,
                    "bloomhdriterations": 6, "bloomhdrscatter": 0.7, {extra}}}}}"#
            )
        };
        let ortho = r#""orthogonalprojection": {"width": 100, "height": 100"#;

        let ldr = build(&general(&format!(
            r#"{ortho}, "postprocessing": "ultra"}}"#
        )));
        assert!(!ldr.hdr);
        let bloom = ldr.bloom.unwrap();
        assert_eq!((bloom.strength, bloom.threshold), (0.5, 0.6));
        assert_eq!(bloom.mip_chain, None);

        let hdr = build(&general(&format!(r#""hdr": true, {ortho}}}"#)));
        assert!(hdr.hdr);
        let bloom = hdr.bloom.unwrap();
        assert_eq!((bloom.strength, bloom.threshold), (3.0, 1.5));
        assert_eq!(bloom.mip_chain, None);

        for preset in ["ultra", "displayhdr"] {
            let chain = build(&general(&format!(
                r#""hdr": true, {ortho}, "postprocessing": "{preset}"}}"#
            )));
            let expected = HdrBloom {
                feather: 0.2,
                iterations: 6,
                scatter: 0.7,
            };
            assert_eq!(chain.bloom.unwrap().mip_chain, Some(expected));
        }

        // Bound to a user property.
        let scene: wpe_assets::Scene = wpe_assets::parse_json(&general(
            r#""hdr": {"user": "gg", "value": true}, "orthogonalprojection": null"#,
        ))
        .unwrap();
        let mut props = UserProperties::default();
        assert!(hdr_intent(&scene, &props));
        props.set("gg", serde_json::json!(false));
        assert!(!hdr_intent(&scene, &props));

        // Overbright particle overrides switch HDR on.
        let overbright = |b: &str| {
            let scene: wpe_assets::Scene = wpe_assets::parse_json(&format!(
                r#"{{"objects": [{{"id": 1, "particle": "p.json",
                    "instanceoverride": {{"brightness": {b}}}}}]}}"#
            ))
            .unwrap();
            hdr_intent(&scene, &UserProperties::default())
        };
        assert!(overbright("5.0"));
        assert!(!overbright("1.0"));
    }
}
