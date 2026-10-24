use glam::{Mat4, Vec2, Vec3};

/// How the scene is fitted to an output whose aspect differs from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillMode {
    /// Distort to fill the output.
    Stretch,
    /// Show the whole scene, letterboxed.
    Fit,
    /// Fill the output, cropping the scene.
    Crop,
    /// As Wallpaper Engine does: crop, but fit a scene whose orientation
    /// differs from the output's (a portrait scene on a landscape output),
    /// which cropping would cut down to a strip.
    #[default]
    Auto,
}

impl FillMode {
    /// The mode `Auto` stands for with these aspect ratios (width / height).
    pub fn resolve(self, scene_aspect: f32, out_aspect: f32) -> Self {
        match self {
            Self::Auto if (scene_aspect > 1.0) != (out_aspect > 1.0) => Self::Fit,
            Self::Auto => Self::Crop,
            mode => mode,
        }
    }
}

/// The scene's `"global"` orthographic camera. World units are scene pixels
/// with the origin at the bottom-left; the camera looks down -Z at the
/// scene centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrthoCamera {
    /// Size of the scene's orthogonal projection (`general.orthogonalprojection`).
    pub scene_size: Vec2,
    /// `general.zoom`; values above 1 magnify.
    pub zoom: f32,
}

impl OrthoCamera {
    /// Visible world-space extent for an output of `output` pixels.
    pub fn extent(&self, output: (u32, u32), fill: FillMode) -> Vec2 {
        let zoom = if self.zoom > 0.0 { self.zoom } else { 1.0 };
        let scene = self.scene_size / zoom;
        let out_aspect = output.0.max(1) as f32 / output.1.max(1) as f32;
        let scene_aspect = scene.x / scene.y;
        let fit_width = Vec2::new(scene.x, scene.x / out_aspect);
        let fit_height = Vec2::new(scene.y * out_aspect, scene.y);
        match fill.resolve(scene_aspect, out_aspect) {
            FillMode::Auto => unreachable!("resolved"),
            FillMode::Stretch => scene,
            FillMode::Fit if out_aspect < scene_aspect => fit_width,
            FillMode::Fit => fit_height,
            FillMode::Crop if out_aspect > scene_aspect => fit_width,
            FillMode::Crop => fit_height,
        }
    }

    /// The mode `fill` amounts to on `output` (`Auto` resolved).
    pub fn fill_for(&self, output: (u32, u32), fill: FillMode) -> FillMode {
        let out_aspect = output.0.max(1) as f32 / output.1.max(1) as f32;
        fill.resolve(self.scene_size.x / self.scene_size.y, out_aspect)
    }

    pub fn view_proj(&self, output: (u32, u32), fill: FillMode) -> Mat4 {
        let half = self.extent(output, fill) / 2.0;
        let c = self.scene_size / 2.0;
        glam::camera::rh::proj::directx::orthographic(
            c.x - half.x,
            c.x + half.x,
            c.y - half.y,
            c.y + half.y,
            -1e4,
            1e4,
        )
    }
}

/// Distance of 2D scenes' `"global_perspective"` eye from the z = 0 plane.
pub const PERSPECTIVE_DISTANCE: f32 = 1000.0;

/// A perspective camera: 3D scenes' `"global"` camera, or 2D scenes'
/// `"global_perspective"` one that `perspective` layers draw with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PerspectiveCamera {
    pub eye: Vec3,
    pub center: Vec3,
    pub up: Vec3,
    /// Vertical field of view in degrees; unused when `fit` is set.
    pub fov: f32,
    pub near: f32,
    pub far: f32,
    /// For 2D scenes, the flat camera whose visible extent this one shows on
    /// the z = 0 plane: the fov and aspect follow it per output and fill mode.
    pub fit: Option<OrthoCamera>,
}

impl PerspectiveCamera {
    /// 2D scenes' `"global_perspective"` camera: looking down -Z at the scene
    /// centre from [`PERSPECTIVE_DISTANCE`], so that z = 0 lines up with `ortho`.
    pub fn global_perspective(ortho: OrthoCamera, near: f32, far: f32) -> Self {
        let c = (ortho.scene_size / 2.0).extend(0.0);
        Self {
            eye: c + Vec3::Z * PERSPECTIVE_DISTANCE,
            center: c,
            up: Vec3::Y,
            fov: 0.0,
            near,
            far,
            fit: Some(ortho),
        }
    }

