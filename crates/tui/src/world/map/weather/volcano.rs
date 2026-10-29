//! A volcano: a cone standing on the land, lit from the west, gullied down its flanks, its foot
//! fading into the land and its shade thrown east-south-east. One whose actions failed this
//! week smokes, lava running down its flanks, puffs rising from its crater and drifting
//! downwind; an older one is a quiet dark crater.

use super::{Pad, Weather, shade_run};
use crate::world::map::motion::{Moment, SMOKE};
use crate::world::map::nature::wind;
use crate::world::map::relief::FORESHORTEN;
use crate::world::raster::noise::{Rng, mix, smooth};
use crate::world::raster::{Blend, Brush, Canvas, Cap, Colour, Gradient, Path, Pen};

/// A cone's picture, the point of it that stands on its place, and how tall it stands.
#[derive(Debug, Clone)]
pub struct Cone {
    pub canvas: Canvas,
    pub foot: (f64, f64),
    pub height: f64,
}

/// The cone of a volcano of radius `r`, smoking or quiet.
pub fn cone(r: f64, smoking: bool, seed: f64) -> Cone {
    let (width, height) = ((r * 4.2 + 10.0).ceil(), (r * 2.2 + 10.0).ceil());
    let mut canvas = Canvas::new(width as u32, height as u32);
    let gx = (r * 1.3 + 4.0).ceil();
    let gy = height - (r * 0.45 + 4.0).ceil();
    let tall = r * 0.9;
    let (top, crater, ry) = (gy - tall, r * 0.3, r * 0.34);

    // Its shade on the ground.
    let (run, (ux, uy)) = shade_run();
    let fall = tall * run;
    let (ax, ay) = (gx + ux * fall, gy + uy * fall * FORESHORTEN);
    let mut shade = Path::new();
    shade.move_to(gx - r * 0.2, gy - ry * 0.9);
    shade.line_to(ax, ay);
    shade.line_to(gx + r * 0.1, gy + ry * 0.95);
    shade.close();
    canvas.fill(&shade, &Brush::new(Colour::rgba(8, 8, 14, 0.34)).blur(1.5));

    // The cone: lit on the west, in shade on the east.
    let body = Gradient::linear(gx - r, 0.0, gx + r, 0.0)
        .stop(0.0, Colour::rgb(172, 152, 128))
        .stop(0.35, Colour::rgb(136, 118, 100))
        .stop(0.7, Colour::rgb(84, 72, 64))
        .stop(1.0, Colour::rgb(48, 42, 40));
    let mut slopes = Path::new();
    slopes.move_to(gx - r, gy);
    slopes.line_to(gx - crater, top);
    slopes.line_to(gx + crater, top);
    slopes.line_to(gx + r, gy);
    slopes.ellipse(gx, gy, r, ry, 0.0, 0.0, std::f64::consts::PI, false);
    slopes.close();
    canvas.fill(&slopes, &Brush::new(body));
    let mut rng = Rng::new((seed * 13.0 + 9.0) as u32);
    for k in 0..9 {
        let u = (f64::from(k) + 0.5) / 9.0 * 2.0 - 1.0 + (rng.next_f64() - 0.5) * 0.08;
        let (bx, by) = (gx + u * r, gy + (1.0 - u * u).max(0.0).sqrt() * ry * 0.9);
        let mut gully = Path::new();
        gully.move_to(gx + u * crater, top + 1.0);
        gully.line_to(bx, by);
        let shade = if u < 0.0 {
            Colour::rgba(40, 30, 26, 0.32)
        } else {
            Colour::rgba(18, 14, 14, 0.38)
        };
        canvas.stroke(&gully, &Pen::new(0.8), &Brush::new(shade));
    }
    // The foot fades into the land.
    let foot = Gradient::linear(0.0, gy - ry * 0.6, 0.0, gy + ry)
        .stop(0.0, Colour::rgba(0, 0, 0, 0.0))
        .stop(1.0, Colour::rgba(0, 0, 0, 0.75));
    canvas.fill_rect(
        gx - r - 2.0,
        gy - ry,
        2.0 * r + 4.0,
        2.0 * ry + 4.0,
        &Brush::new(foot).blend(Blend::DestinationOut),
    );
    let mut mouth = Path::new();
    mouth.ellipse(gx, top, crater, crater * 0.36, 0.0, 0.0, 7.0, false);
    if smoking {
        let mut rng = Rng::new((seed * 31.0 + 1.0) as u32);
        for k in 0..3 {
            let u = -0.45 + f64::from(k) * 0.42 + (rng.next_f64() - 0.5) * 0.15;
            let (ex, ey) = (gx + u * r * 0.95, gy + (1.0 - u * u).sqrt() * ry * 0.7);
            let lava = Gradient::linear(gx, top, ex, ey)
                .stop(0.0, Colour::rgba(255, 200, 90, 1.0))
                .stop(0.6, Colour::rgba(236, 84, 30, 0.9))
                .stop(1.0, Colour::rgba(120, 30, 20, 0.0));
            let mut flow = Path::new();
            flow.move_to(gx + u * crater, top + 1.0);
            flow.quad_to(gx + u * r * 0.55, mix(top, gy, 0.45), ex, ey);
            canvas.stroke(&flow, &Pen::new(1.5).cap(Cap::Round), &Brush::new(lava));
        }
        let glow = Gradient::radial(gx, top, 0.0, gx, top, crater * 1.2)
            .stop(0.0, Colour::rgb(255, 230, 150))
            .stop(0.5, Colour::rgb(255, 112, 40))
            .stop(1.0, Colour::rgb(96, 26, 18));
        canvas.fill(&mouth, &Brush::new(glow));
    } else {
        canvas.fill(&mouth, &Brush::new(Colour::rgb(34, 28, 26)));
    }
    let mut rim = Path::new();
    rim.ellipse(
        gx,
        top,
        crater + 0.8,
        crater * 0.36 + 0.6,
        0.0,
        0.0,
        7.0,
        false,
    );
    canvas.stroke(
        &rim,
        &Pen::new(0.9).cap(Cap::Round),
        &Brush::new(Colour::rgba(214, 196, 176, 0.65)),
    );
    Cone {
        canvas,
        foot: (gx, gy),
        height: tall,
    }
}

