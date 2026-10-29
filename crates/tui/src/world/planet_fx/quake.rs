//! An earthquake on the globe: broken amber rings running out over the ground from the middle
//! of its fault, one for each CLI crash in the last 24 hours, lying flat on the globe. Amber and
//! broken, so they never look like the pointer's white ring.

use std::f64::consts::TAU;

use super::{GlobeGround, Kind, Motion, Weather};
use crate::world::raster::{Brush, Canvas, Colour, Gradient, Path, Pen, Transform};

/// How far the rings run out, in the map's pixels.
const REACH: f64 = 34.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Quake {
    /// The middle of its fault, on the map.
    pub at: (f64, f64),
    /// Crashes in the last 24 hours, one ring each, up to 3.
    pub crashes: u32,
}

impl Weather for Quake {
    fn kind(&self) -> Kind {
        Kind::Quake
    }

    fn place(&self) -> (f64, f64) {
        self.at
    }

    fn motion(&self) -> Motion {
        Motion::QUAKE
    }

    fn moves_on(&self, radius: f64) -> bool {
        radius >= 120.0
    }

    fn draw(&self, canvas: &mut Canvas, ground: &GlobeGround, t: f64, still: bool) {
        let Some(spot) = ground.at(self.at.0, self.at.1) else {
            return;
        };
        if REACH * spot.s < Kind::Quake.reads() {
            return;
        }
        let u = if still {
            0.35
        } else {
            (t / Motion::QUAKE.seconds).rem_euclid(1.0)
        };
        rings(
            canvas,
            spot.on_ground(),
            spot.shows,
            u,
            self.crashes.clamp(1, 3),
        );
    }
}

/// The epicentre's glow and `count` rings `u` of the way through their run out, each with a
/// fainter echo just inside it, drawn through `at`.
fn rings(canvas: &mut Canvas, at: Transform, alpha: f64, u: f64, count: u32) {
    let glow = Gradient::radial(0.0, 0.0, 0.0, 0.0, 0.0, 9.0)
        .stop(0.0, Colour::rgba(255, 200, 120, 0.85))
        .stop(1.0, Colour::rgba(255, 160, 80, 0.0));
    let mut centre = Path::new();
    centre.circle(0.0, 0.0, 9.0);
    canvas.fill(&centre, &Brush::new(glow).at(at).alpha(alpha));
    for k in 0..count {
        let v = (u + f64::from(k) / f64::from(count)).rem_euclid(1.0);
        let r = 6.0 + v * REACH;
        let strength = 1.0 - v * v;
        for (radius, colour, width) in [
            (r, Colour::rgba(255, 190, 96, 0.95 * strength), 2.2),
            (
                (r - 5.0).max(2.0),
                Colour::rgba(255, 226, 170, 0.6 * strength),
                1.0,
            ),
        ] {
            let mut ring = Path::new();
            ring.ellipse(0.0, 0.0, radius, radius, 0.0, 0.0, TAU, false);
            let pen = Pen::new(width).dash(&[6.0, 3.0]);
            canvas.stroke(&ring, &pen, &Brush::new(colour).at(at).alpha(alpha));
        }
    }
}