    pub fn eye(&self) -> Vec3 {
        self.eye
    }

    /// Vertical fov in radians and aspect ratio for an output.
    pub fn fov_aspect(&self, output: (u32, u32), fill: FillMode) -> (f32, f32) {
        match self.fit {
            Some(ortho) => {
                let visible = ortho.extent(output, fill);
                let distance = (self.eye - self.center).length().max(f32::EPSILON);
                (
                    2.0 * (visible.y / 2.0 / distance).atan(),
                    visible.x / visible.y,
                )
            }
            None => (
                self.fov.to_radians(),
                output.0.max(1) as f32 / output.1.max(1) as f32,
            ),
        }
    }

    pub fn view(&self, _output: (u32, u32), _fill: FillMode) -> Mat4 {
        glam::camera::rh::view::look_at_mat4(self.eye, self.center, self.up)
    }

    pub fn proj(&self, output: (u32, u32), fill: FillMode) -> Mat4 {
        let (fov, aspect) = self.fov_aspect(output, fill);
        glam::camera::rh::proj::directx::perspective(fov, aspect, self.near, self.far)
    }

    pub fn view_proj(&self, output: (u32, u32), fill: FillMode) -> Mat4 {
        self.proj(output, fill) * self.view(output, fill)
    }

    /// The `"reflected_perspective"` camera's view: this one mirrored about
    /// the y = 0 floor (the view's y column negated), so points on the floor
    /// stay where this camera sees them.
    pub fn reflected_view(&self, output: (u32, u32), fill: FillMode) -> Mat4 {
        self.view(output, fill) * Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0))
    }

    /// Its projection, flipped vertically: the reflection target stores the
    /// mirror image upside down, so floors sampling it at their own screen
    /// position (with GL's y-up screen UVs) read the mirrored point.
    pub fn reflected_proj(&self, output: (u32, u32), fill: FillMode) -> Mat4 {
        Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0)) * self.proj(output, fill)
    }

    pub fn reflected_view_proj(&self, output: (u32, u32), fill: FillMode) -> Mat4 {
        self.reflected_proj(output, fill) * self.reflected_view(output, fill)
    }

    /// The eye mirrored below the floor.
    pub fn reflected_eye(&self) -> Vec3 {
        self.eye * Vec3::new(1.0, -1.0, 1.0)
    }
}