/// Draws a volcano standing at its place, its smoke rising when it smokes: puffs from the
/// crater drifting downwind, a new one every 0.75 s; held still, the plume stands.
pub fn draw(
    pad: &mut Pad,
    weather: &Weather,
    cone: &Cone,
    place: (f64, f64),
    r: f64,
    smoking: bool,
    moment: Moment,
) {
    let (x, y) = weather.screen(place.0, place.1, 0.0);
    pad.draw(
        &cone.canvas,
        x - cone.foot.0,
        y - cone.foot.1,
        &pad.brush(Colour::WHITE),
    );
    if !smoking {
        return;
    }
    let (wx, wy) = wind();
    let top = y - cone.height;
    let phase = moment.phase(SMOKE, 0.0);
    for k in 0..6 {
        let k = f64::from(k);
        let u = if moment.is_still() {
            (k + 1.0) / 7.0
        } else {
            (phase + k / 6.0) % 1.0
        };
        let (px, py) = (x + wx * u * r * 2.8, top - u * r * 2.6 + wy * u * r * 1.4);
        let pr = r * (0.2 + 0.55 * u);
        let fade = if moment.is_still() {
            1.0
        } else {
            smooth(0.0, 0.1, u)
        };
        let a = 0.6 * (1.0 - u).powf(1.1) * fade;
        let puff = Gradient::radial(px - pr * 0.25, py - pr * 0.3, 0.0, px, py, pr)
            .stop(0.0, Colour::rgba(226, 222, 218, a))
            .stop(0.6, Colour::rgba(150, 146, 144, a * 0.7))
            .stop(1.0, Colour::rgba(120, 118, 118, 0.0));
        let mut round = Path::new();
        round.circle(px, py, pr);
        pad.fill(&round, &pad.brush(puff));
    }
}
