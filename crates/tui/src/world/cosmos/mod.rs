//! The universe: a galaxy for each maker, sized by its share of your actions, a body for each
//! model by how you use it, each model's busiest behaviors as planets round its star, and the
//! sky of earlier months behind them.
//!
//! [`Universe`] works out what there is from the world's facts, and [`Cosmos`] draws each level
//! of it as a [`Scene`](crate::world::Scene) for the screen.

mod disc;
mod galaxy;
mod link;
mod model;
mod motion;
mod paint;
pub mod rules;
mod scenes;
mod shapes;
mod sky;
mod stars;
mod system;
mod universe;
mod zoom;

pub use link::star_actions;
pub use model::{
    Binary, Body, Climate, Comet, Maker, Meteor, Month, Moon, Planet, Star, StarAction, StarKind,
    Stream, System, Universe, maker_logo, maker_rgb, served_logo,
};
pub use scenes::{Cosmos, Level, SkyClusters, View};
pub use shapes::home_radius;
pub use zoom::{HomeShot, Times};

#[cfg(test)]
pub(crate) mod tests;
