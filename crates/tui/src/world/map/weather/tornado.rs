//! A tornado: a grey funnel standing on the land beside the track it scoured, lit from the west,
//! its shade on the ground and dust at its foot, its bands climbing as it turns once a second.
//! Held still, its bands rest where they are.

use std::f64::consts::PI;

use super::{Pad, Weather};
use crate::world::map::motion::{Moment, TORNADO};
use crate::world::map::nature::Feature;
use crate::world::raster::noise::{Rng, mix};
use crate::world::raster::{Colour, Gradient, Path, Pen};

/// Draws a tornado standing at its place.
pub fn draw(pad: &mut Pad, weather: &Weather, feature: &Feature, moment: Moment) {
    let r = feature.radius - 4.0;
    let (x, y) = weather.screen(feature.x, feature.y, 0.0);
    let phase = moment.phase(TORNADO, 0.0) % 1.0;
    let mut rng = Rng::new((feature.behavior * 17 + 5) as u32);
    rng.next_f64();
    let top = y - r * 2.3;
    let lean = (rng.next_f64() - 0.5) * r * 0.5;

    let shade = Gradient::linear(x, y, x + r * 1.8, y + r * 0.7)
        .stop(0.0, Colour::rgba(10, 10, 14, 0.4))
        .stop(1.0, Colour::rgba(10, 10, 14, 0.0));
    let mut cast = Path::new();
    cast.move_to(x, y);
    cast.line_to(x + r * 1.8, y + r * 0.7);
    pad.stroke(&cast, &Pen::new(3.0), &pad.brush(shade));

    let dust = Gradient::radial(x, y, 0.0, x, y, r * 0.8)
        .stop(0.0, Colour::rgba(150, 126, 100, 0.7))
        .stop(1.0, Colour::rgba(150, 126, 100, 0.0));
    let mut cloud = Path::new();
    cloud.ellipse(x, y, r * 0.8, r * 0.45, 0.0, 0.0, 7.0, false);
    pad.fill(&cloud, &pad.brush(dust));

    let centre = |u: f64| x + lean * (PI * u).sin();
    let half = |u: f64| mix(r * 0.62, 0.9, u.powf(0.7));
    let steps = 16;
    let mut funnel = Path::new();
    for s in 0..=steps {
        let u = f64::from(s) / f64::from(steps);
        let cy = mix(top, y, u);
        if s == 0 {
            funnel.move_to(centre(u) - half(u), cy);
        } else {
            funnel.line_to(centre(u) - half(u), cy);
        }
    }
    for s in (0..=steps).rev() {
        let u = f64::from(s) / f64::from(steps);
        funnel.line_to(centre(u) + half(u), mix(top, y, u));
    }
    funnel.close();
    let body = Gradient::linear(x - r * 0.6, 0.0, x + r * 0.6, 0.0)
        .stop(0.0, Colour::rgba(214, 212, 206, 0.95))
        .stop(0.45, Colour::rgba(160, 156, 150, 0.95))
        .stop(1.0, Colour::rgba(84, 80, 78, 0.95));
    pad.fill(&funnel, &pad.brush(body));

    let bands = pad.brush(Colour::rgba(60, 58, 58, 0.5));
    for k in 0..5 {
        let u = 1.0 - ((f64::from(k) + phase) / 5.0) % 1.0;
        let (v, hw, cx) = (mix(top, y, u), half(u), centre(u));
        let mut band = Path::new();
        band.move_to(cx - hw, v - 1.0);
        band.quad_to(cx, v + 1.6, cx + hw, v - 1.0);
        pad.stroke(&band, &Pen::new(0.7), &bands);
    }

    let base = Gradient::radial(x - r * 0.2, top - r * 0.1, 0.0, x, top, r * 1.15)
        .stop(0.0, Colour::rgba(176, 174, 170, 0.95))
        .stop(0.7, Colour::rgba(110, 108, 106, 0.85))
        .stop(1.0, Colour::rgba(90, 88, 88, 0.0));
    let mut hanging = Path::new();
    hanging.ellipse(x, top, r * 1.15, r * 0.5, 0.0, 0.0, 7.0, false);
    pad.fill(&hanging, &pad.brush(base));
}
