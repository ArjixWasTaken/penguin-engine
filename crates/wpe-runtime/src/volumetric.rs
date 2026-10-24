//! Volumetric lights: light scattered by the air inside point and spot
//! lights' volumes, after the reference renderer's `VolumetricChain`
//! (wallpaper-scene-renderer, GPL-2.0) and Wallpaper Engine's
//! `materials/util/volumetrics_*.json`.
//!
//! Per light, a proxy sphere of the light's radius is drawn twice:
//! `volumetricsback` records its far side's depth (back faces into the
//! depth buffer of a half-size scratch target), then `volumetricsfront`
//! ray-marches from its near side to the far side or the scene's depth,
//! whichever is nearer, adding the scattered light into the half-size
//! `_rt_volumetricsLightBuffer`. When the camera is inside the sphere a
//! fullscreen quad (`FULLSCREEN`) marches from the camera instead. The
//! buffer is blurred with `blur_k3` both ways and added over the frame
//! with `passthrough`.
//!
//! Shadow-casting lights march through their shadow maps (`SHADOW`): point
//! lights through their cube tile, spot lights through their projection
//! (`g_AltModelMatrix`) and tile (`g_RenderVar0`). The cookie spot light
//! (`COOKIE`) colours its scattering with the cookie (`g_Texture2`) seen
//! through the same projection.

use std::collections::{BTreeMap, HashMap};

use glam::{Mat4, Vec3};
use wpe_assets::material::{Blending, MaterialPass};
use wpe_render::{
    BlendMode, Camera, CardMesh, CullMode, Mesh, MeshData, RasterState, SceneRenderer,
    ShadowLights, SpotShadow, TargetId, TargetSize, TexRef, point_projection_info, spot_view_proj,
    wgpu,
};
use wpe_scene::{LightKind, LightsByKind, SceneLight};

use super::FrameBuilder;

/// The shadow atlas' render target name in shader annotations.
pub(super) const SHADOW_ATLAS: &str = "_rt_shadowAtlas";
const LIGHT_BUFFER: &str = "_rt_volumetricsLightBuffer";
const LIGHT_BUFFER_B: &str = "_rt_volumetricsLightBufferB";

/// One light's volumetric draws.
#[derive(Debug, Clone)]
struct VolumetricLight {
    /// The light object's id.
    id: i32,
    back: usize,
    front: usize,
    fullscreen: usize,
    /// Index among the shadow-casting lights of its kind (point or spot),
    /// when its scattering is shadowed.
    shadow: Option<usize>,
    /// The scene's cookie light: its scattering samples the cookie.
    cookie: bool,
    /// Centre and radius of its proxy sphere, as last updated.
    sphere: (Vec3, f32),
}

/// The volumetric draws of a frame and what they need each frame.
#[derive(Debug, Clone, Default)]
pub struct Volumetrics {
    lights: Vec<VolumetricLight>,
}

