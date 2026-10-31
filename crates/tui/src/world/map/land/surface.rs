//! The ground's colour at a pixel of the map.

use std::f64::consts::SQRT_2;

use super::coast_at;
use super::compare::Tint;
use super::ice::{Polar, polar_at};
use super::sea::{BEACH, SeaColours};
use crate::base::draw::geo::Plate;
use crate::world::map::climate::land_cover;
use crate::world::map::fields::{
    COARSE, Ground, HEIGHT, Look, Rgb, WIDTH, fine, grey, mix_rgb, scale_rgb,
};
use crate::world::raster::noise::smooth;

/// The land a comparison leaves untinted is mostly grey, so the tint stands out.
const CALM: f64 = 0.62;
const TINT_GAIN: f64 = 1.3;
const RED: Rgb = [222.0, 78.0, 92.0];
const BLUE: Rgb = [92.0, 112.0, 236.0];
/// The counts the step lines mark.
const STEPS: [f64; 4] = [3.0, 10.0, 30.0, 100.0];

/// The sea with what lies under its water at a pixel.
pub type UnderWater = dyn Fn(f64, f64, Rgb) -> Rgb + Sync;
/// The land that sank under the water at a pixel: how much of it shows, and its colour.
pub type Sunk = dyn Fn(f64, f64) -> Option<(f64, Rgb)> + Sync;

/// What nature lays on the ground and under the water, for the ground to draw in.
#[derive(Clone, Copy, Default)]
pub struct Marks<'a> {
    /// Marks lying on the ground, as straight-alpha RGBA over the whole map: lakes, rivers,
    /// faults, scars and the rest.
    pub ground: Option<&'a [u8]>,
    /// Burn scars among those marks, as straight-alpha RGBA over the whole map: the light on
    /// them is kept from falling too dark.
    pub scar: Option<&'a [u8]>,
    /// The sea with what lies under its water at a pixel, such as a shoal's bank.
    pub under_water: Option<&'a UnderWater>,
    /// The land that sank under the water at a pixel: how much of it shows, and its colour.
    pub sunk: Option<&'a Sunk>,
    /// Whether the icebergs show: they come and go with the rest of nature.
    pub icebergs: bool,
}

/// The colour at a pixel, how much of it is land, and how far inside the coast it lies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shown {
    pub colour: Rgb,
    pub land: f64,
    pub coast: f64,
}

/// How the ground is painted.
#[derive(Clone, Copy)]
pub struct Surface<'a> {
    pub ground: &'a Ground,
    pub sea: SeaColours,
    pub tint: Option<&'a Tint>,
    /// Lines on the slopes at 3, 10, 30 and 100 actions, lit on the sun's side.
    pub step_lines: bool,
    /// The flat map's own light, for a picture no relief lights.
    pub shade: bool,
    /// How frozen the land is, 0 to 1.
    pub cold: f64,
    pub polar: Option<&'a Polar>,
    pub marks: Marks<'a>,
}

impl<'a> Surface<'a> {
    /// The ground as the map draws it under its light: the map's own sea, step lines, the
    /// crew's cold and ice.
    pub fn map(ground: &'a Ground, polar: Option<&'a Polar>) -> Self {
        Self {
            ground,
            sea: SeaColours::MAP,
            tint: None,
            step_lines: true,
            shade: false,
            cold: cold(ground),
            polar,
            marks: Marks::default(),
        }
    }

