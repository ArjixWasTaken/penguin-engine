//! Particle system tree: loading, emission, child instancing and stepping.

use std::collections::VecDeque;

use glam::{Mat4, Quat, Vec3};
use serde_json::Value;
use wpe_vfs::Vfs;

use crate::def::{
    CONTROL_POINTS, CP_FOLLOW_PARENT_PARTICLE, CP_LINK_MOUSE, CP_WORLD_SPACE, ControlPointDef,
    EMITTER_ONE_PER_FRAME, EmitterDef, EmitterShape, FLAG_PERSPECTIVE, FLAG_WORLD_SPACE,
    InstanceOverride, ParticleDef, RendererKind, SpawnType,
};
use crate::ops::{self, InitState, OpCtx, Particle, SpawnCtx};
use crate::render::{self, RenderSetup};
use crate::rng::Rng;
use crate::{ParticleError, ParticleMesh, UpdateContext};

/// Longest simulation step; longer updates are split into equal steps.
pub const MAX_STEP: f32 = 1.0 / 30.0;
/// Step used while pre-simulating `starttime`.
pub const PREWARM_STEP: f32 = 1.0 / 15.0;
/// Upper bound on `starttime` pre-simulation, in seconds.
pub const MAX_PREWARM: f32 = 300.0;
/// Per-subsystem particle cap.
pub const MAX_PARTICLES: u32 = 20_000;
const MAX_DEPTH: usize = 8;

