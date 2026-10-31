//! The land stood up in relief, height as count, and laid flat again.
//!
//! The map is a lit relief seen from the south in plan oblique, the way Swiss relief maps stand
//! their mountains up: every place keeps its spot on the flat map and the land rises up the
//! screen by its height. So the map still reads as the map the planet wraps, while each
//! behavior's mountain stands as tall as its count, 5.5 units a doubling and nothing else adding
//! height. The sun is low in the west-north-west, so the faces turned to you and to the west
//! catch it, and every mountain's shadows fall east-south-east. Laid flat, the same picture is
//! the land seen from straight above.
//!
//! The work is split by how often it changes: [`Relief`] once per crew, [`Light`] once per crew
//! for standing and once for lying flat, [`LitGround`] once per grouping or comparison, and the
//! [`Projected`] picture and its [`Dots`] for each height the land is shown at.
//!
//! ```no_run
//! use argonauts_core::behaviors::behavior_map::BehaviorMap;
//! use argonauts_tui::world::map::fields::{Ground, GroundFacts};
//! use argonauts_tui::world::map::land::{Marks, Polar};
//! use argonauts_tui::world::map::relief::{
//!     DotStyle, Dots, Light, LitGround, Projected, Relief, STANDING,
//! };
//!
//! # let map = BehaviorMap::default();
//! # let colours = [[224.0, 138.0, 102.0]];
//! let ground = Ground::grow(GroundFacts::from_map(&map, 41));
//! let relief = Relief::new(&ground);
//! let polar = Polar::of(&ground);
//! let light = Light::new(&ground, &relief, STANDING);
//! let lit = LitGround::new(&ground, &relief, &light, None, Marks::default(), Some(&polar));
//! let land = Projected::new(&relief, &lit, STANDING, 2);
//! let style = DotStyle { groups: &colours, compared: None };
//! let dots = Dots::new(&ground, &relief, style, STANDING);
//! // The land is the scene's bottom layer and the dots go over it; the pointer finds a dot
//! // first, and the ground behind any pixel through `land.under()`.
//! # let _ = (land.under(), dots.marks(0, 9));
//! ```

mod dots;
mod geometry;
mod light;
mod lit;
mod project;

#[cfg(test)]
pub(crate) mod cells;
#[cfg(test)]
mod tests;

pub use dots::{DotStyle, Dots};
pub use geometry::Relief;
pub use light::Light;
pub use lit::LitGround;
pub use project::Projected;

/// The sun over the map: from 290° round from north and 31° up, as a unit vector with x east,
/// y south and z up.
pub fn sun() -> [f64; 3] {
    let azimuth = 290.0_f64.to_radians();
    let elevation = 31.0_f64.to_radians();
    [
        azimuth.sin() * elevation.cos(),
        -azimuth.cos() * elevation.cos(),
        elevation.sin(),
    ]
}

/// How much the ground is foreshortened up the screen, a little, so the far edge's mountains
/// have room.
pub const FORESHORTEN: f64 = 0.94;

/// How far one unit of height lifts a place up the screen when the land stands: the busiest
/// peak, about 45 units, stands about 45 px tall.
pub const STANDING: f64 = 1.0;

/// The screen row the map's top edge lands on when the land lies at sea level.
pub fn top_row() -> f64 {
    crate::world::map::fields::HEIGHT as f64 * (1.0 - FORESHORTEN)
}
