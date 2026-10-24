//! Typed form of a particle definition file (`particles/*.json`).

use std::f32::consts::TAU;

use glam::{Vec2, Vec3, Vec4};
use serde_json::Value;

use crate::json::{Obj, floats};

/// Number of control point slots per particle system.
pub const CONTROL_POINTS: usize = 8;

/// Particle flag: particles live in world space instead of the object's space.
pub const FLAG_WORLD_SPACE: u32 = 1;
/// Particle flag: no blending between sprite sheet frames.
pub const FLAG_NO_FRAME_BLENDING: u32 = 2;
/// Particle flag: render with the scene's perspective camera.
pub const FLAG_PERSPECTIVE: u32 = 4;

/// Control point flag: the point follows the cursor.
pub const CP_LINK_MOUSE: u32 = 1;
/// Control point flag: the offset is in world (scene) coordinates.
pub const CP_WORLD_SPACE: u32 = 2;
/// Control point flag: the point tracks the parent system's event particle.
pub const CP_FOLLOW_PARENT_PARTICLE: u32 = 4;

/// Emitter flag: emit at most one particle per tick.
pub const EMITTER_ONE_PER_FRAME: u32 = 2;

fn cp_index(v: i32) -> usize {
    v.clamp(0, CONTROL_POINTS as i32 - 1) as usize
}

/// Uniform scalar range with an optional bias exponent (`pow(rand, exponent)`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    pub min: f32,
    pub max: f32,
    pub exponent: f32,
}

impl Range {
    fn read(o: Obj, min: f32, max: f32) -> Self {
        Self {
            min: o.f32("min", min),
            max: o.f32("max", max),
            exponent: o.f32("exponent", 1.0),
        }
    }
}

/// Per-component uniform vector range with a bias exponent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VecRange {
    pub min: Vec3,
    pub max: Vec3,
    pub exponent: f32,
}

impl VecRange {
    fn read(o: Obj, min: Vec3, max: Vec3) -> Self {
        Self {
            min: o.vec3("min", min),
            max: o.vec3("max", max),
            exponent: o.f32("exponent", 1.0),
        }
    }
}

/// Lifetime window over which an operator's effect is blended in and out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlendWindow {
    pub in_start: f32,
    pub in_end: f32,
    pub out_start: f32,
    pub out_end: f32,
}

impl BlendWindow {
    fn read(o: Obj) -> Self {
        Self {
            in_start: o.f32("blendinstart", 0.0),
            in_end: o.f32("blendinend", 0.0),
            out_start: o.f32("blendoutstart", 1.0),
            out_end: o.f32("blendoutend", 1.0),
        }
    }

    /// Strength in `[0, 1]` at normalized age `life`.
    pub fn factor(&self, life: f32) -> f32 {
        let mut f = 1.0;
        if self.in_end > self.in_start + 1e-6 {
            f *= ((life - self.in_start) / (self.in_end - self.in_start)).clamp(0.0, 1.0);
        }
        if self.out_end > self.out_start + 1e-6 && self.out_start < 1.0 - 1e-6 {
            f *= 1.0 - ((life - self.out_start) / (self.out_end - self.out_start)).clamp(0.0, 1.0);
        }
        f
    }
}

/// Shape an emitter spawns particles in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitterShape {
    Box,
    Sphere,
}

/// On/off emission cycling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Periodic {
    pub min_delay: f32,
    pub max_delay: f32,
    pub min_duration: f32,
    pub max_duration: f32,
    /// Emission cap per active period; 0 = unlimited.
    pub max_per_period: u32,
}

/// One `emitter` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct EmitterDef {
    pub shape: EmitterShape,
    pub directions: Vec3,
    pub distance_min: Vec3,
    pub distance_max: Vec3,
    pub origin: Vec3,
    /// Forced sign per axis (-1, 0 = free, 1).
    pub sign: [i32; 3],
    /// Particles emitted at once when the emitter starts.
    pub instantaneous: u32,
    pub speed_min: f32,
    pub speed_max: f32,
    /// Particles per second.
    pub rate: f32,
    /// Control point the emitter is anchored to.
    pub control_point: usize,
    pub flags: u32,
    /// Seconds before the emitter starts.
    pub delay: f32,
    /// Seconds the emitter runs for; 0 = forever.
    pub duration: f32,
    pub periodic: Option<Periodic>,
    /// Non-zero when the rate reacts to audio (not simulated; treated as constant).
    pub audio_mode: u32,
}

impl EmitterDef {
    fn read(o: Obj, unknown: &mut Vec<String>) -> Option<Self> {
        let shape = match o.str("name").unwrap_or("") {
            "boxrandom" => EmitterShape::Box,
            "sphererandom" => EmitterShape::Sphere,
            other => {
                unknown.push(format!("emitter:{other}"));
                return None;
            }
        };
        let dist = |key: &str, default: f32| -> Vec3 {
            match shape {
                EmitterShape::Sphere => Vec3::splat(
                    o.get(key)
                        .and_then(floats)
                        .and_then(|f| f.first().copied())
                        .unwrap_or(default),
                ),
                EmitterShape::Box => o.vec3(key, Vec3::splat(default)),
            }
        };
        let sign = o.vec_n::<3>("sign").unwrap_or([0.0; 3]).map(|s| {
            if s > 0.0 {
                1
            } else if s < 0.0 {
                -1
            } else {
                0
            }
        });
        let periodic = Periodic {
            min_delay: o.f32("minperiodicdelay", 0.0),
            max_delay: o.f32("maxperiodicdelay", 0.0),
            min_duration: o.f32("minperiodicduration", 0.0),
            max_duration: o.f32("maxperiodicduration", 0.0),
            max_per_period: o.u32("maxtoemitperperiod", 0),
        };
        let has_periodic = periodic.max_delay > 0.0 || periodic.max_duration > 0.0;
        Some(Self {
            shape,
            directions: o.vec3("directions", Vec3::new(1.0, 1.0, 0.0)),
            distance_min: dist("distancemin", 0.0),
            distance_max: dist("distancemax", 256.0),
            origin: o.vec3("origin", Vec3::ZERO),
            sign,
            instantaneous: o.u32("instantaneous", 0),
            speed_min: o.f32("speedmin", 0.0),
            speed_max: o.f32("speedmax", 0.0),
            rate: o.f32("rate", 5.0),
            control_point: cp_index(o.i32("controlpoint", 0)),
            flags: o.u32("flags", 0),
            delay: o.f32("delay", 0.0).max(0.0),
            duration: o.f32("duration", 0.0).max(0.0),
            periodic: has_periodic.then_some(periodic),
            audio_mode: o.u32("audioprocessingmode", 0),
        })
    }
}

