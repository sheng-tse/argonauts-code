//! The planet's air and its sun: how thick the air is, its colour, which side the sun lights,
//! and how fast the planet turns.

use crate::world::map::fields::{Look, Rgb};

/// The planet's air: as thick as the crew was busy last week, blue over rock and sea, paler
/// over ice, warm over a gas giant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Air {
    /// From 0.08, a crew that hardly worked, to 1 at about 40 actions a day.
    pub thickness: f64,
    pub colour: Rgb,
}

impl Air {
    pub fn of(look: Look, per_day: f64) -> Self {
        let colour = match look {
            Look::Gas => [236.0, 206.0, 160.0],
            Look::Ice => [196.0, 220.0, 255.0],
            Look::Ocean | Look::Rock => [110.0, 170.0, 255.0],
        };
        Self {
            thickness: ((1.0 + per_day.max(0.0)).log10() / 1.6).clamp(0.08, 1.0),
            colour,
        }
    }

    /// How far the air's glow reaches past the globe's edge, in pixels, for a globe of `radius`.
    pub fn reach(&self, radius: f64) -> f64 {
        radius * (0.05 + 0.1 * self.thickness)
    }
}

/// Which side of the planet faces you: the day, or with `n` the night, to see what shines there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Side {
    #[default]
    Day,
    Night,
}

/// The sun as the globe sees it, a unit vector with x right, y up and z toward you, and the
/// half-way vector between it and your eye that the sea glints along.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sun {
    pub to: [f64; 3],
    pub half: [f64; 3],
}

impl Sun {
    /// By day the sun stands about 30° off your eye, up and to the left, so the night is a thin
    /// crescent and no dot hides in it; by night it is behind the planet to the right.
    pub fn of(side: Side) -> Self {
        let to = match side {
            Side::Day => unit([-0.40, 0.30, 0.87]),
            Side::Night => unit([0.92, 0.22, -0.32]),
        };
        Self {
            to,
            half: unit([to[0], to[1], to[2] + 1.0]),
        }
    }

    /// How squarely a point of the globe with this normal faces the sun.
    pub fn facing(&self, normal: [f64; 3]) -> f64 {
        normal[0] * self.to[0] + normal[1] * self.to[1] + normal[2] * self.to[2]
    }
}

fn unit(v: [f64; 3]) -> [f64; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / length, v[1] / length, v[2] / length]
}

/// Seconds one turn of the planet takes at `per_day` actions a day: 24 s at 30, about 50 s at 4,
/// and `None` under one a day, when the planet stands still.
pub fn turn_seconds(per_day: f64) -> Option<f64> {
    (per_day >= 1.0).then(|| 24.0 * 31f64.log2() / (1.0 + per_day).log2())
}
