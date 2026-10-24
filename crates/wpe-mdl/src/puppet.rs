use glam::{EulerRot, Mat4, Quat, Vec3};
use serde_json::Value;

use crate::{Mdl, PlayMode};

/// One entry of an image object's scene `animationlayers` array.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimationLayer {
    /// [`crate::MdlAnimation::id`] this layer plays.
    pub animation: i32,
    /// Playback speed multiplier (negative plays backwards).
    pub rate: f64,
    /// Layer weight, clamped to 0..=1 when applied.
    pub blend: f64,
    /// Hidden layers contribute nothing.
    pub visible: bool,
    /// Additive layers add their delta from frame 0 on top of the layers
    /// below; others blend over them.
    pub additive: bool,
}

impl AnimationLayer {
    /// Read a scene `animationlayers` entry. User-property bound values
    /// (`{"user": …, "value": …}`) use their `value`. Returns `None` without
    /// an `animation` id.
    pub fn from_json(v: &Value) -> Option<Self> {
        let get = |key: &str| {
            let x = v.get(key)?;
            match x.as_object().and_then(|o| o.get("value")) {
                Some(inner) => Some(inner),
                None => Some(x),
            }
        };
        let num = |x: &Value| {
            x.as_f64()
                .or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))
        };
        let flag = |x: &Value| {
            x.as_bool()
                .or_else(|| x.as_f64().map(|n| n != 0.0))
                .or_else(|| x.as_str().map(|s| s == "true" || s == "1"))
        };
        Some(Self {
            animation: get("animation").and_then(num)? as i32,
            rate: get("rate").and_then(num).unwrap_or(1.0),
            blend: get("blend").and_then(num).unwrap_or(1.0),
            visible: get("visible").and_then(flag).unwrap_or(true),
            additive: get("additive").and_then(flag).unwrap_or(false),
        })
    }
}

#[derive(Debug, Clone)]
struct PuppetBone {
    parent: Option<usize>,
    rest_translation: Vec3,
    rest_scale: Vec3,
    rest_rotation: Quat,
    inverse_bind_world: Mat4,
    rest_world: Mat4,
}

#[derive(Debug, Clone, Copy)]
struct Frame {
    position: Vec3,
    scale: Vec3,
    rotation: Quat,
}

#[derive(Debug, Clone)]
struct Animation {
    id: i32,
    mode: PlayMode,
    fps: f64,
    length: u32,
    tracks: Vec<Vec<Frame>>,
    morph_tracks: Vec<Vec<f32>>,
}

impl Animation {
    /// Frames to blend between at `time` seconds of playback, and the factor.
    fn sample_point(&self, time: f64) -> (usize, usize, f32) {
        let length = u64::from(self.length);
        if length == 0 || self.fps.is_nan() || self.fps <= 0.0 || !time.is_finite() {
            return (0, 0, 0.0);
        }
        let frame_time = 1.0 / self.fps;
        let max_time = f64::from(self.length) / self.fps;
        let wrap = |time: f64, length: u64, max_time: f64| {
            let mut cur = time % max_time;
            if cur < 0.0 {
                cur += max_time;
            }
            let rate = cur / frame_time;
            let a = (rate as u64) % length;
            let b = (a + 1) % length;
            (a, b, (rate - a as f64).clamp(0.0, 1.0))
        };
        let (a, b, t) = match self.mode {
            PlayMode::Single if time >= max_time => (length - 1, length - 1, 0.0),
            PlayMode::Single | PlayMode::Loop => wrap(time, length, max_time),
            PlayMode::Mirror => {
                let fold = |f: u64| {
                    if f >= length {
                        (length - 1).saturating_sub(f - length)
                    } else {
                        f
                    }
                };
                let (a, b, t) = wrap(time, length * 2, max_time * 2.0);
                (fold(a), fold(b), t)
            }
        };
        (a as usize, b as usize, t as f32)
    }
}