/// Particle property an event-inherit initializer/operator copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InheritInput {
    Color,
    Size,
    Alpha,
    Velocity,
    Rotation,
    Lifetime,
}

impl InheritInput {
    fn parse(s: &str) -> Option<Self> {
        Some(match s.strip_prefix("particle").unwrap_or(s) {
            "" | "color" => Self::Color,
            "size" => Self::Size,
            "alpha" | "opacity" => Self::Alpha,
            "velocity" => Self::Velocity,
            "rotation" => Self::Rotation,
            "lifetime" => Self::Lifetime,
            _ => return None,
        })
    }
}

/// One `initializer` entry: sets a property of every newly spawned particle.
#[derive(Debug, Clone, PartialEq)]
pub enum Initializer {
    /// Colour in 0..255 space, one shared random factor for all channels.
    ColorRandom(VecRange),
    LifetimeRandom(Range),
    SizeRandom(Range),
    AlphaRandom(Range),
    VelocityRandom {
        range: VecRange,
        /// Also add this control point's velocity.
        inherit_cp: Option<usize>,
    },
    RotationRandom(VecRange),
    AngularVelocityRandom(VecRange),
    TurbulentVelocityRandom {
        scale: f32,
        timescale: f32,
        offset: f32,
        speed_min: f32,
        speed_max: f32,
        forward: Vec3,
        right: Vec3,
    },
    PositionOffsetRandom {
        distance: f32,
    },
    BoxPosition(VecRange),
    SpherePosition(Range),
    RemapInitialValue {
        input_cp: usize,
        input_min: f32,
        input_max: f32,
        output: RemapOutput,
        output_min: Vec3,
        output_max: Vec3,
        operation: RemapOperation,
    },
    MapSequenceBetweenControlPoints {
        count: u32,
        cp_start: usize,
        cp_end: usize,
        mirror: bool,
        arc_amount: f32,
        arc_direction: Vec3,
        size_reduction: f32,
    },
    MapSequenceAroundControlPoint {
        count: f32,
        cp: usize,
        mirror: bool,
        axis: Vec3,
        bounds: Vec2,
        speed_min: Vec3,
        speed_max: Vec3,
    },
    HsvColorRandom {
        hue_min: f32,
        hue_max: f32,
        sat_min: f32,
        sat_max: f32,
        val_min: f32,
        val_max: f32,
        hue_steps: u32,
    },
    ColorList {
        colors: Vec<Vec3>,
        hue_noise: f32,
        sat_noise: f32,
        val_noise: f32,
    },
    InheritControlPointVelocity {
        cp: usize,
        min: f32,
        max: f32,
    },
    InheritInitialValueFromEvent(InheritInput),
}

