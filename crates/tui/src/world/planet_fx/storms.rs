//! The storms on the globe: hurricanes turning, thunderstorms flashing, tornadoes climbing.

use std::f64::consts::{PI, TAU};
use std::sync::Arc;

use super::{GlobeGround, Kind, Motion, Weather};
use crate::world::map::motion::Flashes;
use crate::world::raster::noise::{Rng, hash3_wrapping, mix};
use crate::world::raster::{Blend, Brush, Canvas, Colour, Gradient, Join, Path, Pen};

/// Globes this big or bigger show the motions only big planets read: everything but hurricanes.
const BIG: f64 = 120.0;
/// Pixels a radian on the map, which the small planet's sizes are read against.
const MAP_SCALE: f64 = 220.0;

/// A hurricane: the map's own picture of it lying on the sea at its place, turned at its
/// category's speed, with its shade on the sea to the east.
#[derive(Debug, Clone)]
pub struct Hurricane {
    pub at: (f64, f64),
    /// Its radius on the map, in pixels.
    pub radius: f64,
    pub category: u8,
    /// South of the equator it turns clockwise and its arms are mirrored.
    pub south: bool,
    /// The map's picture of it, centred.
    pub picture: Arc<Canvas>,
}

impl Hurricane {
    /// How far it has turned `t` seconds in: 12° a second a category, clockwise on the screen in
    /// the south and anticlockwise in the north, counted round half a turn, which looks the same
    /// as a whole one.
    pub fn angle(&self, t: f64) -> f64 {
        let degrees = 12.0 * f64::from(self.category) * t;
        let sense = if self.south { 1.0 } else { -1.0 };
        sense * degrees.rem_euclid(180.0) * PI / 180.0
    }
}

impl Weather for Hurricane {
    fn kind(&self) -> Kind {
        Kind::Hurricane
    }

    fn place(&self) -> (f64, f64) {
        self.at
    }

    fn motion(&self) -> Motion {
        Motion::HURRICANE
    }

    fn moves_on(&self, radius: f64) -> bool {
        self.radius * 1.25 * radius / MAP_SCALE >= Kind::Hurricane.reads()
    }

    fn draw(&self, canvas: &mut Canvas, ground: &GlobeGround, t: f64, still: bool) {
        let Some(spot) = ground.at(self.at.0, self.at.1) else {
            return;
        };
        let half = f64::from(self.picture.width()) / 2.0;
        if half * spot.s < Kind::Hurricane.reads() {
            return;
        }
        let angle = if still { 0.0 } else { self.angle(t) };
        let mirror = if self.south { -1.0 } else { 1.0 };
        let base = spot.on_ground();
        let shade = Brush::new(Colour::BLACK)
            .at(base.translate(5.5, 0.0).rotate(angle).scale(1.0, mirror))
            .alpha(0.3 * spot.shows * spot.day)
            .brightness(0.0)
            .blur(1.5);
        canvas.draw(&self.picture, -half, -half, &shade);
        let body = Brush::new(Colour::WHITE)
            .at(base.rotate(angle).scale(1.0, mirror))
            .alpha(spot.shows)
            .brightness(spot.brightness());
        canvas.draw(&self.picture, -half, -half, &body);
    }
}

/// A thunderstorm: its cloud from above, and one flash for each session stopped there, up to 4
/// in each 4 seconds, each a quarter second. The flash lights the cloud from inside and throws a
/// zigzag bolt out of its edge; on the night side it is the brightest thing on the planet. Held
/// still, the bolt stays drawn, so the meaning reads.
#[derive(Debug, Clone)]
pub struct Thunderstorm {
    pub at: (f64, f64),
    /// Its radius on the map, in pixels.
    pub radius: f64,
    /// Its place in nature's list, which picks each bolt's zigzag.
    pub seed: u32,
    /// Sessions stopped there.
    pub stopped: u32,
    /// When it flashes, in step with every other storm on the planet.
    pub flashes: Flashes,
    /// The way the wind blows on the map, a unit vector.
    pub wind: (f64, f64),
    /// The map's picture of its cloud, centred.
    pub picture: Arc<Canvas>,
}

