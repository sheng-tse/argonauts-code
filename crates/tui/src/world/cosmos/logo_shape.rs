//! A galaxy laid along its maker's logo. The shipped picture's ink is read once and kept, so a
//! maker whose logo ships brings its galaxy's shape with it; a maker with none keeps the spiral.

use std::f64::consts::TAU;
use std::sync::OnceLock;

use argonauts_core::look::logos::LogoMark;

use crate::base::kit::logo::{LogoSize, mark_png};
use crate::base::mascot::png;
use crate::base::theme::Brightness;
use crate::world::raster::noise::Rng;

/// Ink fainter than this is the logo's soft edge rather than the logo.
const INKED: f32 = 0.35;
/// How far the glow along the logo spreads, in the logo's pixels at 128 px across.
const GLOW_SPREAD: f64 = 2.5;
/// How far Claude's rays turn at their tips, in radians, so the burst reads as turning.
const RAY_SWIRL: f64 = 0.35;

#[derive(Debug, PartialEq)]
pub(super) struct LogoShape {
    side: usize,
    /// The ink blurred, which the galaxy's light follows.
    glow: Vec<f32>,
    /// Each inked pixel's middle, from -1 to 1 across the logo's square.
    pixels: Vec<(f64, f64)>,
    /// The running total of how many points each inked pixel draws.
    totals: Vec<f64>,
    /// The farthest ink from the middle, which becomes the galaxy's radius 1.
    reach: f64,
    /// How far the shape turns at its rim, in radians.
    swirl: f64,
}

impl LogoShape {
    /// The shape of a shipped logo, read the first time it is asked for.
    pub fn of(mark: LogoMark) -> Option<&'static Self> {
        static SHAPES: [OnceLock<Option<LogoShape>>; LogoMark::ALL.len()] =
            [const { OnceLock::new() }; LogoMark::ALL.len()];
        let at = LogoMark::ALL.iter().position(|m| *m == mark)?;
        SHAPES[at].get_or_init(|| Self::read(mark)).as_ref()
    }

    fn read(mark: LogoMark) -> Option<Self> {
        let image = png::decode(mark_png(mark, LogoSize::Px128, Brightness::Dark))?;
        let side = image.width.min(image.height);
        let ink: Vec<f32> = (0..side * side)
            .map(|at| {
                let (x, y) = (at % side, at / side);
                f32::from(image.pixels[(y * image.width + x) * 4 + 3]) / 255.0
            })
            .collect();
        // Claude's rays read as arms when their points crowd each ray's middle and thin toward
        // its edges and tip, so they are weighted by how deep inside the ink they lie.
        let rays = mark == LogoMark::Claude;
        let depth = rays.then(|| depth_inside(&ink, side));
        let middle = |at: usize| {
            let (x, y) = ((at % side) as f64 + 0.5, (at / side) as f64 + 0.5);
            (x / side as f64 * 2.0 - 1.0, y / side as f64 * 2.0 - 1.0)
        };
        let mut pixels = Vec::new();
        let mut totals = Vec::new();
        let mut total = 0.0;
        for (at, amount) in ink.iter().enumerate() {
            if *amount < INKED {
                continue;
            }
            let weight = match &depth {
                Some(depth) => depth[at].powf(0.7),
                None => f64::from(*amount),
            };
            if weight <= 0.0 {
                continue;
            }
            total += weight;
            pixels.push(middle(at));
            totals.push(total);
        }
        let reach = pixels.iter().map(|(x, y)| x.hypot(*y)).fold(0.0, f64::max);
        if pixels.is_empty() || reach <= 0.0 {
            return None;
        }
        Some(Self {
            side,
            glow: blurred(&ink, side, GLOW_SPREAD),
            pixels,
            totals,
            reach,
            swirl: if rays { RAY_SWIRL } else { 0.0 },
        })
    }

    /// `count` places on the logo, each as a radius from 0 to 1 and an angle, nearest the
    /// middle first. Each pixel draws as many as its weight says, at a random spot inside it.
    pub fn places(&self, count: usize, rng: &mut Rng) -> Vec<(f64, f64)> {
        let total = self.totals.last().copied().unwrap_or(0.0);
        let pixel = 2.0 / self.side as f64;
        let mut places: Vec<(f64, f64)> = (0..count)
            .map(|_| {
                let pick = rng.next_f64() * total;
                let at = self
                    .totals
                    .partition_point(|t| *t <= pick)
                    .min(self.pixels.len() - 1);
                let (x, y) = self.pixels[at];
                let x = x + (rng.next_f64() - 0.5) * pixel;
                let y = y + (rng.next_f64() - 0.5) * pixel;
                let rad = (x.hypot(y) / self.reach).min(1.0);
                (rad, (y.atan2(x) + self.swirl * rad).rem_euclid(TAU))
            })
            .collect();
        places.sort_by(|a, b| a.0.total_cmp(&b.0));
        places
    }

    /// The place on the logo whose radius is nearest `rad`, of a few drawn at random.
    pub fn near(&self, rad: f64, rng: &mut Rng) -> (f64, f64) {
        self.places(24, rng)
            .into_iter()
            .min_by(|a, b| (a.0 - rad).abs().total_cmp(&(b.0 - rad).abs()))
            .unwrap_or((rad, 0.0))
    }

    /// How brightly the logo's glow shows at `(u, v)` in the galaxy's plane, radius 1 at the
    /// rim, from 0 to 1.
    pub fn glow_at(&self, u: f64, v: f64) -> f64 {
        let rad = u.hypot(v);
        let th = v.atan2(u) - self.swirl * rad.min(1.0);
        let (x, y) = (rad * self.reach * th.cos(), rad * self.reach * th.sin());
        let side = self.side as f64;
        if x.abs() > 1.0 || y.abs() > 1.0 {
            return 0.0;
        }
        let last = self.side - 1;
        let (px, py) = (
            ((x + 1.0) / 2.0 * side - 0.5).clamp(0.0, last as f64),
            ((y + 1.0) / 2.0 * side - 0.5).clamp(0.0, last as f64),
        );
        let (i, j) = (px as usize, py as usize);
        let (fx, fy) = (px - i as f64, py - j as f64);
        let at = |i: usize, j: usize| f64::from(self.glow[j.min(last) * self.side + i.min(last)]);
        let top = at(i, j) * (1.0 - fx) + at(i + 1, j) * fx;
        let bottom = at(i, j + 1) * (1.0 - fx) + at(i + 1, j + 1) * fx;
        top * (1.0 - fy) + bottom * fy
    }
}

