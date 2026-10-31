//! A galaxy's light in its own plane: a bulge, two arms broken into clumps (none for a dwarf) or
//! a glow along its maker's logo, pink knots where this week's actions gather, as where a real galaxy is forming stars, and dark
//! dust along the arms where its actions failed, so many failures in one stretch join into the
//! dust lanes a real galaxy has and a clean model's arm stays bright.

use super::logo_shape::LogoShape;
use super::model::step_colour;
use super::shapes::{LOGO_REACH, Point};
use crate::world::raster::Canvas;
use crate::world::raster::noise::{fbm3_shifted, mix, smooth};

/// What a galaxy's disc is drawn from.
pub(super) struct Disc<'a> {
    pub points: &'a [Point],
    pub colour: [u8; 3],
    pub dwarf: bool,
    /// How bright its light is, from its share of the actions.
    pub strength: f64,
    /// Its own clumps, the same every time it is drawn.
    pub seed: f64,
    /// The logo its light follows instead of two arms.
    pub logo: Option<&'a LogoShape>,
    /// Its colours from the core out, where its maker's logo has them.
    pub steps: Option<&'a [(f64, [u8; 3])]>,
}

/// How many pixels across a disc's picture is for a galaxy of radius `radius` pixels drawn at
/// 1/`down` of that size.
pub(super) fn side(radius: f64, down: u32) -> u32 {
    ((radius / f64::from(down) * 1.3).ceil() as u32) * 2 + 2
}

/// How steeply the arms wind, as the points were laid along them.
const PITCH: f64 = 0.52;

