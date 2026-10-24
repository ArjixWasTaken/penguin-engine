use glam::{Mat4, Vec3};

use crate::Motion;

/// Lights the legacy `g_Lights*` arrays hold.
pub const MAX_LIGHTS: usize = 4;
/// Lights of one kind the per-kind `g_L<Kind>_*` arrays hold.
pub const MAX_LIGHTS_PER_KIND: usize = 4;

/// A light object's `light` string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LightKind {
    /// The legacy `point` light, and the fallback for unknown spellings.
    #[default]
    Point,
    LPoint,
    Spot,
    Tube,
    Directional,
}

impl LightKind {
    pub fn parse(s: &str) -> Self {
        match s {
            "lpoint" => Self::LPoint,
            "lspot" | "spot" => Self::Spot,
            "ltube" | "tube" => Self::Tube,
            "ldirectional" | "directional" => Self::Directional,
            _ => Self::Point,
        }
    }
}

/// A light resolved from a light object.
#[derive(Debug, Clone)]
pub struct SceneLight {
    pub id: i32,
    pub name: String,
    pub kind: LightKind,
    /// The light and all its ancestors are visible.
    pub visible: bool,
    /// World transform (parent chain applied).
    pub transform: Mat4,
    /// World position, `transform`'s translation.
    pub position: Vec3,
    /// Unit world direction the light shines in: its local -Z, so a light
    /// with no angles points down -Z like the camera does.
    pub direction: Vec3,
    pub color: Vec3,
    pub radius: f32,
    pub intensity: f32,
    /// Falloff power, `pow(saturate(1 - distance / radius), exponent)`.
    pub exponent: f32,
    /// Spot cone half-angles in degrees: full intensity inside `inner_cone`,
    /// fading out by `outer_cone`.
    pub inner_cone: f32,
    pub outer_cone: f32,
    pub parent: Option<i32>,
    /// What `transform` is made of, for animation.
    pub motion: Motion,
}

impl SceneLight {
    /// Move the light to a new world transform.
    pub fn set_transform(&mut self, world: Mat4) {
        self.transform = world;
        self.position = world.w_axis.truncate();
        self.direction = world.transform_vector3(-Vec3::Z).normalize_or(-Vec3::Z);
    }

    pub fn color_intensity(&self) -> Vec3 {
        self.color * self.intensity
    }

    /// `color · intensity · radius²`, the legacy shaders' light colour.
    pub fn premultiplied_color(&self) -> Vec3 {
        self.color_intensity() * self.radius * self.radius
    }

    /// End points of a tube light: its local X axis from -0.5 to 0.5, so the
    /// object's X scale is the tube length.
    pub fn tube_segment(&self) -> (Vec3, Vec3) {
        (
            self.transform.transform_point3(Vec3::new(-0.5, 0.0, 0.0)),
            self.transform.transform_point3(Vec3::new(0.5, 0.0, 0.0)),
        )
    }
}

/// Global uniform values and combos lit materials read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LightUniforms {
    /// Uniform name → packed floats (arrays of vec4 are flattened).
    pub uniforms: Vec<(String, Vec<f32>)>,
    /// Combo name → value, e.g. `LIGHTS_POINT` = number of point lights.
    pub combos: Vec<(String, i32)>,
}

impl LightUniforms {
    pub fn uniform(&self, name: &str) -> Option<&[f32]> {
        self.uniforms
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_slice())
    }

    pub fn combo(&self, name: &str) -> Option<i32> {
        self.combos.iter().find(|(n, _)| n == name).map(|(_, v)| *v)
    }
}

