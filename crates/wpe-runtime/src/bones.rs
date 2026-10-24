//! Puppet bones as scripts see them: the current pose for queries, and
//! displacements scripts make (`setBoneTransform`, physics impulses) that
//! spring back to the animated pose.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use glam::{Mat4, Quat, Vec3};

/// Spring pulling displaced bones back, per second squared per unit, and
/// its damping per second: settles in about a second with a little wobble.
const STIFFNESS: f32 = 80.0;
const DAMPING: f32 = 9.0;

/// A named point rigged to a bone, in puppet space at rest.
#[derive(Debug, Clone)]
pub struct Attachment {
    pub name: String,
    pub bone: usize,
    pub transform: Mat4,
}

/// One puppet's bones.
#[derive(Debug, Default)]
pub struct PuppetBones {
    pub names: Vec<String>,
    pub parents: Vec<Option<usize>>,
    /// Each bone at rest, in puppet space.
    pub rest: Vec<Mat4>,
    pub attachments: Vec<Attachment>,
    /// The model's animations: name and id (for `createAnimationLayer`).
    pub animations: Vec<(String, i32)>,
    /// Where each bone was last drawn, in puppet space, displacement included.
    pub worlds: Vec<Mat4>,
    /// Rotations scripts gave bones relative to their parent
    /// (`setLocalBoneAngles` / `setLocalBoneTransform`).
    pub rotations: Vec<Option<Quat>>,
    /// Blend shape weights scripts set (`setBlendShapeWeight`), names by
    /// index, and the weights drawn last frame (animation tracks included).
    pub blend_weights: Vec<f32>,
    pub blend_names: Vec<String>,
    pub current_weights: Vec<f32>,
    /// Displacement of each bone from its animated position, and its velocity.
    offsets: Vec<Vec3>,
    velocities: Vec<Vec3>,
}

impl PuppetBones {
    pub fn new(names: Vec<String>, parents: Vec<Option<usize>>) -> Self {
        let n = names.len();
        Self {
            names,
            parents,
            rest: vec![Mat4::IDENTITY; n],
            attachments: Vec::new(),
            animations: Vec::new(),
            worlds: vec![Mat4::IDENTITY; n],
            rotations: vec![None; n],
            blend_weights: Vec::new(),
            blend_names: Vec::new(),
            current_weights: Vec::new(),
            offsets: vec![Vec3::ZERO; n],
            velocities: vec![Vec3::ZERO; n],
        }
    }

    /// Bone index by name, or the number itself (`getBoneTransform("3")`).
    pub fn bone(&self, key: &str) -> Option<usize> {
        self.names
            .iter()
            .position(|n| !n.is_empty() && n == key)
            .or_else(|| key.parse().ok().filter(|&i: &usize| i < self.names.len()))
    }

    /// Attachment index by name, or the number itself.
    pub fn attachment(&self, key: &str) -> Option<usize> {
        self.attachments
            .iter()
            .position(|a| a.name == key)
            .or_else(|| {
                key.parse()
                    .ok()
                    .filter(|&i: &usize| i < self.attachments.len())
            })
    }

    /// Blend shape index by name, or the number itself.
    pub fn blend_shape(&self, key: &str) -> Option<usize> {
        self.blend_names
            .iter()
            .position(|n| !n.is_empty() && n == key)
            .or_else(|| {
                key.parse()
                    .ok()
                    .filter(|&i: &usize| i < self.blend_weights.len())
            })
    }

    /// Where attachment `i` is now, in puppet space: it moves with its bone.
    pub fn attachment_pose(&self, i: usize) -> Option<Mat4> {
        let a = self.attachments.get(i)?;
        let now = self.worlds.get(a.bone).copied().unwrap_or(Mat4::IDENTITY);
        let rest = self.rest.get(a.bone).copied().unwrap_or(Mat4::IDENTITY);
        Some(now * rest.inverse() * a.transform)
    }