impl Disc<'_> {
    /// The disc's light as a square picture for a galaxy of radius `radius` pixels, drawn at
    /// 1/`down` of that size. Dark is clear, so the sky shows through; the picture is
    /// `radius * 2.6 + 2` pixels across at full size.
    pub fn picture(&self, radius: f64, down: u32) -> Canvas {
        let r = radius / f64::from(down);
        let size = side(radius, down) as usize;
        let half = size as f64 / 2.0;
        let mut light = vec![[0.0f64; 3]; size * size];
        let mut dust = vec![0.0f64; size * size];
        let unit = (r / 186.0).max(0.6);
        let mut splat = |x: f64, y: f64, sigma: f64, colour: Option<[f64; 3]>, amp: f64| {
            let reach = (sigma * 2.6).ceil();
            let (top, bottom) = (
                (y - reach).floor().max(0.0),
                (y + reach).ceil().min(size as f64 - 1.0),
            );
            let (left, right) = (
                (x - reach).floor().max(0.0),
                (x + reach).ceil().min(size as f64 - 1.0),
            );
            if top > bottom || left > right {
                return;
            }
            for j in top as usize..=bottom as usize {
                for i in left as usize..=right as usize {
                    let (dx, dy) = (i as f64 + 0.5 - x, j as f64 + 0.5 - y);
                    let k = amp * (-(dx * dx + dy * dy) / (2.0 * sigma * sigma)).exp();
                    let at = j * size + i;
                    match colour {
                        Some(c) => {
                            for (channel, part) in light[at].iter_mut().zip(c) {
                                *channel += part * k;
                            }
                        }
                        None => dust[at] += k,
                    }
                }
            }
        };
        for point in self.points.iter().filter(|p| p.failed) {
            let sigma = (1.6 + 1.8 * point.rad) * unit;
            let step = sigma * 0.9 / (r * point.rad.max(0.12));
            for q in -4..=4 {
                let rad = point.rad * 0.95;
                let th = point.th - 0.05 + f64::from(q) * step;
                let amp = 0.2 * (1.0 - f64::from(q).abs() / 5.5);
                splat(
                    half + th.cos() * rad * r,
                    half + th.sin() * rad * r,
                    sigma,
                    None,
                    amp,
                );
            }
        }
        for point in self.points.iter().filter(|p| p.fresh) {
            let (x, y) = (
                half + point.th.cos() * point.rad * r,
                half + point.th.sin() * point.rad * r,
            );
            splat(x, y, 3.0 * unit, Some([1.0, 0.5, 0.7]), 0.045);
        }
        let colour = self.colour.map(|c| f64::from(c) / 255.0);
        let core = [1.0, 0.9, 0.74];
        let young = [0.62, 0.74, 1.0];
        let octaves = if r > 260.0 { 4 } else { 3 };
        let winding = PITCH.tan();
        for j in 0..size {
            for i in 0..size {
                let u = (i as f64 + 0.5 - half) / r;
                let v = (j as f64 + 0.5 - half) / r;
                let rr = u.hypot(v);
                if rr > 1.3 {
                    continue;
                }
                let th = v.atan2(u);
                // Arms broken into clumps, as real arms are; the clumps are the disc's own
                // stars, no data.
                let clump = 0.5 + 0.5 * fbm3_shifted(u * 7.0, v * 7.0, self.seed, 9.0, octaves);
                let arm = match self.logo {
                    // A soft glow that follows the logo's strokes.
                    Some(logo) => {
                        0.35 * logo.glow_at(u / LOGO_REACH, v / LOGO_REACH) * (0.6 + 0.8 * clump)
                    }
                    None if self.dwarf => 0.3 + 0.5 * clump,
                    None => {
                        (0.5 + 0.5 * (2.0 * (th - (rr / 0.08 + 1.0).ln() / winding)).cos())
                            .powf(3.4)
                            * (0.3 + 1.3 * clump * clump)
                    }
                };
                let (bulge_strength, bulge_width, falloff, halo) = match (self.logo, self.dwarf) {
                    (Some(_), _) => (0.85, 0.075, 0.75, 0.01),
                    (None, true) => (0.55, 0.2, 0.3, 0.06),
                    (None, false) => (0.85, 0.075, 0.36, 0.06),
                };
                let bulge = bulge_strength * (-(rr / bulge_width).powi(2)).exp()
                    + 0.16 * (-rr / 0.18).exp();
                let disc =
                    (-rr / falloff).exp() * (halo + 1.45 * arm) * 0.95 * smooth(1.18, 0.7, rr);
                let heart = (-rr / 0.08).exp();
                let outer = smooth(0.4, 1.0, rr) * 0.3;
                let stepped = self
                    .steps
                    .map(|steps| step_colour(steps, rr.min(1.0)).map(|c| f64::from(c) / 255.0));
                let at = j * size + i;
                for k in 0..3 {
                    let arm_colour = match stepped {
                        Some(step) => step[k],
                        None => mix(mix(colour[k], young[k], outer), core[k], heart),
                    };
                    light[at][k] += self.strength * (bulge * core[k] + disc * arm_colour);
                }
            }
        }
        let mut bytes = vec![0u8; size * size * 4];
        for (at, (l, d)) in light.iter().zip(&dust).enumerate() {
            let mut c = [
                1.0 - (-1.7 * l[0] * (-0.95 * d).exp()).exp(),
                1.0 - (-1.7 * l[1] * (-1.15 * d).exp()).exp(),
                1.0 - (-1.7 * l[2] * (-1.4 * d).exp()).exp(),
            ];
            let hot = smooth(0.62, 0.98, (c[0] + c[1] + c[2]) / 3.0);
            c = [
                mix(c[0], 1.0, hot),
                mix(c[1], 0.98, hot),
                mix(c[2], 0.94, hot),
            ];
            // Light only: clear where dark, so the sky shows through.
            let alpha = c[0].max(c[1]).max(c[2]);
            if alpha > 0.0 {
                for k in 0..3 {
                    bytes[at * 4 + k] = (c[k] / alpha * 255.0).round().clamp(0.0, 255.0) as u8;
                }
                bytes[at * 4 + 3] = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        Canvas::from_rgba(size as u32, size as u32, &bytes).unwrap_or_else(|| Canvas::new(1, 1))
    }
}