impl Initializer {
    fn read(o: Obj, unknown: &mut Vec<String>) -> Option<Self> {
        let name = o.str("name").unwrap_or("");
        Some(match name {
            "colorrandom" => {
                let mut r = VecRange::read(o, Vec3::ZERO, Vec3::splat(255.0));
                if !o.has("max") {
                    r.max = r.min;
                }
                Self::ColorRandom(r)
            }
            "lifetimerandom" => Self::LifetimeRandom(Range::read(o, 0.0, 1.0)),
            "sizerandom" => Self::SizeRandom(Range::read(o, 0.0, 20.0)),
            "alpharandom" => Self::AlphaRandom(Range::read(o, 0.05, 1.0)),
            "velocityrandom" => Self::VelocityRandom {
                range: VecRange::read(o, Vec3::new(-32.0, -32.0, 0.0), Vec3::new(32.0, 32.0, 0.0)),
                inherit_cp: o
                    .bool("inheritcontrolpointvelocity", false)
                    .then(|| cp_index(o.i32("controlpoint", 0))),
            },
            "rotationrandom" => {
                Self::RotationRandom(VecRange::read(o, Vec3::ZERO, Vec3::new(0.0, 0.0, TAU)))
            }
            "angularvelocityrandom" => Self::AngularVelocityRandom(VecRange::read(
                o,
                Vec3::new(0.0, 0.0, -5.0),
                Vec3::new(0.0, 0.0, 5.0),
            )),
            "turbulentvelocityrandom" => Self::TurbulentVelocityRandom {
                scale: o.f32("scale", 1.0),
                timescale: o.f32("timescale", 1.0),
                offset: o.f32("offset", 0.0),
                speed_min: o.f32("speedmin", 100.0),
                speed_max: o.f32("speedmax", 250.0),
                forward: o.vec3("forward", Vec3::Y),
                right: o.vec3("right", Vec3::Z),
            },
            "positionoffsetrandom" => Self::PositionOffsetRandom {
                distance: o.f32("distance", 0.0),
            },
            "box" => Self::BoxPosition(VecRange::read(o, Vec3::ZERO, Vec3::ZERO)),
            "sphere" => Self::SpherePosition(Range::read(o, 0.0, 0.0)),
            "remapinitialvalue" => {
                let output_name = o.str("output").unwrap_or("");
                let (op_prefix, output_name) = strip_operation_prefix(output_name);
                let output = RemapOutput::parse(output_name);
                if !matches!(output, RemapOutput::Size | RemapOutput::Color) {
                    unknown.push(format!("remapinitialvalue.output:{output_name}"));
                }
                Self::RemapInitialValue {
                    input_cp: cp_index(o.i32("inputcontrolpoint0", 0)),
                    input_min: o.f32("inputrangemin", 0.0),
                    input_max: o.f32("inputrangemax", 100.0),
                    output,
                    output_min: o.vec3("outputrangemin", Vec3::ZERO),
                    output_max: o.vec3("outputrangemax", Vec3::ONE),
                    operation: o
                        .str("operation")
                        .or(op_prefix)
                        .map(RemapOperation::parse)
                        .unwrap_or(RemapOperation::Set),
                }
            }
            "mapsequencebetweencontrolpoints" => {
                let flags = o.u32("flags", 0);
                Self::MapSequenceBetweenControlPoints {
                    count: o.u32("count", 1).max(1),
                    cp_start: cp_index(o.i32("controlpointstart", 0)),
                    cp_end: cp_index(o.i32("controlpointend", 1)),
                    mirror: o.str("limitbehavior") == Some("mirror") || flags & 2 != 0,
                    arc_amount: o.f32("arcamount", 0.0),
                    arc_direction: o.vec3("arcdirection", Vec3::ZERO),
                    size_reduction: o.f32("sizereductionamount", 0.0),
                }
            }
            "mapsequencearoundcontrolpoint" => {
                let flags = o.u32("flags", 0);
                let axis = o.vec3("axis", Vec3::Z);
                Self::MapSequenceAroundControlPoint {
                    count: o.f32("count", 1.0).max(1e-3),
                    cp: cp_index(o.i32("controlpoint", 0)),
                    mirror: o.str("limitbehavior") == Some("mirror") || flags & 2 != 0,
                    axis: if axis.length_squared() > 1e-8 {
                        axis.normalize()
                    } else {
                        Vec3::Z
                    },
                    bounds: o.vec2("bounds", Vec2::new(0.0, 1.0)),
                    speed_min: o.vec3("speedmin", Vec3::ZERO),
                    speed_max: o.vec3("speedmax", Vec3::ZERO),
                }
            }
            "hsvcolorrandom" => Self::HsvColorRandom {
                hue_min: o.f32("huemin", 0.0),
                hue_max: o.f32("huemax", 360.0),
                sat_min: o.f32("saturationmin", 0.0),
                sat_max: o.f32("saturationmax", 1.0),
                val_min: o.f32("valuemin", 1.0),
                val_max: o.f32("valuemax", 1.0),
                hue_steps: o.u32("huesteps", 0),
            },
            "colorlist" => {
                let colors: Vec<Vec3> = o
                    .array("colors")
                    .iter()
                    .filter_map(|c| {
                        let f = floats(c)?;
                        let mut v = Vec3::new(
                            *f.first()?,
                            f.get(1).copied().unwrap_or(0.0),
                            f.get(2).copied().unwrap_or(0.0),
                        );
                        if v.max_element() > 1.5 {
                            v /= 255.0;
                        }
                        Some(v)
                    })
                    .collect();
                Self::ColorList {
                    colors: if colors.is_empty() {
                        vec![Vec3::ONE]
                    } else {
                        colors
                    },
                    hue_noise: o.f32("huenoise", 0.0),
                    sat_noise: o.f32("saturationnoise", 0.0),
                    val_noise: o.f32("valuenoise", 0.0),
                }
            }
            "inheritcontrolpointvelocity" => Self::InheritControlPointVelocity {
                cp: cp_index(o.i32("controlpoint", 0)),
                min: o.f32("min", 0.0),
                max: o.f32("max", 1.0),
            },
            "inheritinitialvaluefromevent" => {
                let input = o.str("input").unwrap_or("");
                match InheritInput::parse(input) {
                    Some(i) => Self::InheritInitialValueFromEvent(i),
                    None => {
                        unknown.push(format!("inheritinitialvaluefromevent.input:{input}"));
                        return None;
                    }
                }
            }
            other => {
                unknown.push(format!("initializer:{other}"));
                return None;
            }
        })
    }
}

/// `startvalue → endvalue` ramp over a normalized-age window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValueChange {
    pub start_time: f32,
    pub end_time: f32,
    pub start: Vec3,
    pub end: Vec3,
}

impl ValueChange {
    fn read(o: Obj, start: Vec3, end: Vec3) -> Self {
        Self {
            start_time: o.f32("starttime", 0.0),
            end_time: o.f32("endtime", 1.0),
            start: o.vec3("startvalue", start),
            end: o.vec3("endvalue", end),
        }
    }

    /// Ramp value at normalized age `life`.
    pub fn at(&self, life: f32) -> Vec3 {
        if life <= self.start_time {
            self.start
        } else if life > self.end_time {
            self.end
        } else {
            let t = (life - self.start_time) / (self.end_time - self.start_time).max(1e-6);
            self.start.lerp(self.end, t)
        }
    }
}

/// Per-particle cosine oscillation parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Oscillation {
    pub mask: Vec3,
    pub frequency_min: f32,
    pub frequency_max: f32,
    pub scale_min: f32,
    pub scale_max: f32,
    pub phase_min: f32,
    pub phase_max: f32,
    pub blend: BlendWindow,
}

impl Oscillation {
    fn read(o: Obj, freq_max: f32, scale: (f32, f32)) -> Self {
        let frequency_min = o.f32("frequencymin", 0.0);
        let mut frequency_max = o.f32("frequencymax", freq_max);
        if frequency_max == 0.0 {
            frequency_max = frequency_min;
        }
        let phase_min = o.f32("phasemin", 0.0);
        Self {
            mask: o.vec3("mask", Vec3::new(1.0, 1.0, 0.0)),
            frequency_min,
            frequency_max,
            scale_min: o.f32("scalemin", scale.0),
            scale_max: o.f32("scalemax", scale.1),
            phase_min,
            phase_max: o.f32("phasemax", TAU).max(phase_min),
            blend: BlendWindow::read(o),
        }
    }
}

/// Scalar a `remapvalue` operator reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemapInput {
    SystemTime,
    Lifetime,
    Velocity,
    Position,
    Rotation,
    AngularVelocity,
    Color,
    Size,
    Alpha,
    Speed,
    AngularSpeed,
    MaxLifetime,
    ControlPoint,
    ControlPointVelocity,
    DistanceToControlPoint,
    DeltaToControlPoint,
    DirectionToControlPoint,
    PositionBetweenControlPoints,
    Random,
    Noise,
}

