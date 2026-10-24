//! Ken Perlin's improved noise and the curl field built on it.

use glam::Vec3;

const PERM: [u8; 256] = [
    151, 160, 137, 91, 90, 15, 131, 13, 201, 95, 96, 53, 194, 233, 7, 225, 140, 36, 103, 30, 69,
    142, 8, 99, 37, 240, 21, 10, 23, 190, 6, 148, 247, 120, 234, 75, 0, 26, 197, 62, 94, 252, 219,
    203, 117, 35, 11, 32, 57, 177, 33, 88, 237, 149, 56, 87, 174, 20, 125, 136, 171, 168, 68, 175,
    74, 165, 71, 134, 139, 48, 27, 166, 77, 146, 158, 231, 83, 111, 229, 122, 60, 211, 133, 230,
    220, 105, 92, 41, 55, 46, 245, 40, 244, 102, 143, 54, 65, 25, 63, 161, 1, 216, 80, 73, 209, 76,
    132, 187, 208, 89, 18, 169, 200, 196, 135, 130, 116, 188, 159, 86, 164, 100, 109, 198, 173,
    186, 3, 64, 52, 217, 226, 250, 124, 123, 5, 202, 38, 147, 118, 126, 255, 82, 85, 212, 207, 206,
    59, 227, 47, 16, 58, 17, 182, 189, 28, 42, 223, 183, 170, 213, 119, 248, 152, 2, 44, 154, 163,
    70, 221, 153, 101, 155, 167, 43, 172, 9, 129, 22, 39, 253, 19, 98, 108, 110, 79, 113, 224, 232,
    178, 185, 112, 104, 218, 246, 97, 228, 251, 34, 242, 193, 238, 210, 144, 12, 191, 179, 162,
    241, 81, 51, 145, 235, 249, 14, 239, 107, 49, 192, 214, 31, 181, 199, 106, 157, 184, 84, 204,
    176, 115, 121, 50, 45, 127, 4, 150, 254, 138, 236, 205, 93, 222, 114, 67, 29, 24, 72, 243, 141,
    128, 195, 78, 66, 215, 61, 156, 180,
];

fn perm(i: i32) -> i32 {
    PERM[(i & 255) as usize] as i32
}

fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

fn grad(hash: i32, x: f64, y: f64, z: f64) -> f64 {
    match hash & 0xF {
        0x0 => x + y,
        0x1 => -x + y,
        0x2 => x - y,
        0x3 => -x - y,
        0x4 => x + z,
        0x5 => -x + z,
        0x6 => x - z,
        0x7 => -x - z,
        0x8 => y + z,
        0x9 => -y + z,
        0xA => y - z,
        0xB => -y - z,
        0xC => y + x,
        0xD => -y + z,
        0xE => y - x,
        _ => -y - z,
    }
}

/// 3D Perlin noise in roughly `[-1, 1]`, periodic every 256 units.
pub fn perlin(x: f64, y: f64, z: f64) -> f64 {
    let (fx, fy, fz) = (x.floor(), y.floor(), z.floor());
    let (xi, yi, zi) = (fx as i32 & 255, fy as i32 & 255, fz as i32 & 255);
    let (x, y, z) = (x - fx, y - fy, z - fz);
    let (u, v, w) = (fade(x), fade(y), fade(z));
    let a = perm(xi) + yi;
    let aa = perm(a) + zi;
    let ab = perm(a + 1) + zi;
    let b = perm(xi + 1) + yi;
    let ba = perm(b) + zi;
    let bb = perm(b + 1) + zi;
    lerp(
        w,
        lerp(
            v,
            lerp(u, grad(perm(aa), x, y, z), grad(perm(ba), x - 1.0, y, z)),
            lerp(
                u,
                grad(perm(ab), x, y - 1.0, z),
                grad(perm(bb), x - 1.0, y - 1.0, z),
            ),
        ),
        lerp(
            v,
            lerp(
                u,
                grad(perm(aa + 1), x, y, z - 1.0),
                grad(perm(ba + 1), x - 1.0, y, z - 1.0),
            ),
            lerp(
                u,
                grad(perm(ab + 1), x, y - 1.0, z - 1.0),
                grad(perm(bb + 1), x - 1.0, y - 1.0, z - 1.0),
            ),
        ),
    )
}

/// Fractal sum of `octaves` Perlin layers mapped into `[0, 1]`.
pub fn fbm01(x: f64, octaves: u32) -> f64 {
    let (mut sum, mut amp, mut freq, mut max) = (0.0, 1.0, 1.0, 0.0);
    for _ in 0..octaves.max(1) {
        sum += amp * perlin(x * freq, 0.0, 0.0);
        max += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    ((sum / max + 1.0) * 0.5).clamp(0.0, 1.0)
}

fn perlin3(p: [f64; 3]) -> [f64; 3] {
    [
        perlin(p[0], p[1], p[2]),
        perlin(p[0] + 89.2, p[1] + 33.1, p[2] + 57.3),
        perlin(p[0] + 100.3, p[1] + 120.1, p[2] + 142.2),
    ]
}

/// Curl of a Perlin vector field: divergence-free swirling directions.
pub fn curl(p: Vec3) -> Vec3 {
    const E: f64 = 1e-3;
    let p = [p.x as f64, p.y as f64, p.z as f64];
    let at = |axis: usize| {
        let mut q = p;
        q[axis] += E;
        perlin3(q)
    };
    let c = perlin3(p);
    let (x1, y1, z1) = (at(0), at(1), at(2));
    let x = (y1[2] - c[2]) - (z1[1] - c[1]);
    let y = (z1[0] - c[0]) - (x1[2] - c[2]);
    let z = (x1[1] - c[1]) - (y1[0] - c[0]);
    Vec3::new(x as f32, y as f32, z as f32) / E as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perlin_is_zero_on_lattice_and_bounded() {
        assert_eq!(perlin(1.0, 2.0, 3.0), 0.0);
        for i in 0..1000 {
            let f = i as f64 * 0.137;
            let n = perlin(f, f * 0.7, f * 1.3);
            assert!((-1.5..=1.5).contains(&n));
        }
        assert!((0.0..=1.0).contains(&fbm01(3.7, 4)));
    }

    #[test]
    fn curl_is_finite_and_nonzero() {
        let c = curl(Vec3::new(0.3, 1.7, 2.2));
        assert!(c.is_finite());
        assert!(c.length() > 0.0);
    }
}
