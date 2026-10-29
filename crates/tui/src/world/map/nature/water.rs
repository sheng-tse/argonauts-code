//! Where water runs: a river's bed down the valleys between two behaviors, a current's arc over
//! the sea, and a tsunami's run in from open water to its behavior's coast.

use std::f64::consts::PI;

use crate::world::map::fields::{Ground, HEIGHT, WIDTH};
use crate::world::map::land::Placed;
use crate::world::raster::noise::{hash3, smooth, vnoise3};

/// Pixels between the points a river's bed is searched over.
const BED_STEP: usize = 6;

/// A river's bed between two places: the cheapest way over the land, each step costing more the
/// higher the ground, so it runs down the valleys between the two behaviors' mountains. With no
/// way over land within 1.6 times the straight line, there is none, and the move is a current.
/// The bed is smoothed, then bent a little by noise drawn with `seed`, never at its ends.
pub fn valley_path(
    ground: &Ground,
    from: (f64, f64),
    to: (f64, f64),
    seed: f64,
) -> Option<Vec<(f64, f64)>> {
    let step = BED_STEP as f64;
    let across = WIDTH.div_ceil(BED_STEP) + 1;
    let down = HEIGHT.div_ceil(BED_STEP) + 1;
    // Costs and distances are kept as 32-bit floats, so ties fall the same way every time and the
    // same bed is found.
    let mut cost: Vec<f32> = (0..across * down)
        .map(|at| {
            let (x, y) = ((at % across) as f64 * step, (at / across) as f64 * step);
            let level = ground.land_at(x, y);
            if level < 1.06 {
                f32::INFINITY
            } else {
                (1.0 + 2.4 * (1.0 + ground.height_at(x, y)).log10()
                    + 3.0 * smooth(1.45, 1.06, level)) as f32
            }
        })
        .collect();
    let node = |x: f64, y: f64| {
        let column = (round(x / step).max(0.0) as usize).min(across - 1);
        let row = (round(y / step).max(0.0) as usize).min(down - 1);
        row * across + column
    };
    let (start, end) = (node(from.0, from.1), node(to.0, to.1));
    cost[start] = cost[start].min(3.0);
    cost[end] = cost[end].min(3.0);

    let mut distance = vec![f32::INFINITY; across * down];
    let mut previous = vec![usize::MAX; across * down];
    let mut heap = Heap::default();
    heap.push(0.0, start);
    distance[start] = 0.0;
    while let Some((so_far, at)) = heap.pop() {
        if at == end {
            break;
        }
        if so_far > f64::from(distance[at]) {
            continue;
        }
        let (column, row) = ((at % across) as i64, (at / across) as i64);
        for dj in -1..=1_i64 {
            for dk in -1..=1_i64 {
                if dj == 0 && dk == 0 {
                    continue;
                }
                let (k, j) = (column + dk, row + dj);
                if k < 0 || j < 0 || k >= across as i64 || j >= down as i64 {
                    continue;
                }
                let next = j as usize * across + k as usize;
                let step_cost = cost[next];
                if step_cost == f32::INFINITY {
                    continue;
                }
                let diagonal = if dj != 0 && dk != 0 { 1.414 } else { 1.0 };
                let further = so_far + f64::from(step_cost) * diagonal;
                if further < f64::from(distance[next]) {
                    distance[next] = further as f32;
                    previous[next] = at;
                    heap.push(further, next);
                }
            }
        }
    }
    if distance[end] == f32::INFINITY {
        return None;
    }
    let mut points = Vec::new();
    let mut at = end;
    while at != usize::MAX {
        points.push(((at % across) as f64 * step, (at / across) as f64 * step));
        at = previous[at];
    }
    points.reverse();
    let length: f64 = points
        .windows(2)
        .map(|pair| (pair[1].0 - pair[0].0).hypot(pair[1].1 - pair[0].1))
        .sum();
    if length > 1.6 * (to.0 - from.0).hypot(to.1 - from.1) + 30.0 {
        return None;
    }
    let last = points.len() - 1;
    points[0] = from;
    points[last] = to;
    // Chaikin's corners cut three times, so the bed curves.
    for _ in 0..3 {
        let mut cut = vec![points[0]];
        for pair in points.windows(2) {
            let (p, q) = (pair[0], pair[1]);
            cut.push((0.75 * p.0 + 0.25 * q.0, 0.75 * p.1 + 0.25 * q.1));
            cut.push((0.25 * p.0 + 0.75 * q.0, 0.25 * p.1 + 0.75 * q.1));
        }
        cut.push(points[points.len() - 1]);
        points = cut;
    }
    let last = (points.len() - 1) as f64;
    Some(
        points
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| {
                let t = i as f64 / last;
                let wobble = 2.2 * (PI * t).sin() * vnoise3(x / 9.0, y / 9.0, seed % 97.0, 13.0);
                (x + wobble, y - wobble)
            })
            .collect(),
    )
}