/// Subdivided icosahedron enclosing the unit sphere, counter-clockwise
/// seen from outside.
fn proxy_sphere() -> MeshData {
    let t = (1.0 + 5f32.sqrt()) / 2.0;
    let mut vertices: Vec<Vec3> = [
        [-1.0, t, 0.0],
        [1.0, t, 0.0],
        [-1.0, -t, 0.0],
        [1.0, -t, 0.0],
        [0.0, -1.0, t],
        [0.0, 1.0, t],
        [0.0, -1.0, -t],
        [0.0, 1.0, -t],
        [t, 0.0, -1.0],
        [t, 0.0, 1.0],
        [-t, 0.0, -1.0],
        [-t, 0.0, 1.0],
    ]
    .iter()
    .map(|v| Vec3::from_array(*v).normalize())
    .collect();
    let mut faces: Vec<[u32; 3]> = vec![
        [0, 11, 5],
        [0, 5, 1],
        [0, 1, 7],
        [0, 7, 10],
        [0, 10, 11],
        [1, 5, 9],
        [5, 11, 4],
        [11, 10, 2],
        [10, 7, 6],
        [7, 1, 8],
        [3, 9, 4],
        [3, 4, 2],
        [3, 2, 6],
        [3, 6, 8],
        [3, 8, 9],
        [4, 9, 5],
        [2, 4, 11],
        [6, 2, 10],
        [8, 6, 7],
        [9, 8, 1],
    ];
    for _ in 0..2 {
        let mut midpoints: HashMap<(u32, u32), u32> = HashMap::new();
        let mut mid = |a: u32, b: u32, vertices: &mut Vec<Vec3>| {
            *midpoints.entry((a.min(b), a.max(b))).or_insert_with(|| {
                let m = (vertices[a as usize] + vertices[b as usize]).normalize();
                vertices.push(m);
                vertices.len() as u32 - 1
            })
        };
        faces = faces
            .iter()
            .flat_map(|&[a, b, c]| {
                let ab = mid(a, b, &mut vertices);
                let bc = mid(b, c, &mut vertices);
                let ca = mid(c, a, &mut vertices);
                [[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]
            })
            .collect();
    }
    // Push the faces out until the nearest one touches the unit sphere.
    let inner = faces
        .iter()
        .map(|&[a, b, c]| {
            let (a, b, c) = (
                vertices[a as usize],
                vertices[b as usize],
                vertices[c as usize],
            );
            (b - a).cross(c - a).normalize().dot(a).abs()
        })
        .fold(1.0_f32, f32::min);
    MeshData {
        vertex_count: vertices.len() as u32,
        attributes: vec![(
            "a_Position".to_owned(),
            3,
            vertices
                .iter()
                .flat_map(|v| (*v / inner).to_array())
                .collect(),
        )],
        indices: Some(faces.into_iter().flatten().collect()),
    }
}

/// `QUALITY` of the volumetric passes from the scene's post-processing
/// preset, as the reference renderer picks it.
fn quality(postprocessing: Option<&str>) -> u32 {
    match postprocessing {
        Some("ultra" | "displayhdr") => 4,
        Some("low") => 1,
        _ => 2,
    }
}

/// `volumetricsfront` maps a spot light's projection to UVs with
/// `xy · 0.525 + 0.5` ("Increase UV to hide border inconsistencies") where
/// the lit shaders use `xy · 0.5 + 0.5`. Shrinking the projection's x and
/// y by this makes the scattering sample the same texels the lit surfaces
/// do. Whether Wallpaper Engine compensates likewise is a guess.
const SPOT_UV_SCALE: f32 = 0.5 / 0.525;

/// The `g_AltModelMatrix` of a shadowed or cookie spot light's scattering.
fn spot_volume_projection(l: &SceneLight) -> Mat4 {
    Mat4::from_scale(Vec3::new(SPOT_UV_SCALE, SPOT_UV_SCALE, 1.0))
        * spot_view_proj(&SpotShadow {
            origin: l.position,
            direction: l.direction,
            outer_cone: l.outer_cone,
            radius: l.radius,
        })
}

impl FrameBuilder<'_> {
    /// Whether the scene's lit materials sample shadow maps.
    pub(super) fn shadow_mapping(&self) -> bool {
        self.light_combos
            .get("LIGHTS_SHADOW_MAPPING")
            .is_some_and(|v| v == "1")
    }

    /// Whether the scene's lit materials sample a light cookie.
    pub(super) fn light_cookie(&self) -> bool {
        self.light_combos
            .get("LIGHTS_COOKIE")
            .is_some_and(|v| v == "1")
    }

    /// Add the volumetric passes of `lights` that cast volumetrics, after
    /// the scene's objects. `postprocessing` is the scene's preset.
    pub fn volumetrics(
        &mut self,
        lights: &[SceneLight],
        postprocessing: Option<&str>,
    ) -> Volumetrics {
        let mut out = Volumetrics::default();
        let casting: Vec<&SceneLight> = lights.iter().filter(|l| l.casts_volumetrics()).collect();
        if casting.is_empty() {
            return out;
        }
        let half = TargetSize::Screen { divisor: 2.0 };
        // Scattering adds up over lights: float headroom even in LDR scenes.
        let float = wgpu::TextureFormat::Rgba16Float;
        let buffer = self.target(LIGHT_BUFFER.to_owned(), half, float);
        let buffer_b = self.target(LIGHT_BUFFER_B.to_owned(), half, float);
        let camera = if self.is_3d {
            Camera::Perspective
        } else {
            Camera::Scene
        };
        let by_kind = LightsByKind::new(lights);
        let shadowed_points = LightsByKind::shadowed(&by_kind.points);
        let shadowed_spots = LightsByKind::shadowed(&by_kind.spots);
        let cookie_texture = wpe_scene::cookie_light(lights)
            .and_then(|l| l.cookie.as_deref())
            .and_then(|name| self.cache.store.get(self.vfs, name))
            .map(|t| self.upload(&t, "volumetrics cookie"));
        let sphere = proxy_sphere();
        let quality = quality(postprocessing).to_string();

        for (i, light) in casting.into_iter().enumerate() {
            let point = light.kind != LightKind::Spot;
            let shadowed = if point {
                &shadowed_points
            } else {
                &shadowed_spots
            };
            let shadow = shadowed
                .iter()
                .position(|l| l.id == light.id)
                .filter(|_| self.shadow_mapping());
            let cookie = cookie_texture
                .clone()
                .filter(|_| !point && by_kind.cookie == Some(light.id));
            // The back faces' depth lands in this target's depth buffer.
            let back_target = self.target(
                format!("_rt_volumetricsBack{i}"),
                half,
                wgpu::TextureFormat::R8Unorm,
            );
            let pass = |shader: &str, blending| MaterialPass {
                shader: shader.to_owned(),
                blending,
                ..Default::default()
            };
            let label = format!("volumetrics light {}", light.id);
            let Some(back) = self.resolve_with(
                &label,
                &pass("volumetricsback", Blending::Normal),
                &HashMap::new(),
                &BTreeMap::new(),
            ) else {
                continue;
            };
            let mut fronts = Vec::new();
            for fullscreen in [false, true] {
                let mut combos: BTreeMap<String, String> = [
                    ("QUALITY", quality.as_str()),
                    ("POINTLIGHT", if point { "1" } else { "0" }),
                    ("FULLSCREEN", if fullscreen { "1" } else { "0" }),
                    ("SHADOW", if shadow.is_some() { "1" } else { "0" }),
                    ("REVERSEDEPTH", "0"),
                ]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect();
                // Tested with `#ifdef`: only defined for the cookie light.
                if cookie.is_some() {
                    combos.insert("COOKIE".to_owned(), "1".to_owned());
                }
                let Some(mut r) = self.resolve_with(
                    &label,
                    &pass("volumetricsfront", Blending::Additive),
                    &HashMap::new(),
                    &combos,
                ) else {
                    break;
                };
                if r.textures.len() < 4 {
                    r.textures.resize(4, None);
                }
                if shadow.is_some() {
                    r.textures[0] = Some(TexRef::ShadowAtlas);
                }
                r.textures[1] = Some(TexRef::Depth(back_target));
                if let Some(cookie) = &cookie {
                    r.textures[2] = Some(TexRef::Asset(cookie.clone()));
                }
                r.textures[3] = Some(TexRef::Depth(TargetId::DEFAULT));
                fronts.push(r);
            }
            let mut fronts = fronts.into_iter();
            let (Some(front), Some(full)) = (fronts.next(), fronts.next()) else {
                continue;
            };

            let flat = RasterState::default();
            let back_step = self.steps.len();
            self.push_draw_with(
                back_target,
                back,
                Mat4::IDENTITY,
                Mesh::Triangles(sphere.clone()),
                BlendMode::Normal,
                camera,
                RasterState {
                    depth_test: true,
                    depth_write: true,
                    // The sphere's far side.
                    cull: CullMode::Front,
                },
            );
            let front_step = self.steps.len();
            self.push_draw_with(
                buffer,
                front,
                Mat4::IDENTITY,
                Mesh::Triangles(sphere.clone()),
                BlendMode::Additive,
                camera,
                RasterState {
                    cull: CullMode::Back,
                    ..flat
                },
            );
            let fullscreen_step = self.steps.len();
            self.push_draw_with(
                buffer,
                full,
                Mat4::IDENTITY,
                CardMesh::FULLSCREEN,
                BlendMode::Additive,
                camera,
                flat,
            );
            out.lights.push(VolumetricLight {
                id: light.id,
                back: back_step,
                front: front_step,
                fullscreen: fullscreen_step,
                shadow,
                cookie: cookie.is_some(),
                sphere: (light.position, light.radius),
            });
        }
        if out.lights.is_empty() {
            return out;
        }

        let names = HashMap::from([
            (LIGHT_BUFFER.to_owned(), buffer),
            (LIGHT_BUFFER_B.to_owned(), buffer_b),
        ]);
        for (input, vertical, target) in
            [(LIGHT_BUFFER, "0", buffer_b), (LIGHT_BUFFER_B, "1", buffer)]
        {
            let pass = MaterialPass {
                shader: "blur_k3".to_owned(),
                blending: Blending::Normal,
                textures: vec![Some(input.to_owned())],
                ..Default::default()
            };
            let combos = BTreeMap::from([("VERTICAL".to_owned(), vertical.to_owned())]);
            let Some(r) = self.resolve_with("volumetrics blur", &pass, &names, &combos) else {
                tracing::warn!("volumetrics disabled: blur_k3 failed");
                return Volumetrics::default();
            };
            self.push_draw(
                target,
                r,
                Mat4::IDENTITY,
                CardMesh::FULLSCREEN,
                BlendMode::Normal,
                Camera::Fixed(Mat4::IDENTITY),
            );
        }
        let pass = MaterialPass {
            shader: "passthrough".to_owned(),
            blending: Blending::Additive,
            textures: vec![Some(LIGHT_BUFFER.to_owned())],
            ..Default::default()
        };
        let Some(r) = self.resolve("volumetrics combine", &pass, &names) else {
            tracing::warn!("volumetrics disabled: passthrough failed");
            return Volumetrics::default();
        };
        self.push_draw(
            TargetId::DEFAULT,
            r,
            Mat4::IDENTITY,
            CardMesh::FULLSCREEN,
            BlendMode::Additive,
            Camera::Fixed(Mat4::IDENTITY),
        );
        out
    }
}

