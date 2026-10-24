//! Material/combos/uniforms for a subsystem and CPU vertex generation.
//!
//! Sprites feed `genericparticle`'s non-geometry-shader path exactly as
//! Wallpaper Engine lays it out. Ribbons (`rope`, `ropetrail`) are expanded
//! on the CPU to the same geometry `genericropeparticle.geom` would emit,
//! then encoded so the rope vertex shader's non-GS path reproduces each
//! vertex verbatim (see [`encode_ribbon_vertex`]).

use std::collections::BTreeMap;

use glam::{Mat4, Vec3, Vec4};
use wpe_assets::Material;
use wpe_assets::tex::Texture;
use wpe_vfs::Vfs;

use crate::def::{AnimationMode, FLAG_NO_FRAME_BLENDING, ParticleDef, RendererDef, RendererKind};
use crate::ops::Particle;
use crate::system::Subsystem;
use crate::{ParticleMesh, ViewInfo};

/// Width given to the shader-side ribbon offset, which our encoding cancels out.
const RIBBON_EPSILON: f32 = 1e-4;

/// Everything needed to draw one subsystem.
#[derive(Debug, Clone)]
pub(crate) struct RenderSetup {
    pub kind: RendererKind,
    pub renderer: RendererDef,
    pub material_path: String,
    pub material: Option<Material>,
    pub shader: String,
    pub combos: BTreeMap<String, String>,
    pub uniforms: BTreeMap<String, Vec<f32>>,
    pub thick: bool,
    /// Frame count of the sprite sheet texture, if animated.
    pub sprite_frames: Option<u32>,
    pub animation: AnimationMode,
    pub sequence_multiplier: f32,
}

fn sprite_sheet(vfs: &Vfs, material: &Material, warnings: &mut Vec<String>) -> Option<Texture> {
    let name = material.passes.first()?.textures.first()?.as_deref()?;
    if name.starts_with("_rt_") {
        return None;
    }
    let path = format!("materials/{name}.tex");
    let bytes = vfs.read(&path).ok()?;
    match Texture::parse(&bytes) {
        Ok(t) if t.is_sprite() => Some(t),
        Ok(_) => None,
        Err(e) => {
            warnings.push(format!("{path}: {e}"));
            None
        }
    }
}

fn set(map: &mut BTreeMap<String, String>, k: &str, v: &str) {
    map.insert(k.to_owned(), v.to_owned());
}

