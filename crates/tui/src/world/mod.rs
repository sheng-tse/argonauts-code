//! The world the Behaviors tab draws from your agents' actions: a planet's map whose land,
//! water and weather grow from what they did, the planet itself turning, and the universe of
//! makers and models round it.
//!
//! Every level hands the screen a [`Scene`]: pictures stacked in depth, which the screen keeps and
//! moves rather than sending again, and a [`HitMap`] of what the pointer finds where, so hover
//! lines come from pointing at what was drawn. [`geo`] holds the one projection every level
//! shares: the map is the planet unrolled, and the planet is the map wrapped round a globe.

pub mod cosmos;
pub mod deliver;
pub mod geo;
pub mod map;
pub mod planet;
pub mod planet_fx;
pub mod raster;
pub mod scene;
pub mod wonders;

pub use geo::{Globe, GlobePoint, LonLat, Plate, Surface, View};
pub use scene::{
    Depth, HitMap, Image, Layer, Mark, Part, PartId, Picture, Pointed, Scaled, Scene, Shape,
    Sprite, Under, picture_key,
};
