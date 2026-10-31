//! How the universe moves in the terminal: stepped at the world's rate, 12 or 16 times a second,
//! every loop rounded to whole quarter seconds that fit a 30-second cycle. A loop then takes
//! whole steps at either rate, so each moving thing's pictures repeat and the screen sends each
//! one once. Moving things that overlap are drawn together, over a copy of what lies still behind
//! them.
//!
//! Time is counted here in ticks, 48 a second, so a step at either rate is a whole number of them
//! and the same loop comes round on the same tick.

use crate::world::deliver::clock::Rate;

/// Ticks a second.
pub(super) const TICKS: f64 = 48.0;
/// Every loop's ticks divide this, 30 seconds of them.
pub(super) const CYCLE: u32 = 1_440;
/// The loops that fit the cycle in whole quarter seconds, 12 ticks each.
const LOOPS: [u32; 16] = [
    12, 24, 36, 48, 60, 72, 96, 120, 144, 180, 240, 288, 360, 480, 720, 1_440,
];

/// The ticks a loop of `seconds` takes, rounded to the nearest loop that fits the cycle.
pub(super) fn steps_for(seconds: f64) -> u32 {
    let want = (seconds * TICKS).max(1.0);
    LOOPS
        .iter()
        .copied()
        .min_by(|a, b| {
            (f64::from(*a) - want)
                .abs()
                .total_cmp(&(f64::from(*b) - want).abs())
        })
        .unwrap_or(CYCLE)
}

/// The seconds a loop of `seconds` takes once rounded to whole quarter seconds.
pub(super) fn stepped(seconds: f64) -> f64 {
    f64::from(steps_for(seconds)) / TICKS
}

/// The ticks one step at `rate` takes.
pub(super) fn ticks_a_step(rate: Rate) -> u64 {
    (TICKS as u64 / u64::from(rate.per_second().max(1))).max(1)
}

/// The tick `t` seconds falls in.
pub(super) fn step_of(t: f64) -> u64 {
    (t * TICKS + 1e-6).floor().max(0.0) as u64
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The fewest steps after which loops of `a` and `b` steps both come round.
pub(super) fn lcm(a: u32, b: u32) -> u32 {
    a / gcd(a, b).max(1) * b
}

/// A box in a picture's pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Bounds {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Bounds {
    pub fn around((x, y): (f64, f64), rx: f64, ry: f64) -> Self {
        Bounds {
            left: x - rx,
            top: y - ry,
            right: x + rx,
            bottom: y + ry,
        }
    }

    pub fn union(self, other: Self) -> Self {
        Bounds {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }

    fn overlaps(&self, other: &Self) -> bool {
        self.left < other.right
            && other.left < self.right
            && self.top < other.bottom
            && other.top < self.bottom
    }

    /// The whole pixels it covers inside a picture `width` by `height`, as left, top, width
    /// and height, or `None` where it misses the picture.
    pub fn pixels(&self, width: usize, height: usize) -> Option<(u32, u32, u32, u32)> {
        let left = self.left.floor().max(0.0);
        let top = self.top.floor().max(0.0);
        let right = self.right.ceil().min(width as f64);
        let bottom = self.bottom.ceil().min(height as f64);
        (right > left && bottom > top).then_some((
            left as u32,
            top as u32,
            (right - left) as u32,
            (bottom - top) as u32,
        ))
    }
}

/// Moving things whose boxes overlap, drawn as one picture.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Group {
    pub bounds: Bounds,
    /// Each one's index in the list the groups were made from.
    pub members: Vec<usize>,
    /// The ticks after which its pictures repeat.
    pub cycle: u32,
}

/// Where in its loop a group is at `t` seconds, in seconds, so a loop's pictures repeat.
pub(super) fn looped(group: &Group, t: f64) -> f64 {
    (step_of(t) % u64::from(group.cycle)) as f64 / TICKS
}

/// Gathers moving things into groups, each thing's box and loop given, so no two groups'
/// boxes overlap.
pub(super) fn groups(things: &[(Bounds, u32)]) -> Vec<Group> {
    let mut out: Vec<Group> = things
        .iter()
        .enumerate()
        .map(|(at, (bounds, steps))| Group {
            bounds: *bounds,
            members: vec![at],
            cycle: (*steps).max(1),
        })
        .collect();
    loop {
        let mut merged = false;
        'search: for i in 0..out.len() {
            for j in i + 1..out.len() {
                if out[i].bounds.overlaps(&out[j].bounds) {
                    let other = out.remove(j);
                    let group = &mut out[i];
                    group.bounds = group.bounds.union(other.bounds);
                    group.members.extend(other.members);
                    group.cycle = lcm(group.cycle, other.cycle);
                    merged = true;
                    break 'search;
                }
            }
        }
        if !merged {
            break;
        }
    }
    for group in &mut out {
        group.members.sort_unstable();
    }
    out
}

#[cfg(test)]
mod rate_tests {
    use super::*;

    #[test]
    fn every_loop_comes_round_on_a_whole_step_at_either_rate() {
        for seconds in [0.1, 0.4, 1.3, 2.0, 7.0, 20.0, 45.0] {
            let ticks = steps_for(seconds);
            assert_eq!(CYCLE % ticks, 0, "{seconds}");
            for rate in [Rate::QUICK, Rate::STEADY] {
                let steps = stepped(seconds) * f64::from(rate.per_second());
                assert_eq!(steps.fract(), 0.0, "{seconds} at {rate:?}");
            }
        }
        // A step at 12 a second is 4 ticks, at 16 a second 3.
        assert_eq!(step_of(Rate::STEADY.seconds(7)), 28);
        assert_eq!(step_of(Rate::QUICK.seconds(7)), 21);
    }
}
