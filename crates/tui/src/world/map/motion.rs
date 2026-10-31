//! The frames of the map's motion: its water flowing and its weather turning.
//!
//! Each motion says one thing in the data: a river's flecks run the way the agents moved, a
//! hurricane turns faster the higher its category, a volcano smokes while its failures are
//! fresh. The terminal holds each motion as a short loop of stored frames and steps them at the
//! world's rate, so a moment of the map is the frame each motion shows then. With motion reduced
//! or off, nothing moves: each mark holds its still pose, chosen so it still says what it means.

use crate::world::deliver::clock::Rate;

/// One motion: a loop of `seconds`, held as `frames` stored frames at 8 steps a second, and,
/// where it `follows_rate`, as many more at a faster rate. Each count is even, so it scales to
/// whole frames at 12 and at 16 steps a second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    pub seconds: f64,
    pub frames: u32,
    pub follows_rate: bool,
}

/// Flecks run 12 px a second down a river's bed, 18 px apart.
pub const RIVER: Motion = Motion {
    seconds: 1.5,
    frames: 12,
    follows_rate: true,
};
pub const RIVER_SPEED: f64 = 12.0;
pub const RIVER_GAP: f64 = 18.0;
/// Streaks run 16 px a second along each of a current's lanes, 24 px apart.
pub const CURRENT: Motion = Motion {
    seconds: 1.5,
    frames: 12,
    follows_rate: true,
};
pub const CURRENT_SPEED: f64 = 16.0;
pub const CURRENT_GAP: f64 = 24.0;
/// A hurricane turns 12° a second for each category; its two arms make half a turn look like a
/// whole one, so 24 frames hold 180°, a frame each 7.5°.
pub const HURRICANE: Motion = Motion {
    seconds: 15.0,
    frames: 24,
    follows_rate: true,
};
pub const HURRICANE_DEGREES: f64 = 12.0;
/// A tornado turns once a second.
pub const TORNADO: Motion = Motion {
    seconds: 1.0,
    frames: 8,
    follows_rate: true,
};
/// A puff of smoke every 0.75 s, each living 3 s.
pub const SMOKE: Motion = Motion {
    seconds: 3.0,
    frames: 24,
    follows_rate: true,
};
/// Flames flicker.
pub const FIRE: Motion = Motion {
    seconds: 1.5,
    frames: 12,
    follows_rate: true,
};
/// One flash for each stopped session, up to 4, each 0.25 s, in every 4 s.
pub const LIGHTNING: Motion = Motion {
    seconds: 4.0,
    frames: 32,
    follows_rate: true,
};
/// One great wave rolling in to the coast over 3 s.
pub const TSUNAMI: Motion = Motion {
    seconds: 3.0,
    frames: 24,
    follows_rate: true,
};
/// A ring runs out from the fault every 2 s for each crash of the last 24 hours.
pub const QUAKE: Motion = Motion {
    seconds: 2.0,
    frames: 16,
    follows_rate: true,
};
/// Cloud shadows drift 8 px a second east, 2 frames a second at every rate, their noise repeating
/// every 192 px: each of their frames is the whole map's shade, so more would take more memory
/// than the drift is worth.
pub const CLOUDS: Motion = Motion {
    seconds: 24.0,
    frames: 48,
    follows_rate: false,
};
pub const CLOUD_SPEED: f64 = 8.0;
pub const CLOUD_PERIOD: f64 = 192.0;

/// A moment of the map: the clock's time in seconds, stepping at a rate, or held still.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Moment {
    Still,
    At { t: f64, rate: Rate },
}

impl Moment {
    /// `t` seconds on a clock stepping at the usual rate.
    pub fn at(t: f64) -> Self {
        Self::At {
            t,
            rate: Rate::default(),
        }
    }

    /// The moment of the clock's `tick`, stepping at `rate`.
    pub fn of_tick(tick: u64, rate: Rate) -> Self {
        Self::At {
            t: rate.seconds(tick),
            rate,
        }
    }

    pub fn is_still(self) -> bool {
        matches!(self, Self::Still)
    }