/// A current's way over the sea: a quadratic arc bent to one side by a fifth of its length, the
/// side drawn with `seed`.
pub fn arc(from: (f64, f64), to: (f64, f64), seed: f64) -> Vec<(f64, f64)> {
    let length = (to.0 - from.0).hypot(to.1 - from.1);
    let (nx, ny) = (-(to.1 - from.1) / length, (to.0 - from.0) / length);
    let side = if hash3(seed, 1.0, 2.0, 3.0) < 0.5 {
        -1.0
    } else {
        1.0
    };
    let bend = side * 0.22 * length;
    let (qx, qy) = (
        (from.0 + to.0) / 2.0 + nx * bend,
        (from.1 + to.1) / 2.0 + ny * bend,
    );
    let steps = 10.max((length / 4.0).ceil() as usize);
    (0..=steps)
        .map(|s| {
            let t = s as f64 / steps as f64;
            let (a, b, c) = ((1.0 - t) * (1.0 - t), 2.0 * (1.0 - t) * t, t * t);
            (
                a * from.0 + b * qx + c * to.0,
                a * from.1 + b * qy + c * to.1,
            )
        })
        .collect()
}

/// A tsunami's way in, before its size is known.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Run {
    pub from: (f64, f64),
    pub to: (f64, f64),
    pub out: (f64, f64),
    pub coast: (f64, f64),
}

/// A tsunami's run from open sea to a behavior's coast. Each of 36 ways out from the behavior's
/// middle is walked to the coast and on over the water; the way with the most open sea wins, a
/// little more when it runs across the screen, where its curl reads best, less when it would
/// cross a mark already placed or leave the map's middle band. The wave starts out at sea and
/// ends 10 px off the coast.
pub(super) fn tsunami_run(ground: &Ground, behavior: usize, placed: &[Placed]) -> Option<Run> {
    let (x0, y0) = ground.centre[behavior];
    let (width, height) = (WIDTH as f64, HEIGHT as f64);
    let mut best = None;
    let mut best_score = -1e9;
    for k in 0..36 {
        let angle = f64::from(k) * PI / 18.0;
        let (ux, uy) = (angle.cos(), angle.sin());
        let mut d = 0.0;
        while d < 240.0 && ground.land_at(x0 + ux * d, y0 + uy * d) >= 0.95 {
            d += 2.0;
        }
        if d >= 240.0 {
            continue;
        }
        let mut open = 0.0;
        while open < 150.0 && ground.land_at(x0 + ux * (d + open), y0 + uy * (d + open)) < 0.92 {
            open += 3.0;
        }
        let far = f64::min(open, 96.0) - 8.0;
        if far < 46.0 {
            continue;
        }
        let (sx, sy) = (x0 + ux * (d + far), y0 + uy * (d + far));
        let (ex, ey) = (x0 + ux * (d + 10.0), y0 + uy * (d + 10.0));
        if sx.min(ex) < 40.0
            || sx.max(ex) > width - 40.0
            || sy.min(ey) < 70.0
            || sy.max(ey) > height - 60.0
        {
            continue;
        }
        let mut score = far - 0.3 * d + 28.0 * ux.abs();
        let (mx, my) = ((sx + ex) / 2.0, (sy + ey) / 2.0);
        for mark in placed.iter().filter(|mark| mark.radius != 0.0) {
            let gap = (mark.x - mx).hypot(mark.y - my) - mark.radius - 30.0;
            if gap < 0.0 {
                score += gap * 3.0;
            }
        }
        if score > best_score {
            best_score = score;
            best = Some(Run {
                from: (sx, sy),
                to: (ex, ey),
                out: (ux, uy),
                coast: (x0 + ux * d, y0 + uy * d),
            });
        }
    }
    best
}