    /// The colour at pixel (x, y), with the coast's distance there when it is already known.
    pub fn at(&self, x: f64, y: f64, coast: Option<f64>) -> Shown {
        let ground = self.ground;
        let g = COARSE as f64;
        let (gx, gy) = (x / g, y / g);
        let level = ground.land.sample(gx, gy);
        let coast = coast.unwrap_or_else(|| coast_at(ground, x, y));
        let sea_colours = &self.sea;
        let mut sea = if level >= 0.9 {
            mix_rgb(
                sea_colours.shallow,
                sea_colours.surf,
                smooth(0.9, 1.0, level) * 0.75,
            )
        } else if level >= 0.5 {
            mix_rgb(
                sea_colours.shelf,
                sea_colours.shallow,
                smooth(0.5, 0.9, level),
            )
        } else {
            mix_rgb(
                sea_colours.abyss,
                sea_colours.shelf,
                smooth(0.12, 0.5, level),
            )
        };
        if let Some(under_water) = self.marks.under_water {
            sea = under_water(x, y, sea);
        }
        // Surf: a thin pale line on the beach, not a glow round every coast.
        if coast < 0.0 && coast > -8.0 {
            sea = mix_rgb(sea, sea_colours.foam, 0.22 * (coast / 1.1).exp());
        }
        let land = (coast + 0.5).clamp(0.0, 1.0);
        let mut colour = sea;
        if land > 0.0 {
            let height = ground.height.sample(gx, gy);
            let mut cover = land_cover(ground, height, x, y, gx, gy, self.cold);
            if level < 1.08 {
                cover = mix_rgb(
                    cover,
                    BEACH,
                    smooth(1.08, 1.0, level) * 0.45 * (1.0 - self.cold),
                );
            }
            if let Some(tint) = self.tint {
                cover = mix_rgb(cover, grey(cover), CALM);
                let lean = tint.field.sample(gx, gy);
                if lean.abs() > 0.004 {
                    let towards = if lean > 0.0 { RED } else { BLUE };
                    cover = mix_rgb(cover, towards, (lean.abs() * TINT_GAIN).min(0.85));
                }
            }
            if self.shade {
                cover = scale_rgb(cover, fine(&ground.flat_shade, x, y));
            }
            if self.step_lines && height > 0.5 {
                cover = self.stepped(cover, gx, gy);
            }
            if self.shade && coast < 1.6 {
                cover = scale_rgb(cover, 0.8 + 0.2 * smooth(0.0, 1.6, coast));
            }
            colour = mix_rgb(sea, cover, land);
        }
        if let Some(marks) = self.marks.ground {
            let column = (x as i64).clamp(0, WIDTH as i64 - 1) as usize;
            let row = (y as i64).clamp(0, HEIGHT as i64 - 1) as usize;
            let at = (row * WIDTH + column) * 4;
            let alpha = f64::from(marks[at + 3]) / 255.0;
            if alpha > 0.0 {
                let mark = [marks[at], marks[at + 1], marks[at + 2]].map(f64::from);
                colour = mix_rgb(colour, mark, alpha);
            }
        }
        if let Some(polar) = self.polar
            && land < 0.5
        {
            let plate = Plate::MAP;
            let lat = (plate.height() / 2.0 - y) / plate.scale();
            if lat.abs() > 0.85 {
                let lon = plate.place(x, y).lon;
                let around = lat.cos();
                let sphere = (around * lon.cos(), around * lon.sin(), lat.sin());
                let bergless;
                let polar = if self.marks.icebergs {
                    polar
                } else {
                    bergless = polar.without_bergs();
                    &bergless
                };
                if let Some(ice) = polar_at(polar, lon, lat, sphere, colour, ground.seed()) {
                    colour = ice.colour;
                }
            }
        }
        Shown {
            colour,
            land,
            coast,
        }
    }

    /// The step lines at 3, 10, 30 and 100 actions over the land's colour: pale on slopes that
    /// face the light, dark on the others.
    fn stepped(&self, mut cover: Rgb, gx: f64, gy: f64) -> Rgb {
        let ground = self.ground;
        let log = ground.log_height.sample(gx, gy);
        let ex = ground.log_height_dx.sample(gx, gy);
        let ey = ground.log_height_dy.sample(gx, gy);
        let slope = ex.hypot(ey);
        if slope <= 1e-4 {
            return cover;
        }
        let strength = if self.tint.is_some() { 0.6 } else { 0.8 };
        for step in STEPS.map(f64::log10) {
            let off = (log - step).abs() / slope;
            if off >= 0.95 {
                continue;
            }
            let k = (1.0 - off / 0.95) * strength;
            let lit = (ex + ey) / (SQRT_2 * slope);
            cover = if lit > -0.15 {
                mix_rgb(
                    cover,
                    [255.0, 250.0, 236.0],
                    k * (0.08 + 0.34 * lit.max(0.0)),
                )
            } else {
                mix_rgb(cover, [10.0, 12.0, 22.0], k * (0.2 + 0.4 * -lit))
            };
        }
        cover
    }
}

/// How frozen an idle crew's land is.
pub fn cold(ground: &Ground) -> f64 {
    if ground.look == Look::Ice { 0.45 } else { 0.0 }
}
