//! Seeded randomness for the world: a stream of numbers, a hash of lattice points, and value
//! noise built from it, smooth or layered into fractal noise.
//!
//! Every function gives the same number for the same input on every machine, bit for bit:
//! the arithmetic is done in `f64` in the order written, and whole numbers wrap to 32 bits
//! as two's complement, and products keep their low 32 bits, so a world drawn from a seed
//! always looks the same.

/// A stream of numbers from 0 up to 1, the same stream for the same seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rng {
    state: u32,
}

impl Rng {
    pub fn new(seed: u32) -> Self {
        Self { state: seed }
    }

    /// The next number, at least 0 and under 1.
    pub fn next_f64(&mut self) -> f64 {
        self.state = self.state.wrapping_add(0x6d2b_79f5);
        let mut t = self.state;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }

    /// A number from the bell curve round 0 with a spread of 1, from the next two.
    pub fn normal(&mut self) -> f64 {
        let u = 1.0 - self.next_f64();
        let v = self.next_f64();
        (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
    }
}

/// `value` cut to a whole number toward zero and wrapped into a signed 32-bit integer, as two's
/// complement. Not a number, or an endless one, is 0.
pub fn int32(value: f64) -> i32 {
    uint32(value) as i32
}

/// `value` cut to a whole number toward zero and wrapped into an unsigned 32-bit integer, modulo
/// 2³². Not a number, or an endless one, is 0.
pub fn uint32(value: f64) -> u32 {
    if !value.is_finite() {
        return 0;
    }
    if value.abs() < 9.0e18 {
        // Cut toward zero into 64 bits, whose low 32 are the wrapped number.
        return value as i64 as u32;
    }
    // Past 64 bits a float's remainder is still exact.
    value.trunc().rem_euclid(4_294_967_296.0) as u32
}

/// A number from 0 up to 1 for the lattice point (x, y, z) and a seed, each a whole number
/// held as a float. It mixes them in floating point first, so huge inputs round as a double
/// does.
pub fn hash3(x: f64, y: f64, z: f64, seed: f64) -> f64 {
    let mixed = x * 374_761_393.0 + y * 668_265_263.0 + z * 2_147_483_647.0 + seed * 974_634_221.0;
    finish(int32(mixed) as u32)
}

/// [`hash3`] with every product wrapped to 32 bits before they are mixed, and the inputs cut
/// to whole numbers first.
pub fn hash3_wrapping(x: f64, y: f64, z: f64) -> f64 {
    let x = (int32(x) as u32).wrapping_mul(374_761_393);
    let y = (int32(y) as u32).wrapping_mul(668_265_263);
    let z = (int32(z) as u32).wrapping_mul(2_147_483_647);
    finish(x ^ y ^ z)
}

/// The last scramble every hash shares, and its scale to 0 up to 1.
fn finish(h: u32) -> f64 {
    let h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    f64::from(h ^ (h >> 16)) / 4_294_967_296.0
}

/// Value noise from -1 to 1: the hashes of the eight lattice points round (x, y, z), blended
/// smoothly by where the point lies between them.
pub fn vnoise3(x: f64, y: f64, z: f64, seed: f64) -> f64 {
    blended(x, y, z, |xi, yi, zi| hash3(xi, yi, zi, seed))
}

/// Fractal noise from -1 to 1: `octaves` layers of [`vnoise3`], each finer by 2.03 and half
/// as strong as the last, with its own seed 17 on.
pub fn fbm3(x: f64, y: f64, z: f64, seed: f64, octaves: u32) -> f64 {
    layered(x, y, z, seed, octaves, vnoise3)
}

/// [`vnoise3`] with the seed moving the lattice 131 points along x instead of entering the
/// hash: the variant the sky's noise is made with.
pub fn vnoise3_shifted(x: f64, y: f64, z: f64, seed: f64) -> f64 {
    blended(x, y, z, |xi, yi, zi| hash3(xi + seed * 131.0, yi, zi, 0.0))
}

/// [`fbm3`] layered from [`vnoise3_shifted`].
pub fn fbm3_shifted(x: f64, y: f64, z: f64, seed: f64, octaves: u32) -> f64 {
    layered(x, y, z, seed, octaves, vnoise3_shifted)
}

fn blended(x: f64, y: f64, z: f64, hash: impl Fn(f64, f64, f64) -> f64) -> f64 {
    let (xi, yi, zi) = (x.floor(), y.floor(), z.floor());
    let (xf, yf, zf) = (x - xi, y - yi, z - zi);
    let ease = |t: f64| t * t * (3.0 - 2.0 * t);
    let (u, v, w) = (ease(xf), ease(yf), ease(zf));
    let mut out = 0.0;
    for dz in [0.0, 1.0] {
        for dy in [0.0, 1.0] {
            for dx in [0.0, 1.0] {
                let weight = (if dx == 1.0 { u } else { 1.0 - u })
                    * (if dy == 1.0 { v } else { 1.0 - v })
                    * (if dz == 1.0 { w } else { 1.0 - w });
                out += weight * hash(xi + dx, yi + dy, zi + dz);
            }
        }
    }
    out * 2.0 - 1.0
}

fn layered(
    x: f64,
    y: f64,
    z: f64,
    seed: f64,
    octaves: u32,
    noise: impl Fn(f64, f64, f64, f64) -> f64,
) -> f64 {
    let (mut strength, mut scale, mut sum, mut total) = (0.5, 1.0, 0.0, 0.0);
    for octave in 0..octaves {
        sum += strength
            * noise(
                x * scale,
                y * scale,
                z * scale,
                seed + f64::from(octave) * 17.0,
            );
        total += strength;
        strength *= 0.5;
        scale *= 2.03;
    }
    sum / total
}

/// The point `t` of the way from `a` to `b`, unbounded.
pub fn mix(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// 0 at `from`, 1 at `to`, easing smoothly between and held beyond them.
pub fn smooth(from: f64, to: f64, value: f64) -> f64 {
    let t = ((value - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
