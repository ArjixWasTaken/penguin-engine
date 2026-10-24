//! Camera paths of 3D scenes: the files listed in the scene's
//! `camera.paths`, each holding `paths[]` of `{duration, transforms[]}`.
//! The camera plays the paths one after another, looping, and moves from a
//! path's first transform to its second over its duration. With
//! `general.camerafade` it eases from where it was into each next path.

use glam::Vec3;
use serde::Deserialize;
use wpe_assets::value::Vec3 as JsonVec3;
use wpe_vfs::Vfs;

/// Seconds the camera takes to ease into the next path (`camerafade`).
pub const CAMERA_FADE_SECONDS: f64 = 1.5;

/// Longest step [`CameraPathPlayer::advance`] takes at once, so a long jump
/// in time plays like the frames it skips.
const MAX_STEP: f64 = 1.0 / 60.0;

/// One camera placement along a path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraKeyframe {
    pub eye: Vec3,
    pub center: Vec3,
    pub up: Vec3,
    /// Read but unused: the camera moves over the path's whole duration.
    pub timestamp: f64,
}

/// A camera move: from the first keyframe to the second over `duration`
/// seconds (further keyframes are ignored, as Wallpaper Engine does).
#[derive(Debug, Clone, PartialEq)]
pub struct CameraPath {
    pub duration: f64,
    pub keyframes: Vec<CameraKeyframe>,
}

#[derive(Deserialize)]
struct PathFile {
    #[serde(default)]
    paths: Vec<PathJson>,
}

#[derive(Deserialize)]
struct PathJson {
    #[serde(default)]
    duration: f64,
    #[serde(default)]
    transforms: Vec<TransformJson>,
}

#[derive(Deserialize)]
struct TransformJson {
    #[serde(default)]
    eye: JsonVec3,
    #[serde(default)]
    center: JsonVec3,
    #[serde(default = "up")]
    up: JsonVec3,
    #[serde(default)]
    timestamp: f64,
}

fn up() -> JsonVec3 {
    JsonVec3::from([0.0, 1.0, 0.0])
}

impl CameraPath {
    /// The playable paths of one path file: at least two keyframes and a
    /// positive duration.
    pub fn parse(json: &str) -> Result<Vec<Self>, serde_json::Error> {
        let file: PathFile = serde_json::from_str(json)?;
        Ok(file
            .paths
            .into_iter()
            .map(|p| Self {
                duration: p.duration,
                keyframes: p
                    .transforms
                    .into_iter()
                    .map(|t| CameraKeyframe {
                        eye: Vec3::from_array(t.eye.0),
                        center: Vec3::from_array(t.center.0),
                        up: Vec3::from_array(t.up.0),
                        timestamp: t.timestamp,
                    })
                    .collect(),
            })
            .filter(|p| p.keyframes.len() >= 2 && p.duration > 0.0)
            .collect())
    }

    /// Every playable path of a scene's path files, in order. Files that
    /// are missing or malformed are skipped.
    pub fn load_scene(vfs: &Vfs, scene: &wpe_assets::Scene) -> Vec<Self> {
        let mut paths = Vec::new();
        for file in scene.camera.paths.iter().filter_map(|p| p.as_str()) {
            let loaded = vfs
                .read_to_string(file)
                .map_err(|e| e.to_string())
                .and_then(|text| Self::parse(&text).map_err(|e| e.to_string()));
            match loaded {
                Ok(p) => {
                    tracing::info!("{} camera paths from {file}", p.len());
                    paths.extend(p);
                }
                Err(e) => tracing::warn!("camera path file {file}: {e}"),
            }
        }
        paths
    }
}

/// Plays a scene's camera paths, driving the perspective camera.
#[derive(Debug, Clone)]
pub struct CameraPathPlayer {
    paths: Vec<CameraPath>,
    current: usize,
    /// Seconds into the current path.
    time: f64,
    fade_enabled: bool,
    /// Easing from `fade_from` into the current path, `fade_time` seconds in.
    fading: bool,
    fade_time: f64,
    fade_from: (Vec3, Vec3, Vec3),
    eye: Vec3,
    center: Vec3,
    up: Vec3,
}

impl CameraPathPlayer {
    /// Start at the first keyframe of the first path; `None` without paths.
    pub fn new(paths: Vec<CameraPath>, fade: bool) -> Option<Self> {
        let first = *paths.first()?.keyframes.first()?;
        Some(Self {
            paths,
            current: 0,
            time: 0.0,
            fade_enabled: fade,
            fading: false,
            fade_time: 0.0,
            fade_from: (Vec3::ZERO, Vec3::ZERO, Vec3::Y),
            eye: first.eye,
            center: first.center,
            up: first.up,
        })
    }

    /// The camera's current eye, centre and up vector.
    pub fn look_at(&self) -> (Vec3, Vec3, Vec3) {
        (self.eye, self.center, self.up)
    }

    /// Index of the path playing.
    pub fn current(&self) -> usize {
        self.current
    }

    /// Move `dt` seconds along, in steps of at most a 60th of a second.
    pub fn advance(&mut self, dt: f64) {
        let mut left = dt.max(0.0);
        while left > 0.0 {
            let step = left.min(MAX_STEP);
            self.step(step);
            left -= step;
        }
    }

