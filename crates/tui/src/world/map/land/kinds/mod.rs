//! The land's own kinds, each from one fact: a swamp where agents waited, a plateau where a habit
//! is steady, a basin where agents came from many other behaviors, an oasis where a small
//! behavior is fresh among unused ones. Rainforest and savanna are climates, drawn by the
//! climate itself.
//!
//! Nature places its marks one after another, each clear of the ones before, so the land's
//! kinds are found and placed through a [`Placer`] that holds what is already placed.

mod draw;
mod place;

#[cfg(test)]
mod tests;

use draw::Onto;
pub use draw::{blob, meander};
pub use place::{Placed, Placer, Want, slope_at, stretch_at, wet_rim};

use super::line::days_ago;
use crate::world::map::climate::Climate;
use crate::world::map::fields::{COARSE, Ground};
use crate::world::raster::{Canvas, Transform};

/// A swamp needs this many waits, and this share of its behavior's actions.
const SWAMP_WAITS: u32 = 5;
const SWAMP_SHARE: f64 = 0.2;
/// A basin needs agents coming from this many other behaviors, each this often; a map shows at
/// most two.
const BASIN_SOURCES: usize = 3;
const BASIN_MOVES: u32 = 8;
const BASINS: usize = 2;

/// Which kind, with what it was found from.
#[derive(Debug, Clone, PartialEq)]
pub enum LandKind {
    /// A fifth or more of its behavior's actions came after a wait of 10 minutes or more.
    Swamp { waited: u32 },
    /// A steady habit, by its place among the ground's plateaus.
    Plateau { plateau: usize },
    /// Agents came from each of these behaviors 8 or more times.
    Basin { from: Vec<usize>, total: u32 },
    /// Used in the last 7 days, in land the climate draws dry.
    Oasis,
}

/// A kind of land found and placed by the behavior it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct LandMark {
    pub kind: LandKind,
    pub behavior: usize,
    pub x: f64,
    pub y: f64,
    /// How far it reaches as drawn, and how far the pointer finds it.
    pub radius: f64,
    pub reach: f64,
}

impl LandMark {
    /// Its line, in the app's words.
    pub fn line(&self, ground: &Ground) -> String {
        let quoted = format!("‘{}’", ground.facts.behaviors[self.behavior].name);
        let count = ground.count[self.behavior];
        match &self.kind {
            LandKind::Swamp { waited } => format!(
                "Swamp · {waited} of its {count} {quoted} actions came after a wait of 10 \
                 minutes or more in the same session"
            ),
            LandKind::Plateau { plateau } => {
                let (low, high) = ground.plateaus[*plateau].range();
                let weeks = if low == high {
                    low.to_string()
                } else {
                    format!("{low} to {high}")
                };
                format!(
                    "Plateau · {quoted} · {count} actions, {weeks} in every week of the period: \
                     a steady habit"
                )
            }
            LandKind::Basin { from, total } => format!(
                "Basin · agents came to {quoted} from {} other behaviors, {total} times",
                from.len()
            ),
            LandKind::Oasis => {
                let last = ground.climate.last_used[self.behavior].unwrap_or(0);
                format!(
                    "Oasis · {quoted} was used {}, in dry land: the behaviors round it were last \
                     used 14 or more days ago",
                    days_ago(last)
                )
            }
        }
    }

    /// Draws it onto the map's ground marks. A plateau is drawn by the relief itself.
    pub fn draw(&self, ground: &Ground, canvas: &mut Canvas) {
        let from: Vec<(usize, (f64, f64))> = match &self.kind {
            LandKind::Basin { from, .. } => from.iter().map(|&f| (f, ground.centre[f])).collect(),
            _ => Vec::new(),
        };
        self.draw_onto(Onto::Map(ground, canvas), &from);
    }

    /// Draws it as it is onto a picture of its own, placed by `place`, with no coast to keep
    /// to and no streams from other behaviors.
    pub fn picture(&self, canvas: &mut Canvas, place: Transform) {
        self.draw_onto(Onto::Picture(canvas, place), &[]);
    }

    fn draw_onto(&self, onto: Onto, from: &[(usize, (f64, f64))]) {
        match &self.kind {
            LandKind::Swamp { .. } => draw::swamp(onto, self),
            LandKind::Basin { .. } => draw::basin(onto, self, from),
            LandKind::Oasis => draw::oasis(onto, self.x, self.y, self.behavior),
            LandKind::Plateau { .. } => {}
        }
    }
}