/// Resolve material, shader, combos and uniforms for a renderable definition.
pub(crate) fn setup(
    vfs: &Vfs,
    def: &ParticleDef,
    renderer: RendererDef,
    warnings: &mut Vec<String>,
) -> RenderSetup {
    let material_path = def.material.clone().unwrap_or_default();
    let material = if material_path.is_empty() {
        warnings.push("particle has no material".into());
        None
    } else {
        match wpe_assets::load_json::<Material>(vfs, &material_path) {
            Ok(m) => Some(m),
            Err(e) => {
                warnings.push(format!("material: {e}"));
                None
            }
        }
    };
    let kind = renderer.kind;
    let rope = matches!(kind, RendererKind::Rope | RendererKind::RopeTrail);
    let mut combos = BTreeMap::new();
    if let Some(pass) = material.as_ref().and_then(|m| m.passes.first()) {
        for (k, v) in &pass.combos {
            combos.insert(k.clone(), v.0.to_string());
        }
    }
    set(&mut combos, "GS_ENABLED", "0");
    let mut uniforms: BTreeMap<String, Vec<f32>> = BTreeMap::new();
    uniforms.insert("g_OrientationUp".into(), vec![0.0, 1.0, 0.0]);
    uniforms.insert("g_OrientationRight".into(), vec![1.0, 0.0, 0.0]);
    uniforms.insert("g_OrientationForward".into(), vec![0.0, 0.0, 1.0]);
    uniforms.insert("g_ViewUp".into(), vec![0.0, 1.0, 0.0]);
    uniforms.insert("g_ViewRight".into(), vec![1.0, 0.0, 0.0]);

    let mut sprite_frames = None;
    if rope {
        set(&mut combos, "THICKFORMAT", "1");
        combos.remove("TRAILRENDERER");
        combos.remove("TRAILSCROLLALPHA");
        uniforms.insert("g_RenderVar0".into(), vec![1.0, 1.0, 0.0, 1.0]);
    } else {
        if def.flags & FLAG_NO_FRAME_BLENDING == 0 {
            set(&mut combos, "SPRITESHEETBLEND", "1");
        }
        if kind == RendererKind::SpriteTrail {
            set(&mut combos, "THICKFORMAT", "1");
            set(&mut combos, "TRAILRENDERER", "1");
            uniforms.insert(
                "g_RenderVar0".into(),
                vec![
                    renderer.length,
                    renderer.max_length,
                    renderer.min_length,
                    0.0,
                ],
            );
        }
        if let Some(tex) = material
            .as_ref()
            .and_then(|m| sprite_sheet(vfs, m, warnings))
        {
            let f = tex.sprite_frames[0];
            let n = tex.sprite_frames.len() as u32;
            sprite_frames = Some(n);
            set(&mut combos, "SPRITESHEET", "1");
            set(&mut combos, "THICKFORMAT", "1");
            uniforms.insert(
                "g_RenderVar1".into(),
                vec![f.x_axis[0], f.y_axis[1], n as f32, f.rate],
            );
            let (w, h) = (tex.width, tex.height);
            if w.is_power_of_two() && h.is_power_of_two() && w > 1 && h > 1 {
                set(&mut combos, "SPRITESHEETBLENDNPOT", "1");
                let fw = (f.width.round() as u32).max(1);
                let fh = (f.height.round() as u32).max(1);
                uniforms.insert(
                    "g_Texture0Resolution".into(),
                    vec![w as f32, h as f32, (w - w % fw) as f32, (h - h % fh) as f32],
                );
            }
        }
    }
    let thick = combos.get("THICKFORMAT").is_some_and(|v| v != "0");
    // Sprites draw with the material's own shader when it names one other
    // than the stock particle shaders: a project's legacy `particle` shader
    // reads the same vertex layout (and, say, tints from material colours).
    // Ribbons always need `genericropeparticle`, whose non-GS path our CPU
    // expansion is encoded for.
    let custom = material
        .as_ref()
        .and_then(|m| m.passes.first())
        .map(|p| p.shader.as_str())
        .filter(|s| !s.is_empty() && !matches!(*s, "genericparticle" | "genericropeparticle"));
    let shader = match (rope, custom) {
        (true, _) => "genericropeparticle".to_owned(),
        (false, Some(s)) => s.to_owned(),
        (false, None) => "genericparticle".to_owned(),
    };
    RenderSetup {
        kind,
        renderer,
        material_path,
        material,
        shader,
        combos,
        uniforms,
        thick,
        sprite_frames,
        animation: def.animation_mode,
        sequence_multiplier: def.sequence_multiplier,
    }
}

impl ParticleMesh {
    fn begin(&mut self, layout: &[(&str, u8)]) {
        let reuse = self.attributes.len() == layout.len()
            && self
                .attributes
                .iter()
                .zip(layout)
                .all(|(a, l)| a.0 == l.0 && a.1 == l.1);
        if reuse {
            for a in &mut self.attributes {
                a.2.clear();
            }
        } else {
            self.attributes = layout
                .iter()
                .map(|(n, c)| ((*n).to_owned(), *c, Vec::new()))
                .collect();
        }
        self.indices.clear();
        self.vertex_count = 0;
    }

    fn push_quad_indices(&mut self, base: u32) {
        self.indices
            .extend_from_slice(&[base, base + 1, base + 3, base + 1, base + 2, base + 3]);
    }
}

fn sheet_lifetime(r: &RenderSetup, p: &Particle) -> f32 {
    match (r.animation, r.sprite_frames) {
        (AnimationMode::RandomFrame, Some(n)) if n > 0 => {
            let k = (p.random(0xf4a3e) * n as f32).floor().min(n as f32 - 1.0);
            (k + 1e-3) / n as f32
        }
        (AnimationMode::RandomFrame, _) => p.random(0xf4a3e),
        (AnimationMode::Sequence, _) => p.age01() * r.sequence_multiplier,
    }
}

const SPRITE_UVS: [[f32; 2]; 4] = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];

