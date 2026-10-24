use glam::{Mat4, Vec3};
use wpe_assets::value::Dynamic;

use crate::{Motion, UserProperties};

/// Lights the legacy `g_Lights*` arrays hold.
pub const MAX_LIGHTS: usize = 4;
/// Lights of one kind the per-kind `g_L<Kind>_*` arrays hold.
pub const MAX_LIGHTS_PER_KIND: usize = 4;
/// `LIGHTS_SHADOW_MAPPING_QUALITY` (1 to 4; picks the filtering and the
/// point light cube face border). Wallpaper Engine takes it from the user's
/// quality settings; this is a guess at its default.
pub const SHADOW_QUALITY: i32 = 3;
/// `cascadedistance0..2` when a light has none (the editor's values).
pub const DEFAULT_CASCADES: [f32; 3] = [700.0, 1500.0, 2800.0];
/// A light's `cookie` texture when it has `usecookie` but no `cookie`: the
/// string next to them in `wallpaper64.exe` (`materials/cookie/` holds
/// `flashlight1..4`).
pub const DEFAULT_COOKIE: &str = "cookie/flashlight1";
/// The texture name the lit shaders' cookie sampler defaults to
/// (`COOKIE_SAMPLER`, `"default":"_alias_lightCookie"`); it stands for the
/// scene's cookie light's texture.
pub const COOKIE_ALIAS: &str = "_alias_lightCookie";

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
    /// `castshadow`: point, spot and directional lights render shadow maps
    /// that lit materials sample.
    pub cast_shadow: bool,
    /// `castvolumetrics`: point and spot lights scatter light through the
    /// air of their volume.
    pub cast_volumetrics: bool,
    /// `density` of the volumetric scattering.
    pub density: f32,
    /// `volumetricsexponent`: falloff power of the scattering.
    pub volumetrics_exponent: f32,
    /// `cascadedistance0..2`: how far from the camera a directional light's
    /// shadow cascades reach.
    pub cascades: [f32; 3],
    /// `cookie` texture of a spot light with `usecookie`: projected through
    /// the cone, it colours the light instead of the cone's falloff.
    pub cookie: Option<String>,
}

impl SceneLight {
    /// Move the light to a new world transform.
    pub fn set_transform(&mut self, world: Mat4) {
        self.transform = world;
        self.position = world.w_axis.truncate();
        self.direction = world.transform_vector3(-Vec3::Z).normalize_or(-Vec3::Z);
    }

    /// Whether the light renders shadow maps (tube lights can't).
    pub fn casts_shadow(&self) -> bool {
        self.visible && self.cast_shadow && self.kind != LightKind::Tube
    }

    /// Whether the light gets a volumetric pass (point and spot lights).
    pub fn casts_volumetrics(&self) -> bool {
        self.visible
            && self.cast_volumetrics
            && self.density > 0.0
            && self.radius > 0.0
            && matches!(
                self.kind,
                LightKind::Point | LightKind::LPoint | LightKind::Spot
            )
    }

    /// Whether the light projects a cookie texture (spot lights only, like
    /// the lighting code's `spotcookie` / `spotshadowcookie` variants).
    pub fn has_cookie(&self) -> bool {
        self.visible && self.kind == LightKind::Spot && self.cookie.is_some()
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

/// Read a light object's shadow, volumetric and cookie settings
/// (`castshadow`, `castvolumetrics`, `density`, `volumetricsexponent`,
/// `cascadedistance0..2`, `usecookie`, `cookie`) from its otherwise
/// unparsed keys. Absent keys keep `light`'s values; Wallpaper Engine
/// leaves `castvolumetrics` out unless it is on.
pub fn apply_light_extras(
    light: &mut SceneLight,
    extra: &serde_json::Map<String, serde_json::Value>,
    props: &UserProperties,
) {
    let flag = |key: &str| {
        let d = Dynamic::<bool>::from_value(extra.get(key)?.clone()).ok()?;
        Some(props.flag(&d))
    };
    let number = |key: &str| {
        let d = Dynamic::<f32>::from_value(extra.get(key)?.clone()).ok()?;
        Some(props.value(&d))
    };
    if let Some(v) = flag("castshadow") {
        light.cast_shadow = v;
    }
    if let Some(v) = flag("castvolumetrics") {
        light.cast_volumetrics = v;
    }
    if let Some(v) = number("density") {
        light.density = v;
    }
    if let Some(v) = number("volumetricsexponent") {
        light.volumetrics_exponent = v;
    }
    for (i, cascade) in light.cascades.iter_mut().enumerate() {
        if let Some(v) = number(&format!("cascadedistance{i}")) {
            *cascade = v;
        }
    }
    if let Some(v) = flag("usecookie") {
        light.cookie = v.then(|| {
            let texture = extra.get("cookie").and_then(|c| {
                let d = Dynamic::<String>::from_value(c.clone()).ok()?;
                Some(props.value(&d))
            });
            texture
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| DEFAULT_COOKIE.to_owned())
        });
    }
}

/// The scene's cookie light: the first visible spot light with a cookie.
/// Wallpaper Engine allows one at a time (its editor: "Only {{limit}} spot
/// light with a projected texture can be used at the same time. Others will
/// be reverted to normal lights"), and the lit shaders have one
/// `COOKIE_SAMPLER`.
pub fn cookie_light(lights: &[SceneLight]) -> Option<&SceneLight> {
    lights.iter().find(|l| l.has_cookie())
}

/// The visible lights the per-kind arrays hold, by kind, in array order:
/// scene order with shadow-casting lights first (the lit shaders sample
/// shadow maps for the first `LIGHTS_<KIND>_SHADOW` lights of a kind).
/// Spot lights go: shadowed with the cookie (`LIGHTS_SPOT_SHADOW_COOKIE`),
/// other shadowed ones (together `LIGHTS_SPOT_SHADOW`), the unshadowed
/// cookie light (`LIGHTS_SPOT_COOKIE`), the rest; the shadowed and cookie
/// spots' projections lead `g_LFeature_ShadowProjection` in that order.
#[derive(Debug, Default)]
pub struct LightsByKind<'a> {
    /// `point` and `lpoint` lights.
    pub points: Vec<&'a SceneLight>,
    pub spots: Vec<&'a SceneLight>,
    pub tubes: Vec<&'a SceneLight>,
    pub directionals: Vec<&'a SceneLight>,
    /// Id of the cookie light ([`cookie_light`]), when it is among `spots`.
    pub cookie: Option<i32>,
}

