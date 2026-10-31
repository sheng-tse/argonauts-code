//! Which level of the world the tab shows, and the zoom from one to the next. The map is the
//! planet unrolled; − goes out from it to the planet and on to the universe, where each maker
//! is a galaxy, and inside a galaxy each of its models is a star with a system round it.

use std::time::{Duration, Instant};

use crate::world::deliver::clock::Rate;

/// A level of the world, from the map out to the universe and into it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Level {
    #[default]
    Map,
    Planet,
    Universe,
    /// A maker's galaxy, by its index among the universe's makers.
    Galaxy(usize),
    /// A model's system, by its star's index among the universe's stars.
    System(usize),
}

impl Level {
    /// The digit that jumps to it, and the name the row above the picture gives it: the
    /// universe's galaxies and systems are all the universe's.
    pub fn digit(self) -> u8 {
        match self {
            Level::Map => 1,
            Level::Planet => 2,
            _ => 3,
        }
    }

    /// The map, the planet and the universe, the three a digit reaches.
    pub const NAMED: [(Level, &'static str); 3] = [
        (Level::Map, "Map"),
        (Level::Planet, "Planet"),
        (Level::Universe, "Universe"),
    ];

    /// The universe and the levels inside it.
    pub fn in_cosmos(self) -> bool {
        matches!(self, Level::Universe | Level::Galaxy(_) | Level::System(_))
    }

    /// The levels the dots of your actions are drawn on, which ← → compares groups on.
    pub fn shows_dots(self) -> bool {
        matches!(self, Level::Map | Level::Planet)
    }
}

/// How long a zoom from one level to the next takes: 0.75 s, the 0.7 s it should as near as the
/// world's clock comes at 12 and at 16 steps a second.
const ZOOM_SECONDS: f64 = 0.75;

/// The world moving from one level to another: from `from` to `to`, begun `since`, a frame
/// each step at `rate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zoom {
    pub from: Level,
    pub to: Level,
    since: Instant,
    rate: Rate,
}

impl Zoom {
    pub fn new(from: Level, to: Level, since: Instant, rate: Rate) -> Self {
        Self {
            from,
            to,
            since,
            rate,
        }
    }

    /// When it began.
    pub fn since(&self) -> Instant {
        self.since
    }

    pub fn rate(&self) -> Rate {
        self.rate
    }

    /// The steps it takes.
    pub fn frames(&self) -> u32 {
        (ZOOM_SECONDS * f64::from(self.rate.per_second())).round() as u32
    }

    /// How long it takes.
    pub fn lasts(&self) -> Duration {
        self.rate.step() * self.frames()
    }

    /// The frame the zoom shows at `now`, from 0 at its start to [`Zoom::frames`] once it has
    /// arrived.
    pub fn frame(&self, now: Instant) -> u32 {
        let steps = self
            .rate
            .steps_in(now.saturating_duration_since(self.since));
        u32::try_from(steps).unwrap_or(u32::MAX).min(self.frames())
    }

    pub fn done(&self, now: Instant) -> bool {
        self.frame(now) >= self.frames()
    }

    /// When its next frame is due, while it has one to show.
    pub fn next_frame(&self, now: Instant) -> Option<Instant> {
        let frame = self.frame(now);
        (frame < self.frames()).then(|| self.since + self.rate.step() * (frame + 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zoom_steps_a_frame_each_step_for_three_quarters_of_a_second_then_stays() {
        let start = Instant::now();
        let zoom = Zoom::new(Level::Map, Level::Planet, start, Rate::STEADY);
        assert_eq!(zoom.frames(), 9);
        assert_eq!(zoom.frame(start), 0);
        assert_eq!(zoom.frame(start + Duration::from_millis(83)), 0);
        assert_eq!(zoom.frame(start + Duration::from_millis(84)), 1);
        assert_eq!(
            zoom.next_frame(start + Duration::from_millis(90)),
            Some(start + Rate::STEADY.step() * 2)
        );
        assert!(!zoom.done(start + Duration::from_millis(749)));
        let after = start + Duration::from_millis(750);
        assert!(zoom.done(after));
        assert_eq!(zoom.frame(after + Duration::from_secs(60)), 9);
        assert_eq!(zoom.next_frame(after), None);
        let quick = Zoom::new(Level::Map, Level::Planet, start, Rate::QUICK);
        assert_eq!(quick.frames(), 12);
        assert_eq!(quick.lasts(), Duration::from_millis(750));
    }

    #[test]
    fn each_level_has_its_digit_and_only_the_map_and_the_planet_show_dots() {
        let digits: Vec<u8> = [
            Level::Map,
            Level::Planet,
            Level::Universe,
            Level::Galaxy(0),
            Level::System(2),
        ]
        .map(Level::digit)
        .to_vec();
        assert_eq!(digits, [1, 2, 3, 3, 3]);
        assert!(Level::Map.shows_dots() && Level::Planet.shows_dots());
        assert!(!Level::Universe.shows_dots() && Level::System(0).in_cosmos());
        assert!(!Level::Planet.in_cosmos());
    }
}