/// Grows the proxy sphere's reach a little past the light's radius when
/// deciding whether the camera is inside.
const INSIDE_MARGIN: f32 = 1.02;

impl Volumetrics {
    pub fn is_empty(&self) -> bool {
        self.lights.is_empty()
    }

    /// Place the proxy spheres and write the per-light uniforms
    /// (`g_RenderVar0..4`) from the lights' current state. `shadows` are
    /// the shadow lights the renderer has, for shadowed scattering.
    pub fn update(
        &mut self,
        renderer: &mut SceneRenderer,
        lights: &[SceneLight],
        shadows: &ShadowLights,
    ) {
        let layout = shadows.layout();
        for v in &mut self.lights {
            let Some(l) = lights.iter().find(|l| l.id == v.id) else {
                continue;
            };
            v.sphere = (l.position, l.radius);
            let model =
                Mat4::from_translation(l.position) * Mat4::from_scale(Vec3::splat(l.radius));
            let cols = model.to_cols_array().to_vec();
            for step in [v.back, v.front] {
                renderer.set_model(step, model);
                // The volumetric shaders place their geometry with it.
                renderer.set_uniform(step, "g_AltViewProjectionMatrix", cols.clone());
            }
            let point = l.kind != LightKind::Spot;
            let shadow = v.shadow.zip(layout.as_ref());
            let cos = |deg: f32| deg.to_radians().cos();
            if !point && (v.shadow.is_some() || v.cookie) {
                let projection = spot_volume_projection(l).to_cols_array().to_vec();
                for step in [v.front, v.fullscreen] {
                    renderer.set_uniform(step, "g_AltModelMatrix", projection.clone());
                }
            }
            let vars: [[f32; 4]; 5] = [
                match shadow {
                    Some((k, layout)) if point => layout.point_transform(k),
                    // Shadowed spots' tiles follow the shadowed points'.
                    Some((k, layout)) => layout.spot_transform(shadows.points.len() + k),
                    None => [0.0; 4],
                },
                if point {
                    [l.radius, 0.0, 0.0, l.intensity]
                } else {
                    [l.radius, cos(l.inner_cone), cos(l.outer_cone), l.intensity]
                },
                l.position.extend(l.density).to_array(),
                match (point, shadow) {
                    (true, Some(_)) => point_projection_info(l.radius),
                    (true, None) => [0.0, 0.0, 1.0, 0.0],
                    (false, _) => l.direction.extend(0.0).to_array(),
                },
                // The reference renderer passes `color · intensity` here,
                // but the shader already scales by the intensity in
                // `g_RenderVar1.w`; counting it twice blows out any light
                // brighter than 1, so this is the plain colour (a guess).
                l.color.extend(l.volumetrics_exponent).to_array(),
            ];
            for step in [v.front, v.fullscreen] {
                for (n, var) in vars.iter().enumerate() {
                    renderer.set_uniform(step, &format!("g_RenderVar{n}"), var.to_vec());
                }
            }
        }
    }