impl RemapInput {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "particlesystemtime" | "runtime" | "layertime" => Self::SystemTime,
            "" | "particlelifetime" | "lifetime" => Self::Lifetime,
            "particlevelocity" | "velocity" => Self::Velocity,
            "particleposition" | "position" => Self::Position,
            "particlerotation" | "rotation" => Self::Rotation,
            "particleangularvelocity" | "angularvelocity" => Self::AngularVelocity,
            "particlecolor" | "color" => Self::Color,
            "particlesize" | "size" => Self::Size,
            "particlealpha" | "opacity" | "alpha" => Self::Alpha,
            "speed" => Self::Speed,
            "angularspeed" => Self::AngularSpeed,
            "maxlifetime" => Self::MaxLifetime,
            "controlpoint" | "controlpointposition" => Self::ControlPoint,
            "controlpointvelocity" => Self::ControlPointVelocity,
            "distancetocontrolpoint" => Self::DistanceToControlPoint,
            "deltatocontrolpoint" => Self::DeltaToControlPoint,
            "directiontocontrolpoint" => Self::DirectionToControlPoint,
            "positionbetweentwocontrolpoints" => Self::PositionBetweenControlPoints,
            "random" => Self::Random,
            "noise" => Self::Noise,
            _ => return None,
        })
    }
}

/// Waveform a `remapvalue` operator passes its normalized input through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemapTransform {
    Linear,
    Sine,
    Cosine,
    Square,
    Saw,
    Triangle,
    SimplexNoise,
    FbmNoise,
    Step,
    Smoothstep,
}

impl RemapTransform {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "" | "none" | "linear" => Self::Linear,
            "sine" => Self::Sine,
            "cosine" => Self::Cosine,
            "square" => Self::Square,
            "saw" => Self::Saw,
            "triangle" => Self::Triangle,
            "simplexnoise" => Self::SimplexNoise,
            "fbmnoise" => Self::FbmNoise,
            "step" => Self::Step,
            "smoothstep" => Self::Smoothstep,
            _ => return None,
        })
    }
}

/// How a remapped value combines with the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemapOperation {
    Set,
    Add,
    Subtract,
    Multiply,
}

impl RemapOperation {
    fn parse(s: &str) -> Self {
        match s {
            "add" => Self::Add,
            "subtract" => Self::Subtract,
            "multiply" | "fade" => Self::Multiply,
            _ => Self::Set,
        }
    }

    /// Combine `current` with `value`, weakened toward a no-op by `blend`.
    pub fn apply(self, current: f32, value: f32, blend: f32) -> f32 {
        match self {
            Self::Set => current + (value - current) * blend,
            Self::Add => current + value * blend,
            Self::Subtract => current - value * blend,
            Self::Multiply => current * (1.0 + (value - 1.0) * blend),
        }
    }
}

/// Property a remap writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemapOutput {
    Alpha,
    Size,
    Velocity,
    Speed,
    AngularVelocity,
    Rotation,
    Color,
    Unsupported,
}

impl RemapOutput {
    fn parse(s: &str) -> Self {
        match s.strip_prefix("particle").unwrap_or(s) {
            "opacity" | "alpha" => Self::Alpha,
            "size" => Self::Size,
            "velocity" => Self::Velocity,
            "speed" => Self::Speed,
            "angularvelocity" => Self::AngularVelocity,
            "rotation" => Self::Rotation,
            "color" | "coloropacity" => Self::Color,
            _ => Self::Unsupported,
        }
    }
}

/// Vector-to-scalar reduction for remap inputs, or the written axis for outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    X,
    Y,
    Z,
    XPlusY,
    Sum,
    Average,
    Max,
    Min,
    Magnitude,
}

impl Component {
    fn parse(s: &str) -> Self {
        match s {
            "x" => Self::X,
            "y" => Self::Y,
            "z" => Self::Z,
            "x+y" => Self::XPlusY,
            "sum" => Self::Sum,
            "average" => Self::Average,
            "max" => Self::Max,
            "min" => Self::Min,
            _ => Self::Magnitude,
        }
    }

    /// Reduce `v` to a scalar.
    pub fn reduce(self, v: Vec3) -> f32 {
        match self {
            Self::X => v.x,
            Self::Y => v.y,
            Self::Z => v.z,
            Self::XPlusY => v.x + v.y,
            Self::Sum => v.x + v.y + v.z,
            Self::Average => (v.x + v.y + v.z) / 3.0,
            Self::Max => v.max_element(),
            Self::Min => v.min_element(),
            Self::Magnitude => v.length(),
        }
    }
}

fn strip_operation_prefix(output: &str) -> (Option<&'static str>, &str) {
    for (prefix, op) in [
        ("set", "set"),
        ("add", "add"),
        ("multiply", "multiply"),
        ("subtract", "subtract"),
        ("fade", "multiply"),
    ] {
        if let Some(rest) = output.strip_prefix(prefix)
            && !rest.is_empty()
        {
            return (Some(op), rest);
        }
    }
    (None, output)
}

/// What happens to a particle that hits a collider.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Collision {
    /// Reflect the velocity, keeping `bounce` of the normal component.
    Bounce(f32),
    /// Kill the particle.
    Delete,
}

impl Collision {
    fn read(o: Obj) -> Self {
        match o.str("collisionbehavior") {
            Some("delete" | "kill") => Self::Delete,
            _ => Self::Bounce(o.f32("bouncefactor", 1.0)),
        }
    }
}

