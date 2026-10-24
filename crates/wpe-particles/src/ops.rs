//! Initializers (run once per spawned particle) and operators (run every tick).

use std::f32::consts::{PI, TAU};

use glam::{Quat, Vec3};

use crate::def::{
    CONTROL_POINTS, Collision, Component, InheritInput, Initializer, InstanceOverride, Operator,
    Oscillation, RemapInput, RemapOutput, RemapTransform,
};
use crate::noise;
use crate::rng::{Rng, hash01};

/// One simulated particle. Positions live in the owning subsystem's simulation space.
#[derive(Debug, Clone, PartialEq)]
pub struct Particle {
    pub position: Vec3,
    pub velocity: Vec3,
    /// Euler angles in radians (x, y, z); z spins the sprite in screen space.
    pub rotation: Vec3,
    pub angular_velocity: Vec3,
    pub color: Vec3,
    pub alpha: f32,
    /// Diameter in scene units.
    pub size: f32,
    /// Remaining seconds of life; the particle is dead once this reaches zero.
    pub lifetime: f32,
    pub init_color: Vec3,
    pub init_alpha: f32,
    pub init_size: f32,
    pub init_lifetime: f32,
    /// Stable per-spawn random seed.
    pub seed: u32,
    /// Spawn serial number, unique within the subsystem.
    pub id: u32,
    pub(crate) is_new: bool,
}

impl Default for Particle {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            rotation: Vec3::ZERO,
            angular_velocity: Vec3::ZERO,
            color: Vec3::ONE,
            alpha: 1.0,
            size: 20.0,
            lifetime: 1.0,
            init_color: Vec3::ONE,
            init_alpha: 1.0,
            init_size: 20.0,
            init_lifetime: 1.0,
            seed: 0,
            id: 0,
            is_new: true,
        }
    }
}

impl Particle {
    pub fn alive(&self) -> bool {
        self.lifetime > 0.0
    }

    /// Normalized age: 0 at spawn, 1 at death.
    pub fn age01(&self) -> f32 {
        if self.lifetime <= 0.0 || self.init_lifetime <= 0.0 {
            1.0
        } else {
            1.0 - self.lifetime / self.init_lifetime
        }
    }

    /// Seconds since spawn.
    pub fn age(&self) -> f32 {
        self.init_lifetime - self.lifetime
    }

    /// Stable random value in `[0, 1)` for this particle and `salt`.
    pub fn random(&self, salt: u32) -> f32 {
        hash01(self.seed, salt)
    }

    /// Restore the per-tick properties operators modulate.
    pub(crate) fn reset(&mut self) {
        self.color = self.init_color;
        self.alpha = self.init_alpha;
        self.size = self.init_size;
    }

    fn set_color(&mut self, c: Vec3) {
        self.color = c;
        self.init_color = c;
    }

    fn set_size(&mut self, s: f32) {
        self.size = s;
        self.init_size = s;
    }

    fn set_alpha(&mut self, a: f32) {
        self.alpha = a;
        self.init_alpha = a;
    }

    fn set_lifetime(&mut self, l: f32) {
        self.lifetime = l;
        self.init_lifetime = l;
    }
}

/// Read-only data initializers see while a particle spawns.
pub(crate) struct SpawnCtx<'a> {
    pub cps: &'a [Vec3; CONTROL_POINTS],
    pub cp_velocity: &'a [Vec3; CONTROL_POINTS],
    /// Emitter anchor the shape offset was added to.
    pub base: Vec3,
    pub parent: Option<&'a Particle>,
    /// Seconds between two spawns of the emitting emitter.
    pub emit_interval: f32,
}

/// Mutable state an initializer keeps across spawns.
#[derive(Debug, Clone, Default)]
pub(crate) struct InitState {
    pub sequence: u32,
    pub turbulence: Option<Vec3>,
}

fn biased(rng: &mut Rng, exponent: f32) -> f32 {
    let t = rng.unit();
    if exponent != 1.0 && exponent > 0.0 {
        t.powf(exponent)
    } else {
        t
    }
}

fn biased_vec(rng: &mut Rng, min: Vec3, max: Vec3, exponent: f32) -> Vec3 {
    let t = Vec3::new(
        biased(rng, exponent),
        biased(rng, exponent),
        biased(rng, exponent),
    );
    min + (max - min) * t
}

fn sequence_index(seq: u32, count: u32, mirror: bool) -> u32 {
    if mirror && count > 1 {
        let period = 2 * (count - 1);
        let pos = seq % period;
        if pos < count { pos } else { period - pos }
    } else {
        seq % count.max(1)
    }
}