#[derive(Debug, Clone)]
struct Layer {
    params: AnimationLayer,
    animation: Option<usize>,
}

/// A puppet's skeleton driven by its scene animation layers.
///
/// Evaluation follows Wallpaper Engine (and the reference renderer): every
/// bone starts from its rest pose; visible layers are applied bottom to top —
/// non-additive layers blend toward their interpolated pose by `blend`,
/// additive layers add `blend ×` their delta from frame 0. Bone matrices are
/// `animated_world · inverse(bind_world)`, mapping a vertex from where the
/// file stores it to where its bone currently is.
#[derive(Debug, Clone)]
pub struct Puppet {
    bones: Vec<PuppetBone>,
    animations: Vec<Animation>,
    layers: Vec<Layer>,
    blend_shapes: Vec<String>,
}

impl Puppet {
    /// Build from a parsed model and the image object's `animationlayers`
    /// (raw scene JSON). Layers whose animation id is not in the file are
    /// kept but play nothing.
    pub fn new(mdl: &Mdl, animation_layers: &[Value]) -> Self {
        let source = mdl.skeleton.as_ref().map_or(&[][..], |s| &s.bones[..]);
        let mut bind_world: Vec<Mat4> = Vec::with_capacity(source.len());
        let mut rest_world: Vec<Mat4> = Vec::with_capacity(source.len());
        let mut bones = Vec::with_capacity(source.len());
        for b in source {
            let parent_bind = b.parent.map_or(Mat4::IDENTITY, |p| bind_world[p]);
            let parent_rest = b.parent.map_or(Mat4::IDENTITY, |p| rest_world[p]);
            bind_world.push(parent_bind * b.bind);
            rest_world.push(parent_rest * b.rest_local());
            let (scale, rotation, translation) = b.rest_local().to_scale_rotation_translation();
            bones.push(PuppetBone {
                parent: b.parent,
                rest_translation: translation,
                rest_scale: scale,
                rest_rotation: rotation,
                inverse_bind_world: bind_world.last().unwrap().inverse(),
                rest_world: *rest_world.last().unwrap(),
            });
        }
        let animations: Vec<Animation> = mdl
            .animations
            .iter()
            .map(|a| Animation {
                id: a.id,
                mode: a.mode,
                fps: f64::from(a.fps),
                length: a.length,
                tracks: a
                    .tracks
                    .iter()
                    .map(|t| {
                        t.iter()
                            .map(|f| Frame {
                                position: f.position,
                                scale: f.scale,
                                rotation: Quat::from_euler(
                                    EulerRot::ZYX,
                                    f.angle.z,
                                    f.angle.y,
                                    f.angle.x,
                                ),
                            })
                            .collect()
                    })
                    .collect(),
                morph_tracks: a.morph_tracks.clone(),
            })
            .collect();
        let mut puppet = Self {
            bones,
            animations,
            layers: Vec::new(),
            blend_shapes: mdl.blend_shapes().iter().map(|m| m.name.clone()).collect(),
        };
        puppet.set_layers(
            animation_layers
                .iter()
                .filter_map(AnimationLayer::from_json)
                .collect(),
        );
        puppet
    }

    /// Replace the animation layers (e.g. after a user property changed).
    pub fn set_layers(&mut self, layers: Vec<AnimationLayer>) {
        self.layers = layers
            .into_iter()
            .map(|params| Layer {
                animation: self
                    .animations
                    .iter()
                    .position(|a| a.id == params.animation),
                params,
            })
            .collect();
    }

    /// The current animation layers, bottom to top.
    pub fn layers(&self) -> impl Iterator<Item = &AnimationLayer> {
        self.layers.iter().map(|l| &l.params)
    }

    /// Number of layers whose animation exists in the model.
    pub fn matched_layers(&self) -> usize {
        self.layers.iter().filter(|l| l.animation.is_some()).count()
    }

    /// Parent index of each bone.
    pub fn bone_parents(&self) -> Vec<Option<usize>> {
        self.bones.iter().map(|b| b.parent).collect()
    }

