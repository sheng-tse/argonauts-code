//! The frames of the map's motion: its water flowing and its weather turning.
//!
//! Each motion says one thing in the data: a river's flecks run the way the agents moved, a
//! hurricane turns faster the higher its category, a volcano smokes while its failures are
//! fresh. The terminal holds each motion as a short loop of stored frames and steps them 8 times
//! a second, so a moment of the map is the frame each motion shows then. With motion reduced or
//! off, nothing moves: each mark holds its still pose, chosen so it still says what it means.

/// The terminal steps the map's motion this often, in seconds.
pub const TICK: f64 = 1.0 / 8.0;

/// One motion: a loop of `seconds`, held as `frames` stored frames.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    pub seconds: f64,
    pub frames: u32,
}

/// Flecks run 12 px a second down a river's bed, 18 px apart.
pub const RIVER: Motion = Motion {
    seconds: 1.5,
    frames: 12,
};
pub const RIVER_SPEED: f64 = 12.0;
pub const RIVER_GAP: f64 = 18.0;
/// Streaks run 16 px a second along each of a current's lanes, 24 px apart.
pub const CURRENT: Motion = Motion {
    seconds: 1.5,
    frames: 12,
};
pub const CURRENT_SPEED: f64 = 16.0;
pub const CURRENT_GAP: f64 = 24.0;
/// A hurricane turns 12° a second for each category; its two arms make half a turn look like a
/// whole one, so 24 frames hold 180°, a frame each 7.5°.
pub const HURRICANE: Motion = Motion {
    seconds: 15.0,
    frames: 24,
};
pub const HURRICANE_DEGREES: f64 = 12.0;
/// A tornado turns once a second.
pub const TORNADO: Motion = Motion {
    seconds: 1.0,
    frames: 8,
};
/// A puff of smoke every 0.75 s, each living 3 s.
pub const SMOKE: Motion = Motion {
    seconds: 3.0,
    frames: 24,
};
/// Flames flicker.
pub const FIRE: Motion = Motion {
    seconds: 1.5,
    frames: 12,
};
/// One flash for each stopped session, up to 4, each 0.25 s, in every 4 s.
pub const LIGHTNING: Motion = Motion {
    seconds: 4.0,
    frames: 32,
};
/// One great wave rolling in to the coast over 3 s.
pub const TSUNAMI: Motion = Motion {
    seconds: 3.0,
    frames: 24,
};
/// A ring runs out from the fault every 2 s for each crash of the last 24 hours.
pub const QUAKE: Motion = Motion {
    seconds: 2.0,
    frames: 16,
};
/// Cloud shadows drift 8 px a second east, 2 frames a second, their noise repeating every
/// 192 px.
pub const CLOUDS: Motion = Motion {
    seconds: 24.0,
    frames: 48,
};
pub const CLOUD_SPEED: f64 = 8.0;
pub const CLOUD_PERIOD: f64 = 192.0;

/// A moment of the map: the clock's time in seconds, or held still.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Moment {
    Still,
    At(f64),
}

impl Moment {
    /// The moment of the clock's `tick`, stepping 8 times a second.
    pub fn of_tick(tick: u64) -> Self {
        Self::At(tick as f64 * TICK)
    }

    pub fn is_still(self) -> bool {
        matches!(self, Self::Still)
    }

    /// The time `motion` shows at this moment: the start of the stored frame it is in. Still,
    /// it is 0.
    pub fn time(self, motion: Motion) -> f64 {
        match self {
            Self::Still => 0.0,
            Self::At(t) => f64::from(motion.frame(t)) / f64::from(motion.frames) * motion.seconds,
        }
    }

    /// How far through its loop `motion` is at this moment, from 0 up to 1; `still` when held.
    pub fn phase(self, motion: Motion, still: f64) -> f64 {
        match self {
            Self::Still => still,
            Self::At(_) => self.time(motion) / motion.seconds,
        }
    }
}

impl Motion {
    /// The stored frame shown at time `t`.
    pub fn frame(self, t: f64) -> u32 {
        let within = t.rem_euclid(self.seconds) / self.seconds;
        ((within * f64::from(self.frames)).floor() as u32).min(self.frames - 1)
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

/// The most flashes `storms` start in any second of the loop, as the screen steps it.
#[cfg(test)]
pub fn most_flashes_in_a_second(storms: &[Flashes]) -> u32 {
    let frames = LIGHTNING.frames;
    let lit = |storm: &Flashes, frame: u32| storm.at(f64::from(frame % frames) * TICK).0 > 0.0;
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
    fn the_terminal_steps_a_river_through_12_stored_frames_in_a_second_and_a_half() {
        let frames: Vec<u32> = (0..12).map(|k| RIVER.frame(f64::from(k) * TICK)).collect();
        assert_eq!(frames, (0..12).collect::<Vec<_>>());
        assert_eq!(RIVER.frame(1.5), 0);
        assert_eq!(Moment::At(0.2).time(RIVER), 0.125);
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
        assert_eq!(Moment::of_tick(8), Moment::At(1.0));
    }
}