/// HSV (hue in degrees) to RGB.
pub(crate) fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Vec3 {
    let h = h.rem_euclid(360.0);
    let (s, v) = (s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let c = v * s;
    let sextant = h / 60.0;
    let x = c * (1.0 - (sextant % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match sextant as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Vec3::new(r + m, g + m, b + m)
}

/// RGB to HSV (hue in degrees).
pub(crate) fn rgb_to_hsv(c: Vec3) -> Vec3 {
    let mx = c.max_element();
    let mn = c.min_element();
    let d = mx - mn;
    let mut h = 0.0;
    if d > 0.0 {
        h = if mx == c.x {
            60.0 * ((c.y - c.z) / d).rem_euclid(6.0)
        } else if mx == c.y {
            60.0 * ((c.z - c.x) / d + 2.0)
        } else {
            60.0 * ((c.x - c.y) / d + 4.0)
        };
    }
    Vec3::new(h, if mx > 0.0 { d / mx } else { 0.0 }, mx)
}

fn inherit(p: &mut Particle, from: &Particle, input: InheritInput) {
    match input {
        InheritInput::Color => p.set_color(from.color),
        InheritInput::Size => p.set_size(from.size),
        InheritInput::Alpha => p.set_alpha(from.alpha),
        InheritInput::Velocity => p.velocity = from.velocity,
        InheritInput::Rotation => p.rotation = from.rotation,
        InheritInput::Lifetime => p.set_lifetime(from.lifetime),
    }
}

/// Apply one initializer to a freshly spawned particle.
pub(crate) fn initialize(
    init: &Initializer,
    state: &mut InitState,
    p: &mut Particle,
    rng: &mut Rng,
    ctx: &SpawnCtx,
) {
    match init {
        Initializer::ColorRandom(r) => {
            let t = biased(rng, r.exponent);
            p.set_color(r.min.lerp(r.max, t) / 255.0);
        }
        Initializer::LifetimeRandom(r) => {
            p.set_lifetime(r.min + (r.max - r.min) * biased(rng, r.exponent));
        }
        Initializer::SizeRandom(r) => p.set_size(r.min + (r.max - r.min) * biased(rng, r.exponent)),
        Initializer::AlphaRandom(r) => {
            p.set_alpha(r.min + (r.max - r.min) * biased(rng, r.exponent))
        }
        Initializer::VelocityRandom { range, inherit_cp } => {
            p.velocity += biased_vec(rng, range.min, range.max, range.exponent);
            if let Some(cp) = inherit_cp {
                p.velocity += ctx.cp_velocity[*cp];
            }
        }
        Initializer::RotationRandom(r) => {
            p.rotation += biased_vec(rng, r.min, r.max, r.exponent);
        }
        Initializer::AngularVelocityRandom(r) => {
            p.angular_velocity += biased_vec(rng, r.min, r.max, r.exponent);
        }
        Initializer::TurbulentVelocityRandom {
            scale,
            timescale,
            offset,
            speed_min,
            speed_max,
            forward,
            right,
        } => {
            let pos = state
                .turbulence
                .get_or_insert_with(|| rng.vec3(Vec3::ZERO, Vec3::splat(10.0)));
            let speed = rng.range(*speed_min, *speed_max);
            let mut duration = ctx.emit_interval;
            if !duration.is_finite() || duration > 10.0 {
                pos.x += speed;
                duration = 0.0;
            }
            let step = 0.005 / timescale.max(1e-4);
            let mut dir;
            let mut iterations = 0;
            loop {
                dir = noise::curl(*pos).normalize_or(Vec3::Y);
                *pos += dir * step;
                duration -= 0.01;
                iterations += 1;
                if duration <= 0.01 || iterations >= 2000 {
                    break;
                }
            }
            let fwd = forward.normalize_or(Vec3::Y);
            let angle = dir.dot(fwd).clamp(-1.0, 1.0).acos() / PI;
            let cone = scale / 2.0;
            if angle > cone {
                let axis = dir.cross(fwd);
                if axis.length_squared() > 1e-12 {
                    dir =
                        Quat::from_axis_angle(axis.normalize(), (angle - angle * cone) * PI) * dir;
                }
            }
            if *offset != 0.0 {
                dir = Quat::from_axis_angle(right.normalize_or(Vec3::Z), *offset) * dir;
            }
            p.velocity += dir * speed;
        }
        Initializer::PositionOffsetRandom { distance } => {
            if *distance > 0.0 {
                let r = distance * rng.unit().cbrt();
                let z = rng.range(-1.0, 1.0);
                let phi = rng.range(0.0, TAU);
                let rxy = (1.0 - z * z).max(0.0).sqrt();
                p.position += Vec3::new(rxy * phi.cos(), rxy * phi.sin(), z) * r;
            }
        }
        Initializer::BoxPosition(r) => {
            p.position = ctx.base + biased_vec(rng, r.min, r.max, r.exponent);
        }
        Initializer::SpherePosition(r) => {
            let radius = r.min + (r.max - r.min) * rng.unit().cbrt();
            let z = rng.range(-1.0, 1.0);
            let phi = rng.range(0.0, TAU);
            let rxy = (1.0 - z * z).max(0.0).sqrt();
            p.position = ctx.base + Vec3::new(rxy * phi.cos(), rxy * phi.sin(), z) * radius;
        }
        Initializer::RemapInitialValue {
            input_cp,
            input_min,
            input_max,
            output,
            output_min,
            output_max,
            operation,
        } => {
            let d = p.position.distance(ctx.cps[*input_cp]);
            let t = if input_max > input_min {
                ((d - input_min) / (input_max - input_min)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let v = output_min.lerp(*output_max, t);
            match output {
                RemapOutput::Color => {
                    let c = p.color;
                    p.set_color(Vec3::new(
                        operation.apply(c.x, v.x, 1.0),
                        operation.apply(c.y, v.y, 1.0),
                        operation.apply(c.z, v.z, 1.0),
                    ));
                }
                RemapOutput::Size => p.set_size(operation.apply(p.size, v.x, 1.0)),
                _ => {}
            }
        }
        Initializer::MapSequenceBetweenControlPoints {
            count,
            cp_start,
            cp_end,
            mirror,
            arc_amount,
            arc_direction,
            size_reduction,
        } => {
            let idx = sequence_index(state.sequence, *count, *mirror);
            state.sequence = state.sequence.wrapping_add(1);
            let t = if *count > 1 {
                idx as f32 / (*count - 1) as f32
            } else {
                0.0
            };
            let (a, b) = (ctx.cps[*cp_start], ctx.cps[*cp_end]);
            let line = b - a;
            let mut path = a + line * t;
            if arc_amount.abs() > 1e-6 {
                let mut dir = *arc_direction;
                if dir.length_squared() < 1e-6 {
                    dir = Vec3::new(-line.y, line.x, 0.0);
                }
                path +=
                    dir.normalize_or(Vec3::Z) * arc_amount * 4.0 * t * (1.0 - t) * line.length();
            }
            if *size_reduction > 1e-6 {
                p.set_size(p.size * (1.0 - size_reduction * t).max(0.0));
            }
            p.position = p.position - ctx.base + path;
            p.velocity = Vec3::ZERO;
        }
        Initializer::MapSequenceAroundControlPoint {
            count,
            cp,
            mirror,
            axis,
            bounds,
            speed_min,
            speed_max,
        } => {
            let seq = state.sequence;
            state.sequence = state.sequence.wrapping_add(1);
            let mut f = (seq as f32 / count).fract();
            if *mirror {
                let k = (seq as f32 / count).floor() as i64;
                if k % 2 == 1 {
                    f = 1.0 - f;
                }
            }
            let angle = (bounds.x + (bounds.y - bounds.x) * f) * TAU;
            let center = ctx.cps[*cp];
            let rel = p.position - center;
            let axial = *axis * rel.dot(*axis);
            let radius = (rel - axial).length();
            let (u, v) = axis.any_orthonormal_pair();
            p.position = center + axial + (u * angle.cos() + v * angle.sin()) * radius;
            p.velocity = rng.vec3(*speed_min, *speed_max);
        }
        Initializer::HsvColorRandom {
            hue_min,
            hue_max,
            sat_min,
            sat_max,
            val_min,
            val_max,
            hue_steps,
        } => {
            let mut range = hue_max - hue_min;
            if range < 0.0 {
                range += 360.0;
            }
            let h = if *hue_steps >= 1 {
                hue_min + rng.int(0, *hue_steps as i32 - 1) as f32 * (range / *hue_steps as f32)
            } else {
                hue_min + rng.unit() * range
            };
            let s = sat_min + rng.unit() * (sat_max - sat_min).max(0.0);
            let v = val_min + rng.unit() * (val_max - val_min).max(0.0);
            p.set_color(hsv_to_rgb(h, s, v));
        }
        Initializer::ColorList {
            colors,
            hue_noise,
            sat_noise,
            val_noise,
        } => {
            let base = colors[rng.int(0, colors.len() as i32 - 1) as usize];
            if *hue_noise > 1e-6 || *sat_noise > 1e-6 || *val_noise > 1e-6 {
                let mut hsv = rgb_to_hsv(base);
                hsv.x += rng.range(-1.0, 1.0) * hue_noise;
                hsv.y += rng.range(-1.0, 1.0) * sat_noise;
                hsv.z += rng.range(-1.0, 1.0) * val_noise;
                p.set_color(hsv_to_rgb(hsv.x, hsv.y, hsv.z));
            } else {
                p.set_color(base);
            }
        }
        Initializer::InheritControlPointVelocity { cp, min, max } => {
            p.velocity += ctx.cp_velocity[*cp] * rng.range(*min, *max);
        }
        Initializer::InheritInitialValueFromEvent(input) => {
            if let Some(parent) = ctx.parent {
                inherit(p, parent, *input);
            }
        }
    }
}

/// Apply the scene object's `instanceoverride` multipliers after all initializers.
pub(crate) fn apply_override(p: &mut Particle, o: &InstanceOverride) {
    if o.lifetime > 0.0 {
        p.set_lifetime(p.lifetime * o.lifetime);
    }
    p.set_alpha(p.alpha * o.alpha);
    p.set_size(p.size * o.size);
    p.velocity *= o.speed;
    if let Some(c) = o.color {
        p.set_color(c);
    }
    if o.brightness != 1.0 {
        p.set_color(p.color * o.brightness);
    }
}

/// Read-only data operators see each tick.
pub(crate) struct OpCtx<'a> {
    pub cps: &'a [Vec3; CONTROL_POINTS],
    pub cp_velocity: &'a [Vec3; CONTROL_POINTS],
    /// Subsystem clock in seconds.
    pub time: f64,
    pub dt: f32,
    pub parent: Option<&'a Particle>,
    /// `instanceoverride.speed`, which also scales gravity.
    pub speed: f32,
    /// Two random values drawn once per operator when the system is seeded.
    pub random: [f32; 2],
    /// Operator index, salting per-particle random values.
    pub salt: u32,
}

fn oscillation_params(p: &Particle, o: &Oscillation, salt: u32) -> (f32, f32, f32) {
    let r = |k: u32| p.random(salt.wrapping_mul(16).wrapping_add(k));
    (
        o.frequency_min + (o.frequency_max - o.frequency_min) * r(0),
        o.scale_min + (o.scale_max - o.scale_min) * r(1),
        o.phase_min + (o.phase_max - o.phase_min) * r(2),
    )
}

fn oscillation_factor(p: &Particle, o: &Oscillation, salt: u32) -> f32 {
    let (freq, _, phase) = oscillation_params(p, o, salt);
    let wave = ((freq * p.age() + phase).cos() + 1.0) * 0.5;
    let m = o.scale_min + (o.scale_max - o.scale_min) * wave;
    1.0 + (m - 1.0) * o.blend.factor(p.age01())
}

fn reflect(v: Vec3, n: Vec3, bounce: f32) -> Vec3 {
    let vn = v.dot(n);
    if vn >= 0.0 {
        v
    } else {
        v - (1.0 + bounce) * vn * n
    }
}

fn collide(p: &mut Particle, response: Collision, surface: Vec3, normal: Vec3) {
    match response {
        Collision::Delete => p.lifetime = 0.0,
        Collision::Bounce(b) => {
            p.position = surface;
            p.velocity = reflect(p.velocity, normal, b);
        }
    }
}

fn transform(f: RemapTransform, x: f32, octaves: u32, offset: f32) -> f32 {
    match f {
        RemapTransform::Linear => x,
        RemapTransform::Sine => ((x * TAU).sin() + 1.0) * 0.5,
        RemapTransform::Cosine => ((x * TAU).cos() + 1.0) * 0.5,
        RemapTransform::Square => {
            if (x * TAU).sin() >= 0.0 {
                1.0
            } else {
                0.0
            }
        }
        RemapTransform::Saw => x - x.floor(),
        RemapTransform::Triangle => (2.0 * (x - x.floor()) - 1.0).abs(),
        RemapTransform::SimplexNoise => {
            ((noise::perlin((x + offset) as f64, 0.0, 0.0) + 1.0) * 0.5).clamp(0.0, 1.0) as f32
        }
        RemapTransform::FbmNoise => noise::fbm01((x + offset) as f64, octaves) as f32,
        RemapTransform::Step => x.floor(),
        RemapTransform::Smoothstep => {
            let c = x.clamp(0.0, 1.0);
            c * c * (3.0 - 2.0 * c)
        }
    }
}

/// Run one operator over every live particle.
pub(crate) fn operate(op: &Operator, particles: &mut [Particle], ctx: &OpCtx) {
    let dt = ctx.dt;
    let live = particles.iter_mut().filter(|p| p.alive());
    match op {
        Operator::Movement {
            gravity,
            drag,
            blend,
        } => {
            for p in live {
                let f = blend.factor(p.age01());
                p.velocity += *gravity * ctx.speed * f * dt;
                p.position += p.velocity * dt;
                if *drag > 0.0 {
                    p.velocity *= (1.0 - drag * f * dt).max(0.0);
                }
            }
        }
        Operator::AngularMovement { force, drag, blend } => {
            for p in live {
                let f = blend.factor(p.age01());
                p.angular_velocity += *force * f * dt;
                p.rotation += p.angular_velocity * dt;
                if *drag > 0.0 {
                    p.angular_velocity *= (1.0 - drag * f * dt).max(0.0);
                }
            }
        }
        Operator::SizeChange(vc) => {
            for p in live {
                p.size *= vc.at(p.age01()).x;
            }
        }
        Operator::AlphaChange(vc) => {
            for p in live {
                p.alpha *= vc.at(p.age01()).x;
            }
        }
        Operator::ColorChange(vc) => {
            for p in live {
                p.color *= vc.at(p.age01());
            }
        }
        Operator::AlphaFade { fade_in, fade_out } => {
            for p in live {
                if p.init_lifetime <= 1e-6 {
                    continue;
                }
                let life = p.age01();
                let a = if life < *fade_in {
                    if *fade_in > 1e-6 { life / fade_in } else { 1.0 }
                } else if life > *fade_out {
                    if *fade_out < 1.0 - 1e-6 {
                        (1.0 - life) / (1.0 - fade_out)
                    } else {
                        1.0
                    }
                } else {
                    1.0
                };
                p.alpha *= a;
            }
        }
        Operator::OscillateAlpha(o) => {
            for p in live {
                p.alpha *= oscillation_factor(p, o, ctx.salt);
            }
        }
        Operator::OscillateSize(o) => {
            for p in live {
                p.size *= oscillation_factor(p, o, ctx.salt);
            }
        }
        Operator::OscillatePosition(o) => {
            for p in live {
                let f = o.blend.factor(p.age01());
                let t = p.age();
                let mut delta = Vec3::ZERO;
                for axis in 0..3 {
                    if o.mask[axis] < 0.01 {
                        continue;
                    }
                    let salt = ctx.salt.wrapping_mul(4).wrapping_add(axis as u32 + 1);
                    let (freq, scale, phase) = oscillation_params(p, o, salt);
                    delta[axis] = -scale * freq * (freq * t + phase).sin() * dt * f;
                }
                p.position += delta;
            }
        }
        Operator::Turbulence {
            phase_min,
            phase_max,
            speed_min,
            speed_max,
            timescale,
            scale,
            mask,
            blend,
        } => {
            let phase = phase_min + (phase_max - phase_min) * ctx.random[0];
            let speed = speed_min + (speed_max - speed_min) * ctx.random[1];
            let shift = phase as f64 + *timescale as f64 * ctx.time;
            for p in live {
                let mut q = p.position;
                q.x = (q.x as f64 + shift) as f32;
                let dir = noise::curl(q * scale * 2.0).normalize_or_zero() * *mask;
                p.velocity += dir * speed * blend.factor(p.age01()) * dt;
            }
        }
        Operator::Vortex {
            cp,
            distance_inner,
            distance_outer,
            speed_inner,
            speed_outer,
            flags,
            offset,
            axis,
            blend,
        } => {
            let center = ctx.cps[*cp] + *offset;
            let axis = axis.normalize_or(Vec3::Z);
            let span = distance_outer - distance_inner;
            for p in live {
                let mut r = p.position - center;
                if flags & 1 != 0 {
                    r -= axis * r.dot(axis);
                }
                let d = r.length();
                let speed = if span < 0.0 || d < *distance_inner {
                    *speed_inner
                } else if d > *distance_outer {
                    *speed_outer
                } else {
                    let t = (d - distance_inner) / (span + 0.1);
                    speed_inner + (speed_outer - speed_inner) * t
                };
                let dir = -axis.cross(r).normalize_or_zero();
                p.velocity += dir * speed * blend.factor(p.age01()) * dt;
            }
        }
        Operator::VortexV2 {
            cp,
            distance_inner,
            distance_outer,
            speed_inner,
            speed_outer,
            flags,
            offset,
            axis,
            ring_radius,
            ring_width,
            ring_pull,
        } => {
            let center = ctx.cps[*cp] + *offset;
            let span = distance_outer - distance_inner;
            for p in live {
                let radial = p.position - center;
                let d = radial.length();
                let speed = if span < 0.0 || d < *distance_inner {
                    *speed_inner
                } else if d > *distance_outer {
                    *speed_outer
                } else {
                    let t = (d - distance_inner) / (span + 0.1);
                    speed_inner + (speed_outer - speed_inner) * t
                };
                if flags & 2 != 0 && d > 1e-3 {
                    let rotated = Quat::from_axis_angle(*axis, speed / d * dt) * radial;
                    p.position = center + rotated;
                    p.velocity = Vec3::ZERO;
                } else {
                    p.velocity += axis.cross(radial).normalize_or_zero() * speed * dt;
                }
                if *ring_radius > 0.0 && d > 1e-3 {
                    let diff = ring_radius - d;
                    if diff.abs() > *ring_width {
                        p.velocity += radial / d * ring_pull * diff.signum() * dt;
                    }
                }
            }
        }
        Operator::ControlPointAttract {
            cp,
            scale,
            threshold,
            origin,
            blend,
        } => {
            let target = ctx.cps[*cp] + *origin;
            for p in live {
                let toward = target - p.position;
                let d = toward.length();
                if d >= *threshold || d < 1e-6 {
                    continue;
                }
                p.velocity += toward / d * scale * blend.factor(p.age01()) * dt;
            }
        }
        Operator::MaintainDistanceBetweenControlPoints => {
            let (a, b) = (ctx.cps[0], ctx.cps[1]);
            let line = b - a;
            let len2 = line.length_squared();
            if len2 < 1e-10 {
                return;
            }
            for p in live {
                let t = (p.position - a).dot(line) / len2;
                if !(0.0..=1.0).contains(&t) {
                    p.position += line * (t.clamp(0.0, 1.0) - t);
                }
            }
        }
        Operator::MaintainDistanceToControlPoint {
            cp,
            distance,
            strength,
            blend,
        } => {
            let c = ctx.cps[*cp];
            for p in live {
                let d = p.position - c;
                let dist = d.length();
                if dist < 1e-6 {
                    continue;
                }
                let f = blend.factor(p.age01());
                if *strength <= 0.0 {
                    p.position += (c + d / dist * distance - p.position) * f;
                } else {
                    p.velocity += d / dist * (distance - dist) * strength * f * dt;
                }
            }
        }
        Operator::ReduceMovementNearControlPoint {
            cp,
            distance_inner,
            distance_outer,
            reduction_inner,
        } => {
            let c = ctx.cps[*cp];
            for p in live {
                let d = p.position.distance(c);
                if d >= *distance_outer {
                    continue;
                }
                let reduction = if d <= *distance_inner {
                    *reduction_inner
                } else {
                    let t = (d - distance_inner) / (distance_outer - distance_inner).max(1e-6);
                    reduction_inner * (1.0 - t)
                };
                p.velocity *= 1.0 / (1.0 + reduction * dt);
            }
        }
        Operator::RemapValue {
            input,
            input_component,
            input_cp,
            input_min,
            input_max,
            transform: tf,
            transform_scale,
            octaves,
            output,
            output_component,
            output_min,
            output_max,
            operation,
            blend,
        } => {
            let span = input_max - input_min;
            let inv = if span.abs() > 1e-9 { 1.0 / span } else { 0.0 };
            let cp = ctx.cps[*input_cp];
            for p in live {
                let raw = match input {
                    RemapInput::SystemTime => ctx.time as f32,
                    RemapInput::Lifetime => p.age01(),
                    RemapInput::Velocity => input_component.reduce(p.velocity),
                    RemapInput::Position => input_component.reduce(p.position),
                    RemapInput::Rotation => input_component.reduce(p.rotation),
                    RemapInput::AngularVelocity => input_component.reduce(p.angular_velocity),
                    RemapInput::Color => input_component.reduce(p.color),
                    RemapInput::Size => p.size,
                    RemapInput::Alpha => p.alpha,
                    RemapInput::Speed => p.velocity.length(),
                    RemapInput::AngularSpeed => p.angular_velocity.length(),
                    RemapInput::MaxLifetime => p.init_lifetime,
                    RemapInput::ControlPoint => input_component.reduce(cp),
                    RemapInput::ControlPointVelocity => {
                        input_component.reduce(ctx.cp_velocity[*input_cp])
                    }
                    RemapInput::DistanceToControlPoint => p.position.distance(cp),
                    RemapInput::DeltaToControlPoint => input_component.reduce(p.position - cp),
                    RemapInput::DirectionToControlPoint => {
                        input_component.reduce((cp - p.position).normalize_or_zero())
                    }
                    RemapInput::PositionBetweenControlPoints => {
                        let end = ctx.cps[(*input_cp + 1).min(CONTROL_POINTS - 1)];
                        let line = end - cp;
                        let len2 = line.length_squared();
                        if len2 > 1e-9 {
                            ((p.position - cp).dot(line) / len2).clamp(0.0, 1.0)
                        } else {
                            0.0
                        }
                    }
                    RemapInput::Random => p.random(0x5eed),
                    RemapInput::Noise => {
                        let q = p.position * 0.01;
                        let n = noise::perlin(q.x as f64 + ctx.time, q.y as f64, q.z as f64);
                        ((n + 1.0) * 0.5).clamp(0.0, 1.0) as f32
                    }
                };
                let t = ((raw - input_min) * inv).clamp(0.0, 1.0);
                let offset = p.random(ctx.salt.wrapping_add(0x00a0_0000)) * 256.0;
                let x = transform(*tf, t * transform_scale, *octaves, offset);
                let mapped = *output_min + (*output_max - *output_min) * x;
                let f = blend.factor(p.age01());
                let apply3 = |v: Vec3| -> Vec3 {
                    let mut v = v;
                    match output_component {
                        Component::X => v.x = operation.apply(v.x, mapped.x, f),
                        Component::Y => v.y = operation.apply(v.y, mapped.x, f),
                        Component::Z => v.z = operation.apply(v.z, mapped.x, f),
                        _ => {
                            for i in 0..3 {
                                v[i] = operation.apply(v[i], mapped[i], f);
                            }
                        }
                    }
                    v
                };
                match output {
                    RemapOutput::Alpha => p.alpha = operation.apply(p.alpha, mapped.x, f),
                    RemapOutput::Size => p.size = operation.apply(p.size, mapped.x, f),
                    RemapOutput::Velocity => p.velocity = apply3(p.velocity),
                    RemapOutput::Speed => {
                        let s = p.velocity.length();
                        let ns = operation.apply(s, mapped.x, f);
                        p.velocity = if s > 1e-9 {
                            p.velocity * (ns / s)
                        } else {
                            Vec3::ZERO
                        };
                    }
                    RemapOutput::AngularVelocity => p.angular_velocity = apply3(p.angular_velocity),
                    RemapOutput::Rotation => p.rotation = apply3(p.rotation),
                    RemapOutput::Color => p.color = apply3(p.color),
                    RemapOutput::Unsupported => {}
                }
            }
        }
        Operator::CapVelocity { max_speed, blend } => {
            for p in live {
                let s = p.velocity.length();
                if s <= *max_speed || s < 1e-9 {
                    continue;
                }
                let factor = 1.0 + (max_speed / s - 1.0) * blend.factor(p.age01());
                p.velocity *= factor;
            }
        }
        Operator::CollisionPlane {
            plane,
            cp,
            distance,
            response,
        } => {
            let n = plane.truncate().normalize_or(Vec3::Y);
            let d = plane.w - n.dot(ctx.cps[*cp]) - distance;
            for p in live {
                let sd = n.dot(p.position) + d;
                if sd < 0.0 {
                    collide(p, *response, p.position - sd * n, n);
                }
            }
        }
        Operator::CollisionSphere {
            origin,
            cp,
            radius,
            response,
        } => {
            let center = ctx.cps[*cp] + *origin;
            for p in live {
                let d = p.position - center;
                let dist = d.length();
                if dist >= *radius || dist < 1e-9 {
                    continue;
                }
                let n = d / dist;
                collide(p, *response, center + n * *radius, n);
            }
        }
        Operator::CollisionQuad {
            origin,
            normal,
            forward,
            size,
            cp,
            response,
        } => {
            let n = *normal;
            let mut u = *forward - n * forward.dot(n);
            if u.length_squared() < 1e-12 {
                u = n.any_orthogonal_vector();
            }
            let u = u.normalize();
            let v = u.cross(n).normalize();
            let center = ctx.cps[*cp] + *origin;
            let half = *size * 0.5;
            for p in live {
                let sd = (p.position - center).dot(n);
                let prev = sd - p.velocity.dot(n) * dt;
                if (sd >= 0.0) == (prev >= 0.0) {
                    continue;
                }
                let proj = p.position - sd * n;
                let local = proj - center;
                if local.dot(u).abs() > half.x || local.dot(v).abs() > half.y {
                    continue;
                }
                collide(p, *response, proj, if prev >= 0.0 { n } else { -n });
            }
        }
        Operator::CollisionBox {
            half_size,
            cp,
            response,
        } => {
            let c = ctx.cps[*cp];
            for p in live {
                let mut local = p.position - c;
                let mut hit = false;
                for a in 0..3 {
                    let h = half_size[a];
                    if local[a] > h || local[a] < -h {
                        local[a] = local[a].clamp(-h, h);
                        hit = true;
                        if let Collision::Bounce(b) = response
                            && p.velocity[a] * local[a].signum() > 0.0
                        {
                            p.velocity[a] = -p.velocity[a] * b;
                        }
                    }
                }
                if hit {
                    match response {
                        Collision::Delete => p.lifetime = 0.0,
                        Collision::Bounce(_) => p.position = c + local,
                    }
                }
            }
        }
        Operator::Boids {
            neighbor_threshold,
            separation_threshold,
            separation_factor,
            cohesion_factor,
            alignment_factor,
            max_speed,
            blend,
        } => {
            let snapshot: Vec<(Vec3, Vec3, bool)> = particles
                .iter()
                .map(|p| (p.position, p.velocity, p.alive()))
                .collect();
            let n2 = neighbor_threshold * neighbor_threshold;
            let s2 = separation_threshold * separation_threshold;
            for (i, p) in particles.iter_mut().enumerate() {
                if !p.alive() {
                    continue;
                }
                let (pos, vel, _) = snapshot[i];
                let (mut sep, mut ali, mut coh) = (Vec3::ZERO, Vec3::ZERO, Vec3::ZERO);
                let (mut nn, mut ns) = (0u32, 0u32);
                for (j, &(qp, qv, alive)) in snapshot.iter().enumerate() {
                    if i == j || !alive {
                        continue;
                    }
                    let d = qp - pos;
                    let sd = d.length_squared();
                    if sd >= n2 {
                        continue;
                    }
                    nn += 1;
                    ali += qv;
                    coh += qp;
                    if sd < s2 && sd > 1e-12 {
                        let dist = sd.sqrt();
                        sep -= d / dist * (separation_threshold - dist);
                        ns += 1;
                    }
                }
                let mut force = Vec3::ZERO;
                if ns > 0 {
                    force += sep / ns as f32 * *separation_factor;
                }
                if nn > 0 {
                    force += (ali / nn as f32 - vel) * *alignment_factor;
                    force += (coh / nn as f32 - pos) * *cohesion_factor;
                }
                p.velocity += force * blend.factor(p.age01()) * dt;
                if let Some(max) = max_speed {
                    let s = p.velocity.length();
                    if s > *max && s > 1e-9 {
                        p.velocity *= max / s;
                    }
                }
            }
        }
        Operator::InheritValueFromEvent { input, blend } => {
            let Some(parent) = ctx.parent else { return };
            for p in live {
                let f = blend.factor(p.age01());
                if f <= 0.0 {
                    continue;
                }
                match input {
                    InheritInput::Color => p.color = p.color.lerp(parent.color, f),
                    InheritInput::Size => p.size += (parent.size - p.size) * f,
                    InheritInput::Alpha => p.alpha += (parent.alpha - p.alpha) * f,
                    InheritInput::Velocity => p.velocity = p.velocity.lerp(parent.velocity, f),
                    InheritInput::Rotation => p.rotation = p.rotation.lerp(parent.rotation, f),
                    InheritInput::Lifetime => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::def::{BlendWindow, ValueChange};

    fn ctx<'a>(cps: &'a [Vec3; 8]) -> OpCtx<'a> {
        OpCtx {
            cps,
            cp_velocity: cps,
            time: 0.0,
            dt: 0.1,
            parent: None,
            speed: 1.0,
            random: [0.5, 0.5],
            salt: 0,
        }
    }

    #[test]
    fn movement_integrates_gravity_and_drag() {
        let cps = [Vec3::ZERO; 8];
        let mut ps = vec![Particle::default()];
        let op = Operator::Movement {
            gravity: Vec3::new(0.0, -10.0, 0.0),
            drag: 0.0,
            blend: BlendWindow {
                in_start: 0.0,
                in_end: 0.0,
                out_start: 1.0,
                out_end: 1.0,
            },
        };
        operate(&op, &mut ps, &ctx(&cps));
        assert!((ps[0].velocity.y + 1.0).abs() < 1e-6);
        assert!((ps[0].position.y + 0.1).abs() < 1e-6);
    }

    #[test]
    fn alpha_fade_is_a_tent() {
        let cps = [Vec3::ZERO; 8];
        let mut ps: Vec<Particle> = [0.95, 0.5, 0.05]
            .iter()
            .map(|&left| Particle {
                lifetime: left,
                ..Default::default()
            })
            .collect();
        operate(
            &Operator::AlphaFade {
                fade_in: 0.5,
                fade_out: 0.5,
            },
            &mut ps,
            &ctx(&cps),
        );
        assert!((ps[0].alpha - 0.1).abs() < 1e-5);
        assert!((ps[1].alpha - 1.0).abs() < 1e-5);
        assert!((ps[2].alpha - 0.1).abs() < 1e-5);
    }

    #[test]
    fn size_change_and_dead_particles_untouched() {
        let cps = [Vec3::ZERO; 8];
        let mut ps = vec![
            Particle {
                lifetime: 0.5,
                ..Default::default()
            },
            Particle {
                lifetime: 0.0,
                ..Default::default()
            },
        ];
        let vc = ValueChange {
            start_time: 0.0,
            end_time: 1.0,
            start: Vec3::ONE,
            end: Vec3::ZERO,
        };
        operate(&Operator::SizeChange(vc), &mut ps, &ctx(&cps));
        assert!((ps[0].size - 10.0).abs() < 1e-4);
        assert_eq!(ps[1].size, 20.0);
    }

    #[test]
    fn collision_plane_bounces_and_deletes() {
        let cps = [Vec3::ZERO; 8];
        let below = Particle {
            position: Vec3::new(0.0, -5.0, 0.0),
            velocity: Vec3::new(1.0, -10.0, 0.0),
            ..Default::default()
        };
        let mut ps = vec![below.clone()];
        let plane = Vec4::new(0.0, 1.0, 0.0, 0.0);
        operate(
            &Operator::CollisionPlane {
                plane,
                cp: 0,
                distance: 0.0,
                response: Collision::Bounce(0.5),
            },
            &mut ps,
            &ctx(&cps),
        );
        assert_eq!(ps[0].position.y, 0.0);
        assert!((ps[0].velocity.y - 5.0).abs() < 1e-5);
        let mut ps = vec![below];
        operate(
            &Operator::CollisionPlane {
                plane,
                cp: 0,
                distance: 0.0,
                response: Collision::Delete,
            },
            &mut ps,
            &ctx(&cps),
        );
        assert!(!ps[0].alive());
    }

    #[test]
    fn hsv_round_trip() {
        for c in [
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.2, 0.5, 0.9),
            Vec3::new(0.3, 0.3, 0.3),
        ] {
            let hsv = rgb_to_hsv(c);
            assert!((hsv_to_rgb(hsv.x, hsv.y, hsv.z) - c).length() < 1e-4);
        }
    }

    #[test]
    fn mirrored_sequence_ping_pongs() {
        let idx: Vec<u32> = (0..8).map(|s| sequence_index(s, 4, true)).collect();
        assert_eq!(idx, vec![0, 1, 2, 3, 2, 1, 0, 1]);
        assert_eq!(sequence_index(5, 4, false), 1);
    }

    use glam::Vec4;
}
