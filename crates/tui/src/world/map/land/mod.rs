//! The kinds of land and the line the land itself says when you point at it.
//!
//! The ground's colour at any pixel comes from [`Surface`]: the sea's depth to its surf, the
//! coast's ragged edge, what grows on the land, the step lines at 3, 10, 30 and 100 actions, a
//! compared group's tint, what nature lays on the ground, and the polar ice. The planet's
//! texture is the same ground wrapped round the globe, and [`line_at`] says what lies at a
//! point in the app's words.

mod coast;
mod compare;
mod ice;
mod kinds;
mod line;
mod sea;
mod surface;
mod texture;

#[cfg(test)]
mod tests;

pub use coast::{coast_at, coast_detail};
pub use compare::{Compared, Leaning, Tint};
pub use ice::{Berg, Ice, Polar, polar_at};
pub use kinds::{
    LandKind, LandMark, Placed, Placer, Want, blob, land_marks, meander, slope_at, stretch_at,
    wet_rim,
};
pub use line::{GroundKind, GroundLine, IceKind, days_ago, line_at};
pub use sea::{BEACH, SeaColours, TUNDRA};
pub use surface::{Marks, Shown, Sunk, Surface, UnderWater, cold};
pub use texture::Texture;