/// One `operator` entry: runs every tick over all particles.
#[derive(Debug, Clone, PartialEq)]
pub enum Operator {
    Movement {
        gravity: Vec3,
        drag: f32,
        blend: BlendWindow,
    },
    AngularMovement {
        force: Vec3,
        drag: f32,
        blend: BlendWindow,
    },
    SizeChange(ValueChange),
    AlphaChange(ValueChange),
    ColorChange(ValueChange),
    AlphaFade {
        fade_in: f32,
        fade_out: f32,
    },
    OscillateAlpha(Oscillation),
    OscillateSize(Oscillation),
    OscillatePosition(Oscillation),
    Turbulence {
        phase_min: f32,
        phase_max: f32,
        speed_min: f32,
        speed_max: f32,
        timescale: f32,
        scale: f32,
        mask: Vec3,
        blend: BlendWindow,
    },
    Vortex {
        cp: usize,
        distance_inner: f32,
        distance_outer: f32,
        speed_inner: f32,
        speed_outer: f32,
        flags: u32,
        offset: Vec3,
        axis: Vec3,
        blend: BlendWindow,
    },
    VortexV2 {
        cp: usize,
        distance_inner: f32,
        distance_outer: f32,
        speed_inner: f32,
        speed_outer: f32,
        flags: u32,
        offset: Vec3,
        axis: Vec3,
        ring_radius: f32,
        ring_width: f32,
        ring_pull: f32,
    },
    ControlPointAttract {
        cp: usize,
        scale: f32,
        threshold: f32,
        origin: Vec3,
        blend: BlendWindow,
    },
    MaintainDistanceBetweenControlPoints,
    MaintainDistanceToControlPoint {
        cp: usize,
        distance: f32,
        strength: f32,
        blend: BlendWindow,
    },
    ReduceMovementNearControlPoint {
        cp: usize,
        distance_inner: f32,
        distance_outer: f32,
        reduction_inner: f32,
    },
    RemapValue {
        input: RemapInput,
        input_component: Component,
        input_cp: usize,
        input_min: f32,
        input_max: f32,
        transform: RemapTransform,
        transform_scale: f32,
        octaves: u32,
        output: RemapOutput,
        output_component: Component,
        output_min: Vec3,
        output_max: Vec3,
        operation: RemapOperation,
        blend: BlendWindow,
    },
    CapVelocity {
        max_speed: f32,
        blend: BlendWindow,
    },
    CollisionPlane {
        plane: Vec4,
        cp: usize,
        distance: f32,
        response: Collision,
    },
    CollisionSphere {
        origin: Vec3,
        cp: usize,
        radius: f32,
        response: Collision,
    },
    CollisionQuad {
        origin: Vec3,
        normal: Vec3,
        forward: Vec3,
        size: Vec2,
        cp: usize,
        response: Collision,
    },
    CollisionBox {
        half_size: Vec3,
        cp: usize,
        response: Collision,
    },
    Boids {
        neighbor_threshold: f32,
        separation_threshold: f32,
        separation_factor: f32,
        cohesion_factor: f32,
        alignment_factor: f32,
        max_speed: Option<f32>,
        blend: BlendWindow,
    },
    InheritValueFromEvent {
        input: InheritInput,
        blend: BlendWindow,
    },
}

impl Operator {
    fn read(o: Obj, unknown: &mut Vec<String>) -> Option<Self> {
        let name = o.str("name").unwrap_or("");
        let blend = BlendWindow::read(o);
        let cp = cp_index(o.i32("controlpoint", 0));
        Some(match name {
            "movement" => Self::Movement {
                gravity: o.vec3("gravity", Vec3::ZERO),
                drag: o.f32("drag", 0.0),
                blend,
            },
            "angularmovement" => Self::AngularMovement {
                force: o.vec3("force", Vec3::ZERO),
                drag: o.f32("drag", 0.0),
                blend,
            },
            "sizechange" => Self::SizeChange(ValueChange::read(o, Vec3::ONE, Vec3::ZERO)),
            "alphachange" => Self::AlphaChange(ValueChange::read(o, Vec3::ONE, Vec3::ZERO)),
            "colorchange" => Self::ColorChange(ValueChange::read(o, Vec3::ONE, Vec3::ONE)),
            "alphafade" => Self::AlphaFade {
                fade_in: o.f32("fadeintime", 0.5),
                fade_out: o.f32("fadeouttime", 0.5),
            },
            "oscillatealpha" => Self::OscillateAlpha(Oscillation::read(o, 10.0, (0.0, 1.0))),
            "oscillatesize" => Self::OscillateSize(Oscillation::read(o, 10.0, (0.8, 1.2))),
            "oscillateposition" => Self::OscillatePosition(Oscillation::read(o, 5.0, (0.0, 1.0))),
            "turbulence" => Self::Turbulence {
                phase_min: o.f32("phasemin", 0.0),
                phase_max: o.f32("phasemax", 0.0),
                speed_min: o.f32("speedmin", 500.0),
                speed_max: o.f32("speedmax", 1000.0),
                timescale: o.f32("timescale", 20.0),
                scale: o.f32("scale", 0.01),
                mask: o.vec3("mask", Vec3::new(1.0, 1.0, 0.0)),
                blend,
            },
            "vortex" => Self::Vortex {
                cp,
                distance_inner: o.f32("distanceinner", 500.0),
                distance_outer: o.f32("distanceouter", 650.0),
                speed_inner: o.f32("speedinner", 2500.0),
                speed_outer: o.f32("speedouter", 0.0),
                flags: o.u32("flags", 0),
                offset: o.vec3("offset", Vec3::ZERO),
                axis: o.vec3("axis", Vec3::Z),
                blend,
            },
            "vortex_v2" => Self::VortexV2 {
                cp,
                distance_inner: o.f32("distanceinner", 500.0),
                distance_outer: o.f32("distanceouter", 650.0),
                speed_inner: o.f32("speedinner", 2500.0),
                speed_outer: o.f32("speedouter", 0.0),
                flags: o.u32("flags", 0),
                offset: o.vec3("offset", Vec3::ZERO),
                axis: o.vec3("axis", Vec3::Z).normalize_or(Vec3::Z),
                ring_radius: o.f32("ringradius", 0.0),
                ring_width: o.f32("ringwidth", 0.0),
                ring_pull: o.f32("ringpulldistance", 0.0),
            },
            "controlpointattract" | "controlpointforce" => {
                let scale = o.f32("scale", 512.0);
                Self::ControlPointAttract {
                    cp,
                    scale: if name == "controlpointforce" {
                        -scale
                    } else {
                        scale
                    },
                    threshold: o.f32("threshold", o.f32("threadhold", 512.0)),
                    origin: o.vec3("origin", o.vec3("offset", Vec3::ZERO)),
                    blend,
                }
            }
            "maintaindistancebetweencontrolpoints" => Self::MaintainDistanceBetweenControlPoints,
            "maintaindistancetocontrolpoint" => Self::MaintainDistanceToControlPoint {
                cp,
                distance: o.f32("distance", 100.0),
                strength: o.f32("variablestrength", 0.0),
                blend,
            },
            "reducemovementnearcontrolpoint" => Self::ReduceMovementNearControlPoint {
                cp,
                distance_inner: o.f32("distanceinner", 0.0),
                distance_outer: o.f32("distanceouter", 100.0),
                reduction_inner: o.f32("reductioninner", 1.0),
            },
            "remapvalue" => {
                let input_name = o.str("input").unwrap_or("");
                let Some(input) = RemapInput::parse(input_name) else {
                    unknown.push(format!("remapvalue.input:{input_name}"));
                    return None;
                };
                let transform_name = o.str("transformfunction").unwrap_or("");
                let transform = RemapTransform::parse(transform_name).unwrap_or_else(|| {
                    unknown.push(format!("remapvalue.transform:{transform_name}"));
                    RemapTransform::Linear
                });
                let (op_prefix, output_name) =
                    strip_operation_prefix(o.str("output").unwrap_or(""));
                let output = RemapOutput::parse(output_name);
                if output == RemapOutput::Unsupported {
                    unknown.push(format!("remapvalue.output:{output_name}"));
                    return None;
                }
                Self::RemapValue {
                    input,
                    input_component: Component::parse(o.str("inputcomponent").unwrap_or("")),
                    input_cp: cp_index(o.i32("inputcontrolpoint0", 0)),
                    input_min: o.f32("inputrangemin", 0.0),
                    input_max: o.f32("inputrangemax", 1.0),
                    transform,
                    transform_scale: o.f32("transforminputscale", 1.0),
                    octaves: o.u32("transformoctaves", 1).clamp(1, 8),
                    output,
                    output_component: Component::parse(o.str("outputcomponent").unwrap_or("")),
                    output_min: o.vec3("outputrangemin", Vec3::ZERO),
                    output_max: o.vec3("outputrangemax", Vec3::ONE),
                    operation: o
                        .str("operation")
                        .or(op_prefix)
                        .map(RemapOperation::parse)
                        .unwrap_or(RemapOperation::Set),
                    blend,
                }
            }
            "capvelocity" => Self::CapVelocity {
                max_speed: o.f32("maxspeed", 100.0),
                blend,
            },
            "collisionplane" => Self::CollisionPlane {
                plane: o.vec4("plane", Vec4::new(0.0, 1.0, 0.0, 0.0)),
                cp,
                distance: o.f32("distance", 0.0),
                response: Collision::read(o),
            },
            "collisionsphere" => Self::CollisionSphere {
                origin: o.vec3("origin", Vec3::ZERO),
                cp,
                radius: o.f32("radius", 100.0),
                response: Collision::read(o),
            },
            "collisionquad" => Self::CollisionQuad {
                origin: o.vec3("origin", Vec3::ZERO),
                normal: o.vec3("plane", Vec3::Y).normalize_or(Vec3::Y),
                forward: o.vec3("forward", Vec3::X),
                size: o.vec2("size", Vec2::splat(100.0)),
                cp,
                response: Collision::read(o),
            },
            "collisionbox" | "collisionbounds" => Self::CollisionBox {
                half_size: o.vec3("halfsize", Vec3::new(1024.0, 1024.0, 256.0)),
                cp,
                response: Collision::read(o),
            },
            "boids" => Self::Boids {
                neighbor_threshold: o.f32("neighborthreshold", 1.0),
                separation_threshold: o.f32("separationthreshold", 1.0),
                separation_factor: o.f32("separationfactor", 1.0),
                cohesion_factor: o.f32("cohesionfactor", 1.0),
                alignment_factor: o.f32("alignmentfactor", 1.0),
                max_speed: o.f32_opt("maxspeed"),
                blend,
            },
            "inheritvaluefromevent" => {
                let input = o.str("input").unwrap_or("");
                match InheritInput::parse(input) {
                    Some(input) => Self::InheritValueFromEvent { input, blend },
                    None => {
                        unknown.push(format!("inheritvaluefromevent.input:{input}"));
                        return None;
                    }
                }
            }
            other => {
                unknown.push(format!("operator:{other}"));
                return None;
            }
        })
    }
}

