//! How the universe moves in the terminal: stepped 8 times a second, every loop rounded to a
//! whole number of steps that fits a 30-second cycle, so each moving thing's pictures repeat and
//! the screen sends each one once. Moving things that overlap are drawn together, over a copy of
//! what lies still behind them.

/// Steps a second.
pub(super) const FPS: f64 = 8.0;
/// Every loop's steps divide this, 30 seconds of them.
pub(super) const CYCLE: u32 = 240;
const LOOPS: [u32; 20] = [
    1, 2, 3, 4, 5, 6, 8, 10, 12, 15, 16, 20, 24, 30, 40, 48, 60, 80, 120, 240,
];

/// The steps a loop of `seconds` takes, rounded to the nearest that divides the cycle.
pub(super) fn steps_for(seconds: f64) -> u32 {
    let want = (seconds * FPS).max(1.0);
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

/// The seconds a loop of `seconds` takes once rounded to whole steps.
pub(super) fn stepped(seconds: f64) -> f64 {
    f64::from(steps_for(seconds)) / FPS
}

/// The step `t` seconds falls in.
pub(super) fn step_of(t: f64) -> u64 {
    (t * FPS + 1e-6).floor().max(0.0) as u64
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
    /// The steps after which its pictures repeat.
    pub cycle: u32,
}

/// Where in its loop a group is at `t` seconds, in seconds, so a loop's pictures repeat.
pub(super) fn looped(group: &Group, t: f64) -> f64 {
    (step_of(t) % u64::from(group.cycle)) as f64 / FPS
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