    /// Number of bones; the value of the `BONECOUNT` shader combo.
    pub fn bone_count(&self) -> usize {
        self.bones.len()
    }

    /// Rest-pose world transform of bone `i` in puppet space (where the bone
    /// sits on the assembled character; used for attachments).
    pub fn rest_world(&self, i: usize) -> Option<Mat4> {
        self.bones.get(i).map(|b| b.rest_world)
    }

    /// Skinning matrices at `time` seconds since the layer started playing.
    pub fn bone_matrices(&self, time: f64, out: &mut Vec<Mat4>) {
        self.bone_matrices_with(
            |_, p| LayerPose {
                time: time * p.rate,
                blend: p.blend,
                visible: p.visible,
            },
            out,
        );
    }

    /// Skinning matrices with each animation layer (by index) where `pose`
    /// puts it, e.g. under a script's control.
    pub fn bone_matrices_with(
        &self,
        pose: impl Fn(usize, &AnimationLayer) -> LayerPose,
        out: &mut Vec<Mat4>,
    ) {
        self.pose_bones(pose, |_, world| world, out);
        self.skin(out);
    }

    /// Turn bone world transforms from [`Self::pose_bones`] into skinning
    /// matrices.
    pub fn skin(&self, worlds: &mut [Mat4]) {
        for (m, bone) in worlds.iter_mut().zip(&self.bones) {
            *m *= bone.inverse_bind_world;
        }
    }

    /// Where each bone is in puppet space (the mesh's space) with each
    /// animation layer where `pose` puts it. `adjust` may move a bone (e.g.
    /// dragged or bouncing); its children follow.
    pub fn pose_bones(
        &self,
        pose: impl Fn(usize, &AnimationLayer) -> LayerPose,
        adjust: impl Fn(usize, Mat4) -> Mat4,
        out: &mut Vec<Mat4>,
    ) {
        let poses: Vec<LayerPose> = self
            .layers
            .iter()
            .enumerate()
            .map(|(i, l)| pose(i, &l.params))
            .collect();
        out.clear();
        out.reserve(self.bones.len());
        for (i, bone) in self.bones.iter().enumerate() {
            let mut translation = bone.rest_translation;
            let mut scale = bone.rest_scale;
            let mut rotation = bone.rest_rotation;
            for (layer, pose) in self.layers.iter().zip(&poses) {
                let Some(anim) = layer.animation.map(|a| &self.animations[a]) else {
                    continue;
                };
                if !pose.visible {
                    continue;
                }
                let Some(track) = anim.tracks.get(i).filter(|t| !t.is_empty()) else {
                    continue;
                };
                let (a, b, t) = anim.sample_point(pose.time);
                let last = track.len() - 1;
                let (fa, fb, base) = (track[a.min(last)], track[b.min(last)], track[0]);
                let pos = fa.position.lerp(fb.position, t);
                let scl = fa.scale.lerp(fb.scale, t);
                let rot = fa.rotation.slerp(fb.rotation, t);
                let w = pose.blend.clamp(0.0, 1.0) as f32;
                if layer.params.additive {
                    translation += w * (pos - base.position);
                    scale += w * (scl - base.scale);
                    let delta = rot * base.rotation.conjugate();
                    rotation *= Quat::IDENTITY.slerp(delta, w);
                } else {
                    translation = translation.lerp(pos, w);
                    scale = scale.lerp(scl, w);
                    rotation = rotation.slerp(rot, w);
                }
            }
            let local = Mat4::from_scale_rotation_translation(scale, rotation, translation);
            let world = bone.parent.map_or(local, |p| out[p] * local);
            out.push(adjust(i, world));
        }
    }

