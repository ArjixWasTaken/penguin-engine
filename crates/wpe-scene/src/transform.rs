use glam::{Mat4, Quat, Vec2, Vec3};

/// A node's local transform: translate · Rz · Ry · Rx · scale, angles in radians.
pub fn local_transform(origin: Vec3, scale: Vec3, angles: Vec3) -> Mat4 {
    let rotation = Quat::from_rotation_z(angles.z)
        * Quat::from_rotation_y(angles.y)
        * Quat::from_rotation_x(angles.x);
    Mat4::from_scale_rotation_translation(scale, rotation, origin)
}

/// Offset moving a card's centre so that `alignment` (e.g. `"topleft"`)
/// names the corner or edge sitting at the layer origin.
pub fn alignment_offset(alignment: &str, size: Vec2) -> Vec2 {
    let half = size / 2.0;
    let mut offset = Vec2::ZERO;
    if alignment.contains("top") {
        offset.y -= half.y;
    }
    if alignment.contains("bottom") {
        offset.y += half.y;
    }
    if alignment.contains("left") {
        offset.x += half.x;
    }
    if alignment.contains("right") {
        offset.x -= half.x;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    #[test]
    fn scale_then_rotate_then_translate() {
        let m = local_transform(
            Vec3::new(10.0, 20.0, 0.0),
            Vec3::new(2.0, 1.0, 1.0),
            Vec3::new(0.0, 0.0, FRAC_PI_2),
        );
        let p = m.transform_point3(Vec3::new(1.0, 0.0, 0.0));
        assert!((p - Vec3::new(10.0, 22.0, 0.0)).length() < 1e-5);
    }

    #[test]
    fn alignment() {
        let size = Vec2::new(100.0, 50.0);
        assert_eq!(alignment_offset("center", size), Vec2::ZERO);
        assert_eq!(alignment_offset("topleft", size), Vec2::new(50.0, -25.0));
        assert_eq!(
            alignment_offset("bottomright", size),
            Vec2::new(-50.0, 25.0)
        );
    }
}
