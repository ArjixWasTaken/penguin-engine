//! CPU particle simulation for Wallpaper Engine particle systems.
//!
//! [`ParticleSystem::load`] reads a `particles/*.json` definition (plus its
//! `children`), [`ParticleSystem::update`] advances it, and
//! [`Subsystem::mesh`] produces vertex streams laid out for Wallpaper
//! Engine's own `genericparticle` / `genericropeparticle` shaders with
//! `GS_ENABLED=0`; ribbons are expanded on the CPU instead of in a geometry
//! shader.
//!
//! Each node of the tree ([`ParticleSystem::subsystems`], root first,
//! parents before children) is drawn separately with its own material,
//! combos and uniforms. Simulation is deterministic for a given seed and
//! sequence of `dt` values.

pub mod def;
mod json;
mod noise;
mod ops;
mod render;
pub mod rng;
mod system;

use std::collections::BTreeMap;

use glam::{Mat4, Vec3};
use serde_json::Value;
use wpe_vfs::Vfs;

pub use def::{InstanceOverride, ParticleDef, RendererKind, SpawnType};
pub use ops::Particle;
pub use system::{MAX_PARTICLES, MAX_PREWARM, MAX_STEP, PREWARM_STEP, Subsystem};

/// Loading a particle system failed.
#[derive(Debug, thiserror::Error)]
pub enum ParticleError {
    #[error(transparent)]
    Asset(#[from] wpe_assets::AssetError),
    #[error("{path}: {message}")]
    Invalid { path: String, message: String },
}

/// Camera information used to orient ribbons (`rope`, `ropetrail`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewInfo {
    /// Eye position in world space (used for perspective cameras).
    pub eye: Vec3,
    /// Viewing direction in world space (used for orthographic cameras).
    pub forward: Vec3,
    pub orthographic: bool,
}

impl Default for ViewInfo {
    fn default() -> Self {
        Self {
            eye: Vec3::new(0.0, 0.0, 1000.0),
            forward: Vec3::NEG_Z,
            orthographic: true,
        }
    }
}

/// Per-frame inputs from the renderer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UpdateContext {
    /// Object-to-world transform of the scene particle object.
    pub transform: Mat4,
    /// Cursor position in world space; drives link-mouse control points.
    pub cursor: Option<Vec3>,
    /// Camera for ribbon orientation.
    pub view: ViewInfo,
    /// Emission-rate multiplier for audio-reactive emitters (constant 1 until audio exists).
    pub audio_rate: f32,
}

impl Default for UpdateContext {
    fn default() -> Self {
        Self {
            transform: Mat4::IDENTITY,
            cursor: None,
            view: ViewInfo::default(),
            audio_rate: 1.0,
        }
    }
}

/// CPU vertex data for one draw: named attribute streams plus a triangle list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParticleMesh {
    /// `(shader attribute name, components per vertex, data)`.
    pub attributes: Vec<(String, u8, Vec<f32>)>,
    pub indices: Vec<u32>,
    pub vertex_count: u32,
}

impl ParticleMesh {
    /// Data of attribute `name`.
    pub fn attribute(&self, name: &str) -> Option<&[f32]> {
        self.attributes
            .iter()
            .find(|a| a.0 == name)
            .map(|a| a.2.as_slice())
    }
}

/// A loaded particle system tree and its simulation state.
#[derive(Debug, Clone)]
pub struct ParticleSystem {
    subs: Vec<Subsystem>,
    rng: rng::Rng,
    seed: u64,
    view: ViewInfo,
    warmed: bool,
    time: f64,
    warnings: Vec<String>,
}