    /// One frame of `dt` seconds.
    fn step(&mut self, dt: f64) {
        self.time += dt;
        let duration = self.paths[self.current].duration;
        if self.time >= duration {
            if self.fade_enabled {
                self.fade_from = (self.eye, self.center, self.up);
                self.fading = true;
                self.fade_time = 0.0;
            }
            self.time -= duration;
            self.current = (self.current + 1) % self.paths.len();
            if self.time >= self.paths[self.current].duration {
                self.time = 0.0;
            }
        }

        let path = &self.paths[self.current];
        let (k0, k1) = (path.keyframes[0], path.keyframes[1]);
        let t = (self.time / path.duration).clamp(0.0, 1.0) as f32;
        let mut eye = k0.eye.lerp(k1.eye, t);
        let mut center = k0.center.lerp(k1.center, t);
        let mut up = k0.up.lerp(k1.up, t).normalize();

        if self.fading {
            self.fade_time += dt;
            let ft = (self.fade_time / CAMERA_FADE_SECONDS).clamp(0.0, 1.0);
            if ft < 1.0 {
                let a = (ft * ft * (3.0 - 2.0 * ft)) as f32;
                let (from_eye, from_center, from_up) = self.fade_from;
                eye = from_eye.lerp(eye, a);
                center = from_center.lerp(center, a);
                up = from_up.lerp(up, a).normalize();
            } else {
                self.fading = false;
            }
        }
        (self.eye, self.center, self.up) = (eye, center, up);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(duration: f64, from: Vec3, to: Vec3) -> CameraPath {
        let kf = |eye| CameraKeyframe {
            eye,
            center: Vec3::ZERO,
            up: Vec3::Y,
            timestamp: 0.0,
        };
        CameraPath {
            duration,
            keyframes: vec![kf(from), kf(to)],
        }
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-4
    }

    #[test]
    fn parses_playable_paths() {
        let paths = CameraPath::parse(
            r#"{"paths": [
                {"duration": 4, "transforms": [
                    {"eye": "0 1 5", "center": "0 0 0", "up": "0 1 0", "timestamp": 0},
                    {"eye": [5, 1, 0], "center": "0 0.5 0", "up": "0 1 0", "timestamp": 4}
                ]},
                {"duration": 3, "transforms": [{"eye": "1 1 1"}]},
                {"duration": 0, "transforms": [{"eye": "1 1 1"}, {"eye": "2 2 2"}]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].duration, 4.0);
        assert_eq!(paths[0].keyframes[1].eye, Vec3::new(5.0, 1.0, 0.0));
        assert_eq!(paths[0].keyframes[1].center, Vec3::new(0.0, 0.5, 0.0));
    }

    #[test]
    fn starts_at_first_keyframe_and_interpolates_linearly() {
        let mut p = CameraPathPlayer::new(vec![path(2.0, Vec3::ZERO, Vec3::X * 10.0)], false)
            .expect("one path");
        assert_eq!(p.look_at().0, Vec3::ZERO);
        p.advance(0.5);
        assert!(close(p.look_at().0, Vec3::X * 2.5));
        p.advance(1.0);
        assert!(close(p.look_at().0, Vec3::X * 7.5));
        assert!(CameraPathPlayer::new(Vec::new(), false).is_none());
    }

    #[test]
    fn plays_paths_in_turn_and_loops() {
        let paths = vec![
            path(1.0, Vec3::ZERO, Vec3::X),
            path(2.0, Vec3::Y * 10.0, Vec3::Y * 12.0),
        ];
        let mut p = CameraPathPlayer::new(paths, false).unwrap();
        p.advance(0.5);
        assert_eq!(p.current(), 0);
        assert!(close(p.look_at().0, Vec3::X * 0.5));
        // 1.5 s: half a second into the second path, without easing.
        p.advance(1.0);
        assert_eq!(p.current(), 1);
        assert!(close(p.look_at().0, Vec3::Y * 10.5));
        // 3.25 s: back on the first path.
        p.advance(1.75);
        assert_eq!(p.current(), 0);
        assert!(close(p.look_at().0, Vec3::X * 0.25));
    }

    #[test]
    fn up_is_normalized_between_keyframes() {
        let mut path = path(1.0, Vec3::ZERO, Vec3::ZERO);
        path.keyframes[1].up = Vec3::X;
        let mut p = CameraPathPlayer::new(vec![path], false).unwrap();
        p.advance(0.5);
        assert!(close(p.look_at().2, Vec3::new(1.0, 1.0, 0.0).normalize()));
    }

    #[test]
    fn fade_eases_into_the_next_path() {
        let paths = vec![
            path(1.0, Vec3::ZERO, Vec3::X),
            path(4.0, Vec3::Y * 10.0, Vec3::Y * 10.0),
        ];
        let mut p = CameraPathPlayer::new(paths, true).unwrap();
        p.advance(1.0);
        // The switch happens on the step crossing 1 s; the camera starts
        // easing from where the first path left it (just short of its end).
        p.advance(0.75 + 1e-9);
        let eye = p.look_at().0;
        assert_eq!(p.current(), 1);
        assert!(
            eye.y > 4.0 && eye.y < 6.0,
            "half-way through the fade: {eye}"
        );
        assert!(eye.x > 0.4 && eye.x < 0.6, "{eye}");
        p.advance(1.0);
        assert!(close(p.look_at().0, Vec3::Y * 10.0));
    }
}
