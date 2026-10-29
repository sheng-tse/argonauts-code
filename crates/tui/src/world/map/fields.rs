//! The fields the ground is grown from: how many actions stand where, smoothed over the map.
//!
//! Each behavior is a mountain as tall as its count, the coast reaches out round the behaviors
//! as far as they are big, and a steady habit's mountain is a flat-topped plateau. Everything
//! is worked out once per crew on two grids over the map: a coarse one of 4 px a point that
//! holds the land and its heights, and a fine one of 2 px a point that holds the relief the
//! light sees. Both are read smoothly between their points.

mod facts;
mod grow;
mod plateau;
mod rgb;
mod rows;

#[cfg(test)]
pub(crate) mod crews;
#[cfg(test)]
mod tests;

pub use facts::{BehaviorDays, Dot, GroundFacts, Moved};
pub use plateau::Plateau;
pub use rgb::{Rgb, byte, grey, mix_rgb, scale_rgb, to_bytes};
pub use rows::in_rows;

use super::climate::ClimateFields;
use crate::world::geo::Plate;
use crate::world::raster::Grid;

/// The map's size in pixels, [`Plate::MAP`], which every number of the ground is given for.
pub const WIDTH: usize = 1152;
pub const HEIGHT: usize = 546;

/// Pixels between points of the coarse grid, and its points across and down.
pub const COARSE: usize = 4;
pub const COARSE_WIDTH: usize = WIDTH.div_ceil(COARSE) + 1;
pub const COARSE_HEIGHT: usize = HEIGHT.div_ceil(COARSE) + 1;

/// Pixels between points of the fine grid, and its points across and down.
pub const FINE: usize = 2;
pub const FINE_WIDTH: usize = WIDTH.div_ceil(FINE) + 1;
pub const FINE_HEIGHT: usize = HEIGHT.div_ceil(FINE) + 1;

/// How much of the map the land takes each way, so open sea and the polar ice show round it.
pub const LAND: f64 = Plate::LAND;

/// What a crew's planet looks like as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// Fewer than 40 actions: a gas giant, with no climate yet.
    Gas,
    /// Idle 10 days or more: frozen.
    Ice,
    /// Land on less than 30% of the map.
    Ocean,
    Rock,
}

/// A crew's ground, worked out once from its facts.
#[derive(Debug, Clone)]
pub struct Ground {
    pub facts: GroundFacts,
    /// Each dot's pixel on the map.
    pub dots: Vec<(f32, f32)>,
    /// Each behavior's actions, in all and in each group.
    pub count: Vec<u32>,
    pub by_group: Vec<Vec<u32>>,
    pub group_count: Vec<u32>,
    /// The middle of each behavior's dots, or its own place when it has none.
    pub centre: Vec<(f64, f64)>,
    /// Each behavior's own mountain, in actions, on the coarse grid; `None` without dots.
    pub mountains: Vec<Option<Grid>>,
    /// The mountains together, before the ground is warped.
    pub peaks: Grid,
    pub plateaus: Vec<Plateau>,
    /// How far each coarse point is up a plateau: 1 on its flat top, 0 off it. `None` with no
    /// plateau.
    pub mesa: Option<Grid>,
    /// How strongly the behaviors round a point pull the coast out to it, the bigger more.
    pub coast_pull: Grid,
    /// How far each coarse point's ground is moved, in coarse points, so heights and climates
    /// read as land rather than as circles.
    pub warp_x: Grid,
    pub warp_y: Grid,
    /// The heights on the warped ground, in actions: what the colours and the words read.
    pub height: Grid,
    /// The land field: 1 and over is land.
    pub land: Grid,
    /// How fast the land field changes per pixel, which turns it into a distance to the coast.
    pub land_slope: Grid,
    /// The heights' tens logarithm and its slope per pixel, for the step lines.
    pub log_height: Grid,
    pub log_height_dx: Grid,
    pub log_height_dy: Grid,
    /// The relief in drawing units, 5.5 a doubling of the count, on the coarse grid.
    pub relief: Grid,
    /// The same on the fine grid with the ridges of busy mountains, and its slopes per pixel.
    pub fine_relief: Grid,
    pub fine_slope_x: Grid,
    pub fine_slope_y: Grid,
    /// The flat map's light on each fine point: four lights and the open sky.
    pub flat_shade: Grid,
    /// The share of the map that is land.
    pub land_share: f64,
    pub look: Look,
    /// The behavior with the most actions.
    pub busiest: Option<usize>,
    pub climate: ClimateFields,
}

impl Ground {
    /// Grows a crew's ground from its facts.
    pub fn grow(facts: GroundFacts) -> Self {
        grow::grow(facts)
    }

    /// The seed every noise of this crew's ground is drawn with.
    pub fn seed(&self) -> f64 {
        f64::from(self.facts.seed)
    }

    pub fn behaviors(&self) -> usize {
        self.count.len()
    }

    /// The land field at a pixel: 1 and over is land.
    pub fn land_at(&self, x: f64, y: f64) -> f64 {
        coarse(&self.land, x, y)
    }

    /// The heights at a pixel, in actions.
    pub fn height_at(&self, x: f64, y: f64) -> f64 {
        coarse(&self.height, x, y)
    }

    /// How far a coarse point lies up a plateau, read on the same warped ground as the heights.
    pub fn mesa_at(&self, gx: f64, gy: f64) -> f64 {
        self.mesa.as_ref().map_or(0.0, |mesa| {
            mesa.sample(
                gx + self.warp_x.sample(gx, gy),
                gy + self.warp_y.sample(gx, gy),
            )
        })
    }

    /// Whether a pixel is at sea, with a little margin off the coast.
    pub fn on_sea(&self, x: f64, y: f64) -> bool {
        self.land_at(x, y) < 0.98
    }
}

/// A coarse field read at a pixel.
pub fn coarse(field: &Grid, x: f64, y: f64) -> f64 {
    field.sample(x / COARSE as f64, y / COARSE as f64)
}

/// A fine field read at a pixel.
pub fn fine(field: &Grid, x: f64, y: f64) -> f64 {
    field.sample(x / FINE as f64, y / FINE as f64)
}

/// Where a point of the land's unit square, `x` east and `y` north, is drawn on the map.
pub fn map_pixel(x: f64, y: f64) -> (f64, f64) {
    Plate::MAP.land_pixel(x, y)
}
