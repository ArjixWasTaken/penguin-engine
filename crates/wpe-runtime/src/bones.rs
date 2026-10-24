//! Puppet bones as scripts see them: the current pose for queries, and
//! displacements scripts make (`setBoneTransform`, physics impulses) that
//! spring back to the animated pose.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use glam::{Mat4, Vec3};

/// Spring pulling displaced bones back, per second squared per unit, and
/// its damping per second: settles in about a second with a little wobble.
const STIFFNESS: f32 = 80.0;
const DAMPING: f32 = 9.0;

/// One puppet's bones.
#[derive(Debug, Default)]
pub struct PuppetBones {
    pub names: Vec<String>,
    pub parents: Vec<Option<usize>>,
    /// Where each bone was last drawn, in puppet space, displacement included.
    pub worlds: Vec<Mat4>,
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
            worlds: vec![Mat4::IDENTITY; n],
            offsets: vec![Vec3::ZERO; n],
            velocities: vec![Vec3::ZERO; n],
        }
    }

    /// The displacement to apply to bone `i` on top of its animated pose.
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