fn sprite_mesh(sub: &Subsystem, r: &RenderSetup, out: &mut ParticleMesh) {
    let mut layout = vec![("a_Position", 3), ("a_TexCoordVec4", 4), ("a_Color", 4)];
    if r.thick {
        layout.push(("a_TexCoordVec4C1", 4));
    }
    layout.push(("a_TexCoordC2", 2));
    out.begin(&layout);
    let mut n = 0u32;
    for p in sub.particles() {
        let life = sheet_lifetime(r, p);
        let half = p.size / 2.0;
        let rz = p.rotation.z;
        let [pos, tc, col] = &mut out.attributes[..3] else {
            unreachable!()
        };
        for uv in SPRITE_UVS {
            pos.2.extend_from_slice(&p.position.to_array());
            tc.2.extend_from_slice(&[uv[0], uv[1], rz, half]);
            col.2
                .extend_from_slice(&[p.color.x, p.color.y, p.color.z, p.alpha]);
        }
        let mut next = 3;
        if r.thick {
            let c1 = &mut out.attributes[next].2;
            for _ in 0..4 {
                c1.extend_from_slice(&[p.velocity.x, p.velocity.y, p.velocity.z, life]);
            }
            next += 1;
        }
        let c2 = &mut out.attributes[next].2;
        for _ in 0..4 {
            c2.extend_from_slice(&[p.rotation.x, p.rotation.y]);
        }
        out.push_quad_indices(n * 4);
        n += 1;
    }
    out.vertex_count = n * 4;
}

/// One point a ribbon passes through.
#[derive(Debug, Clone, Copy)]
struct RibbonPoint {
    pos: Vec3,
    half_width: f32,
    color: Vec4,
    v: f32,
}

const RIBBON_LAYOUT: [(&str, u8); 7] = [
    ("a_PositionVec4", 4),
    ("a_TexCoordVec4", 4),
    ("a_TexCoordVec4C1", 4),
    ("a_TexCoordVec4C2", 4),
    ("a_TexCoordVec4C3", 4),
    ("a_TexCoordC4", 2),
    ("a_Color", 4),
];

/// Encode a final vertex for `genericropeparticle.vert` (non-GS, THICKFORMAT).
///
/// The shader computes `mix(start, end, uvs.y) + right * uvs.x * 2 - 1` with
/// `right = normalize(cross(eye, hint)) * size`. Setting start = end =
/// `pos + 1`, `uvs.y = 0` and a vanishing size places the vertex at `pos`;
/// a trail length of 2 with trail position `1 - v` makes the shader's V
/// coordinate exactly `v`. `hint` must not be parallel to the eye direction.
fn encode_ribbon_vertex(
    out: &mut ParticleMesh,
    pos: Vec3,
    u: f32,
    v: f32,
    color: Vec4,
    hint: Vec3,
) {
    let q = pos + Vec3::ONE;
    let a = &mut out.attributes;
    a[0].2.extend_from_slice(&[q.x, q.y, q.z, RIBBON_EPSILON]);
    a[1].2.extend_from_slice(&[q.x, q.y, q.z, 2.0]);
    let c1 = q - hint;
    a[2].2.extend_from_slice(&[c1.x, c1.y, c1.z, 1.0 - v]);
    let c2 = q + hint;
    a[3].2
        .extend_from_slice(&[c2.x, c2.y, c2.z, RIBBON_EPSILON]);
    a[4].2.extend_from_slice(&color.to_array());
    a[5].2.extend_from_slice(&[u, 0.0]);
    a[6].2.extend_from_slice(&color.to_array());
    out.vertex_count += 1;
}

fn bezier(a: Vec3, b: Vec3, c: Vec3, d: Vec3, t: f32) -> Vec3 {
    let u = 1.0 - t;
    a * (u * u * u) + b * (3.0 * t * u * u) + c * (3.0 * t * t * u) + d * (t * t * t)
}