/// How deep inside the ink each pixel lies, from 0 outside to 1 at the deepest, by the 3-4
/// chamfer distance to the nearest pixel without ink.
fn depth_inside(ink: &[f32], side: usize) -> Vec<f64> {
    let far = u32::MAX / 2;
    let mut depth: Vec<u32> = ink
        .iter()
        .map(|a| if *a >= INKED { far } else { 0 })
        .collect();
    let get = |depth: &[u32], x: isize, y: isize| -> u32 {
        if x < 0 || y < 0 || x >= side as isize || y >= side as isize {
            0
        } else {
            depth[y as usize * side + x as usize]
        }
    };
    let s = side as isize;
    for y in 0..s {
        for x in 0..s {
            let at = (y * s + x) as usize;
            if depth[at] == 0 {
                continue;
            }
            let near = [
                get(&depth, x - 1, y) + 3,
                get(&depth, x, y - 1) + 3,
                get(&depth, x - 1, y - 1) + 4,
                get(&depth, x + 1, y - 1) + 4,
            ];
            depth[at] = near.into_iter().fold(depth[at], u32::min);
        }
    }
    for y in (0..s).rev() {
        for x in (0..s).rev() {
            let at = (y * s + x) as usize;
            if depth[at] == 0 {
                continue;
            }
            let near = [
                get(&depth, x + 1, y) + 3,
                get(&depth, x, y + 1) + 3,
                get(&depth, x + 1, y + 1) + 4,
                get(&depth, x - 1, y + 1) + 4,
            ];
            depth[at] = near.into_iter().fold(depth[at], u32::min);
        }
    }
    let deepest = f64::from(depth.iter().copied().max().unwrap_or(0).max(1));
    depth.iter().map(|d| f64::from(*d) / deepest).collect()
}