/// Rounds half up, as a point is rounded to its nearest grid point.
fn round(v: f64) -> f64 {
    (v + 0.5).floor()
}

/// A binary heap of distances in which equal distances leave in the order they came, so the
/// same bed is found.
#[derive(Default)]
struct Heap {
    items: Vec<(f64, usize)>,
}

impl Heap {
    fn push(&mut self, key: f64, at: usize) {
        self.items.push((key, at));
        let mut i = self.items.len() - 1;
        while i > 0 {
            let parent = (i - 1) >> 1;
            if self.items[parent].0 <= self.items[i].0 {
                break;
            }
            self.items.swap(parent, i);
            i = parent;
        }
    }

    fn pop(&mut self) -> Option<(f64, usize)> {
        if self.items.is_empty() {
            return None;
        }
        let top = self.items[0];
        let last = self.items.pop()?;
        if !self.items.is_empty() {
            self.items[0] = last;
            let mut i = 0;
            loop {
                let (left, right) = (2 * i + 1, 2 * i + 2);
                let mut least = i;
                if left < self.items.len() && self.items[left].0 < self.items[least].0 {
                    least = left;
                }
                if right < self.items.len() && self.items[right].0 < self.items[least].0 {
                    least = right;
                }
                if least == i {
                    break;
                }
                self.items.swap(least, i);
                i = least;
            }
        }
        Some(top)
    }
}

/// One stretch of an ocean current over open water, with what its band is drawn from.
#[derive(Debug, Clone, PartialEq)]
pub struct CurrentRun {
    pub points: Vec<(f64, f64)>,
    /// How far along the stretch each point lies, and its whole length.
    pub lengths: Vec<f64>,
    pub total: f64,
    /// The unit normal at each point, to the left of the way the agents went.
    pub normals: Vec<(f64, f64)>,
    /// How wide the band is at each point, from 0 at each coast to 1 within 18 px of it.
    pub taper: Vec<f64>,
    /// The band's full width: 8 px and 2.4 more per square root of the move's count.
    pub band: f64,
}

/// The stretches of a current's arc that run over the sea, each long enough to read.
pub fn current_runs(ground: &Ground, stream: &super::Stream) -> Vec<CurrentRun> {
    runs_at_sea(stream, |x, y| ground.land_at(x, y) < 0.9)
}

/// The stretches of a current's arc where `at_sea` holds, each long enough to read.
pub fn runs_at_sea(stream: &super::Stream, at_sea: impl Fn(f64, f64) -> bool) -> Vec<CurrentRun> {
    let band = 8.0 + 2.4 * f64::from(stream.moves).sqrt();
    let mut stretches: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut open: Vec<(f64, f64)> = Vec::new();
    for &(x, y) in &stream.path {
        if at_sea(x, y) {
            open.push((x, y));
        } else if !open.is_empty() {
            if open.len() > 3 {
                stretches.push(std::mem::take(&mut open));
            }
            open.clear();
        }
    }
    if open.len() > 3 {
        stretches.push(open);
    }
    stretches
        .into_iter()
        .map(|points| {
            let mut lengths = vec![0.0];
            for pair in points.windows(2) {
                let last = lengths[lengths.len() - 1];
                lengths.push(last + (pair[1].0 - pair[0].0).hypot(pair[1].1 - pair[0].1));
            }
            let total = lengths[lengths.len() - 1];
            let n = points.len();
            let normals = (0..n)
                .map(|i| {
                    let (a, b) = (points[i.saturating_sub(1)], points[(i + 1).min(n - 1)]);
                    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                    let l = dx.hypot(dy);
                    let l = if l == 0.0 { 1.0 } else { l };
                    (-dy / l, dx / l)
                })
                .collect();
            let taper = lengths
                .iter()
                .map(|v| smooth(0.0, 18.0, *v) * smooth(0.0, 18.0, total - v))
                .collect();
            CurrentRun {
                points,
                lengths,
                total,
                normals,
                taper,
                band,
            }
        })
        .filter(|run| run.total > 16.0)
        .collect()
}