/// The light uniforms of Wallpaper Engine's lit shaders, from the visible
/// `lights` in scene order.
///
/// Legacy arrays (`generic*`, `genericimage2`, and `PerformLighting_V1`):
/// * `g_LightsPosition[4]`: world position, falloff exponent in `.w` (the
///   vertex shaders that declare `vec3[4]` read the same 16-byte stride);
/// * `g_LightsColorRadius[4]`: `color · intensity`, radius in `.w`;
/// * `g_LightsColorPremultiplied[3]`: `color · intensity · radius²` of the
///   first three lights, with the fourth light's in their `.w`s.
///
/// Per-kind arrays (`generic3`, `genericimage3`), sized by the `LIGHTS_POINT`,
/// `LIGHTS_SPOT`, `LIGHTS_TUBE` and `LIGHTS_DIRECTIONAL` combos and only
/// present when non-empty. Colours carry `color · intensity` with the radius in
/// `.w` (shader version 62+ scales by `.w²`):
/// * `g_LPoint_Color`, `g_LPoint_Origin` (exponent in `.w`);
/// * `g_LSpot_Color`, `g_LSpot_Origin` (cos inner cone in `.w`),
///   `g_LSpot_Direction` (the way it shines, cos outer cone in `.w`);
/// * `g_LTube_Color`, `g_LTube_OriginA`, `g_LTube_OriginB` (exponent in `.w`);
/// * `g_LDirectional_Color` (`.w` 1), `g_LDirectional_Direction` (towards the light).
///
/// Plus `g_LightAmbientColor` and `g_LightSkylightColor`, and the
/// `LIGHTS_SHADOW_MAPPING` / `LIGHTS_COOKIE` combos, always 0.
pub fn light_uniforms(lights: &[SceneLight], ambient: Vec3, skylight: Vec3) -> LightUniforms {
    let visible: Vec<&SceneLight> = lights.iter().filter(|l| l.visible).collect();

    let mut position = [0.0; MAX_LIGHTS * 4];
    let mut color_radius = [0.0; MAX_LIGHTS * 4];
    let mut premultiplied = [0.0; 3 * 4];
    for (i, l) in visible.iter().take(MAX_LIGHTS).enumerate() {
        position[i * 4..i * 4 + 4].copy_from_slice(&l.position.extend(l.exponent).to_array());
        color_radius[i * 4..i * 4 + 4]
            .copy_from_slice(&l.color_intensity().extend(l.radius).to_array());
        let c = l.premultiplied_color();
        if i < 3 {
            premultiplied[i * 4..i * 4 + 3].copy_from_slice(&c.to_array());
        } else {
            for (k, v) in c.to_array().into_iter().enumerate() {
                premultiplied[k * 4 + 3] = v;
            }
        }
    }

    let mut out = LightUniforms::default();
    let mut put = |name: &str, v: Vec<f32>| out.uniforms.push((name.to_owned(), v));
    put("g_LightsPosition", position.to_vec());
    put("g_LightsColorRadius", color_radius.to_vec());
    put("g_LightsColorPremultiplied", premultiplied.to_vec());
    put("g_LightAmbientColor", ambient.to_array().to_vec());
    put("g_LightSkylightColor", skylight.to_array().to_vec());

    let of_kind = |kinds: &[LightKind]| -> Vec<&SceneLight> {
        visible
            .iter()
            .copied()
            .filter(|l| kinds.contains(&l.kind))
            .take(MAX_LIGHTS_PER_KIND)
            .collect()
    };
    let pack = |ls: &[&SceneLight], f: &dyn Fn(&SceneLight) -> [f32; 4]| -> Vec<f32> {
        ls.iter().flat_map(|l| f(l)).collect()
    };
    let color = |l: &SceneLight| l.color_intensity().extend(l.radius).to_array();
    let cos = |deg: f32| deg.to_radians().cos();

    let points = of_kind(&[LightKind::Point, LightKind::LPoint]);
    let spots = of_kind(&[LightKind::Spot]);
    let tubes = of_kind(&[LightKind::Tube]);
    let dirs = of_kind(&[LightKind::Directional]);
    let mut arrays: Vec<(&str, Vec<f32>)> = Vec::new();
    if !points.is_empty() {
        arrays.push(("g_LPoint_Color", pack(&points, &color)));
        arrays.push((
            "g_LPoint_Origin",
            pack(&points, &|l| l.position.extend(l.exponent).to_array()),
        ));
    }
    if !spots.is_empty() {
        arrays.push(("g_LSpot_Color", pack(&spots, &color)));
        arrays.push((
            "g_LSpot_Origin",
            pack(&spots, &|l| l.position.extend(cos(l.inner_cone)).to_array()),
        ));
        arrays.push((
            "g_LSpot_Direction",
            pack(&spots, &|l| {
                l.direction.extend(cos(l.outer_cone)).to_array()
            }),
        ));
    }
    if !tubes.is_empty() {
        arrays.push(("g_LTube_Color", pack(&tubes, &color)));
        arrays.push((
            "g_LTube_OriginA",
            pack(&tubes, &|l| {
                l.tube_segment().0.extend(l.exponent).to_array()
            }),
        ));
        arrays.push((
            "g_LTube_OriginB",
            pack(&tubes, &|l| {
                l.tube_segment().1.extend(l.exponent).to_array()
            }),
        ));
    }
    if !dirs.is_empty() {
        arrays.push((
            "g_LDirectional_Color",
            pack(&dirs, &|l| l.color_intensity().extend(1.0).to_array()),
        ));
        arrays.push((
            "g_LDirectional_Direction",
            pack(&dirs, &|l| (-l.direction).extend(0.0).to_array()),
        ));
    }
    out.uniforms
        .extend(arrays.into_iter().map(|(n, v)| (n.to_owned(), v)));

    out.combos = [
        ("LIGHTS_POINT", points.len() as i32),
        ("LIGHTS_SPOT", spots.len() as i32),
        ("LIGHTS_TUBE", tubes.len() as i32),
        ("LIGHTS_DIRECTIONAL", dirs.len() as i32),
        ("LIGHTS_SHADOW_MAPPING", 0),
        ("LIGHTS_COOKIE", 0),
    ]
    .into_iter()
    .map(|(n, v)| (n.to_owned(), v))
    .collect();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    fn light(kind: LightKind, position: Vec3) -> SceneLight {
        let mut l = SceneLight {
            id: 0,
            name: String::new(),
            kind,
            visible: true,
            transform: Mat4::IDENTITY,
            position: Vec3::ZERO,
            direction: -Vec3::Z,
            color: Vec3::new(1.0, 0.5, 0.25),
            radius: 2.0,
            intensity: 3.0,
            exponent: 1.5,
            inner_cone: 60.0,
            outer_cone: 90.0,
            parent: None,
            motion: Motion::default(),
        };
        l.set_transform(Mat4::from_translation(position));
        l
    }

    #[test]
    fn legacy_arrays() {
        let mut lights: Vec<SceneLight> = (0..6)
            .map(|i| light(LightKind::LPoint, Vec3::splat(i as f32)))
            .collect();
        lights[1].visible = false;
        let u = light_uniforms(&lights, Vec3::splat(0.2), Vec3::splat(0.3));

        let pos = u.uniform("g_LightsPosition").unwrap();
        // The hidden light is skipped; only four fit.
        assert_eq!(
            pos,
            [
                0., 0., 0., 1.5, 2., 2., 2., 1.5, 3., 3., 3., 1.5, 4., 4., 4., 1.5
            ]
        );
        assert_eq!(
            &u.uniform("g_LightsColorRadius").unwrap()[..4],
            [3.0, 1.5, 0.75, 2.0]
        );
        // color · intensity · radius² = (12, 6, 3); the 4th light rides in .w.
        assert_eq!(
            u.uniform("g_LightsColorPremultiplied").unwrap(),
            [12., 6., 3., 12., 12., 6., 3., 6., 12., 6., 3., 3.]
        );
        assert_eq!(u.uniform("g_LightAmbientColor").unwrap(), [0.2; 3]);
        assert_eq!(u.uniform("g_LightSkylightColor").unwrap(), [0.3; 3]);
        assert_eq!(u.combo("LIGHTS_POINT"), Some(4));
        assert_eq!(u.combo("LIGHTS_SPOT"), Some(0));
        assert!(u.uniform("g_LSpot_Color").is_none());
    }

    #[test]
    fn per_kind_arrays() {
        let mut spot = light(LightKind::Spot, Vec3::new(0.0, 10.0, 0.0));
        // Pitch down: local -Z turns to -Y.
        spot.set_transform(
            Mat4::from_translation(Vec3::new(0.0, 10.0, 0.0)) * Mat4::from_rotation_x(-FRAC_PI_2),
        );
        let mut tube = light(LightKind::Tube, Vec3::ZERO);
        tube.set_transform(Mat4::from_scale(Vec3::new(10.0, 1.0, 1.0)));
        let sun = light(LightKind::Directional, Vec3::ZERO);
        let u = light_uniforms(
            &[spot, tube, sun, light(LightKind::Point, Vec3::ONE)],
            Vec3::ZERO,
            Vec3::ZERO,
        );

        let dir = u.uniform("g_LSpot_Direction").unwrap();
        assert!((Vec3::from_slice(dir) - Vec3::new(0.0, -1.0, 0.0)).length() < 1e-6);
        assert!(dir[3].abs() < 1e-6); // cos 90°
        assert!((u.uniform("g_LSpot_Origin").unwrap()[3] - 0.5).abs() < 1e-6);
        assert_eq!(u.uniform("g_LTube_OriginA").unwrap()[..3], [-5.0, 0.0, 0.0]);
        assert_eq!(u.uniform("g_LTube_OriginB").unwrap()[..3], [5.0, 0.0, 0.0]);
        assert_eq!(
            u.uniform("g_LDirectional_Direction").unwrap(),
            [0.0, 0.0, 1.0, 0.0]
        );
        assert_eq!(
            u.uniform("g_LDirectional_Color").unwrap(),
            [3.0, 1.5, 0.75, 1.0]
        );
        assert_eq!(u.uniform("g_LPoint_Origin").unwrap(), [1.0, 1.0, 1.0, 1.5]);
        for kind in [
            "LIGHTS_POINT",
            "LIGHTS_SPOT",
            "LIGHTS_TUBE",
            "LIGHTS_DIRECTIONAL",
        ] {
            assert_eq!(u.combo(kind), Some(1), "{kind}");
        }
        assert_eq!(u.combo("LIGHTS_SHADOW_MAPPING"), Some(0));
    }

    #[test]
    fn kinds() {
        assert_eq!(LightKind::parse("lspot"), LightKind::Spot);
        assert_eq!(LightKind::parse("ldirectional"), LightKind::Directional);
        assert_eq!(LightKind::parse("weird"), LightKind::Point);
    }
}
