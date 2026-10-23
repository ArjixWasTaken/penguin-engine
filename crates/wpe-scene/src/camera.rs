use glam::{Mat4, Vec2};

/// How the scene is fitted to an output whose aspect differs from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillMode {
    /// Distort to fill the output.
    Stretch,
    /// Show the whole scene, letterboxed.
    Fit,
    /// Fill the output, cropping the scene.
    #[default]
    Crop,
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
        match fill {
            FillMode::Stretch => scene,
            FillMode::Fit if out_aspect < scene_aspect => fit_width,
            FillMode::Fit => fit_height,
            FillMode::Crop if out_aspect > scene_aspect => fit_width,
            FillMode::Crop => fit_height,
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec4;

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
