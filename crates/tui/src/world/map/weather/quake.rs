//! An earthquake: a session ended on its behavior because its CLI crashed in the last 24 hours.
//! Broken amber rings run out over the ground and the dots from the middle of its fault, one for
//! each such crash up to three, spaced through a 2 s loop and fading as they spread; held still,
//! they rest at their spacing. Amber and broken, they never read as the pointer's white ring.

use super::{Pad, Weather};
use crate::world::map::motion::{Moment, QUAKE};
use crate::world::map::relief::FORESHORTEN;
use crate::world::raster::{Colour, Gradient, Path, Pen};

/// How far the rings run out, in pixels.
const REACH: f64 = 34.0;

/// Draws an earthquake's rings round its place, `rings` of them.
pub fn draw(pad: &mut Pad, weather: &Weather, place: (f64, f64), rings: usize, moment: Moment) {
    let at = weather.screen(place.0, place.1, 0.0);
    let u = moment.phase(QUAKE, 0.35) % 1.0;
    ring_out(pad, at, REACH, u, rings.clamp(1, 3), FORESHORTEN * 0.8);
}

/// Rings running out from (x, y) to `reach` px, `u` of the way through their loop, squashed
/// down by `squash`, with the glowing epicentre under them.
pub fn ring_out(pad: &mut Pad, (x, y): (f64, f64), reach: f64, u: f64, rings: usize, squash: f64) {
    let glow = Gradient::radial(x, y, 0.0, x, y, 9.0)
        .stop(0.0, Colour::rgba(255, 200, 120, 0.85))
        .stop(1.0, Colour::rgba(255, 160, 80, 0.0));
    let mut epicentre = Path::new();
    epicentre.circle(x, y, 9.0);
    pad.fill(&epicentre, &pad.brush(glow));
    let thousandths = |v: f64| (v * 1000.0).round() / 1000.0;
    for k in 0..rings {
        let v = (u + k as f64 / rings as f64) % 1.0;
        let r = 6.0 + v * reach;
        let fade = 1.0 - v * v;
        let mut ring = Path::new();
        ring.ellipse(x, y, r, r * squash, 0.0, 0.0, 7.0, false);
        let amber = Colour::rgba(255, 190, 96, thousandths(0.95 * fade));
        pad.stroke(&ring, &Pen::new(2.2).dash(&[6.0, 3.0]), &pad.brush(amber));
        let inner = (r - 5.0).max(2.0);
        let mut echo = Path::new();
        echo.ellipse(x, y, inner, inner * squash, 0.0, 0.0, 7.0, false);
        let pale = Colour::rgba(255, 226, 170, thousandths(0.6 * fade));
        pad.stroke(&echo, &Pen::new(1.0).dash(&[6.0, 3.0]), &pad.brush(pale));
    }
}
