//! Seeded pseudo-random numbers and stateless hashes.

use glam::Vec3;

/// PCG32 generator: small, fast and reproducible across platforms.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
    inc: u64,
}

impl Rng {
    /// A generator whose whole sequence is determined by `seed`.
    pub fn new(seed: u64) -> Self {
        let mut r = Self {
            state: 0,
            inc: (seed.rotate_left(17) ^ 0xda3e_39cb_94b9_5bdb) << 1 | 1,
        };
        r.next_u32();
        r.state = r.state.wrapping_add(seed);
        r.next_u32();
        r
    }

    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 * (1.0 / 16_777_216.0)
    }

    /// Uniform in `[a, b)` (or `[b, a)`).
    pub fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.unit()
    }

    /// Uniform integer in `[lo, hi]`.
    pub fn int(&mut self, lo: i32, hi: i32) -> i32 {
        if hi <= lo {
            return lo;
        }
        let span = (hi as i64 - lo as i64 + 1) as u64;
        lo + ((self.next_u32() as u64 * span) >> 32) as i32
    }

    /// Normally distributed with the given mean and standard deviation.
    pub fn normal(&mut self, mean: f32, sd: f32) -> f32 {
        let u1 = self.unit().max(1e-7);
        let u2 = self.unit();
        mean + sd * (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
    }

    /// Per-component uniform between `a` and `b`.
    pub fn vec3(&mut self, a: Vec3, b: Vec3) -> Vec3 {
        Vec3::new(
            self.range(a.x, b.x),
            self.range(a.y, b.y),
            self.range(a.z, b.z),
        )
    }
}

/// Stateless hash of `(seed, salt)` to a uniform float in `[0, 1)`.
pub fn hash01(seed: u32, salt: u32) -> f32 {
    let mut x = seed ^ salt.wrapping_mul(0x9e37_79b9);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    (x >> 8) as f32 * (1.0 / 16_777_216.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        let mut c = Rng::new(43);
        let sa: Vec<u32> = (0..16).map(|_| a.next_u32()).collect();
        let sb: Vec<u32> = (0..16).map(|_| b.next_u32()).collect();
        let sc: Vec<u32> = (0..16).map(|_| c.next_u32()).collect();
        assert_eq!(sa, sb);
        assert_ne!(sa, sc);
    }

    #[test]
    fn ranges_stay_in_bounds() {
        let mut r = Rng::new(1);
        for _ in 0..10_000 {
            let u = r.unit();
            assert!((0.0..1.0).contains(&u));
            let i = r.int(-2, 3);
            assert!((-2..=3).contains(&i));
            assert!((0.0..1.0).contains(&hash01(r.next_u32(), 7)));
        }
    }
}