    /// Pick each light's near-side pass or, with the camera (`eye`, in 3D
    /// scenes) inside its sphere, the fullscreen one.
    pub fn select(&self, renderer: &mut SceneRenderer, eye: Option<Vec3>) {
        for v in &self.lights {
            let (center, radius) = v.sphere;
            let inside = eye.is_some_and(|e| e.distance(center) < radius * INSIDE_MARGIN);
            renderer.set_visible(v.front, !inside);
            renderer.set_visible(v.fullscreen, inside);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_sphere_encloses_the_unit_sphere() {
        let m = proxy_sphere();
        let positions = &m.attributes[0].2;
        let vertices: Vec<Vec3> = positions.chunks_exact(3).map(Vec3::from_slice).collect();
        let indices = m.indices.unwrap();
        assert_eq!(indices.len(), 320 * 3);
        for f in indices.chunks_exact(3) {
            let [a, b, c] = [0, 1, 2].map(|i| vertices[f[i] as usize]);
            let normal = (b - a).cross(c - a).normalize();
            // Counter-clockwise from outside, and no closer than 1.
            assert!(normal.dot(a) >= 1.0 - 1e-4, "{}", normal.dot(a));
        }
        assert!(vertices.iter().all(|v| v.length() < 1.05));
    }

    #[test]
    fn quality_follows_the_preset() {
        assert_eq!(quality(Some("ultra")), 4);
        assert_eq!(quality(Some("low")), 1);
        assert_eq!(quality(None), 2);
    }
}
