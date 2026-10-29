//! The shadows of clouds drifting east over the land and the sea: the actions no label fits,
//! covering the same share of the map as of the actions, when that is a tenth or more. They fall
//! full on the land, where a passing shade darkens green and sand, but at a quarter over the sea,
//! where a full one reads as a hole, with soft edges, and they keep off the polar ice. Held
//! still, there are none.

use super::Pad;
use crate::world::map::fields::{Ground, HEIGHT, WIDTH, byte};
use crate::world::map::land::Polar;
use crate::world::map::motion::{CLOUD_PERIOD, CLOUD_SPEED, CLOUDS, Moment};
use crate::world::map::relief::{FORESHORTEN, Projected, Relief, top_row};
use crate::world::raster::noise::{hash3, mix, smooth};
use crate::world::raster::{Blend, Brush, Canvas, Colour, Grid};

/// Pixels a point of the shadows' noise.
const CELL: f64 = 4.0;
/// How strongly a shade falls over the sea, against the land.
const OVER_SEA: f64 = 0.25;

/// The drifting shadows of a crew's clouds.
#[derive(Debug, Clone)]
pub struct CloudShadows {
    /// The noise the shadows are cut from, repeating every 192 px across, and the level that
    /// cuts the share of it the clouds cover.
    noise: Grid,
    threshold: f64,
    /// The shadows themselves, one pixel a point of the noise.
    picture: Canvas,
    /// How strongly a shade falls at each pixel of the screen: full on land, a quarter at sea,
    /// none on the ice.
    mask: Canvas,
}

impl CloudShadows {
    /// The shadows covering `cover` of the map, for the land as `land` shows it.
    pub fn new(ground: &Ground, relief: &Relief, land: &Projected, cover: f64) -> Self {
        let seed = ground.seed();
        let across = ((WIDTH as f64 + CLOUD_PERIOD) / CELL).ceil() as usize;
        let down = (HEIGHT as f64 / CELL).ceil() as usize;
        let mut noise = Grid::new(across, down);
        for j in 0..down {
            for i in 0..across {
                let (x, y) = (i as f64 * CELL, j as f64 * CELL * 1.6);
                let v = 0.55 * repeating(x, y, 48.0, seed + 3.0)
                    + 0.3 * repeating(x, y, 24.0, seed + 4.0)
                    + 0.15 * repeating(x, y, 12.0, seed + 5.0);
                noise.set(i, j, v);
            }
        }
        let mut sorted: Vec<f32> = noise.values().to_vec();
        sorted.sort_by(f32::total_cmp);
        let cut = ((sorted.len() as f64 * (1.0 - cover)).floor() as usize).min(sorted.len() - 1);
        let threshold = f64::from(sorted[cut]);
        let mut rgba = vec![0_u8; across * down * 4];
        for (at, v) in noise.values().iter().enumerate() {
            rgba[at * 4 + 3] =
                byte(smooth(threshold - 0.1, threshold + 0.07, f64::from(*v)) * 0.34 * 255.0);
        }
        let picture = Canvas::from_rgba(across as u32, down as u32, &rgba)
            .unwrap_or_else(|| Canvas::new(1, 1));
        Self {
            noise,
            threshold,
            picture,
            mask: mask(ground, relief, land),
        }
    }

    /// How far the shadows have drifted east at a moment, within their repeat.
    fn drift(moment: Moment) -> f64 {
        (moment.time(CLOUDS) * CLOUD_SPEED) % CLOUD_PERIOD
    }

    pub fn draw(&self, pad: &mut Pad, moment: Moment) {
        if let Some(shadows) = self.picture(moment, 1.0) {
            pad.draw(&shadows, 0.0, 0.0, &pad.brush(Colour::WHITE));
        }
    }