/// `ink` blurred by a bell curve `spread` pixels wide, across and then down, and scaled so its
/// brightest pixel is 1.
fn blurred(ink: &[f32], side: usize, spread: f64) -> Vec<f32> {
    let reach = (spread * 3.0).ceil() as isize;
    let kernel: Vec<f64> = (-reach..=reach)
        .map(|k| (-(k * k) as f64 / (2.0 * spread * spread)).exp())
        .collect();
    let sum: f64 = kernel.iter().sum();
    let pass = |from: &[f64], across: bool| -> Vec<f64> {
        let mut out = vec![0.0; side * side];
        for y in 0..side {
            for x in 0..side {
                let mut value = 0.0;
                for (k, weight) in (-reach..=reach).zip(&kernel) {
                    let (sx, sy) = if across {
                        (x as isize + k, y as isize)
                    } else {
                        (x as isize, y as isize + k)
                    };
                    if sx >= 0 && sy >= 0 && (sx as usize) < side && (sy as usize) < side {
                        value += weight * from[sy as usize * side + sx as usize];
                    }
                }
                out[y * side + x] = value / sum;
            }
        }
        out
    };
    let ink: Vec<f64> = ink.iter().map(|a| f64::from(*a)).collect();
    let glow = pass(&pass(&ink, true), false);
    let brightest = glow.iter().copied().fold(0.0, f64::max).max(f64::EPSILON);
    glow.iter().map(|g| (g / brightest) as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAKERS: [LogoMark; 7] = [
        LogoMark::Claude,
        LogoMark::OpenAi,
        LogoMark::Gemini,
        LogoMark::Qwen,
        LogoMark::Meta,
        LogoMark::Mistral,
        LogoMark::Kimi,
    ];

    #[test]
    fn every_makers_shipped_logo_gives_a_shape() {
        for mark in MAKERS {
            let shape = LogoShape::of(mark).unwrap_or_else(|| panic!("{mark:?} has a shape"));
            let places = shape.places(400, &mut Rng::new(3));
            assert_eq!(places.len(), 400);
            assert!(places.windows(2).all(|w| w[0].0 <= w[1].0), "nearest first");
            assert!(places.iter().all(|(rad, _)| (0.0..=1.0).contains(rad)));
            assert!(
                places.last().is_some_and(|(rad, _)| *rad > 0.6),
                "{mark:?} reaches out"
            );
        }
    }

    #[test]
    fn its_places_lie_on_the_logos_ink() {
        for mark in MAKERS {
            let shape = LogoShape::of(mark).unwrap_or_else(|| panic!("{mark:?}"));
            for (rad, th) in shape.places(300, &mut Rng::new(11)) {
                let glow = shape.glow_at(rad * th.cos(), rad * th.sin());
                assert!(glow > 0.15, "{mark:?} at {rad:.2} {th:.2} glows {glow:.2}");
            }
            assert!(shape.glow_at(1.6, 0.0) == 0.0, "nothing past the logo");
        }
    }

    #[test]
    fn claudes_rays_are_arms_that_turn_and_crowd_their_middles() {
        let shape = LogoShape::of(LogoMark::Claude).expect("Claude's starburst");
        assert!(shape.swirl > 0.0);
        assert_eq!(LogoShape::of(LogoMark::OpenAi).map(|s| s.swirl), Some(0.0));
        // Across the outer half, the points fall into separate rays with dark gaps between.
        let mut bins = [0u32; 72];
        for (rad, th) in shape.places(4000, &mut Rng::new(5)) {
            if rad > 0.55 {
                bins[((th / TAU * 72.0) as usize).min(71)] += 1;
            }
        }
        let empty = bins.iter().filter(|b| **b == 0).count();
        assert!(empty >= 12, "gaps between the rays: {bins:?}");
        let rays = bins
            .iter()
            .zip(bins.iter().cycle().skip(1))
            .filter(|(a, b)| **a == 0 && **b > 0)
            .count();
        assert!(rays >= 8, "{rays} rays: {bins:?}");
    }
}