    /// The time `motion` shows at this moment: the start of the stored frame it is in. Still,
    /// it is 0.
    pub fn time(self, motion: Motion) -> f64 {
        match self {
            Self::Still => 0.0,
            Self::At { t, rate } => {
                let motion = motion.at(rate);
                f64::from(motion.frame(t)) / f64::from(motion.frames) * motion.seconds
            }
        }
    }

    /// The stored frame `motion` shows at this moment, `None` held still.
    pub fn frame(self, motion: Motion) -> Option<u32> {
        match self {
            Self::Still => None,
            Self::At { t, rate } => Some(motion.at(rate).frame(t)),
        }
    }

    /// How far through its loop `motion` is at this moment, from 0 up to 1; `still` when held.
    pub fn phase(self, motion: Motion, still: f64) -> f64 {
        match self {
            Self::Still => still,
            Self::At { .. } => self.time(motion) / motion.seconds,
        }
    }
}

impl Motion {
    /// The motion stepped at `rate`: where it follows the rate, as many more frames as `rate`
    /// has steps over 8 a second.
    pub fn at(self, rate: Rate) -> Self {
        if !self.follows_rate {
            return self;
        }
        Self {
            frames: self.frames * rate.per_second() / 8,
            ..self
        }
    }

    /// The stored frame shown at time `t`.
    pub fn frame(self, t: f64) -> u32 {
        let within = t.rem_euclid(self.seconds) / self.seconds;
        // A step at 12 a second isn't a whole number of seconds in binary, so a frame that
        // starts on one lands a hair under it.
        ((within * f64::from(self.frames) + 1e-9).floor() as u32).min(self.frames - 1)
    }
}

/// How many flashes lightning may start in any one second across the whole screen, so it never
/// flickers fast enough to hurt.
pub const FLASHES_A_SECOND: u32 = 3;
/// How long one flash lasts, in seconds.
pub const FLASH_SECONDS: f64 = 0.25;
/// The most flashes the lightning's loop holds: spread evenly, each comes 0.4 s after the last,
/// so no second ever holds more than [`FLASHES_A_SECOND`].
const FLASHES_A_LOOP: usize = 10;

/// When one storm's flashes start in the lightning's loop, in seconds from its start.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Flashes {
    starts: [f64; 4],
    count: usize,
}

impl Flashes {
    /// Every storm's flashes on one screen, for storms that stopped `sessions` each, in the
    /// order nature lists them: one flash a stopped session, up to 4 a storm, taken in turn from
    /// each storm and spread evenly over the loop. Past [`FLASHES_A_LOOP`] in all, each storm
    /// gives up its last flashes first. A storm alone flashes as it always did.
    pub fn for_storms(sessions: &[u32]) -> Vec<Flashes> {
        let wanted: Vec<usize> = sessions.iter().map(|n| (*n).min(4) as usize).collect();
        let mut order = Vec::new();
        for round in 0..4 {
            for (storm, want) in wanted.iter().enumerate() {
                if round < *want && order.len() < FLASHES_A_LOOP {
                    order.push(storm);
                }
            }
        }
        let total = order.len().max(1) as f64;
        let mut out = vec![Flashes::default(); sessions.len()];
        for (at, storm) in order.into_iter().enumerate() {
            let flashes = &mut out[storm];
            flashes.starts[flashes.count] = (at as f64 + 0.3) / total * LIGHTNING.seconds;
            flashes.count += 1;
        }
        out
    }

    /// How many times it flashes in a loop.
    pub fn count(&self) -> u32 {
        self.count as u32
    }

    /// How brightly it flashes `t` seconds into the loop, 0 between its flashes, and which of
    /// them it is, from 1.
    pub fn at(&self, t: f64) -> (f64, u32) {
        let t = t.rem_euclid(LIGHTNING.seconds);
        let mut flash = (0.0, 0);
        for (k, start) in self.starts[..self.count].iter().enumerate() {
            let since = t - start;
            if (0.0..FLASH_SECONDS).contains(&since) {
                flash = (1.0 - since / FLASH_SECONDS * 0.5, k as u32 + 1);
            }
        }
        flash
    }
}