/// Expand a polyline into the triangle ribbon `genericropeparticle.geom` emits.
fn ribbon(
    points: &[RibbonPoint],
    subdivision: u32,
    eye: &dyn Fn(Vec3) -> Vec3,
    out: &mut ParticleMesh,
) {
    let n = points.len();
    if n < 2 {
        return;
    }
    let steps = subdivision + 1;
    for k in 0..n - 1 {
        let (p0, p1) = (points[k], points[k + 1]);
        let delta = p1.pos - p0.pos;
        if delta.length_squared() < 1e-10 {
            continue;
        }
        let t0 = if k > 0 {
            (p1.pos - points[k - 1].pos) * 0.5
        } else {
            delta
        };
        let t1 = if k + 2 < n {
            (points[k + 2].pos - p0.pos) * 0.5
        } else {
            delta
        };
        let e = eye((p0.pos + p1.pos) * 0.5);
        let rs = e
            .cross(t0)
            .try_normalize()
            .or_else(|| e.cross(delta).try_normalize());
        let re = e
            .cross(t1)
            .try_normalize()
            .or_else(|| e.cross(delta).try_normalize());
        let (Some(rs), Some(re)) = (rs, re) else {
            continue;
        };
        let (rs, re) = (rs * p0.half_width, re * p1.half_width);
        let (b, c) = (p0.pos + t0 * 0.15, p1.pos - t1 * 0.15);
        let base = out.vertex_count;
        for j in 0..=steps {
            let x = j as f32 / steps as f32;
            let s = x * x * (3.0 - 2.0 * x);
            let pos = match j {
                0 => p0.pos,
                j if j == steps => p1.pos,
                _ => bezier(p0.pos, b, c, p1.pos, s),
            };
            let right = rs.lerp(re, s);
            let color = p0.color.lerp(p1.color, s);
            let v = p0.v + (p1.v - p0.v) * s;
            let hint = right.try_normalize().unwrap_or(Vec3::X);
            encode_ribbon_vertex(out, pos - right, 0.0, v, color, hint);
            encode_ribbon_vertex(out, pos + right, 1.0, v, color, hint);
        }
        for j in 0..steps {
            let a = base + j * 2;
            out.indices
                .extend_from_slice(&[a, a + 1, a + 3, a, a + 3, a + 2]);
        }
    }
}

fn rope_point(p: &Particle, v: f32) -> RibbonPoint {
    RibbonPoint {
        pos: p.position,
        half_width: p.size / 4.0,
        color: p.color.extend(p.alpha),
        v,
    }
}

fn ribbon_mesh(sub: &Subsystem, r: &RenderSetup, view: &ViewInfo, out: &mut ParticleMesh) {
    out.begin(&RIBBON_LAYOUT);
    let to_sim: Mat4 = sub.sim_to_world.inverse();
    let eye_pos = to_sim.transform_point3(view.eye);
    let forward = to_sim
        .transform_vector3(view.forward)
        .normalize_or(Vec3::NEG_Z);
    let ortho = view.orthographic;
    let eye = move |at: Vec3| {
        if ortho {
            forward
        } else {
            (at - eye_pos).normalize_or(forward)
        }
    };
    let mut pts = Vec::new();
    for inst in &sub.instances {
        match r.kind {
            RendererKind::Rope => {
                let alive: Vec<&Particle> = inst.particles.iter().filter(|p| p.alive()).collect();
                let n = alive.len();
                pts.clear();
                pts.extend(alive.iter().enumerate().map(|(i, p)| {
                    let v = if n > 1 {
                        1.0 - i as f32 / (n - 1) as f32
                    } else {
                        0.0
                    };
                    rope_point(p, v)
                }));
                ribbon(&pts, r.renderer.subdivision, &eye, out);
            }
            RendererKind::RopeTrail => {
                let length = r.renderer.length.max(1e-4);
                for (p, trail) in inst.particles.iter().zip(&inst.trails) {
                    if !p.alive() {
                        continue;
                    }
                    pts.clear();
                    pts.push(rope_point(p, 0.0));
                    let mut prev_age = 0.0f32;
                    let mut prev_pos = p.position;
                    for tp in &trail.points {
                        if tp.position.distance_squared(prev_pos) < 1e-8 && tp.age < 1e-6 {
                            continue;
                        }
                        let (pos, age, last) = if tp.age > length {
                            let f = (length - prev_age) / (tp.age - prev_age).max(1e-6);
                            (prev_pos.lerp(tp.position, f.clamp(0.0, 1.0)), length, true)
                        } else {
                            (tp.position, tp.age, false)
                        };
                        pts.push(RibbonPoint {
                            pos,
                            half_width: tp.size / 4.0,
                            color: tp.color.extend(p.alpha),
                            v: age / length,
                        });
                        prev_age = tp.age;
                        prev_pos = tp.position;
                        if last {
                            break;
                        }
                    }
                    if r.renderer.fade_alpha {
                        for q in &mut pts {
                            q.color.w *= 1.0 - q.v;
                        }
                    }
                    ribbon(&pts, r.renderer.subdivision, &eye, out);
                }
            }
            _ => {}
        }
    }
}