fn local_transform(origin: Vec3, scale: Vec3, angles: Vec3) -> Mat4 {
    let rot = Quat::from_rotation_z(angles.z)
        * Quat::from_rotation_y(angles.y)
        * Quat::from_rotation_x(angles.x);
    Mat4::from_scale_rotation_translation(scale, rot, origin)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Bound {
    parent_instance: usize,
    particle: usize,
    particle_id: u32,
    /// Event position in this subsystem's simulation space.
    pos: Vec3,
}

#[derive(Debug, Clone, Default)]
struct PeriodicState {
    active: bool,
    remaining: f32,
    emitted: u32,
}

#[derive(Debug, Clone, Default)]
struct EmitterState {
    timer: f32,
    elapsed: f32,
    burst_done: bool,
    periodic: Option<PeriodicState>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TrailPoint {
    pub position: Vec3,
    pub size: f32,
    pub color: Vec3,
    pub age: f32,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Trail {
    /// Newest first.
    pub points: VecDeque<TrailPoint>,
    timer: f32,
}

#[derive(Debug, Clone)]
pub(crate) struct Instance {
    pub particles: Vec<Particle>,
    pub trails: Vec<Trail>,
    emitters: Vec<EmitterState>,
    bound: Option<Bound>,
    dead: bool,
    pub no_live: bool,
    cps: [Vec3; CONTROL_POINTS],
    cp_from_bound: [bool; CONTROL_POINTS],
    cp_velocity: [Vec3; CONTROL_POINTS],
    cp_prev: Option<[Vec3; CONTROL_POINTS]>,
}

impl Instance {
    fn new(emitters: usize, bound: Option<Bound>) -> Self {
        Self {
            particles: Vec::new(),
            trails: Vec::new(),
            emitters: vec![EmitterState::default(); emitters],
            bound,
            dead: false,
            no_live: false,
            cps: [Vec3::ZERO; CONTROL_POINTS],
            cp_from_bound: [false; CONTROL_POINTS],
            cp_velocity: [Vec3::ZERO; CONTROL_POINTS],
            cp_prev: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SpawnRequest {
    parent_instance: usize,
    particle: usize,
    particle_id: u32,
    /// World-space event position.
    world: Vec3,
}

/// One node of a particle system tree: the root definition or a child.
#[derive(Debug, Clone)]
pub struct Subsystem {
    name: String,
    pub(crate) def: ParticleDef,
    parent: Option<usize>,
    children: Vec<(usize, SpawnType)>,
    spawn: SpawnType,
    max_instances: u32,
    probability: f32,
    cp_shift: i32,
    cp_defs: [ControlPointDef; CONTROL_POINTS],
    local_to_object: Mat4,
    max_count: u32,
    time_scale: f32,
    rate_scale: f32,
    over: InstanceOverride,
    pub(crate) instances: Vec<Instance>,
    time: f64,
    next_id: u32,
    op_random: Vec<[f32; 2]>,
    init_state: Vec<InitState>,
    pending: Vec<SpawnRequest>,
    pub(crate) sim_to_world: Mat4,
    world_to_sim: Mat4,
    pub(crate) render: Option<RenderSetup>,
}

impl Subsystem {
    /// Particle definition path this subsystem was loaded from.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Index of the parent subsystem in [`crate::ParticleSystem::subsystems`].
    pub fn parent(&self) -> Option<usize> {
        self.parent
    }

    /// How this subsystem creates instances relative to its parent.
    pub fn spawn_type(&self) -> SpawnType {
        self.spawn
    }

    /// The parsed definition.
    pub fn definition(&self) -> &ParticleDef {
        &self.def
    }

    /// Whether this subsystem draws anything (spawner-only systems do not).
    pub fn is_renderable(&self) -> bool {
        self.render.is_some()
    }

    /// Renderer kind, `None` for spawner-only subsystems.
    pub fn renderer(&self) -> Option<RendererKind> {
        self.render.as_ref().map(|r| r.kind)
    }

    /// Material JSON path (relative to the asset root), empty when not renderable.
    pub fn material(&self) -> &str {
        self.render
            .as_ref()
            .map_or("", |r| r.material_path.as_str())
    }

    /// The loaded material, if it parsed.
    pub fn material_def(&self) -> Option<&wpe_assets::Material> {
        self.render.as_ref().and_then(|r| r.material.as_ref())
    }

    /// Shader to compile instead of the material's (`genericparticle` / `genericropeparticle`).
    pub fn shader(&self) -> &str {
        self.render.as_ref().map_or("", |r| r.shader)
    }

    /// Shader combos: the material pass combos plus the ones the vertex layout requires.
    pub fn combos(&self) -> std::collections::BTreeMap<String, String> {
        self.render
            .as_ref()
            .map(|r| r.combos.clone())
            .unwrap_or_default()
    }

    /// Constant uniforms the renderer must set, overriding its own values.
    pub fn uniforms(&self) -> std::collections::BTreeMap<String, Vec<f32>> {
        self.render
            .as_ref()
            .map(|r| r.uniforms.clone())
            .unwrap_or_default()
    }

    /// Transform from this subsystem's local space to the scene object's space
    /// (identity for the root; child `origin`/`scale`/`angles` otherwise).
    pub fn local_to_object(&self) -> Mat4 {
        self.local_to_object
    }

    /// Particles are simulated (and meshed) in world space; draw with an identity model matrix.
    pub fn world_space(&self) -> bool {
        self.def.flags & FLAG_WORLD_SPACE != 0
    }

    /// Draw with the scene's perspective camera instead of the orthographic one.
    pub fn perspective(&self) -> bool {
        self.def.flags & FLAG_PERSPECTIVE != 0
    }

    /// Model matrix for the last update: simulation space to world space.
    pub fn model_matrix(&self) -> Mat4 {
        self.sim_to_world
    }

    /// Live particles of every instance.
    pub fn particles(&self) -> impl Iterator<Item = &Particle> {
        self.instances
            .iter()
            .flat_map(|i| i.particles.iter())
            .filter(|p| p.alive())
    }

    /// Number of live particles.
    pub fn particle_count(&self) -> usize {
        self.particles().count()
    }

    /// Number of instances (one per bound parent particle for event children).
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    /// Fill `out` with this subsystem's vertex data. Empty when not renderable.
    pub fn mesh(&self, view: &crate::ViewInfo, out: &mut ParticleMesh) {
        render::build_mesh(self, view, out);
    }

    fn reset_state(&mut self, rng: &mut Rng) {
        self.instances.clear();
        self.time = 0.0;
        self.next_id = 0;
        self.pending.clear();
        self.init_state = vec![InitState::default(); self.def.initializers.len()];
        self.op_random = (0..self.def.operators.len())
            .map(|_| [rng.unit(), rng.unit()])
            .collect();
    }
}

fn read_def(vfs: &Vfs, path: &str) -> Result<ParticleDef, ParticleError> {
    let v: Value = wpe_assets::load_json(vfs, path)?;
    ParticleDef::from_json(&v).map_err(|message| ParticleError::Invalid {
        path: path.to_owned(),
        message,
    })
}

/// Load `path` and its children, depth first, appending to `out`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn load_tree(
    vfs: &Vfs,
    path: &str,
    def: ParticleDef,
    over: &InstanceOverride,
    parent: Option<usize>,
    child: Option<&crate::def::ChildDef>,
    depth: usize,
    out: &mut Vec<Subsystem>,
    warnings: &mut Vec<String>,
) {
    let mut cp_defs = def.control_points;
    for (slot, o) in over.control_points.iter().enumerate() {
        if let Some(offset) = o {
            cp_defs[slot].offset = *offset;
            cp_defs[slot].null_offset = false;
        }
    }
    let rel = child.map_or(Mat4::IDENTITY, |c| {
        local_transform(c.origin, c.scale, c.angles)
    });
    let local_to_object = parent.map_or(Mat4::IDENTITY, |p| out[p].local_to_object) * rel;
    let count = if over.count > 0.0 { over.count } else { 1.0 };
    let max_count = ((def.max_count as f32 * count).ceil() as u32).clamp(1, MAX_PARTICLES);
    let render = def.renderer.map(|r| render::setup(vfs, &def, r, warnings));
    let children = def.children.clone();
    let index = out.len();
    out.push(Subsystem {
        name: path.to_owned(),
        parent,
        children: Vec::new(),
        spawn: child.map_or(SpawnType::Static, |c| c.spawn),
        max_instances: child.map_or(1, |c| c.max_count),
        probability: child.map_or(1.0, |c| c.probability),
        cp_shift: child.map_or(0, |c| c.cp_start_index),
        cp_defs,
        local_to_object,
        max_count,
        time_scale: over.time_scale(),
        rate_scale: count,
        over: over.clone(),
        instances: Vec::new(),
        time: 0.0,
        next_id: 0,
        op_random: Vec::new(),
        init_state: Vec::new(),
        pending: Vec::new(),
        sim_to_world: Mat4::IDENTITY,
        world_to_sim: Mat4::IDENTITY,
        render,
        def,
    });
    if let Some(p) = parent {
        let spawn = out[index].spawn;
        out[p].children.push((index, spawn));
    }
    if depth >= MAX_DEPTH {
        if !children.is_empty() {
            warnings.push(format!("{path}: children nested deeper than {MAX_DEPTH}"));
        }
        return;
    }
    for c in &children {
        match read_def(vfs, &c.name) {
            Ok(d) => load_tree(
                vfs,
                &c.name,
                d,
                over,
                Some(index),
                Some(c),
                depth + 1,
                out,
                warnings,
            ),
            Err(e) => {
                tracing::warn!("particle child {}: {e}", c.name);
                warnings.push(format!("child {}: {e}", c.name));
            }
        }
    }
}

pub(crate) fn load_root(
    vfs: &Vfs,
    path: &str,
    over: &InstanceOverride,
    out: &mut Vec<Subsystem>,
    warnings: &mut Vec<String>,
) -> Result<(), ParticleError> {
    let def = read_def(vfs, path)?;
    load_tree(vfs, path, def, over, None, None, 0, out, warnings);
    Ok(())
}

pub(crate) fn reset(subs: &mut [Subsystem], rng: &mut Rng) {
    for s in subs {
        s.reset_state(rng);
    }
}

fn emit_count(
    e: &EmitterDef,
    st: &mut EmitterState,
    dt: f32,
    rate_scale: f32,
    rng: &mut Rng,
) -> u32 {
    st.elapsed += dt;
    if st.elapsed < e.delay {
        return 0;
    }
    let mut n = 0u32;
    if !st.burst_done {
        st.burst_done = true;
        n += e.instantaneous;
    }
    if e.duration > 0.0 && st.elapsed - e.delay > e.duration {
        return n;
    }
    let rate = e.rate * rate_scale;
    if rate <= 0.0 || !rate.is_finite() {
        return n;
    }
    if let Some(p) = &e.periodic {
        let ps = st.periodic.get_or_insert_with(|| PeriodicState {
            active: true,
            remaining: rng.range(p.min_duration, p.max_duration.max(p.min_duration)),
            emitted: 0,
        });
        ps.remaining -= dt;
        let mut guard = 0;
        while ps.remaining <= 0.0 && guard < 64 {
            guard += 1;
            ps.active = !ps.active;
            let (lo, hi) = if ps.active {
                (p.min_duration, p.max_duration)
            } else {
                (p.min_delay, p.max_delay)
            };
            ps.remaining += rng.range(lo, hi.max(lo)).max(0.01);
            ps.emitted = 0;
            if ps.active {
                st.timer = 0.0;
            }
        }
        if !ps.active {
            return n;
        }
    }
    st.timer += dt;
    let mut k = (st.timer * rate).floor();
    st.timer -= k / rate;
    if st.timer < 0.0 {
        st.timer = 0.0;
    }
    if e.flags & EMITTER_ONE_PER_FRAME != 0 {
        k = k.min(1.0);
    }
    let mut k = k as u32;
    if let (Some(p), Some(ps)) = (&e.periodic, st.periodic.as_mut())
        && p.max_per_period > 0
    {
        k = k.min(p.max_per_period.saturating_sub(ps.emitted));
        ps.emitted += k;
    }
    n + k
}

fn shape_offset(e: &EmitterDef, rng: &mut Rng) -> Vec3 {
    let mut off = match e.shape {
        EmitterShape::Box => {
            let mut v = Vec3::ZERO;
            for i in 0..3 {
                let mag = e.distance_min[i] + (e.distance_max[i] - e.distance_min[i]) * rng.unit();
                let sign = if rng.unit() < 0.5 { -1.0 } else { 1.0 };
                v[i] = mag * sign * e.directions[i];
            }
            v
        }
        EmitterShape::Sphere => {
            let (lo, hi) = (e.distance_min.x, e.distance_max.x);
            let r = lo + (hi - lo) * rng.unit().cbrt();
            let d = e.directions;
            let mut n = Vec3::ZERO;
            for i in 0..3 {
                if d[i] > 0.0 {
                    n[i] = rng.normal(0.0, d[i]);
                }
            }
            n.normalize_or_zero() * r
        }
    };
    for i in 0..3 {
        if e.sign[i] != 0 {
            off[i] = off[i].abs() * e.sign[i] as f32;
        }
    }
    off
}

fn sort_rope(particles: &mut [Particle]) {
    particles.sort_by_key(|p| match (p.alive(), p.is_new) {
        (true, false) => 0u8,
        (true, true) => 1,
        (false, _) => 2,
    });
}

fn resolve_cps(
    sub: &Subsystem,
    parent: Option<&Subsystem>,
    inst: &mut Instance,
    ctx: &UpdateContext,
    dt: f32,
) {
    let bound = inst.bound.map(|b| b.pos);
    let local_to_sim = sub.world_to_sim * ctx.transform * sub.local_to_object;
    let mut resolved = [Vec3::ZERO; CONTROL_POINTS];
    let mut from_bound = [false; CONTROL_POINTS];
    for slot in 0..CONTROL_POINTS {
        let d = &sub.cp_defs[slot];
        let mut chained = d.parent;
        if chained == 0 && sub.cp_shift > 0 {
            chained = slot as i32 + sub.cp_shift;
        }
        let mut is_null = d.null_offset;
        let mut fb = false;
        let value = if d.flags & CP_LINK_MOUSE != 0
            && let Some(c) = ctx.cursor
        {
            is_null = false;
            sub.world_to_sim.transform_point3(c)
        } else if d.flags & CP_WORLD_SPACE != 0 {
            sub.world_to_sim.transform_point3(d.offset)
        } else if d.flags & CP_FOLLOW_PARENT_PARTICLE != 0
            && let Some(b) = bound
        {
            fb = true;
            b
        } else if chained > 0
            && let Some(par) = parent
        {
            let pi = inst.bound.map_or(0, |b| b.parent_instance);
            match par.instances.get(pi) {
                Some(pinst) => {
                    let idx = (chained as usize).min(CONTROL_POINTS - 1);
                    is_null = par.cp_defs[idx].null_offset;
                    fb = pinst.cp_from_bound[idx];
                    let w = par.sim_to_world.transform_point3(pinst.cps[idx]);
                    sub.world_to_sim.transform_point3(w)
                }
                None => local_to_sim.transform_point3(d.offset),
            }
        } else {
            local_to_sim.transform_point3(d.offset)
        };
        resolved[slot] = value;
        from_bound[slot] = fb;
        if is_null
            && !fb
            && let Some(b) = bound
        {
            resolved[slot] = b;
            from_bound[slot] = true;
        }
    }
    if let Some(prev) = inst.cp_prev
        && dt > 1e-9
    {
        for i in 0..CONTROL_POINTS {
            inst.cp_velocity[i] = (resolved[i] - prev[i]) / dt;
        }
    } else {
        inst.cp_velocity = [Vec3::ZERO; CONTROL_POINTS];
    }
    inst.cps = resolved;
    inst.cp_from_bound = from_bound;
    inst.cp_prev = Some(resolved);
}

fn event_parent(parent: Option<&Subsystem>, bound: Option<Bound>) -> Option<&Particle> {
    let b = bound?;
    let p = parent?
        .instances
        .get(b.parent_instance)?
        .particles
        .get(b.particle)?;
    (p.alive() && p.id == b.particle_id).then_some(p)
}

/// Advance every subsystem by one step of `dt` seconds.
pub(crate) fn step(subs: &mut [Subsystem], dt: f32, ctx: &UpdateContext, rng: &mut Rng) {
    for s in subs.iter_mut() {
        s.sim_to_world = if s.world_space() {
            Mat4::IDENTITY
        } else {
            ctx.transform * s.local_to_object
        };
        s.world_to_sim = s.sim_to_world.inverse();
    }
    for i in 0..subs.len() {
        let (head, tail) = subs.split_at_mut(i);
        let (sub, rest) = tail.split_first_mut().expect("index in range");
        let parent = sub.parent.map(|p| &head[p]);
        let requests = step_one(sub, parent, dt, ctx, rng);
        for (child, req) in requests {
            let c = &mut rest[child - i - 1];
            c.pending.push(req);
        }
    }
}

fn instantiate(sub: &mut Subsystem, req: SpawnRequest, rng: &mut Rng) {
    if rng.unit() > sub.probability {
        return;
    }
    let bound = Some(Bound {
        parent_instance: req.parent_instance,
        particle: req.particle,
        particle_id: req.particle_id,
        pos: sub.world_to_sim.transform_point3(req.world),
    });
    let emitters = sub.def.emitters.len();
    if let Some(inst) = sub.instances.iter_mut().find(|i| i.dead && i.no_live) {
        let mut fresh = Instance::new(emitters, bound);
        std::mem::swap(&mut fresh.particles, &mut inst.particles);
        fresh.particles.clear();
        *inst = fresh;
    } else if (sub.instances.len() as u32) < sub.max_instances {
        sub.instances.push(Instance::new(emitters, bound));
    }
}

fn step_one(
    sub: &mut Subsystem,
    parent: Option<&Subsystem>,
    dt: f32,
    ctx: &UpdateContext,
    rng: &mut Rng,
) -> Vec<(usize, SpawnRequest)> {
    let mut requests = Vec::new();
    let pending = std::mem::take(&mut sub.pending);
    for req in pending {
        instantiate(sub, req, rng);
    }
    let dt = dt * sub.time_scale;
    sub.time += dt as f64;
    let parent_follows = parent.is_some_and(|p| p.spawn == SpawnType::EventFollow);
    if sub.spawn == SpawnType::Static && !parent_follows && sub.instances.is_empty() {
        sub.instances
            .push(Instance::new(sub.def.emitters.len(), None));
    }
    let bound_death = matches!(sub.spawn, SpawnType::EventFollow | SpawnType::EventSpawn)
        || (sub.spawn == SpawnType::Static && parent_follows);
    let trail_kind = sub.render.as_ref().map(|r| r.kind);
    let trail_def = sub.def.renderer;
    let sort = trail_kind == Some(RendererKind::Rope);

    let mut instances = std::mem::take(&mut sub.instances);
    for (ii, inst) in instances.iter_mut().enumerate() {
        if let Some(b) = inst.bound.as_mut() {
            let pinst = parent.and_then(|p| p.instances.get(b.parent_instance));
            let pp = pinst
                .and_then(|pi| pi.particles.get(b.particle))
                .filter(|p| p.alive() && p.id == b.particle_id);
            match pp {
                Some(p) if sub.spawn != SpawnType::EventDeath => {
                    let w = parent
                        .expect("bound implies parent")
                        .sim_to_world
                        .transform_point3(p.position);
                    b.pos = sub.world_to_sim.transform_point3(w);
                }
                Some(_) => {}
                None => {
                    if bound_death {
                        inst.dead = true;
                    }
                }
            }
            if bound_death && pinst.is_none_or(|pi| pi.dead && pi.no_live) {
                inst.dead = true;
            }
        }
        if inst.dead && inst.no_live {
            continue;
        }
        resolve_cps(sub, parent, inst, ctx, dt);
        let event_parent = event_parent(parent, inst.bound);

        if !inst.dead {
            let mut emitted_any = false;
            for (ei, e) in sub.def.emitters.iter().enumerate() {
                let n = emit_count(
                    e,
                    &mut inst.emitters[ei],
                    dt,
                    sub.rate_scale * ctx.audio_rate,
                    rng,
                );
                if n == 0 {
                    continue;
                }
                let cp_base = inst.cps[e.control_point];
                let base = match inst.bound {
                    Some(b) if !inst.cp_from_bound[e.control_point] => cp_base + b.pos,
                    _ => cp_base,
                };
                let spawn_ctx = SpawnCtx {
                    cps: &inst.cps,
                    cp_velocity: &inst.cp_velocity,
                    base,
                    parent: event_parent,
                    emit_interval: 1.0 / (e.rate * sub.rate_scale).max(1e-6),
                };
                let mut cursor = 0usize;
                for _ in 0..n {
                    while cursor < inst.particles.len() && inst.particles[cursor].alive() {
                        cursor += 1;
                    }
                    if cursor >= inst.particles.len()
                        && inst.particles.len() as u32 >= sub.max_count
                    {
                        break;
                    }
                    let off = shape_offset(e, rng);
                    let speed = rng.range(e.speed_min, e.speed_max);
                    let mut p = Particle {
                        position: base + e.origin + off,
                        velocity: off.normalize_or_zero() * speed,
                        seed: rng.next_u32() | 1,
                        id: sub.next_id,
                        ..Default::default()
                    };
                    sub.next_id = sub.next_id.wrapping_add(1);
                    for (k, init) in sub.def.initializers.iter().enumerate() {
                        ops::initialize(init, &mut sub.init_state[k], &mut p, rng, &spawn_ctx);
                    }
                    ops::apply_override(&mut p, &sub.over);
                    if cursor < inst.particles.len() {
                        inst.particles[cursor] = p;
                        if let Some(t) = inst.trails.get_mut(cursor) {
                            *t = Trail::default();
                        }
                    } else {
                        inst.particles.push(p);
                    }
                    emitted_any = true;
                }
            }
            if sort && emitted_any {
                sort_rope(&mut inst.particles);
            }
        }

        if sub.spawn == SpawnType::EventDeath && inst.no_live && !inst.particles.is_empty() {
            inst.dead = true;
        }

        let mut has_live = false;
        for (pi, p) in inst.particles.iter_mut().enumerate() {
            let to_world = sub.sim_to_world;
            if p.is_new {
                for &(c, spawn) in &sub.children {
                    let wants = match spawn {
                        SpawnType::EventFollow | SpawnType::EventSpawn => true,
                        SpawnType::Static => sub.spawn == SpawnType::EventFollow,
                        SpawnType::EventDeath => false,
                    };
                    if wants {
                        requests.push((
                            c,
                            request(ii, pi, p.id, to_world.transform_point3(p.position)),
                        ));
                    }
                }
            }
            p.is_new = false;
            if !p.alive() {
                continue;
            }
            p.reset();
            p.lifetime -= dt;
            if p.alive() {
                has_live = true;
            } else {
                for &(c, spawn) in &sub.children {
                    if spawn == SpawnType::EventDeath {
                        requests.push((
                            c,
                            request(ii, pi, p.id, to_world.transform_point3(p.position)),
                        ));
                    }
                }
            }
        }
        inst.no_live = !has_live;

        let op_ctx = OpCtx {
            cps: &inst.cps,
            cp_velocity: &inst.cp_velocity,
            time: sub.time,
            dt,
            parent: event_parent,
            speed: sub.over.speed,
            random: [0.5, 0.5],
            salt: 0,
        };
        for (k, op) in sub.def.operators.iter().enumerate() {
            let c = OpCtx {
                random: sub.op_random.get(k).copied().unwrap_or([0.5, 0.5]),
                salt: k as u32 + 1,
                ..op_ctx
            };
            ops::operate(op, &mut inst.particles, &c);
        }

        if trail_kind == Some(RendererKind::RopeTrail)
            && let Some(r) = trail_def
        {
            record_trails(inst, r.length, r.segments, dt);
        }
    }
    sub.instances = instances;
    requests
}

fn request(parent_instance: usize, particle: usize, particle_id: u32, world: Vec3) -> SpawnRequest {
    SpawnRequest {
        parent_instance,
        particle,
        particle_id,
        world,
    }
}

fn record_trails(inst: &mut Instance, length: f32, segments: u32, dt: f32) {
    let interval = (length / segments as f32).max(1e-3);
    if inst.trails.len() < inst.particles.len() {
        inst.trails
            .resize_with(inst.particles.len(), Trail::default);
    }
    for (p, t) in inst.particles.iter().zip(inst.trails.iter_mut()) {
        if !p.alive() {
            t.points.clear();
            t.timer = 0.0;
            continue;
        }
        for pt in t.points.iter_mut() {
            pt.age += dt;
        }
        t.timer += dt;
        if t.points.is_empty() || t.timer >= interval {
            t.timer = 0.0;
            t.points.push_front(TrailPoint {
                position: p.position,
                size: p.size,
                color: p.color,
                age: 0.0,
            });
        }
        while t.points.len() > segments as usize + 1
            || t.points.back().is_some_and(|b| b.age > length + interval)
        {
            t.points.pop_back();
        }
    }
}