/// The land's kinds on a crew's ground, found and placed in turn: swamps, plateaus, basins,
/// then oases, each added to the placer as it is placed.
pub fn land_marks(ground: &Ground, placer: &mut Placer) -> Vec<LandMark> {
    let mut marks = Vec::new();
    let mut add = |mark: LandMark, placer: &mut Placer, flat: bool| {
        placer.add(Placed {
            x: mark.x,
            y: mark.y,
            radius: mark.radius,
            flat,
        });
        marks.push(mark);
    };

    for behavior in 0..ground.behaviors() {
        let count = ground.count[behavior];
        let waited = ground.facts.behaviors[behavior].after_a_wait;
        if count == 0 || waited < SWAMP_WAITS || f64::from(waited) / f64::from(count) < SWAMP_SHARE
        {
            continue;
        }
        let radius = (12.0 + 2.2 * f64::from(waited).sqrt()).min(24.0);
        let bias = |x: f64, y: f64| -2.0 * f64::from(placer.dots_near(x, y, radius));
        if let Some((x, y)) = placer.spot(ground, behavior, radius, Want::Low, 0.7, radius, &bias) {
            let mark = LandMark {
                kind: LandKind::Swamp { waited },
                behavior,
                x,
                y,
                radius,
                reach: radius + 2.0,
            };
            add(mark, placer, true);
        }
    }

    for (index, plateau) in ground.plateaus.iter().enumerate() {
        let (x, y) = ground.centre[plateau.behavior];
        let mark = LandMark {
            kind: LandKind::Plateau { plateau: index },
            behavior: plateau.behavior,
            x,
            y,
            radius: 0.0,
            reach: 0.0,
        };
        add(mark, placer, false);
    }

    let mut into: Vec<(usize, Vec<(usize, u32)>)> = Vec::new();
    for moved in &ground.facts.moves {
        if moved.from == moved.to
            || moved.count < BASIN_MOVES
            || ground.count[moved.from] == 0
            || ground.count[moved.to] == 0
        {
            continue;
        }
        match into.iter_mut().find(|(to, _)| *to == moved.to) {
            Some((_, list)) => list.push((moved.from, moved.count)),
            None => into.push((moved.to, vec![(moved.from, moved.count)])),
        }
    }
    into.retain(|(_, list)| list.len() >= BASIN_SOURCES);
    into.sort_by_key(|(_, list)| std::cmp::Reverse(list.len()));
    for (to, list) in into.into_iter().take(BASINS) {
        let (cx, cy) = ground.centre[to];
        let (mut mx, mut my) = (0.0, 0.0);
        for &(from, count) in &list {
            let (dx, dy) = (ground.centre[from].0 - cx, ground.centre[from].1 - cy);
            let length = dx.hypot(dy);
            let length = if length == 0.0 { 1.0 } else { length };
            mx += f64::from(count) * dx / length;
            my += f64::from(count) * dy / length;
        }
        let toward = my.atan2(mx);
        let total = list.iter().map(|(_, count)| count).sum();
        // On its own land, near it, low, on the side the agents come from.
        let extra = |x: f64, y: f64| {
            if ground.height_at(x, y) >= 4.0 {
                return f64::NEG_INFINITY;
            }
            9.0 * ((y - cy).atan2(x - cx) - toward).cos()
                - 2.0 * f64::from(placer.dots_near(x, y, 20.0))
                - 0.12 * (x - cx).hypot(y - cy)
        };
        if let Some((x, y)) = placer.flat_spot(ground, to, 17.0, 1.3, Some(&extra)) {
            let mark = LandMark {
                kind: LandKind::Basin {
                    from: list.iter().map(|(from, _)| *from).collect(),
                    total,
                },
                behavior: to,
                x,
                y,
                radius: 17.0,
                reach: 19.0,
            };
            add(mark, placer, true);
        }
    }

    let g = COARSE as f64;
    for behavior in 0..ground.behaviors() {
        let (cx, cy) = ground.centre[behavior];
        let fresh = ground.climate.last_used[behavior].is_some_and(|days| days < 7);
        let dry =
            Climate::of_field(ground.climate.field.sample(cx / g, cy / g)) <= Climate::Savanna;
        if ground.count[behavior] == 0 || !fresh || !dry {
            continue;
        }
        // Flat ground or ground facing the viewer: the stood-up map squashes ground facing north.
        let facing = |x: f64, y: f64| {
            ((ground.relief.sample(x / g, (y + 4.0) / g)
                - ground.relief.sample(x / g, (y - 4.0) / g))
                / 8.0)
                .max(0.0)
        };
        let bias =
            |x: f64, y: f64| -10.0 * f64::from(placer.dots_near(x, y, 18.0)) - 150.0 * facing(x, y);
        if let Some((x, y)) = placer.spot(ground, behavior, 13.0, Want::Low, 0.6, 13.0, &bias) {
            let mark = LandMark {
                kind: LandKind::Oasis,
                behavior,
                x,
                y,
                radius: 16.0,
                reach: 18.0,
            };
            add(mark, placer, true);
        }
    }
    marks
}

impl Ground {
    /// The plateau a pixel of the flat map lies on, its cliffs or its flat top, by its place
    /// among the ground's plateaus.
    pub fn plateau_at(&self, x: f64, y: f64) -> Option<usize> {
        let g = COARSE as f64;
        let (gx, gy) = (x / g, y / g);
        if self.mesa_at(gx, gy) <= 0.2 {
            return None;
        }
        let (wx, wy) = (
            gx + self.warp_x.sample(gx, gy),
            gy + self.warp_y.sample(gx, gy),
        );
        self.plateaus.iter().position(|plateau| {
            self.mountains[plateau.behavior]
                .as_ref()
                .is_some_and(|own| {
                    own.sample(wx, wy) >= 0.1 * f64::from(self.count[plateau.behavior])
                })
        })
    }
}