/// Fill `out` with `sub`'s vertex data for its renderer.
pub(crate) fn build_mesh(sub: &Subsystem, view: &ViewInfo, out: &mut ParticleMesh) {
    let Some(r) = &sub.render else {
        out.begin(&[]);
        return;
    };
    match r.kind {
        RendererKind::Sprite | RendererKind::SpriteTrail => sprite_mesh(sub, r, out),
        RendererKind::Rope | RendererKind::RopeTrail => ribbon_mesh(sub, r, view, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CPU replica of `genericropeparticle.vert`'s non-GS position and V math.
    fn shade(a: &[(String, u8, Vec<f32>)], i: usize, eye: Vec3) -> (Vec3, f32, f32) {
        let get = |k: usize, n: usize| -> Vec<f32> { a[k].2[i * n..i * n + n].to_vec() };
        let p = get(0, 4);
        let t = get(1, 4);
        let c1 = get(2, 4);
        let c2 = get(3, 4);
        let uv = get(5, 2);
        let start = Vec3::new(p[0], p[1], p[2]);
        let end = Vec3::new(t[0], t[1], t[2]);
        let cp_start = start - Vec3::new(c1[0], c1[1], c1[2]);
        let cp_end = end - Vec3::new(c2[0], c2[1], c2[2]);
        let delta = end - start;
        let rs = eye.cross(delta + cp_start).normalize() * p[3];
        let re = eye.cross(delta - cp_end).normalize() * c2[3];
        let pos = start.lerp(end, uv[1]);
        let right = rs.lerp(re, uv[1]);
        let pos = pos + right * uv[0] * 2.0 - Vec3::ONE;
        let usable = t[3] - 1.0;
        let v_min = 1.0 - c1[3] / usable;
        let v = v_min + (-1.0 / usable) * uv[1];
        (pos, uv[0], v)
    }

    #[test]
    fn ribbon_encoding_survives_the_rope_shader() {
        let mut out = ParticleMesh::default();
        out.begin(&RIBBON_LAYOUT);
        let pts = [
            RibbonPoint {
                pos: Vec3::new(0.0, 0.0, 0.0),
                half_width: 5.0,
                color: Vec4::ONE,
                v: 1.0,
            },
            RibbonPoint {
                pos: Vec3::new(100.0, 0.0, 0.0),
                half_width: 5.0,
                color: Vec4::ONE,
                v: 0.0,
            },
        ];
        let eye = Vec3::NEG_Z;
        ribbon(&pts, 0, &|_| eye, &mut out);
        assert_eq!(out.vertex_count, 4);
        assert_eq!(out.indices.len(), 6);
        let (p0, u0, v0) = shade(&out.attributes, 0, eye);
        let (p1, u1, _) = shade(&out.attributes, 1, eye);
        let (p3, _, v3) = shade(&out.attributes, 3, eye);
        assert!((p0.y.abs() - 5.0).abs() < 1e-2 && p0.x.abs() < 1e-2);
        assert!((p1.y + p0.y).abs() < 1e-2);
        assert!((p3.x - 100.0).abs() < 1e-2);
        assert_eq!((u0, u1), (0.0, 1.0));
        assert!((v0 - 1.0).abs() < 1e-5 && v3.abs() < 1e-5);
    }

    #[test]
    fn subdivision_adds_vertex_pairs() {
        let mut out = ParticleMesh::default();
        out.begin(&RIBBON_LAYOUT);
        let pts: Vec<RibbonPoint> = (0..3)
            .map(|i| RibbonPoint {
                pos: Vec3::new(i as f32 * 10.0, (i * i) as f32, 0.0),
                half_width: 1.0,
                color: Vec4::ONE,
                v: 0.0,
            })
            .collect();
        ribbon(&pts, 3, &|_| Vec3::NEG_Z, &mut out);
        assert_eq!(out.vertex_count, 2 * 5 * 2);
        assert_eq!(out.indices.len(), 2 * 4 * 6);
        assert!(out.indices.iter().all(|&i| i < out.vertex_count));
    }
}