    /// The bone nearest to puppet-space `point`.
    pub fn nearest(&self, point: Vec3) -> Option<usize> {
        (0..self.worlds.len()).min_by(|&a, &b| {
            let d = |i: usize| self.worlds[i].w_axis.truncate().distance_squared(point);
            d(a).total_cmp(&d(b))
        })
    }

    /// Bone `i`'s world transform with script rotation and displacement,
    /// given its animated `world` and its parent's final world.
    pub fn adjust(&self, i: usize, world: Mat4, parent: Mat4) -> Mat4 {
        let mut world = world;
        if let Some(Some(rotation)) = self.rotations.get(i) {
            let (scale, _, translation) =
                (parent.inverse() * world).to_scale_rotation_translation();
            world = parent * Mat4::from_scale_rotation_translation(scale, *rotation, translation);
        }
        match self.offsets.get(i) {
            Some(&o) if o != Vec3::ZERO => Mat4::from_translation(o) * world,
            _ => world,
        }
    }

    /// The displacement to apply to bone `i` on top of its animated pose.
    #[cfg(test)]
    pub fn offset(&self, i: usize) -> Vec3 {
        self.offsets.get(i).copied().unwrap_or(Vec3::ZERO)
    }

    /// Move bone `i` to puppet-space `position`; it springs back from there.
    pub fn place(&mut self, i: usize, position: Vec3) {
        let (Some(world), Some(offset)) = (self.worlds.get(i), self.offsets.get(i)) else {
            return;
        };
        let animated = world.w_axis.truncate() - *offset;
        self.offsets[i] = position - animated;
        self.velocities[i] = Vec3::ZERO;
    }

    /// Push bone `i` with a puppet-space velocity.
    pub fn impulse(&mut self, i: usize, velocity: Vec3) {
        if let Some(v) = self.velocities.get_mut(i) {
            *v += velocity;
        }
    }

    pub fn reset(&mut self, bone: Option<usize>) {
        for i in 0..self.offsets.len() {
            if bone.is_none_or(|b| b == i) {
                self.offsets[i] = Vec3::ZERO;
                self.velocities[i] = Vec3::ZERO;
            }
        }
    }

    /// Give bone `i` a rotation relative to its parent (`None` returns it
    /// to its animation).
    pub fn rotate(&mut self, i: usize, rotation: Option<Quat>) {
        if let Some(r) = self.rotations.get_mut(i) {
            *r = rotation;
        }
    }

    fn step(&mut self, dt: f32) {
        // Small fixed steps keep the spring stable at low frame rates.
        let steps = (dt / (1.0 / 120.0)).ceil().clamp(1.0, 60.0) as usize;
        let h = dt / steps as f32;
        for _ in 0..steps {
            for (x, v) in self.offsets.iter_mut().zip(&mut self.velocities) {
                *v += (-STIFFNESS * *x - DAMPING * *v) * h;
                *x += *v * h;
            }
        }
    }
}

/// Every puppet's bones, by layer id.
#[derive(Debug, Default)]
pub struct Bones {
    pub puppets: HashMap<i32, PuppetBones>,
}

/// Shared by the frame's bone bindings and the script host.
pub type SharedBones = Rc<RefCell<Bones>>;

impl Bones {
    /// Add a puppet unless it's known (displacements outlive rebuilds).
    pub fn register(&mut self, layer: i32, bones: PuppetBones) {
        self.puppets.entry(layer).or_insert(bones);
    }

    /// Advance every displaced bone's spring by `dt` seconds.
    pub fn step(&mut self, dt: f32) {
        for puppet in self.puppets.values_mut() {
            puppet.step(dt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placed_bones_spring_back() {
        let mut p = PuppetBones::new(vec!["a".into()], vec![None]);
        p.worlds[0] = Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0));
        p.place(0, Vec3::new(30.0, 0.0, 0.0));
        assert_eq!(p.offset(0), Vec3::new(20.0, 0.0, 0.0));
        for _ in 0..120 {
            p.step(1.0 / 60.0);
        }
        assert!(p.offset(0).length() < 0.5, "settled: {}", p.offset(0));
    }
}
