//! A hurricane, typhoon or cyclone: a spiral of cloud round a clear eye, turning as a real storm
//! turns where it stands, anticlockwise north of the equator and clockwise south of it, faster
//! the higher its category. It flies a little above the ground, its shade on the sea below.

use std::f64::consts::PI;

use super::Pad;
use crate::world::map::fields::{Rgb, byte, mix_rgb};
use crate::world::map::motion::{HURRICANE, HURRICANE_DEGREES, Moment};
use crate::world::map::nature::Hurricane;
use crate::world::map::relief::FORESHORTEN;
use crate::world::raster::noise::{fbm3, smooth};
use crate::world::raster::{Canvas, Colour, Transform};

/// How far above the ground a hurricane flies, in pixels: small, so it stays off the dots.
pub const ALTITUDE: f64 = 12.0;

/// A hurricane's cloud seen from above at radius `r`, as a northern storm winds: its arms the
/// same on both sides, so it repeats every half turn.
pub fn sprite(r: f64, seed: f64) -> Canvas {
    let side = (r * 2.5).ceil() as usize;
    let half = side as f64 / 2.0;
    let mut rgba = vec![0_u8; side * side * 4];
    for j in 0..side {
        for i in 0..side {
            let dx = (i as f64 + 0.5 - half) / r;
            let dy = (j as f64 + 0.5 - half) / r;
            let rr = dx.hypot(dy);
            if rr > 1.2 {
                continue;
            }
            let theta = dy.atan2(dx);
            let noise = 0.5
                * (fbm3(dx * 4.0, dy * 4.0, seed, 3.0, 4)
                    + fbm3(-dx * 4.0, -dy * 4.0, seed, 3.0, 4))
                * 1.3;
            let arm = 0.5 + 0.5 * (2.0 * (theta - 2.4 * (rr + 0.04).ln()) + 1.3 * noise).cos();
            let core = (-(rr / 0.36).powf(2.2)).exp();
            let mut density =
                0.95 * core + 0.85 * arm.powf(1.6) * smooth(1.18, 0.25, rr) + 0.28 * noise;
            density *= smooth(0.055, 0.11, rr);
            density += 0.25 * (-((rr - 0.12) / 0.03).powi(2)).exp();
            let alpha = density.clamp(0.0, 1.0) * 0.94 * smooth(1.2, 0.95, rr);
            if alpha <= 0.01 {
                continue;
            }
            let colour: Rgb = mix_rgb(
                [168.0, 178.0, 196.0],
                [252.0, 253.0, 255.0],
                (density * 1.1).clamp(0.0, 1.0),
            );
            let at = (j * side + i) * 4;
            rgba[at] = byte(colour[0]);
            rgba[at + 1] = byte(colour[1]);
            rgba[at + 2] = byte(colour[2]);
            rgba[at + 3] = byte(alpha * 255.0);
        }
    }
    Canvas::from_rgba(side as u32, side as u32, &rgba).unwrap_or_else(|| Canvas::new(1, 1))
}

/// The seed a hurricane's cloud is drawn with: its behavior and its category.
pub fn seed(behavior: usize, storm: &Hurricane) -> f64 {
    (behavior + 3 * storm.category as usize) as f64
}

/// How far a hurricane has turned at a moment, in its frames: 12° a second for each category,
/// a frame each 180° over its frames (7.5° at 8 steps a second), back to 0 each half turn, which
/// its two arms make look like a whole one. Held still, 0.
pub fn frame(storm: &Hurricane, moment: Moment) -> u32 {
    let Moment::At { t, rate } = moment else {
        return 0;
    };
    let frames = HURRICANE.at(rate).frames;
    let degrees = HURRICANE_DEGREES * f64::from(storm.category) * t;
    let turned = (degrees / 180.0 * f64::from(frames) + 1e-9).floor() as u64;
    (turned % u64::from(frames)) as u32
}

/// How far a hurricane has turned at a moment, in radians clockwise on the screen, a frame at a
/// time: anticlockwise in the north and clockwise in the south.
pub fn angle(storm: &Hurricane, moment: Moment) -> f64 {
    let Moment::At { rate, .. } = moment else {
        return 0.0;
    };
    let step = PI / f64::from(HURRICANE.at(rate).frames);
    let sense = if storm.south { 1.0 } else { -1.0 };
    sense * f64::from(frame(storm, moment)) * step
}

/// Draws a hurricane's cloud, or its shade, centred at (x, y) on the map, turned by `angle` and
/// foreshortened as the map is; the south's arms wind the other way.
pub fn draw(
    pad: &mut Pad,
    cloud: &Canvas,
    (x, y): (f64, f64),
    angle: f64,
    south: bool,
    shade: bool,
) {
    let mut turn = Transform::IDENTITY
        .translate(x, y)
        .scale(1.0, FORESHORTEN)
        .rotate(angle);
    if south {
        turn = turn.scale(1.0, -1.0);
    }
    let mut brush = pad.brush(Colour::WHITE).at(pad.place(turn));
    if shade {
        brush = brush.alpha(0.3).brightness(0.0).blur(1.5);
    }
    let (w, h) = (f64::from(cloud.width()), f64::from(cloud.height()));
    pad.draw(cloud, -w / 2.0, -h / 2.0, &brush);
}
