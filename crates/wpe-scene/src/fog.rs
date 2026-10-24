//! Scene fog: the editor's "Distance Fog" and "Height Fog" scene options
//! (`general.fogdistance*` / `general.fogheight*`), applied by the stock
//! shaders that include `common_fog.h`.

use glam::Vec3;
use wpe_assets::scene::SceneGeneral;

use crate::UserProperties;

/// One kind of fog: from `start` to `end` (camera distance, or world height)
/// its density goes from `start_density` to `end_density`, quadratically.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FogRange {
    pub start: f32,
    pub end: f32,
    pub start_density: f32,
    pub end_density: f32,
    pub color: Vec3,
}

impl FogRange {
    /// `g_Fog<Kind>Params`: `common_fog.h` computes
    /// `f = saturate((x - p.x) / p.y)` and mixes in the fog colour by
    /// `p.z + p.w · f²`, so this is `(start, end - start, start density,
    /// end density - start density)`.
    pub fn params(&self) -> [f32; 4] {
        let span = self.end - self.start;
        let span = if span.abs() < 1e-6 {
            1e-6_f32.copysign(span)
        } else {
            span
        };
        [
            self.start,
            span,
            self.start_density,
            self.end_density - self.start_density,
        ]
    }
}

/// The scene's enabled fogs.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Fog {
    pub distance: Option<FogRange>,
    pub height: Option<FogRange>,
}

impl Fog {
    pub fn from_general(general: &SceneGeneral, props: &UserProperties) -> Self {
        let color = |c: &wpe_assets::value::Dynamic<wpe_assets::value::Vec3>| {
            Vec3::from_array(props.value(c).0)
        };
        let distance = props.flag(&general.fogdistance).then(|| FogRange {
            start: props.value(&general.fogdistancestart),
            end: props.value(&general.fogdistanceend),
            start_density: props.value(&general.fogdistancestartdensity),
            end_density: props.value(&general.fogdistanceenddensity),
            color: color(&general.fogdistancecolor),
        });
        let height = props.flag(&general.fogheight).then(|| FogRange {
            start: props.value(&general.fogheightstart),
            end: props.value(&general.fogheightend),
            start_density: props.value(&general.fogheightstartdensity),
            end_density: props.value(&general.fogheightenddensity),
            color: color(&general.fogheightcolor),
        });
        Self { distance, height }
    }

    pub fn is_enabled(&self) -> bool {
        self.distance.is_some() || self.height.is_some()
    }

    /// Combos every material gets: `FOG_DIST` / `FOG_HEIGHT` for the
    /// enabled fogs (names from `common_fog.h`), and `FOG_COMPUTED`, which
    /// `genericimage3/4` need before they fog image layers. When Wallpaper
    /// Engine sets `FOG_COMPUTED` isn't known (its name is among the
    /// engine's combo strings); setting it whenever fog is on is a guess.
    pub fn combos(&self) -> Vec<(String, i32)> {
        if !self.is_enabled() {
            return Vec::new();
        }
        let mut combos = Vec::new();
        if self.distance.is_some() {
            combos.push(("FOG_DIST".to_owned(), 1));
        }
        if self.height.is_some() {
            combos.push(("FOG_HEIGHT".to_owned(), 1));
        }
        combos.push(("FOG_COMPUTED".to_owned(), 1));
        combos
    }

    /// The global uniforms of `common_fog.h` for the enabled fogs.
    pub fn uniforms(&self) -> Vec<(String, Vec<f32>)> {
        let mut out = Vec::new();
        for (range, color, params) in [
            (&self.distance, "g_FogDistanceColor", "g_FogDistanceParams"),
            (&self.height, "g_FogHeightColor", "g_FogHeightParams"),
        ] {
            if let Some(r) = range {
                out.push((color.to_owned(), r.color.to_array().to_vec()));
                out.push((params.to_owned(), r.params().to_vec()));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fog_from_general() {
        let general: SceneGeneral = serde_json::from_value(serde_json::json!({
            "fogdistance": true,
            "fogdistancestart": 10.0,
            "fogdistanceend": 110.0,
            "fogdistancecolor": "0.5 0.6 0.7",
            "fogdistancestartdensity": 0.25,
            "fogdistanceenddensity": 0.75,
            "fogheight": {"user": "h", "value": false},
        }))
        .unwrap();
        let mut props = UserProperties::default();
        let fog = Fog::from_general(&general, &props);
        assert!(fog.height.is_none());
        let d = fog.distance.unwrap();
        assert_eq!(d.params(), [10.0, 100.0, 0.25, 0.5]);
        assert_eq!(
            fog.combos(),
            [("FOG_DIST".to_owned(), 1), ("FOG_COMPUTED".to_owned(), 1)]
        );
        let u = fog.uniforms();
        assert_eq!(u[0].0, "g_FogDistanceColor");
        assert_eq!(u[0].1, [0.5, 0.6, 0.7]);

        props.set("h", serde_json::Value::from(true));
        let fog = Fog::from_general(&general, &props);
        assert!(fog.height.is_some());
        assert_eq!(fog.combos().len(), 3);

        let off = Fog::from_general(&SceneGeneral::default(), &props);
        assert!(!off.is_enabled() && off.combos().is_empty() && off.uniforms().is_empty());
    }
}