impl Thunderstorm {
    /// How bright the flash is `t` seconds in, from 0, and which of the flashes it is, from 1.
    pub fn flash(&self, t: f64, still: bool) -> (f64, u32) {
        if still {
            return (0.75, 0);
        }
        self.flashes.at(t)
    }
}

impl Weather for Thunderstorm {
    fn kind(&self) -> Kind {
        Kind::Thunderstorm
    }

    fn place(&self) -> (f64, f64) {
        self.at
    }

    fn motion(&self) -> Motion {
        Motion::LIGHTNING
    }

    fn moves_on(&self, radius: f64) -> bool {
        radius >= BIG
    }

    #[cfg(test)]
    fn flashes(&self) -> Option<Flashes> {
        Some(self.flashes)
    }

    fn draw(&self, canvas: &mut Canvas, ground: &GlobeGround, t: f64, still: bool) {
        let Some(spot) = ground.at(self.at.0, self.at.1) else {
            return;
        };
        let r = self.radius.round();
        if r * 2.0 * spot.s < Kind::Thunderstorm.reads() {
            return;
        }
        let half = f64::from(self.picture.width()) / 2.0;
        let (flash, which) = self.flash(t, still);
        let base = spot.on_ground();
        let shade = Brush::new(Colour::BLACK)
            .at(base)
            .alpha(0.3 * spot.shows * spot.day)
            .brightness(0.0);
        canvas.draw(&self.picture, -half + 5.0, -half + 7.0, &shade);
        let body = Brush::new(Colour::WHITE)
            .at(base)
            .alpha(spot.shows)
            .brightness(spot.brightness());
        canvas.draw(&self.picture, -half, -half, &body);
        if flash <= 0.0 {
            return;
        }
        let (fx, fy) = (-self.wind.0 * r * 0.15, -self.wind.1 * r * 0.15);
        let night = 1.0 - spot.day;
        let reach = r * (0.75 + 0.5 * night);
        let light = Gradient::radial(fx, fy, 0.0, fx, fy, reach)
            .stop(
                0.0,
                Colour::rgba(214, 226, 255, (0.55 + 0.35 * night) * flash * spot.shows),
            )
            .stop(1.0, Colour::rgba(190, 210, 255, 0.0));
        let mut glow = Path::new();
        glow.circle(fx, fy, reach);
        canvas.fill(&glow, &Brush::new(light).at(base).blend(Blend::Lighter));
        // The bolt zigzags, each leg turning the other way.
        let mut rng = Rng::new(self.seed.wrapping_mul(19).wrapping_add(7 + which * 101));
        let start = 1.2 + rng.next_f64() * 0.8;
        let mut bolt = vec![(start.cos() * r * 0.55, start.sin() * r * 0.55)];
        for k in 0..4 {
            let last = bolt[k];
            let turn = if k % 2 == 1 { 1.0 } else { -1.0 };
            let leg = start + turn * (0.6 + 0.25 * rng.next_f64());
            bolt.push((last.0 + leg.cos() * r * 0.27, last.1 + leg.sin() * r * 0.27));
        }
        let mut path = Path::new();
        for (index, (x, y)) in bolt.iter().enumerate() {
            if index == 0 {
                path.move_to(*x, *y);
            } else {
                path.line_to(*x, *y);
            }
        }
        for (colour, width) in [
            (Colour::rgba(150, 170, 255, 0.55), 3.4),
            (Colour::rgba(255, 250, 214, 1.0), 1.4),
        ] {
            let pen = Pen::new(width / spot.s.max(0.6)).join(Join::Miter);
            let brush = Brush::new(colour)
                .at(base)
                .alpha(flash * spot.shows)
                .blend(Blend::Lighter);
            canvas.stroke(&path, &pen, &brush);
        }
    }
}

/// A tornado standing upright at its place, as the wonders stand on the globe, its bands climbing
/// one turn a second, with the dust it raises at its foot.
#[derive(Debug, Clone)]
pub struct Tornado {
    pub at: (f64, f64),
    /// Its radius on the map, in pixels.
    pub radius: f64,
    /// Its place in nature's list, which picks its lean.
    pub seed: u32,
}

