//! What moves on the planet: everything that moves on the map moves on the globe too, at the
//! same speed and for the same fact, drawn over the turning ground.
//!
//! | What | How it moves |
//! |---|---|
//! | hurricane | turns at its category's speed, 12° a second a category; anticlockwise north of the equator, clockwise and mirrored south |
//! | thunderstorm | its cloud flashes once for each session stopped there, up to 4 in each 4 s |
//! | volcano | smoke rises and drifts downwind, only where something failed this week |
//! | wildfire | the front flickers and its smoke drifts, only where work was dropped this week |
//! | tornado | its bands climb, one turn a second |
//! | tsunami | one great wave rolls in from the open sea to its behavior's coast |
//! | river, current | light runs along the water the way the agents moved, on the day side |
//! | earthquake | broken amber rings run out from the fault, one for each crash in the last 24 hours |
//! | clouds | drift east over the ground at the map's 8 px a second |
//!
//! Each thing is drawn where it lies on the map, through the globe's own bend at that place
//! ([`Spot::on_ground`]), only on the side facing you, faded toward the rim and lit by the
//! planet's sun; and it is left out where it would be too small to read, so on the small planet
//! in the universe only hurricanes and the clouds move. With motion reduced, each holds its
//! still pose.

mod fire;
mod ground;
mod quake;
mod storms;
mod water;

#[cfg(test)]
mod tests;

use std::fmt::Debug;

pub use fire::{Volcano, Wildfire};
pub use ground::{GlobeGround, Spot};
pub use quake::Quake;
pub use storms::{Hurricane, Thunderstorm, Tornado};
pub use water::{Current, Flow, Stretch, Tsunami};

use crate::base::draw::scene::picture_key;
use crate::world::raster::Canvas;

/// Which kind of thing moves, in the order they are drawn: the water's light first, the clouds
/// of storms last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Flow,
    Tsunami,
    Quake,
    Wildfire,
    Volcano,
    Tornado,
    Thunderstorm,
    Hurricane,
}

impl Kind {
    /// The smallest size on the screen, in pixels, at which its motion still reads.
    pub fn reads(self) -> f64 {
        match self {
            Kind::Hurricane => 7.0,
            Kind::Thunderstorm => 9.0,
            Kind::Volcano => 6.0,
            Kind::Wildfire => 5.0,
            Kind::Tornado => 8.0,
            Kind::Tsunami => 10.0,
            Kind::Quake => 12.0,
            Kind::Flow => 0.9,
        }
    }
}

/// A motion's loop: how long it runs before it starts over, and how many pictures of it the
/// terminal keeps, shown 8 times a second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    pub seconds: f64,
    pub frames: u32,
}

impl Motion {
    pub const RIVER: Motion = Motion::new(1.5, 12);
    pub const CURRENT: Motion = Motion::new(1.5, 12);
    /// 24 pictures hold half a turn: a hurricane's two arms make half a turn look like a whole.
    pub const HURRICANE: Motion = Motion::new(15.0, 24);
    pub const TORNADO: Motion = Motion::new(1.0, 8);
    pub const SMOKE: Motion = Motion::new(3.0, 24);
    pub const FIRE: Motion = Motion::new(1.5, 12);
    pub const LIGHTNING: Motion = Motion::new(4.0, 32);
    pub const TSUNAMI: Motion = Motion::new(3.0, 24);
    pub const QUAKE: Motion = Motion::new(2.0, 16);

    pub const fn new(seconds: f64, frames: u32) -> Self {
        Self { seconds, frames }
    }

    /// The moment the `index`th of its pictures shows.
    pub fn moment(&self, index: u32) -> f64 {
        f64::from(index % self.frames.max(1)) / f64::from(self.frames.max(1)) * self.seconds
    }

    /// Which of its pictures shows `t` seconds in.
    pub fn picture_at(&self, t: f64) -> u32 {
        let frames = f64::from(self.frames.max(1));
        let into = t.rem_euclid(self.seconds) / self.seconds;
        ((into * frames).floor() as u32).min(self.frames.max(1) - 1)
    }
}

/// One thing that moves on the planet, from what nature put on the map.
pub trait Weather: Debug + Send + Sync {
    fn kind(&self) -> Kind;

    /// Where it lies on the map, in the map's pixels.
    fn place(&self) -> (f64, f64);

    /// Its loop.
    fn motion(&self) -> Motion;

    /// Whether it moves on a globe of `radius` px: on the small planet in the universe most
    /// things are too small to read, and some don't move at all, such as a volcano that has
    /// been quiet all week.
    fn moves_on(&self, radius: f64) -> bool;

    /// Draws it at `t` seconds into its loop, or in its still pose.
    fn draw(&self, canvas: &mut Canvas, ground: &GlobeGround, t: f64, still: bool);

    /// Tells it from other weather in the keys of the pictures it is drawn into.
    fn fingerprint(&self) -> u64 {
        let (x, y) = self.place();
        picture_key([self.kind() as u64, x.to_bits(), y.to_bits()])
    }

    /// When it flashes, for a storm.
    #[cfg(test)]
    fn flashes(&self) -> Option<crate::world::map::motion::Flashes> {
        None
    }
}

/// Everything in `weather` drawn in order, each at its own moment from `moment`.
pub fn draw_all(
    canvas: &mut Canvas,
    ground: &GlobeGround,
    weather: &[&dyn Weather],
    moment: impl Fn(&dyn Weather) -> f64,
    still: bool,
) {
    let mut order: Vec<&dyn Weather> = weather.to_vec();
    order.sort_by_key(|thing| thing.kind());
    for thing in order {
        thing.draw(canvas, ground, moment(thing), still);
    }
}

/// How fast the clouds drift east over the ground: the map's cloud shade runs 8 px a second
/// over 1152 px for 300°, so about 2.1° a second, which the turning ground visibly outpaces.
pub const DRIFT_DEGREES_A_SECOND: f64 = 8.0 * 300.0 / 1152.0;

/// How far the clouds have drifted east after `seconds`, in radians, stepped twice a second as
/// the terminal steps them.
pub fn drift(seconds: f64) -> f64 {
    let stepped = (seconds * 2.0).floor() / 2.0;
    (stepped * DRIFT_DEGREES_A_SECOND).to_radians()
}

/// The map's wind, a unit vector: east-north-east, the way smoke drifts.
pub fn wind() -> (f64, f64) {
    let length = 1.0_f64.hypot(-0.35);
    (1.0 / length, -0.35 / length)
}
