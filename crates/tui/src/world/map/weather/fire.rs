//! A wildfire burning this week: flames flickering along its front where the scar meets the land
//! downwind, and their smoke drifting downwind, brown near the fire and grey as it spreads.
//! Held still, the flames stand at their own heights and the smoke stands in a plume.

use std::f64::consts::TAU;

use super::{Pad, Weather};
use crate::world::map::motion::{FIRE, Moment, SMOKE};
use crate::world::map::nature::wind;
use crate::world::raster::noise::{hash3, mix, smooth};
use crate::world::raster::{Colour, Gradient, Path};

/// Draws a burning front's flames and smoke; `front` lies on the flat map.
pub fn draw(
    pad: &mut Pad,
    weather: &Weather,
    front: &[(f64, f64)],
    behavior: usize,
    moment: Moment,
) {
    if front.len() < 3 {
        return;
    }
    let l = behavior as f64;
    let points: Vec<(f64, f64)> = front
        .iter()
        .map(|&(x, y)| weather.screen(x, y, 0.0))
        .collect();
    let still = moment.is_still();
    let flicker = moment.phase(FIRE, 0.0);
    let mut k = 1;
    while k + 1 < points.len() {
        let (x, y) = points[k];
        let kf = k as f64;
        let phase = if still {
            0.5 + 0.3 * (kf * 2.3).sin()
        } else {
            0.5 + 0.5 * (flicker * TAU * (1.0 + (k % 3) as f64) + kf * 1.7).sin()
        };
        let tall = 5.0 + 6.0 * phase * (0.6 + 0.4 * hash3(kf, 2.0, 3.0, l));
        let wide = 2.2 + 0.9 * hash3(kf, 5.0, 1.0, l);
        let glow = Gradient::radial(x, y - 2.0, 0.0, x, y - 2.0, 9.0)
            .stop(0.0, Colour::rgba(255, 150, 50, 0.4))
            .stop(1.0, Colour::rgba(255, 90, 20, 0.0));
        let mut halo = Path::new();
        halo.circle(x, y - 2.0, 9.0);
        pad.fill(&halo, &pad.brush(glow));
        let colour = Gradient::linear(0.0, y, 0.0, y - tall)
            .stop(0.0, Colour::rgba(255, 236, 160, 0.95))
            .stop(0.45, Colour::rgba(255, 140, 40, 0.9))
            .stop(1.0, Colour::rgba(210, 50, 20, 0.0));
        let mut flame = Path::new();
        flame.move_to(x - wide, y);
        flame.quad_to(x - wide * 0.6, y - tall * 0.6, x, y - tall);
        flame.quad_to(x + wide * 0.6, y - tall * 0.6, x + wide, y);
        flame.close();
        pad.fill(&flame, &pad.brush(colour));
        k += 3;
    }
    let (wx, wy) = wind();
    let (mx, my) = points[points.len() >> 1];
    let rising = moment.phase(SMOKE, 0.0);
    for k in 0..5 {
        let kf = f64::from(k);
        let u = if still {
            (kf + 1.0) / 6.0
        } else {
            (rising + kf / 5.0) % 1.0
        };
        let (px, py) = (
            mx + wx * (6.0 + u * 62.0),
            my - 6.0 - u * 22.0 + wy * u * 40.0,
        );
        let pr = 7.0 + 16.0 * u;
        let birth = if still { 1.0 } else { smooth(0.0, 0.12, u) };
        let a = 0.5 * (1.0 - 0.7 * u) * birth * (1.0 - smooth(0.8, 1.0, u));
        let shade = |from: f64, to: f64| mix(from, to, u).trunc() as u8;
        let puff = Gradient::radial(px, py, 0.0, px, py, pr)
            .stop(
                0.0,
                Colour::rgba(
                    shade(120.0, 190.0),
                    shade(100.0, 186.0),
                    shade(86.0, 182.0),
                    a,
                ),
            )
            .stop(1.0, Colour::rgba(160, 156, 150, 0.0));
        let mut round = Path::new();
        round.circle(px, py, pr);
        pad.fill(&round, &pad.brush(puff));
    }
}
