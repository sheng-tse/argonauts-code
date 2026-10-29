//! The coast: where the land field crosses 1, with a ragged edge of capes, coves and islets
//! within a few pixels of it. The edge carries no data and never moves a dot or a mark, each of
//! which keeps 20 px of land or more; it only makes the land's edge read as a coast instead of a
//! soft blob.

use crate::world::map::fields::{Ground, coarse};
use crate::world::raster::noise::{fbm3, smooth, vnoise3};

/// The coast's ragged edge at a pixel, in pixels in or out.
pub fn coast_detail(ground: &Ground, x: f64, y: f64) -> f64 {
    let seed = ground.seed() + 91.0;
    let ux = x / 22.0 + 0.9 * vnoise3(x / 55.0, y / 55.0, 2.2, seed);
    let uy = y / 22.0 + 0.9 * vnoise3(x / 55.0, y / 55.0, 5.3, seed);
    6.0 * fbm3(ux, uy, 0.7, seed + 1.0, 5)
}

/// How far a pixel lies inside the coast, in pixels, negative at sea, with the ragged edge.
pub fn coast_at(ground: &Ground, x: f64, y: f64) -> f64 {
    let level = ground.land_at(x, y);
    let mut inside = (level - 1.0) / coarse(&ground.land_slope, x, y).max(1e-4);
    if inside > -14.0 && inside < 14.0 {
        inside += coast_detail(ground, x, y) * (1.0 - smooth(9.0, 14.0, inside.abs()));
    }
    inside
}