    /// The shadows over the whole map at a moment, drawn `scale` times the map's own size each
    /// way; `None` held still. A shade is soft at its edges and fades over a coast through a
    /// ramp a few pixels wide, so at half the size it looks the same stretched back over the map.
    pub fn picture(&self, moment: Moment, scale: f64) -> Option<Canvas> {
        if moment.is_still() {
            return None;
        }
        let width = (WIDTH as f64 * scale).round();
        let height = (HEIGHT as f64 * scale).round();
        let mut shadows = Canvas::new(width as u32, height as u32);
        let (across, down) = (self.picture.width(), self.picture.height());
        shadows.draw_sized(
            &self.picture,
            (Self::drift(moment) - CLOUD_PERIOD) * scale,
            0.0,
            f64::from(across) * CELL * scale,
            f64::from(down) * CELL * scale,
            &Brush::new(Colour::WHITE),
        );
        shadows.draw_sized(
            &self.mask,
            0.0,
            0.0,
            width,
            height,
            &Brush::new(Colour::WHITE).blend(Blend::DestinationIn),
        );
        Some(shadows)
    }

    /// Whether a passing cloud's shade covers a point of the screen at a moment.
    pub fn shades(&self, x: f64, y: f64, moment: Moment) -> bool {
        if moment.is_still() {
            return false;
        }
        let across = x - Self::drift(moment) + CLOUD_PERIOD;
        let i = ((across / CELL).floor().max(0.0) as usize).min(self.noise.width() - 1);
        let j = ((y / CELL).floor().max(0.0) as usize).min(self.noise.height() - 1);
        self.noise.get(i, j) > self.threshold
    }

    /// The share of the map the shadows cover.
    pub fn cover(&self) -> f64 {
        let over = self
            .noise
            .values()
            .iter()
            .filter(|v| f64::from(**v) > self.threshold)
            .count();
        over as f64 / self.noise.values().len() as f64
    }
}

/// Value noise that repeats every 192 px across, so the shadows drift round without a seam.
fn repeating(x: f64, y: f64, cell: f64, seed: f64) -> f64 {
    let n = CLOUD_PERIOD / cell;
    let (xi, yi) = ((x / cell).floor(), (y / cell).floor());
    let (xf, yf) = (x / cell - xi, y / cell - yi);
    let (u, v) = (xf * xf * (3.0 - 2.0 * xf), yf * yf * (3.0 - 2.0 * yf));
    let h = |i: f64, j: f64| hash3(i.rem_euclid(n), j, 7.0, seed);
    mix(
        mix(h(xi, yi), h(xi + 1.0, yi), u),
        mix(h(xi, yi + 1.0), h(xi + 1.0, yi + 1.0), u),
        v,
    )
}

/// How strongly a shade falls at each pixel of the screen. It follows the land as it stands,
/// so a shade fades as it drifts off a coast, and fades out over the last stretch of sea
/// before the polar ice.
fn mask(ground: &Ground, relief: &Relief, land: &Projected) -> Canvas {
    let polar = Polar::of(ground);
    let scale = WIDTH as f64 / std::f64::consts::TAU;
    let row_of =
        |lat: f64| (top_row() + FORESHORTEN * (HEIGHT as f64 / 2.0 - lat * scale)) / HEIGHT as f64;
    let ramp = |y: f64, a: f64, b: f64| smooth(a * HEIGHT as f64, b * HEIGHT as f64, y);
    let mut rgba = vec![0_u8; WIDTH * HEIGHT * 4];
    for y in 0..HEIGHT {
        let row = y as f64 + 0.5;
        let mut off_ice = 1.0;
        if polar.cap_north.is_some() {
            let edge = polar.edge_north();
            off_ice *= ramp(row, row_of(edge - 0.1), row_of(edge - 0.2));
        }
        if polar.cap_south.is_some() {
            let edge = polar.edge_south();
            off_ice *= 1.0 - ramp(row, row_of(-(edge - 0.2)), row_of(-(edge - 0.1)));
        }
        for x in 0..WIDTH {
            let shown = land.rows[y * WIDTH + x];
            let on_land = if shown < 0.0 {
                0.0
            } else {
                let row = ((f64::from(shown) - 0.5).round().max(0.0) as usize).min(HEIGHT - 1);
                smooth(-1.0, 3.0, relief.coast.get(x, row))
            };
            rgba[(y * WIDTH + x) * 4 + 3] = byte(255.0 * off_ice * mix(OVER_SEA, 1.0, on_land));
        }
    }
    Canvas::from_rgba(WIDTH as u32, HEIGHT as u32, &rgba).unwrap_or_else(|| Canvas::new(1, 1))
}