/// 3D scenes' `"global_ortho"` overlay camera as the reference renderer
/// sizes it on every resize: a window one unit tall and `aspect` wide,
/// centred on the world origin.
pub fn overlay_view_proj(output: (u32, u32)) -> Mat4 {
    let aspect = output.0.max(1) as f32 / output.1.max(1) as f32;
    glam::camera::rh::proj::directx::orthographic(
        -aspect / 2.0,
        aspect / 2.0,
        -0.5,
        0.5,
        -5000.0,
        5000.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec4;

    fn project(m: Mat4, p: Vec3) -> Vec2 {
        let c = m * p.extend(1.0);
        c.truncate().truncate() / c.w
    }

    #[test]
    fn auto_fits_only_across_orientations() {
        let portrait = OrthoCamera {
            scene_size: Vec2::new(1080.0, 1920.0),
            zoom: 1.0,
        };
        let wide = OrthoCamera {
            scene_size: Vec2::new(2560.0, 1080.0),
            zoom: 1.0,
        };
        let landscape = (1920, 1080);
        // A portrait scene on a landscape output: all of its height shows.
        assert_eq!(
            portrait.extent(landscape, FillMode::Auto),
            portrait.extent(landscape, FillMode::Fit)
        );
        assert_eq!(portrait.extent(landscape, FillMode::Auto).y, 1920.0);
        // Same orientation: cropped as before.
        assert_eq!(
            wide.extent(landscape, FillMode::Auto),
            wide.extent(landscape, FillMode::Crop)
        );
        assert_eq!(
            wide.extent((1080, 1920), FillMode::Auto),
            wide.extent((1080, 1920), FillMode::Fit)
        );
    }

    #[test]
    fn global_perspective_matches_ortho_at_z0() {
        for zoom in [1.0, 1.7] {
            let ortho = OrthoCamera {
                scene_size: Vec2::new(1920.0, 1080.0),
                zoom,
            };
            let persp = PerspectiveCamera::global_perspective(ortho, 0.01, 10000.0);
            for fill in [FillMode::Stretch, FillMode::Fit, FillMode::Crop] {
                for output in [(1920, 1080), (1000, 1000), (800, 1600), (3440, 1440)] {
                    let a = ortho.view_proj(output, fill);
                    let b = persp.view_proj(output, fill);
                    for p in [
                        Vec3::ZERO,
                        Vec3::new(960.0, 540.0, 0.0),
                        Vec3::new(1920.0, 1080.0, 0.0),
                        Vec3::new(100.0, 900.0, 0.0),
                        Vec3::new(-300.0, 2000.0, 0.0),
                    ] {
                        let (pa, pb) = (project(a, p), project(b, p));
                        assert!(
                            (pa - pb).length() < 1e-4,
                            "{fill:?} {output:?} {p}: {pa} vs {pb}"
                        );
                    }
                    // Depth stays within the 0..1 clip range in front of the camera.
                    let c = b * Vec4::new(960.0, 540.0, 0.0, 1.0);
                    assert!((0.0..=1.0).contains(&(c.z / c.w)));
                }
            }
        }
    }

    #[test]
    fn perspective_3d_uses_output_aspect() {
        let cam = PerspectiveCamera {
            eye: Vec3::new(0.0, 0.0, 10.0),
            center: Vec3::ZERO,
            up: Vec3::Y,
            fov: 90.0,
            near: 0.1,
            far: 100.0,
            fit: None,
        };
        let vp = cam.view_proj((2000, 1000), FillMode::Crop);
        // At distance 10 with a 90° vertical fov the view is 20 tall, 40 wide.
        assert!((project(vp, Vec3::new(20.0, 10.0, 0.0)) - Vec2::ONE).length() < 1e-5);
        assert_eq!(cam.eye(), Vec3::new(0.0, 0.0, 10.0));
    }

    #[test]
    fn reflected_camera_keeps_the_floor_and_mirrors_the_rest() {
        let cam = PerspectiveCamera {
            eye: Vec3::new(1.7, 1.0, 2.9),
            center: Vec3::ZERO,
            up: Vec3::Y,
            fov: 50.0,
            near: 0.1,
            far: 100.0,
            fit: None,
        };
        let out = (640, 360);
        let main = cam.view_proj(out, FillMode::Crop);
        let refl = cam.reflected_view_proj(out, FillMode::Crop);
        let flip = Vec2::new(1.0, -1.0);
        // A floor point lands on the same pixel, stored upside down.
        let floor = Vec3::new(0.3, 0.0, -0.4);
        assert!((project(refl, floor) - project(main, floor) * flip).length() < 1e-5);
        // A point above the floor shows where its mirror image below would.
        let above = Vec3::new(0.3, 0.5, -0.4);
        let below = above * Vec3::new(1.0, -1.0, 1.0);
        assert!((project(refl, above) - project(main, below) * flip).length() < 1e-5);
        assert_eq!(cam.reflected_eye(), Vec3::new(1.7, -1.0, 2.9));
    }

    #[test]
    fn crop_keeps_scene_centred() {
        let cam = OrthoCamera {
            scene_size: Vec2::new(1920.0, 1080.0),
            zoom: 1.0,
        };
        // A square output crops the sides.
        assert_eq!(
            cam.extent((1000, 1000), FillMode::Crop),
            Vec2::new(1080.0, 1080.0)
        );
        assert_eq!(
            cam.extent((1000, 1000), FillMode::Fit),
            Vec2::new(1920.0, 1920.0)
        );

        let vp = cam.view_proj((1920, 1080), FillMode::Crop);
        let bottom_left = vp * Vec4::new(0.0, 0.0, 0.0, 1.0);
        let top_right = vp * Vec4::new(1920.0, 1080.0, 0.0, 1.0);
        assert!((bottom_left.truncate() - glam::Vec3::new(-1.0, -1.0, 0.5)).length() < 1e-5);
        assert!((top_right.truncate() - glam::Vec3::new(1.0, 1.0, 0.5)).length() < 1e-5);
    }
}