/// How particles are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererKind {
    /// One camera-facing quad per particle.
    Sprite,
    /// Quads stretched along the particle velocity.
    SpriteTrail,
    /// One ribbon through every live particle in emission order.
    Rope,
    /// One ribbon per particle through its position history.
    RopeTrail,
}

/// The first `renderer` entry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RendererDef {
    pub kind: RendererKind,
    /// Sprite trail: stretch per unit of speed. Rope trail: history length in seconds.
    pub length: f32,
    /// Sprite trail: maximum stretch.
    pub max_length: f32,
    /// Sprite trail: minimum stretch.
    pub min_length: f32,
    /// Ribbon subdivisions per segment (spline smoothing).
    pub subdivision: u32,
    /// Rope trail: history points per particle.
    pub segments: u32,
    /// Rope trail: fade alpha toward the tail.
    pub fade_alpha: bool,
}

impl RendererDef {
    fn read(o: Obj, unknown: &mut Vec<String>) -> Self {
        let name = o.str("name").unwrap_or("sprite");
        let kind = match name {
            "sprite" => RendererKind::Sprite,
            "spritetrail" => RendererKind::SpriteTrail,
            "ropetrail" => RendererKind::RopeTrail,
            n if n.starts_with("rope") => RendererKind::Rope,
            other => {
                unknown.push(format!("renderer:{other}"));
                RendererKind::Sprite
            }
        };
        let default_length = if kind == RendererKind::RopeTrail {
            1.0
        } else {
            0.05
        };
        Self {
            kind,
            length: o.f32("length", default_length),
            max_length: o.f32("maxlength", 10.0),
            min_length: o.f32("minlength", 0.0),
            subdivision: o.u32("subdivision", 3).min(32),
            segments: o.u32("segments", 10).clamp(1, 64),
            fade_alpha: o.bool("fadealpha", false),
        }
    }
}

/// One `controlpoint` slot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlPointDef {
    pub offset: Vec3,
    /// `offset: null` in the file (slot never assigned in the editor).
    pub null_offset: bool,
    pub flags: u32,
    /// Parent system control point this slot chains to; 0 = none.
    pub parent: i32,
}

impl Default for ControlPointDef {
    fn default() -> Self {
        Self {
            offset: Vec3::ZERO,
            null_offset: false,
            flags: 0,
            parent: 0,
        }
    }
}

/// When a child system creates an instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnType {
    /// One instance that runs alongside the parent.
    Static,
    /// One instance per parent particle, following it.
    EventFollow,
    /// One instance per parent particle, created when it spawns.
    EventSpawn,
    /// One instance per parent particle, created where it dies.
    EventDeath,
}