impl ParticleSystem {
    /// Load `path` (the scene object's `particle` key) with the object's
    /// `instanceoverride` applied. Children that fail to load are skipped
    /// and reported in [`Self::warnings`].
    pub fn load(
        vfs: &Vfs,
        path: &str,
        instance_override: Option<&Value>,
    ) -> Result<Self, ParticleError> {
        let over = InstanceOverride::from_json(instance_override);
        let mut subs = Vec::new();
        let mut warnings = Vec::new();
        system::load_root(vfs, path, &over, &mut subs, &mut warnings)?;
        let mut s = Self {
            subs,
            rng: rng::Rng::new(0),
            seed: 0,
            view: ViewInfo::default(),
            warmed: false,
            time: 0.0,
            warnings,
        };
        s.set_seed(0);
        Ok(s)
    }

    /// Replace the `instanceoverride` (scripts' `thisLayer.instance`).
    pub fn set_override(&mut self, over: &InstanceOverride) {
        for sub in &mut self.subs {
            sub.set_override(over);
        }
    }

    /// Emit `count` extra particles from the root at the next update
    /// (`emitParticles`).
    pub fn emit(&mut self, count: u32) {
        self.subs[0].add_burst(count);
    }

    /// Restart the simulation from scratch with a new random seed.
    pub fn set_seed(&mut self, seed: u64) {
        self.seed = seed;
        self.rng = rng::Rng::new(seed);
        system::reset(&mut self.subs, &mut self.rng);
        self.warmed = false;
        self.time = 0.0;
    }

    /// The seed of the current run.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Simulated seconds since the start (excluding `starttime` pre-simulation).
    pub fn time(&self) -> f64 {
        self.time
    }

    /// Every node of the tree, root first; parents precede their children.
    pub fn subsystems(&self) -> &[Subsystem] {
        &self.subs
    }

    /// The root subsystem.
    pub fn root(&self) -> &Subsystem {
        &self.subs[0]
    }

    /// Material JSON path of the root subsystem.
    pub fn material(&self) -> &str {
        self.root().material()
    }

    /// Shader combos of the root subsystem.
    pub fn combos(&self) -> BTreeMap<String, String> {
        self.root().combos()
    }

    /// Constant uniforms of the root subsystem.
    pub fn uniforms(&self) -> BTreeMap<String, Vec<f32>> {
        self.root().uniforms()
    }

    /// Unsupported entries (`operator:foo`, ...) across the whole tree.
    pub fn unknown_features(&self) -> impl Iterator<Item = &str> {
        self.subs
            .iter()
            .flat_map(|s| s.def.unknown.iter().map(String::as_str))
    }

    /// Non-fatal load problems (missing children, materials, textures).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Live particles across the whole tree.
    pub fn particle_count(&self) -> usize {
        self.subs.iter().map(Subsystem::particle_count).sum()
    }

    /// Advance by `dt` seconds, split into equal steps of at most [`MAX_STEP`].
    /// The first call also pre-simulates the definitions' `starttime`.
    pub fn update(&mut self, dt: f32, ctx: &UpdateContext) {
        self.view = ctx.view;
        if !self.warmed {
            self.warmed = true;
            let warm = self
                .subs
                .iter()
                .map(|s| s.def.start_time)
                .fold(0.0f32, f32::max)
                .min(MAX_PREWARM);
            self.advance(warm, PREWARM_STEP, ctx);
        }
        if dt.is_finite() && dt > 0.0 {
            self.advance(dt, MAX_STEP, ctx);
            self.time += dt as f64;
        }
    }

    fn advance(&mut self, dt: f32, max_step: f32, ctx: &UpdateContext) {
        if dt <= 1e-6 {
            return;
        }
        let steps = ((dt / max_step) - 1e-3).ceil().max(1.0) as u32;
        let step = dt / steps as f32;
        for _ in 0..steps {
            system::step(&mut self.subs, step, ctx, &mut self.rng);
        }
    }

    /// Vertex data of the root subsystem; see [`Subsystem::mesh`] for the others.
    pub fn mesh(&self, out: &mut ParticleMesh) {
        self.subs[0].mesh(&self.view, out);
    }

    /// Camera used by the last update, for [`Subsystem::mesh`].
    pub fn view(&self) -> &ViewInfo {
        &self.view
    }
}