    /// `g_Bones` contents at `time` seconds: 16 floats per bone, each bone a
    /// column-major 4×4 affine matrix (columns 0–2 linear part, column 3
    /// translation; the fourth float of every column is 0, 0, 0, 1).
    ///
    /// This is byte-for-byte the std140 layout of `uniform mat4x3
    /// g_Bones[BONECOUNT]` (four vec3 columns, each padded to 16 bytes, so 64
    /// bytes per element) and needs no transpose: the shaders compute
    /// `mul(vec4(pos, 1), g_Bones[i])` = `g_Bones[i] * vec4(pos, 1)`. For a
    /// tightly packed `mat4x3` drop every fourth float.
    pub fn bones(&self, time: f64, out: &mut Vec<f32>) {
        let mut mats = Vec::with_capacity(self.bones.len());
        self.bone_matrices(time, &mut mats);
        out.clear();
        out.extend(mats.iter().flat_map(Mat4::to_cols_array));
    }

    /// Number of blend shapes of the warp mesh.
    pub fn blend_shape_count(&self) -> usize {
        self.blend_shapes.len()
    }

    /// Names of the warp mesh's blend shapes, by index (empty strings when
    /// the file stores none).
    pub fn blend_shape_names(&self) -> &[String] {
        &self.blend_shapes
    }

    /// Index of the blend shape called `name` (SceneScript
    /// `getBlendShapeIndex`, which reports a miss as -1).
    pub fn blend_shape_index(&self, name: &str) -> Option<usize> {
        self.blend_shapes.iter().position(|n| n == name)
    }

    /// Blend shape weights at `time` seconds since the layers started
    /// playing, starting from `base` (e.g. weights set by a script; missing
    /// entries are 0). See [`Self::morph_weights_with`].
    pub fn morph_weights(&self, time: f64, base: &[f32], out: &mut Vec<f32>) {
        self.morph_weights_with(
            |_, p| LayerPose {
                time: time * p.rate,
                blend: p.blend,
                visible: p.visible,
            },
            base,
            out,
        );
    }

    /// Blend shape weights (one per blend shape of the warp mesh) with each
    /// animation layer where `pose` puts it. Layers combine like bone
    /// channels: a weight track interpolated between frames; non-additive
    /// layers blend from the weight below toward it by `blend`, additive
    /// layers add `blend ×` its change since frame 0. Layers without a track
    /// for a blend shape leave it alone.
    pub fn morph_weights_with(
        &self,
        pose: impl Fn(usize, &AnimationLayer) -> LayerPose,
        base: &[f32],
        out: &mut Vec<f32>,
    ) {
        out.clear();
        out.extend((0..self.blend_shapes.len()).map(|k| base.get(k).copied().unwrap_or(0.0)));
        for (i, layer) in self.layers.iter().enumerate() {
            let Some(anim) = layer.animation.map(|a| &self.animations[a]) else {
                continue;
            };
            if anim.morph_tracks.is_empty() {
                continue;
            }
            let pose = pose(i, &layer.params);
            if !pose.visible {
                continue;
            }
            let (a, b, t) = anim.sample_point(pose.time);
            let w = pose.blend.clamp(0.0, 1.0) as f32;
            for (weight, track) in out.iter_mut().zip(&anim.morph_tracks) {
                if track.is_empty() {
                    continue;
                }
                let last = track.len() - 1;
                let value = track[a.min(last)] + (track[b.min(last)] - track[a.min(last)]) * t;
                if layer.params.additive {
                    *weight += w * (value - track[0]);
                } else {
                    *weight += (value - *weight) * w;
                }
            }
        }
    }

    /// Length of animation layer `layer`'s animation in seconds, if it exists.
    pub fn layer_duration(&self, layer: usize) -> Option<f64> {
        let anim = &self.animations[self.layers.get(layer)?.animation?];
        (anim.fps > 0.0).then(|| f64::from(anim.length) / anim.fps)
    }
}

/// Where one animation layer is: its time in seconds (rate applied), weight
/// and visibility.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerPose {
    pub time: f64,
    pub blend: f64,
    pub visible: bool,
}