/// One `children` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildDef {
    pub name: String,
    pub spawn: SpawnType,
    /// Maximum simultaneous instances.
    pub max_count: u32,
    /// Shift applied when chaining control points to the parent.
    pub cp_start_index: i32,
    pub probability: f32,
    pub origin: Vec3,
    pub scale: Vec3,
    pub angles: Vec3,
}

impl ChildDef {
    fn read(o: Obj, unknown: &mut Vec<String>) -> Option<Self> {
        let name = o.str("name")?.to_owned();
        let spawn = match o.str("type").unwrap_or("static") {
            "static" => SpawnType::Static,
            "eventfollow" => SpawnType::EventFollow,
            "eventspawn" => SpawnType::EventSpawn,
            "eventdeath" => SpawnType::EventDeath,
            other => {
                unknown.push(format!("child.type:{other}"));
                SpawnType::Static
            }
        };
        Some(Self {
            name,
            spawn,
            max_count: o.u32("maxcount", 20).max(1),
            cp_start_index: o.i32("controlpointstartindex", 0),
            probability: o.f32("probability", 1.0),
            origin: o.vec3("origin", Vec3::ZERO),
            scale: o.vec3("scale", Vec3::ONE),
            angles: o.vec3("angles", Vec3::ZERO),
        })
    }
}

/// Sprite sheet frame selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationMode {
    /// Play the frames over the particle's life.
    Sequence,
    /// One random frame per particle.
    RandomFrame,
}

/// A parsed particle definition file.
#[derive(Debug, Clone, PartialEq)]
pub struct ParticleDef {
    pub emitters: Vec<EmitterDef>,
    pub initializers: Vec<Initializer>,
    pub operators: Vec<Operator>,
    /// `None` for spawner-only systems that draw nothing themselves.
    pub renderer: Option<RendererDef>,
    pub control_points: [ControlPointDef; CONTROL_POINTS],
    pub material: Option<String>,
    pub children: Vec<ChildDef>,
    pub animation_mode: AnimationMode,
    pub sequence_multiplier: f32,
    pub max_count: u32,
    /// Seconds simulated before the first frame.
    pub start_time: f32,
    pub flags: u32,
    /// `kind:name` of every entry this crate does not simulate.
    pub unknown: Vec<String>,
}

impl ParticleDef {
    /// Parse a particle definition document.
    pub fn from_json(v: &Value) -> Result<Self, String> {
        let o = Obj::of(v);
        if !o.has("emitter") && !o.has("children") {
            return Err("particle has no emitter".into());
        }
        let mut unknown = Vec::new();
        let emitters = o
            .array("emitter")
            .iter()
            .filter_map(|e| EmitterDef::read(Obj::of(e), &mut unknown))
            .collect();
        let initializers = o
            .array("initializer")
            .iter()
            .filter_map(|e| Initializer::read(Obj::of(e), &mut unknown))
            .collect();
        let operators = o
            .array("operator")
            .iter()
            .filter_map(|e| Operator::read(Obj::of(e), &mut unknown))
            .collect();
        let renderer = match o.get("renderer") {
            None => Some(RendererDef::read(Obj::of(&Value::Null), &mut unknown)),
            Some(Value::Array(a)) => a
                .first()
                .map(|r| RendererDef::read(Obj::of(r), &mut unknown)),
            Some(_) => None,
        };
        let mut control_points = [ControlPointDef::default(); CONTROL_POINTS];
        for (i, cp) in o.array("controlpoint").iter().enumerate() {
            let c = Obj::of(cp);
            let slot = match c.i32("id", i as i32) {
                id @ 0..8 => id as usize,
                _ if i < CONTROL_POINTS => i,
                _ => continue,
            };
            control_points[slot] = ControlPointDef {
                offset: c.vec3("offset", Vec3::ZERO),
                null_offset: c.0.get("offset").is_some_and(Value::is_null),
                flags: c.u32("flags", 0),
                parent: c.i32("parentcontrolpoint", 0),
            };
        }
        let children = o
            .array("children")
            .iter()
            .filter_map(|c| ChildDef::read(Obj::of(c), &mut unknown))
            .collect();
        let animation_mode = match o.str("animationmode") {
            Some("randomframe") => AnimationMode::RandomFrame,
            _ => AnimationMode::Sequence,
        };
        Ok(Self {
            emitters,
            initializers,
            operators,
            renderer,
            control_points,
            material: o.str("material").map(str::to_owned),
            children,
            animation_mode,
            sequence_multiplier: o.f32("sequencemultiplier", 1.0),
            max_count: o.u32("maxcount", 1),
            start_time: o.f32("starttime", 0.0).max(0.0),
            flags: o.u32("flags", 0),
            unknown,
        })
    }
}

/// The scene object's `instanceoverride` block.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceOverride {
    pub alpha: f32,
    pub brightness: f32,
    /// Scales the particle budget and the emission rate.
    pub count: f32,
    pub lifetime: f32,
    /// Simulation speed multiplier.
    pub rate: f32,
    pub speed: f32,
    pub size: f32,
    /// Replacement colour (0..1).
    pub color: Option<Vec3>,
    /// Replacement control point offsets.
    pub control_points: [Option<Vec3>; CONTROL_POINTS],
}

impl Default for InstanceOverride {
    fn default() -> Self {
        Self {
            alpha: 1.0,
            brightness: 1.0,
            count: 1.0,
            lifetime: 1.0,
            rate: 1.0,
            speed: 1.0,
            size: 1.0,
            color: None,
            control_points: [None; CONTROL_POINTS],
        }
    }
}

impl InstanceOverride {
    /// Parse an `instanceoverride` object; anything else yields the identity override.
    pub fn from_json(v: Option<&Value>) -> Self {
        let Some(v) = v.filter(|v| v.is_object()) else {
            return Self::default();
        };
        let o = Obj::of(v);
        let color = if o.has("color") {
            o.vec3_opt("color").map(|c| c / 255.0)
        } else {
            o.vec3_opt("colorn")
        };
        let mut control_points = [None; CONTROL_POINTS];
        for (i, cp) in control_points.iter_mut().enumerate() {
            *cp = o.vec3_opt(&format!("controlpoint{i}"));
        }
        Self {
            alpha: o.f32("alpha", 1.0),
            brightness: o.f32("brightness", 1.0),
            count: o.f32("count", 1.0).max(0.0),
            lifetime: o.f32("lifetime", 1.0),
            rate: o.f32("rate", 1.0),
            speed: o.f32("speed", 1.0),
            size: o.f32("size", 1.0),
            color,
            control_points,
        }
    }