impl Weather for Tornado {
    fn kind(&self) -> Kind {
        Kind::Tornado
    }

    fn place(&self) -> (f64, f64) {
        self.at
    }

    fn motion(&self) -> Motion {
        Motion::TORNADO
    }

    fn moves_on(&self, radius: f64) -> bool {
        radius >= BIG
    }

    fn draw(&self, canvas: &mut Canvas, ground: &GlobeGround, t: f64, still: bool) {
        let Some(spot) = ground.at(self.at.0, self.at.1) else {
            return;
        };
        let k = ground.radius() / 240.0;
        let r = (self.radius - 4.0) * k;
        if r * 2.3 < Kind::Tornado.reads() * 2.0 {
            return;
        }
        let phase = if still {
            0.0
        } else {
            (t / Motion::TORNADO.seconds).rem_euclid(1.0)
        };
        let (x, y) = (spot.x, spot.y);
        let top = y - r * 2.3;
        let lean = (hash3_wrapping(f64::from(self.seed), 5.0, 17.0) - 0.5) * r * 0.5;
        let mut brush = Brush::new(Colour::WHITE).alpha(spot.shows * (0.4 + 0.6 * spot.day));
        if spot.day < 0.99 {
            brush = brush.brightness(0.18 + 0.82 * spot.day);
        }
        let dust = Gradient::radial(x, y, 0.0, x, y, r * 0.8)
            .stop(0.0, Colour::rgba(150, 126, 100, 0.7))
            .stop(1.0, Colour::rgba(150, 126, 100, 0.0));
        let mut foot = Path::new();
        foot.ellipse(x, y, r * 0.8, r * 0.45, 0.0, 0.0, TAU, false);
        canvas.fill(&foot, &brush.clone().ink(dust));

        const STEPS: usize = 14;
        let centre_at = |u: f64| x + lean * (PI * u).sin();
        let half_width = |u: f64| mix(r * 0.62, 0.9 * k, u.powf(0.7));
        let mut body = Path::new();
        for step in 0..=STEPS {
            let u = step as f64 / STEPS as f64;
            let (px, py) = (centre_at(u) - half_width(u), mix(top, y, u));
            if step == 0 {
                body.move_to(px, py);
            } else {
                body.line_to(px, py);
            }
        }
        for step in (0..=STEPS).rev() {
            let u = step as f64 / STEPS as f64;
            body.line_to(centre_at(u) + half_width(u), mix(top, y, u));
        }
        body.close();
        let shaded = Gradient::linear(x - r * 0.6, 0.0, x + r * 0.6, 0.0)
            .stop(0.0, Colour::rgba(214, 212, 206, 0.95))
            .stop(0.45, Colour::rgba(160, 156, 150, 0.95))
            .stop(1.0, Colour::rgba(84, 80, 78, 0.95));
        canvas.fill(&body, &brush.clone().ink(shaded));
        // The bands, climbing as it turns.
        let band_ink = brush.clone().ink(Colour::rgba(60, 58, 58, 0.55));
        for band in 0..5 {
            let u = 1.0 - ((f64::from(band) + phase) / 5.0).rem_euclid(1.0);
            let (v, h, c) = (mix(top, y, u), half_width(u), centre_at(u));
            let mut line = Path::new();
            line.move_to(c - h, v - 1.0);
            line.quad_to(c, v + 1.6 * k, c + h, v - 1.0);
            canvas.stroke(&line, &Pen::new(0.8), &band_ink);
        }
        let cloud = Gradient::radial(x - r * 0.2, top - r * 0.1, 0.0, x, top, r * 1.15)
            .stop(0.0, Colour::rgba(176, 174, 170, 0.95))
            .stop(0.7, Colour::rgba(110, 108, 106, 0.85))
            .stop(1.0, Colour::rgba(90, 88, 88, 0.0));
        let mut base = Path::new();
        base.ellipse(x, top, r * 1.15, r * 0.5, 0.0, 0.0, TAU, false);
        canvas.fill(&base, &brush.ink(cloud));
    }
}