impl<'a> LightsByKind<'a> {
    pub fn new(lights: &'a [SceneLight]) -> Self {
        let cookie = cookie_light(lights).map(|l| l.id);
        let of_kind = |kinds: &[LightKind]| -> Vec<&SceneLight> {
            let mut ls: Vec<&SceneLight> = lights
                .iter()
                .filter(|l| l.visible && kinds.contains(&l.kind))
                .collect();
            ls.sort_by_key(|l| (!l.casts_shadow(), Some(l.id) != cookie));
            ls.truncate(MAX_LIGHTS_PER_KIND);
            ls
        };
        let spots = of_kind(&[LightKind::Spot]);
        let cookie = cookie.filter(|id| spots.iter().any(|l| l.id == *id));
        Self {
            points: of_kind(&[LightKind::Point, LightKind::LPoint]),
            spots,
            tubes: of_kind(&[LightKind::Tube]),
            directionals: of_kind(&[LightKind::Directional]),
            cookie,
        }
    }

    /// The cookie light, when it is unshadowed (the shadowed one has its
    /// projection among the shadow maps').
    pub fn unshadowed_cookie(&self) -> Option<&'a SceneLight> {
        self.spots
            .iter()
            .copied()
            .find(|l| Some(l.id) == self.cookie && !l.casts_shadow())
    }

    /// The shadow-casting lights among `lights` (a kind's list).
    pub fn shadowed(lights: &[&'a SceneLight]) -> Vec<&'a SceneLight> {
        lights
            .iter()
            .copied()
            .filter(|l| l.casts_shadow())
            .collect()
    }

    /// Whether any light renders shadow maps.
    pub fn has_shadows(&self) -> bool {
        [&self.points, &self.spots, &self.directionals]
            .iter()
            .any(|ls| ls.iter().any(|l| l.casts_shadow()))
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
/// `LIGHTS_SHADOW_MAPPING` / `LIGHTS_COOKIE` combos. Shadow mapping is on
/// when some light casts shadows: then shadow-casting lights come first in
/// their kind's arrays (see [`LightsByKind`]), `LIGHTS_POINT_SHADOW`,
/// `LIGHTS_SPOT_SHADOW` and `LIGHTS_DIRECTIONAL_SHADOW` count them and
/// `LIGHTS_SHADOW_MAPPING_QUALITY` is [`SHADOW_QUALITY`]; the shadow maps'
/// own uniforms come from the renderer. With a cookie light
/// `LIGHTS_COOKIE` is 1 and `LIGHTS_SPOT_SHADOW_COOKIE` or
/// `LIGHTS_SPOT_COOKIE` (names from `wallpaper64.exe`) place it in the spot
/// arrays; its projection is the renderer's too.
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

    let by_kind = LightsByKind::new(lights);
    let pack = |ls: &[&SceneLight], f: &dyn Fn(&SceneLight) -> [f32; 4]| -> Vec<f32> {
        ls.iter().flat_map(|l| f(l)).collect()
    };
    let color = |l: &SceneLight| l.color_intensity().extend(l.radius).to_array();
    let cos = |deg: f32| deg.to_radians().cos();

    let (points, spots, tubes, dirs) = (
        &by_kind.points,
        &by_kind.spots,
        &by_kind.tubes,
        &by_kind.directionals,
    );
    let mut arrays: Vec<(&str, Vec<f32>)> = Vec::new();
    if !points.is_empty() {
        arrays.push(("g_LPoint_Color", pack(points, &color)));
        arrays.push((
            "g_LPoint_Origin",
            pack(points, &|l| l.position.extend(l.exponent).to_array()),
        ));
    }
    if !spots.is_empty() {
        arrays.push(("g_LSpot_Color", pack(spots, &color)));
        arrays.push((
            "g_LSpot_Origin",
            pack(spots, &|l| l.position.extend(cos(l.inner_cone)).to_array()),
        ));
        arrays.push((
            "g_LSpot_Direction",
            pack(spots, &|l| l.direction.extend(cos(l.outer_cone)).to_array()),
        ));
        arrays.push((
            "g_LSpot_Exponent",
            pack(spots, &|l| [l.exponent, 0.0, 0.0, 0.0]),
        ));
    }
    if !tubes.is_empty() {
        arrays.push(("g_LTube_Color", pack(tubes, &color)));
        arrays.push((
            "g_LTube_OriginA",
            pack(tubes, &|l| l.tube_segment().0.extend(l.exponent).to_array()),
        ));
        arrays.push((
            "g_LTube_OriginB",
            pack(tubes, &|l| l.tube_segment().1.extend(l.exponent).to_array()),
        ));
    }
    if !dirs.is_empty() {
        arrays.push((
            "g_LDirectional_Color",
            pack(dirs, &|l| l.color_intensity().extend(1.0).to_array()),
        ));
        arrays.push((
            "g_LDirectional_Direction",
            pack(dirs, &|l| (-l.direction).extend(0.0).to_array()),
        ));
    }
    out.uniforms
        .extend(arrays.into_iter().map(|(n, v)| (n.to_owned(), v)));

    let shadowed = |ls: &[&SceneLight]| LightsByKind::shadowed(ls).len() as i32;
    let shadows = by_kind.has_shadows();
    out.combos = [
        ("LIGHTS_POINT", points.len() as i32),
        ("LIGHTS_SPOT", spots.len() as i32),
        ("LIGHTS_TUBE", tubes.len() as i32),
        ("LIGHTS_DIRECTIONAL", dirs.len() as i32),
        ("LIGHTS_SHADOW_MAPPING", shadows as i32),
        ("LIGHTS_COOKIE", 0),
    ]
    .into_iter()
    .map(|(n, v)| (n.to_owned(), v))
    .collect();
    if let Some(id) = by_kind.cookie {
        let shadowed = spots.iter().any(|l| l.id == id && l.casts_shadow());
        out.combos.retain(|(n, _)| n != "LIGHTS_COOKIE");
        out.combos.extend(
            [
                ("LIGHTS_COOKIE", 1),
                ("LIGHTS_SPOT_SHADOW_COOKIE", shadowed as i32),
                ("LIGHTS_SPOT_COOKIE", !shadowed as i32),
            ]
            .map(|(n, v)| (n.to_owned(), v)),
        );
    }
    if shadows {
        out.combos.extend(
            [
                ("LIGHTS_SHADOW_MAPPING_QUALITY", SHADOW_QUALITY),
                ("LIGHTS_POINT_SHADOW", shadowed(points)),
                ("LIGHTS_SPOT_SHADOW", shadowed(spots)),
                ("LIGHTS_DIRECTIONAL_SHADOW", shadowed(dirs)),
            ]
            .map(|(n, v)| (n.to_owned(), v)),
        );
    }
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
            cast_shadow: false,
            cast_volumetrics: false,
            density: 1.0,
            volumetrics_exponent: 1.0,
            cascades: DEFAULT_CASCADES,
            cookie: None,
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
    fn shadow_casters_come_first() {
        let mut lights: Vec<SceneLight> = (0..3)
            .map(|i| light(LightKind::LPoint, Vec3::splat(i as f32)))
            .collect();
        let u = light_uniforms(&lights, Vec3::ZERO, Vec3::ZERO);
        assert_eq!(u.combo("LIGHTS_SHADOW_MAPPING"), Some(0));
        assert_eq!(u.combo("LIGHTS_POINT_SHADOW"), None);

        lights[2].cast_shadow = true;
        let mut tube = light(LightKind::Tube, Vec3::ZERO);
        tube.cast_shadow = true;
        lights.push(tube);
        let u = light_uniforms(&lights, Vec3::ZERO, Vec3::ZERO);
        assert_eq!(u.combo("LIGHTS_SHADOW_MAPPING"), Some(1));
        assert_eq!(u.combo("LIGHTS_POINT_SHADOW"), Some(1));
        assert_eq!(u.combo("LIGHTS_SPOT_SHADOW"), Some(0));
        assert_eq!(
            u.combo("LIGHTS_SHADOW_MAPPING_QUALITY"),
            Some(SHADOW_QUALITY)
        );
        // The shadow-casting light leads the point arrays; the legacy
        // arrays keep scene order.
        assert_eq!(
            &u.uniform("g_LPoint_Origin").unwrap()[..4],
            [2.0, 2.0, 2.0, 1.5]
        );
        assert_eq!(
            &u.uniform("g_LPoint_Origin").unwrap()[4..8],
            [0.0, 0.0, 0.0, 1.5]
        );
        assert_eq!(
            &u.uniform("g_LightsPosition").unwrap()[..4],
            [0.0, 0.0, 0.0, 1.5]
        );
    }

    #[test]
    fn shadow_and_volumetric_settings() {
        let mut l = light(LightKind::LPoint, Vec3::ZERO);
        let extra = serde_json::json!({
            "castshadow": true,
            "castvolumetrics": {"user": "fog", "value": false},
            "density": 2.5,
            "volumetricsexponent": 3,
            "cascadedistance1": 900.0,
        });
        let mut props = UserProperties::default();
        props.set("fog", serde_json::Value::from(true));
        apply_light_extras(&mut l, extra.as_object().unwrap(), &props);
        assert!(l.cast_shadow && l.cast_volumetrics);
        assert_eq!((l.density, l.volumetrics_exponent), (2.5, 3.0));
        assert_eq!(l.cascades, [700.0, 900.0, 2800.0]);
        assert!(l.casts_volumetrics() && l.casts_shadow());
    }

    #[test]
    fn cookie_lights() {
        let mut extra = serde_json::json!({"usecookie": true});
        let mut a = light(LightKind::Spot, Vec3::ZERO);
        apply_light_extras(
            &mut a,
            extra.as_object().unwrap(),
            &UserProperties::default(),
        );
        assert_eq!(a.cookie.as_deref(), Some(DEFAULT_COOKIE));
        extra["cookie"] = "cookie/flashlight3".into();
        let mut b = light(LightKind::Spot, Vec3::ONE);
        b.id = 1;
        apply_light_extras(
            &mut b,
            extra.as_object().unwrap(),
            &UserProperties::default(),
        );
        assert_eq!(b.cookie.as_deref(), Some("cookie/flashlight3"));
        let mut plain = light(LightKind::Spot, Vec3::splat(2.0));
        plain.id = 2;

        // Only the first cookie light counts; it leads the unshadowed spots.
        let lights = [plain.clone(), a.clone(), b.clone()];
        assert_eq!(cookie_light(&lights).map(|l| l.id), Some(0));
        let u = light_uniforms(&lights, Vec3::ZERO, Vec3::ZERO);
        assert_eq!(u.combo("LIGHTS_COOKIE"), Some(1));
        assert_eq!(u.combo("LIGHTS_SPOT_COOKIE"), Some(1));
        assert_eq!(u.combo("LIGHTS_SPOT_SHADOW_COOKIE"), Some(0));
        assert_eq!(&u.uniform("g_LSpot_Origin").unwrap()[..3], [0.0; 3]);
        assert_eq!(&u.uniform("g_LSpot_Origin").unwrap()[4..7], [2.0; 3]);
        assert_eq!(
            u.combos
                .iter()
                .filter(|(n, _)| n == "LIGHTS_COOKIE")
                .count(),
            1
        );

        // Shadowed spots come first, the shadowed cookie light leading.
        let mut a2 = a.clone();
        a2.cast_shadow = true;
        let mut shadowed = plain.clone();
        shadowed.cast_shadow = true;
        let lights = [shadowed, plain, a2];
        let by_kind = LightsByKind::new(&lights);
        assert_eq!(
            by_kind.spots.iter().map(|l| l.id).collect::<Vec<_>>(),
            [0, 2, 2]
        );
        assert!(by_kind.unshadowed_cookie().is_none());
        let u = light_uniforms(&lights, Vec3::ZERO, Vec3::ZERO);
        assert_eq!(u.combo("LIGHTS_SPOT_SHADOW_COOKIE"), Some(1));
        assert_eq!(u.combo("LIGHTS_SPOT_COOKIE"), Some(0));
        assert_eq!(u.combo("LIGHTS_SPOT_SHADOW"), Some(2));
    }

    #[test]
    fn kinds() {
        assert_eq!(LightKind::parse("lspot"), LightKind::Spot);
        assert_eq!(LightKind::parse("ldirectional"), LightKind::Directional);
        assert_eq!(LightKind::parse("weird"), LightKind::Point);
    }
}