/// The most flashes `storms` start in any second of the loop, as the screen steps it at 8 a
/// second, the fewest steps a flash can be seen in.
#[cfg(test)]
pub fn most_flashes_in_a_second(storms: &[Flashes]) -> u32 {
    let frames = LIGHTNING.frames;
    let lit = |storm: &Flashes, frame: u32| storm.at(f64::from(frame % frames) / 8.0).0 > 0.0;
    let starts = |frame: u32| {
        storms
            .iter()
            .filter(|storm| lit(storm, frame) && !lit(storm, frame + frames - 1))
            .count() as u32
    };
    (0..frames)
        .map(|from| (from..from + 8).map(starts).sum())
        .max()
        .unwrap_or(0)
}

/// Every motion the map has, for working out when its picture next changes.
pub const ALL: [Motion; 10] = [
    RIVER, CURRENT, HURRICANE, TORNADO, SMOKE, FIRE, LIGHTNING, TSUNAMI, QUAKE, CLOUDS,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_terminal_steps_a_river_through_a_stored_frame_each_step_of_its_loop() {
        for rate in [Rate::QUICK, Rate::STEADY] {
            let steps = rate.per_second() * 3 / 2;
            let frames: Vec<u32> = (0..steps)
                .map(|k| {
                    Moment::of_tick(u64::from(k), rate)
                        .frame(RIVER)
                        .unwrap_or(0)
                })
                .collect();
            assert_eq!(frames, (0..steps).collect::<Vec<_>>(), "{rate:?}");
            assert_eq!(RIVER.at(rate).frame(1.5), 0);
        }
        assert_eq!(Moment::at(0.2).time(RIVER), 0.1875);
    }

    #[test]
    fn every_motion_takes_whole_frames_at_each_rate() {
        for motion in ALL {
            for rate in [Rate::QUICK, Rate::STEADY] {
                let at = motion.at(rate);
                let scale = if motion.follows_rate {
                    rate.per_second()
                } else {
                    8
                };
                assert_eq!(at.frames * 8, motion.frames * scale, "{motion:?}");
            }
        }
    }

    #[test]
    fn held_still_every_motion_shows_its_still_pose() {
        for motion in ALL {
            assert_eq!(Moment::Still.time(motion), 0.0);
            assert_eq!(Moment::Still.phase(motion, 0.72), 0.72);
        }
    }

    #[test]
    fn lightning_never_flashes_more_than_three_times_a_second_across_the_screen() {
        for sessions in [vec![1, 2, 3, 4], vec![4, 4, 4, 4], vec![4; 9], vec![2, 3]] {
            let storms = Flashes::for_storms(&sessions);
            assert!(
                most_flashes_in_a_second(&storms) <= FLASHES_A_SECOND,
                "{sessions:?}: {storms:?}"
            );
            assert!(storms.iter().all(|storm| storm.count() >= 1), "{storms:?}");
        }
        // Few enough storms keep a flash for every session they stopped.
        let storms = Flashes::for_storms(&[1, 2, 3]);
        assert_eq!(
            storms.iter().map(Flashes::count).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        // A storm alone flashes as it always did: 4 sessions, a flash at 0.3 s into each second.
        let alone = Flashes::for_storms(&[4]);
        for second in 0..4 {
            assert!(alone[0].at(f64::from(second) + 0.3 + 0.01).0 > 0.9);
            assert_eq!(alone[0].at(f64::from(second) + 0.6).0, 0.0);
        }
    }

    #[test]
    fn a_frame_holds_until_the_next_one_is_due() {
        assert_eq!(HURRICANE.frame(0.6), 0);
        assert_eq!(HURRICANE.frame(0.63), 1);
        assert_eq!(CLOUDS.frame(23.99), 47);
        assert_eq!(Moment::of_tick(16, Rate::QUICK), Moment::at(1.0));
        assert_eq!(
            Moment::of_tick(18, Rate::STEADY).frame(HURRICANE),
            Some(HURRICANE.at(Rate::STEADY).frame(1.5))
        );
    }
}
