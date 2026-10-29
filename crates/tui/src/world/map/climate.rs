//! The climate each part of the land takes from the behaviors round it, the bigger ones more.
//!
//! Each behavior's climate comes from when it was last used, counted back from the period's
//! end so a past month's map never changes: the longer a behavior lies unused, the drier its
//! land. The land between behaviors takes their climates weighted by their size, and its edges
//! are roughened so they read as land.

mod cover;

#[cfg(test)]
mod tests;

pub use cover::{ROCK, ROCK_DRY, SNOW, cover_colour, land_cover, rock_grain};

use super::fields::{
    COARSE, COARSE_HEIGHT, COARSE_WIDTH, FINE, FINE_HEIGHT, FINE_WIDTH, Ground, GroundFacts, LAND,
    Rgb, fine,
};
use crate::world::raster::Grid;
use crate::world::raster::noise::fbm3;

/// The five climates, driest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Climate {
    /// Last used 21 or more days ago.
    Desert,
    /// Last used 14 to 20 days ago.
    Savanna,
    /// Last used 7 to 13 days ago.
    Grassland,
    /// Used in the last 7 days.
    Forest,
    /// Used on 4 or more of the last 7 days.
    Rainforest,
}

impl Climate {
    pub const ALL: [Climate; 5] = [
        Climate::Desert,
        Climate::Savanna,
        Climate::Grassland,
        Climate::Forest,
        Climate::Rainforest,
    ];

    /// The climate of a behavior from its days.
    pub fn of(last_used: Option<u32>, days_in_last_7: u32) -> Self {
        let last = last_used.unwrap_or(u32::MAX);
        if days_in_last_7 >= 4 {
            Self::Rainforest
        } else if last < 7 {
            Self::Forest
        } else if last < 14 {
            Self::Grassland
        } else if last < 21 {
            Self::Savanna
        } else {
            Self::Desert
        }
    }

    /// The climate a value of the climate field stands for, each between the halves.
    pub fn of_field(value: f64) -> Self {
        match value {
            v if v < 0.5 => Self::Desert,
            v if v < 1.5 => Self::Savanna,
            v if v < 2.5 => Self::Grassland,
            v if v < 3.5 => Self::Forest,
            _ => Self::Rainforest,
        }
    }

    /// Its place from the driest, as the climate field counts.
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Desert => "desert",
            Self::Savanna => "savanna",
            Self::Grassland => "grassland",
            Self::Forest => "forest",
            Self::Rainforest => "rainforest",
        }
    }

    pub fn rule(self) -> &'static str {
        match self {
            Self::Desert => "last used 21 or more days ago",
            Self::Savanna => "last used 14 to 20 days ago",
            Self::Grassland => "last used 7 to 13 days ago",
            Self::Forest => "used in the last 7 days",
            Self::Rainforest => "used on 4 or more of the last 7 days",
        }
    }

    /// Its colour in a legend's swatch.
    pub fn rgb(self) -> Rgb {
        match self {
            Self::Desert => [206.0, 172.0, 118.0],
            Self::Savanna => [188.0, 160.0, 88.0],
            Self::Grassland => [124.0, 142.0, 72.0],
            Self::Forest => [62.0, 92.0, 46.0],
            Self::Rainforest => [32.0, 70.0, 34.0],
        }
    }
}

/// How far round a behavior its climate reaches, in pixels.
const CLIMATE_REACH: f64 = 40.0 * LAND;

/// Each behavior's climate and the climate field over the map.
#[derive(Debug, Clone)]
pub struct ClimateFields {
    /// Each behavior's climate; `None` without actions in the period.
    pub class: Vec<Option<Climate>>,
    /// Each behavior's days back to its last action, and its days used of the last 7.
    pub last_used: Vec<Option<u32>>,
    pub days_in_last_7: Vec<u32>,
    /// The climate on each coarse point: the climates round it weighted by the behaviors' size,
    /// 0 desert to 4 rainforest.
    pub field: Grid,
    /// Fine noise that roughens where one climate gives way to the next.
    pub edge_noise: Grid,
}

impl ClimateFields {
    pub fn new(
        facts: &GroundFacts,
        count: &[u32],
        centre: &[(f64, f64)],
        warp_x: &Grid,
        warp_y: &Grid,
    ) -> Self {
        let class: Vec<Option<Climate>> = facts
            .behaviors
            .iter()
            .zip(count)
            .map(|(days, count)| {
                (*count > 0).then(|| Climate::of(days.last_used, days.days_in_last_7))
            })
            .collect();
        let reach = 2.0 * CLIMATE_REACH * CLIMATE_REACH;
        let g = COARSE as f64;
        let field = Grid::from_fn(COARSE_WIDTH, COARSE_HEIGHT, |k, j| {
            let x = (k as f64 + warp_x.get(k, j)) * g;
            let y = (j as f64 + warp_y.get(k, j)) * g;
            let (mut num, mut den) = (0.0, 0.0);
            for (at, climate) in class.iter().enumerate() {
                let Some(climate) = climate else { continue };
                let (dx, dy) = (x - centre[at].0, y - centre[at].1);
                let weight = f64::from(count[at]).sqrt() * (-(dx * dx + dy * dy) / reach).exp();
                num += weight * climate.index() as f64;
                den += weight;
            }
            if den > 1e-9 { num / den } else { 2.0 }
        });
        let seed = f64::from(facts.seed);
        let f = FINE as f64;
        let edge_noise = Grid::from_fn(FINE_WIDTH, FINE_HEIGHT, |k, j| {
            fbm3(
                k as f64 * f / 26.0,
                j as f64 * f / 26.0,
                4.4,
                seed + 77.0,
                3,
            )
        });
        Self {
            class,
            last_used: facts.behaviors.iter().map(|days| days.last_used).collect(),
            days_in_last_7: facts
                .behaviors
                .iter()
                .map(|days| days.days_in_last_7)
                .collect(),
            field,
            edge_noise,
        }
    }

    /// The climate field at a pixel with its roughened edges, as the land is drawn.
    pub fn roughened_at(&self, x: f64, y: f64) -> f64 {
        super::fields::coarse(&self.field, x, y) + 0.55 * fine(&self.edge_noise, x, y)
    }
}

impl Ground {
    /// The climate the land at a pixel is drawn in.
    pub fn climate_at(&self, x: f64, y: f64) -> Climate {
        Climate::of_field(self.climate.roughened_at(x, y))
    }
}