    /// Simulation time scale; non-positive overrides are ignored.
    pub fn time_scale(&self) -> f32 {
        if self.rate > 0.0 { self.rate } else { 1.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_core_entries_with_defaults() {
        let v = json!({
            "emitter": [{"name": "sphererandom", "distancemax": 10, "rate": "20"},
                        {"name": "boxrandom", "distancemax": "1 2 3", "flags": 2,
                         "maxperiodicdelay": 2, "maxperiodicduration": 1},
                        {"name": "conerandom"}],
            "initializer": [{"name": "colorrandom", "min": "255 0 0"},
                            {"name": "lifetimerandom", "min": 1, "max": 2, "exponent": 2},
                            {"name": "foo"}],
            "operator": [{"name": "movement", "gravity": "0 -100 0"},
                         {"name": "alphafade", "fadeintime": 0.1},
                         {"name": "remapvalue", "output": "setvelocity", "outputrangemax": "1 2 3"},
                         {"name": "bar"}],
            "renderer": [{"name": "spritetrail", "length": 0.01}],
            "controlpoint": [{"id": 1, "offset": "5 0 0", "flags": 1}, {"id": 2, "offset": null}],
            "material": "materials/particle/halo.json",
            "maxcount": 50, "starttime": 3, "flags": 5, "animationmode": "randomframe"
        });
        let d = ParticleDef::from_json(&v).unwrap();
        assert_eq!(d.emitters.len(), 2);
        assert_eq!(d.emitters[0].shape, EmitterShape::Sphere);
        assert_eq!(d.emitters[0].distance_max, Vec3::splat(10.0));
        assert_eq!(d.emitters[0].rate, 20.0);
        assert_eq!(d.emitters[0].directions, Vec3::new(1.0, 1.0, 0.0));
        assert_eq!(d.emitters[1].distance_max, Vec3::new(1.0, 2.0, 3.0));
        assert!(d.emitters[1].periodic.is_some());
        assert_eq!(d.emitters[1].flags & EMITTER_ONE_PER_FRAME, 2);
        assert_eq!(
            d.initializers[0],
            Initializer::ColorRandom(VecRange {
                min: Vec3::new(255.0, 0.0, 0.0),
                max: Vec3::new(255.0, 0.0, 0.0),
                exponent: 1.0
            })
        );
        assert_eq!(
            d.initializers[1],
            Initializer::LifetimeRandom(Range {
                min: 1.0,
                max: 2.0,
                exponent: 2.0
            })
        );
        assert!(matches!(
            d.operators[2],
            Operator::RemapValue {
                output: RemapOutput::Velocity,
                operation: RemapOperation::Set,
                input: RemapInput::Lifetime,
                ..
            }
        ));
        assert_eq!(d.renderer.unwrap().kind, RendererKind::SpriteTrail);
        assert_eq!(d.control_points[1].offset, Vec3::new(5.0, 0.0, 0.0));
        assert_eq!(d.control_points[1].flags, CP_LINK_MOUSE);
        assert!(d.control_points[2].null_offset);
        assert_eq!(d.animation_mode, AnimationMode::RandomFrame);
        assert_eq!(d.flags & FLAG_PERSPECTIVE, FLAG_PERSPECTIVE);
        assert_eq!(
            d.unknown,
            vec!["emitter:conerandom", "initializer:foo", "operator:bar"]
        );
    }

    #[test]
    fn renderer_key_presence_decides_spawner_only() {
        let base = json!({"emitter": [], "material": "m.json"});
        assert_eq!(
            ParticleDef::from_json(&base)
                .unwrap()
                .renderer
                .unwrap()
                .kind,
            RendererKind::Sprite
        );
        let mut empty = base.clone();
        empty["renderer"] = json!([]);
        assert!(ParticleDef::from_json(&empty).unwrap().renderer.is_none());
        let mut rope = base;
        rope["renderer"] = json!([{"name": "ropetrail", "segments": 6}]);
        let r = ParticleDef::from_json(&rope).unwrap().renderer.unwrap();
        assert_eq!(
            (r.kind, r.segments, r.length),
            (RendererKind::RopeTrail, 6, 1.0)
        );
    }

    #[test]
    fn instance_override_reads_multipliers_and_colors() {
        let o = InstanceOverride::from_json(Some(&json!({
            "alpha": 0.5, "size": {"user": "s", "value": 2}, "rate": 5, "count": 0.5,
            "colorn": "0 1 1", "controlpoint1": "10 20 0"
        })));
        assert_eq!((o.alpha, o.size, o.rate, o.count), (0.5, 2.0, 5.0, 0.5));
        assert_eq!(o.color, Some(Vec3::new(0.0, 1.0, 1.0)));
        assert_eq!(o.control_points[1], Some(Vec3::new(10.0, 20.0, 0.0)));
        assert_eq!(o.control_points[0], None);
        let c = InstanceOverride::from_json(Some(&json!({"color": "255 0 0"})));
        assert_eq!(c.color, Some(Vec3::new(1.0, 0.0, 0.0)));
        assert_eq!(
            InstanceOverride::from_json(None),
            InstanceOverride::default()
        );
        assert_eq!(
            InstanceOverride::from_json(Some(&json!({"rate": -1}))).time_scale(),
            1.0
        );
    }

    #[test]
    fn blend_window_and_value_change() {
        let bw = BlendWindow {
            in_start: 0.0,
            in_end: 0.5,
            out_start: 1.0,
            out_end: 1.0,
        };
        assert_eq!(bw.factor(0.25), 0.5);
        assert_eq!(bw.factor(0.75), 1.0);
        let vc = ValueChange {
            start_time: 0.0,
            end_time: 1.0,
            start: Vec3::ONE,
            end: Vec3::ZERO,
        };
        assert_eq!(vc.at(0.5), Vec3::splat(0.5));
        assert_eq!(vc.at(1.5), Vec3::ZERO);
    }
}
