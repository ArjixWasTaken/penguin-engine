//! Shadow-casting lights and objects, and the volumetric passes, kept in
//! step with the scene's lights.

use std::collections::HashMap;

use wpe_assets::value::Dynamic;
use wpe_render::{
    DirectionalShadow, FrameGlobals, PointShadow, SceneRenderer, ShadowLights, SpotShadow,
};
use wpe_scene::{LightsByKind, SHADOW_QUALITY, SceneLight, UserProperties};

use crate::frame::{LayerSteps, Volumetrics};

/// The shadow maps `lights` need, in the order the lit shaders index them
/// (see [`LightsByKind`]). `perspective`: the scene is 3D, so directional
/// cascades follow its perspective camera.
pub(crate) fn shadow_lights(lights: &[SceneLight], perspective: bool) -> ShadowLights {
    let by_kind = LightsByKind::new(lights);
    ShadowLights {
        points: LightsByKind::shadowed(&by_kind.points)
            .iter()
            .map(|l| PointShadow {
                origin: l.position,
                radius: l.radius,
            })
            .collect(),
        spots: LightsByKind::shadowed(&by_kind.spots)
            .iter()
            .map(|l| SpotShadow {
                origin: l.position,
                direction: l.direction,
                outer_cone: l.outer_cone,
                radius: l.radius,
            })
            .collect(),
        directionals: LightsByKind::shadowed(&by_kind.directionals)
            .iter()
            .map(|l| DirectionalShadow {
                direction: l.direction,
                cascades: l.cascades,
            })
            .collect(),
        quality: SHADOW_QUALITY as u32,
        perspective,
    }
}

/// Draws of the image and model objects with `castshadow` set: their
/// draws placed with the object's transform.
pub(crate) fn casters(
    objects: &[serde_json::Value],
    props: &UserProperties,
    steps: &HashMap<i32, LayerSteps>,
) -> Vec<usize> {
    let mut casters: Vec<usize> = objects
        .iter()
        .filter(|o| o.get("light").is_none())
        .filter(|o| {
            o.get("castshadow")
                .and_then(|v| Dynamic::<bool>::from_value(v.clone()).ok())
                .is_some_and(|d| props.flag(&d))
        })
        .filter_map(|o| o.get("id")?.as_i64())
        .filter_map(|id| steps.get(&(id as i32)))
        .flat_map(|s| s.model.iter().copied())
        .collect();
    casters.sort_unstable();
    casters.dedup();
    casters
}

/// The frame's shadow lights and volumetric passes, refreshed when lights
/// move.
#[derive(Default)]
pub(crate) struct LightEffects {
    volumetrics: Volumetrics,
    /// 3D scene: perspective camera.
    perspective: bool,
}

impl LightEffects {
    pub fn new(volumetrics: Volumetrics, perspective: bool) -> Self {
        Self {
            volumetrics,
            perspective,
        }
    }

    /// Hand the renderer the lights' current shadow maps and volumetric
    /// uniforms.
    pub fn update(&mut self, renderer: &mut SceneRenderer, lights: &[SceneLight]) {
        let shadows = shadow_lights(lights, self.perspective);
        self.volumetrics.update(renderer, lights, &shadows);
        renderer.set_shadow_lights(shadows);
    }

    /// Per-frame choices that follow the camera.
    pub fn select(&self, renderer: &mut SceneRenderer, globals: &FrameGlobals) {
        if !self.volumetrics.is_empty() {
            let eye = self.perspective.then_some(globals.perspective_eye);
            self.volumetrics.select(renderer, eye);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn casters_are_flagged_objects_model_draws() {
        let objects: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"id": 1, "image": "a", "castshadow": true},
                {"id": 2, "model": "b", "castshadow": {"user": "s", "value": false}},
                {"id": 3, "image": "c"},
                {"id": 4, "light": "lpoint", "castshadow": true}]"#,
        )
        .unwrap();
        let steps = HashMap::from([
            (
                1,
                LayerSteps {
                    model: vec![5],
                    ..Default::default()
                },
            ),
            (
                2,
                LayerSteps {
                    model: vec![7, 8],
                    ..Default::default()
                },
            ),
            (
                3,
                LayerSteps {
                    model: vec![9],
                    ..Default::default()
                },
            ),
        ]);
        let mut props = UserProperties::default();
        assert_eq!(casters(&objects, &props, &steps), [5]);
        props.set("s", serde_json::Value::from(true));
        assert_eq!(casters(&objects, &props, &steps), [5, 7, 8]);
    }
}
