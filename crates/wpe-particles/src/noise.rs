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

/// Gradient vector `grad` dots with for each hash value.
const GRADIENTS: [[f64; 3]; 16] = [
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0],
    [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [1.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0],
    [0.0, -1.0, 1.0],
    [0.0, 1.0, -1.0],
    [0.0, -1.0, -1.0],
    [1.0, 1.0, 0.0],
    [0.0, -1.0, 1.0],
    [-1.0, 1.0, 0.0],
    [0.0, -1.0, -1.0],
];

fn fade_derivative(t: f64) -> f64 {
    30.0 * t * t * (t - 1.0) * (t - 1.0)
}

/// [`perlin`] and its analytic gradient `[d/dx, d/dy, d/dz]`.
pub fn perlin_gradient(x: f64, y: f64, z: f64) -> (f64, [f64; 3]) {
    let (fx, fy, fz) = (x.floor(), y.floor(), z.floor());
    let (xi, yi, zi) = (fx as i32 & 255, fy as i32 & 255, fz as i32 & 255);
    let (x, y, z) = (x - fx, y - fy, z - fz);
    let a = perm(xi) + yi;
    let aa = perm(a) + zi;
    let ab = perm(a + 1) + zi;
    let b = perm(xi + 1) + yi;
    let ba = perm(b) + zi;
    let bb = perm(b + 1) + zi;
    let corner = |hash: i32, dx: f64, dy: f64, dz: f64| {
        let g = GRADIENTS[(hash & 0xF) as usize];
        (g[0] * (x - dx) + g[1] * (y - dy) + g[2] * (z - dz), g)
    };
    let (va, ga) = corner(perm(aa), 0.0, 0.0, 0.0);
    let (vb, gb) = corner(perm(ba), 1.0, 0.0, 0.0);
    let (vc, gc) = corner(perm(ab), 0.0, 1.0, 0.0);
    let (vd, gd) = corner(perm(bb), 1.0, 1.0, 0.0);
    let (ve, ge) = corner(perm(aa + 1), 0.0, 0.0, 1.0);
    let (vf, gf) = corner(perm(ba + 1), 1.0, 0.0, 1.0);
    let (vg, gg) = corner(perm(ab + 1), 0.0, 1.0, 1.0);
    let (vh, gh) = corner(perm(bb + 1), 1.0, 1.0, 1.0);
    let (u, v, w) = (fade(x), fade(y), fade(z));
    let (du, dv, dw) = (fade_derivative(x), fade_derivative(y), fade_derivative(z));
    let k1 = vb - va;
    let k2 = vc - va;
    let k3 = ve - va;
    let k4 = va - vb - vc + vd;
    let k5 = va - vc - ve + vg;
    let k6 = va - vb - ve + vf;
    let k7 = -va + vb + vc - vd + ve - vf - vg + vh;
    let value =
        va + u * k1 + v * k2 + w * k3 + u * v * k4 + v * w * k5 + w * u * k6 + u * v * w * k7;
    let mut grad = [0.0; 3];
    for i in 0..3 {
        grad[i] = ga[i]
            + u * (gb[i] - ga[i])
            + v * (gc[i] - ga[i])
            + w * (ge[i] - ga[i])
            + u * v * (ga[i] - gb[i] - gc[i] + gd[i])
            + v * w * (ga[i] - gc[i] - ge[i] + gg[i])
            + w * u * (ga[i] - gb[i] - ge[i] + gf[i])
            + u * v * w * (-ga[i] + gb[i] + gc[i] - gd[i] + ge[i] - gf[i] - gg[i] + gh[i]);
    }
    grad[0] += du * (k1 + k4 * v + k6 * w + k7 * v * w);
    grad[1] += dv * (k2 + k5 * w + k4 * u + k7 * w * u);
    grad[2] += dw * (k3 + k6 * u + k5 * v + k7 * u * v);
    (value, grad)
}

/// Curl of a Perlin vector field at `(x, y, z)`: divergence-free swirling directions.
pub fn curl_at(x: f64, y: f64, z: f64) -> Vec3 {
    let (_, fx) = perlin_gradient(x, y, z);
    let (_, fy) = perlin_gradient(x + 89.2, y + 33.1, z + 57.3);
    let (_, fz) = perlin_gradient(x + 100.3, y + 120.1, z + 142.2);
    Vec3::new(
        (fz[1] - fy[2]) as f32,
        (fx[2] - fz[0]) as f32,
        (fy[0] - fx[1]) as f32,
    )
}

/// [`curl_at`] for a single-precision point.
pub fn curl(p: Vec3) -> Vec3 {
    curl_at(p.x as f64, p.y as f64, p.z as f64)
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
    fn analytic_gradient_matches_finite_differences() {
        const E: f64 = 1e-6;
        for i in 0..200 {
            let p = [
                i as f64 * 0.731 - 40.0,
                i as f64 * 0.377 + 3.1,
                i as f64 * -0.19 + 7.7,
            ];
            let (v, g) = perlin_gradient(p[0], p[1], p[2]);
            assert!((v - perlin(p[0], p[1], p[2])).abs() < 1e-12);
            for axis in 0..3 {
                let mut q = p;
                q[axis] += E;
                let mut r = p;
                r[axis] -= E;
                let fd = (perlin(q[0], q[1], q[2]) - perlin(r[0], r[1], r[2])) / (2.0 * E);
                assert!(
                    (fd - g[axis]).abs() < 1e-5,
                    "axis {axis}: {fd} vs {}",
                    g[axis]
                );
            }
        }
    }

    #[test]
    fn curl_is_finite_and_nonzero() {
        let c = curl(Vec3::new(0.3, 1.7, 2.2));
        assert!(c.is_finite());
        assert!(c.length() > 0.0);
    }
}
