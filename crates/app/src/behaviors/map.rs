//! The behavior map's arithmetic, as Hodoscope (arXiv 2604.11072) works it: a Gaussian density
//! for each group, its bandwidth by Scott's rule; a group's density minus the mean of the
//! others' (equation 1); and Summaries' list, the group's actions in density-weighted
//! farthest-point order (equation 2, with beta = 0.1), so it starts where the group is most its
//! own and then spreads across the map. On the label map every action of a label sits in one
//! cloud, so equation 2 would rank clouds by size; its list ranks labels by how much more of the
//! group's share each is than of the others'.

use argonauts_core::behavior_map::GRID;

/// A group with fewer actions than this draws no density, and counts in nobody's mean.
pub(crate) const SMALLEST: usize = 5;
/// The weight an action keeps however common it is elsewhere, as the paper sets it.
const BETA: f64 = 0.1;
/// How far an action strays from its label's place, in map units: enough that a label's
/// actions show as a small cloud of dots rather than one.
const JITTER: f64 = 0.018;
/// The jitter's seed, so a cloud keeps its shape from one opening to the next.
const SEED: u64 = 0x6265_6861_7669_6f72;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    fn distance(self, other: Point) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

/// An action's dot: its label's place moved a little, by an amount fixed by the action's own
/// key, so adding actions never moves the ones already drawn. Kept inside the map.
pub(crate) fn jittered(place: (f64, f64), key: &str) -> Point {
    let mut state = SEED ^ fnv(key);
    let (dx, dy) = normal_pair(&mut state);
    Point {
        x: (place.0 + dx * JITTER).clamp(0.0, 1.0),
        y: (place.1 + dy * JITTER).clamp(0.0, 1.0),
    }
}

fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Two standard normal numbers, by Box and Muller.
fn normal_pair(state: &mut u64) -> (f64, f64) {
    let unit = |state: &mut u64| (splitmix(state) >> 11) as f64 / (1u64 << 53) as f64;
    let radius = (-2.0 * (1.0 - unit(state)).ln()).sqrt();
    let angle = std::f64::consts::TAU * unit(state);
    (radius * angle.cos(), radius * angle.sin())
}

/// A Gaussian kernel density over some points, its kernel the points' covariance scaled by
/// Scott's factor, n^(-1/6) in two dimensions, as scipy's `gaussian_kde` builds it.
pub(crate) struct Density<'a> {
    points: &'a [Point],
    /// The kernel covariance's inverse, as (a, b, c) of [[a, b], [b, c]].
    inverse: (f64, f64, f64),
    scale: f64,
}

impl<'a> Density<'a> {
    /// `None` for fewer than two points, which have no spread to measure.
    pub fn new(points: &'a [Point]) -> Option<Self> {
        let n = points.len();
        if n < 2 {
            return None;
        }
        let count = n as f64;
        let mean_x = points.iter().map(|p| p.x).sum::<f64>() / count;
        let mean_y = points.iter().map(|p| p.y).sum::<f64>() / count;
        let (mut xx, mut xy, mut yy) = (0.0, 0.0, 0.0);
        for p in points {
            let (dx, dy) = (p.x - mean_x, p.y - mean_y);
            xx += dx * dx;
            xy += dx * dy;
            yy += dy * dy;
        }
        let factor = count.powf(-1.0 / 6.0).powi(2) / (count - 1.0);
        // Points along one line, or all in one place, have no area: a floor of the jitter's
        // own spread keeps the kernel round there instead of infinitely thin.
        let floor = (JITTER * JITTER) * count.powf(-1.0 / 3.0);
        let a = (xx * factor).max(floor);
        let c = (yy * factor).max(floor);
        let mut b = xy * factor;
        let limit = 0.999 * (a * c).sqrt();
        b = b.clamp(-limit, limit);
        let det = a * c - b * b;
        Some(Self {
            points,
            inverse: (c / det, -b / det, a / det),
            scale: 1.0 / (std::f64::consts::TAU * det.sqrt() * count),
        })
    }

    pub fn at(&self, at: Point) -> f64 {
        let (a, b, c) = self.inverse;
        let sum: f64 = self
            .points
            .iter()
            .map(|p| {
                let (dx, dy) = (at.x - p.x, at.y - p.y);
                (-0.5 * (a * dx * dx + 2.0 * b * dx * dy + c * dy * dy)).exp()
            })
            .sum();
        sum * self.scale
    }
}

/// The grid's places, row by row from `y = 0`, each row from `x = 0`.
pub(crate) fn grid_places() -> impl Iterator<Item = Point> {
    let step = 1.0 / (GRID - 1) as f64;
    (0..GRID).flat_map(move |row| {
        (0..GRID).map(move |column| Point {
            x: column as f64 * step,
            y: row as f64 * step,
        })
    })
}

/// Why a group has no overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoOverlay {
    /// It has fewer than [`SMALLEST`] actions.
    Small(usize),
    /// No other group has enough to compare it with.
    Alone,
}

/// One group's density minus the mean of the others', on the grid and at the group's own
/// actions.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Difference {
    pub grid: Vec<f64>,
    /// At each of the group's points, in their order.
    pub at_points: Vec<f64>,
}

impl Difference {
    /// The grid scaled so the largest difference either way is 1 across, for drawing.
    pub fn scaled(&self) -> Vec<f32> {
        let largest = self.grid.iter().fold(0.0f64, |most, v| most.max(v.abs()));
        let largest = if largest > 0.0 { largest } else { 1.0 };
        self.grid.iter().map(|v| (v / largest) as f32).collect()
    }
}

/// Equation 1 for every group: its density less the mean of every other group's that has
/// [`SMALLEST`] actions or more. Each group's density is worked out on the grid once, however
/// many groups there are, and the difference at a group's own actions is read off the grid.
pub(crate) fn differences(groups: &[Vec<Point>]) -> Vec<Result<Difference, NoOverlay>> {
    let drawable = |points: &Vec<Point>| points.len() >= SMALLEST;
    // With one group to draw there is nothing to compare it with, and no grid to work out.
    let compared = groups.iter().filter(|points| drawable(points)).count() > 1;
    let grids: Vec<Option<Vec<f64>>> = groups
        .iter()
        .map(|points| {
            let density = Density::new(points).filter(|_| compared && drawable(points))?;
            Some(grid_places().map(|at| density.at(at)).collect())
        })
        .collect();
    let drawn = grids.iter().flatten().count();
    let mut all = vec![0.0; GRID * GRID];
    for grid in grids.iter().flatten() {
        for (sum, value) in all.iter_mut().zip(grid) {
            *sum += value;
        }
    }
    groups
        .iter()
        .zip(&grids)
        .map(|(own, grid)| {
            if own.len() < SMALLEST {
                return Err(NoOverlay::Small(own.len()));
            }
            let Some(mine) = grid.as_ref().filter(|_| drawn > 1) else {
                return Err(NoOverlay::Alone);
            };
            let others = drawn - 1;
            let grid: Vec<f64> = mine
                .iter()
                .zip(&all)
                .map(|(mine, sum)| mine - (sum - mine) / others as f64)
                .collect();
            let at_points = own.iter().map(|point| on_grid(&grid, *point)).collect();
            Ok(Difference { grid, at_points })
        })
        .collect()
}

/// The grid's value at `at`, between its four nearest places.
fn on_grid(grid: &[f64], at: Point) -> f64 {
    let last = (GRID - 1) as f64;
    let (x, y) = (at.x.clamp(0.0, 1.0) * last, at.y.clamp(0.0, 1.0) * last);
    let (column, row) = (x.floor().min(last - 1.0), y.floor().min(last - 1.0));
    let (dx, dy) = (x - column, y - row);
    let value = |row: f64, column: f64| grid[row as usize * GRID + column as usize];
    let top = value(row, column) * (1.0 - dx) + value(row, column + 1.0) * dx;
    let bottom = value(row + 1.0, column) * (1.0 - dx) + value(row + 1.0, column + 1.0) * dx;
    top * (1.0 - dy) + bottom * dy
}

/// Equation 2: the group's points in density-weighted farthest-point order, at most `k`. Each
/// point weighs 0.1 to 1 where the group is over-represented, by how much, and 0 to 0.1 where
/// it is under; the first is the heaviest, and each next one the heaviest times its distance
/// from those already taken.
pub(crate) fn farthest_first(points: &[Point], gaps: &[f64], k: usize) -> Vec<usize> {
    let most = gaps.iter().fold(0.0f64, |m, g| m.max(*g));
    let least = gaps.iter().fold(0.0f64, |m, g| m.max(-*g));
    let weights: Vec<f64> = gaps
        .iter()
        .map(|&gap| {
            if gap > 0.0 {
                BETA + (1.0 - BETA) * gap / if most > 0.0 { most } else { 1.0 }
            } else {
                BETA * (1.0 - (-gap) / if least > 0.0 { least } else { 1.0 })
            }
        })
        .collect();
    let mut chosen: Vec<usize> = Vec::new();
    let mut nearest = vec![f64::INFINITY; points.len()];
    while chosen.len() < k.min(points.len()) {
        let score = |at: usize| match chosen.is_empty() {
            true => weights[at],
            false => weights[at] * nearest[at],
        };
        let next = (0..points.len())
            .filter(|at| !chosen.contains(at))
            .max_by(|a, b| score(*a).total_cmp(&score(*b)).then(b.cmp(a)));
        let Some(next) = next else { break };
        chosen.push(next);
        for (at, point) in points.iter().enumerate() {
            nearest[at] = nearest[at].min(point.distance(points[next]));
        }
    }
    chosen
}

/// A behavior's counts in one group and in the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Counted {
    pub behavior: String,
    pub mine: usize,
    pub others: usize,
}

/// How many times a group does a behavior before the label map lists it.
pub(crate) const LISTED_FROM: usize = 3;

/// The label map's list: each behavior the group did [`LISTED_FROM`] times or more, by how
/// much more of its share it is than of the others', the others' count smoothed by one so a
/// behavior nobody else shows doesn't divide by nothing.
pub(crate) fn by_share(mut counted: Vec<Counted>, mine: usize, others: usize) -> Vec<Counted> {
    let (mine, others) = (mine.max(1) as f64, others.max(1) as f64);
    let lift = |c: &Counted| (c.mine as f64 / mine) / ((c.others as f64 + 1.0) / others);
    counted.retain(|c| c.mine >= LISTED_FROM);
    counted.sort_by(|a, b| {
        lift(b)
            .total_cmp(&lift(a))
            .then(b.mine.cmp(&a.mine))
            .then_with(|| a.behavior.cmp(&b.behavior))
    });
    counted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cloud(place: (f64, f64), count: usize, name: &str) -> Vec<Point> {
        (0..count)
            .map(|n| jittered(place, &format!("{name}-{n}")))
            .collect()
    }

    #[test]
    fn a_labels_actions_sit_in_a_small_cloud_that_keeps_its_shape() {
        let first = cloud((0.5, 0.5), 30, "run-1");
        assert_eq!(first, cloud((0.5, 0.5), 30, "run-1"), "a fixed seed");
        let spread = first.iter().map(|p| p.distance(Point { x: 0.5, y: 0.5 }));
        let widest = spread.fold(0.0f64, f64::max);
        assert!(widest > 0.01 && widest < 0.1, "{widest}");
        let distinct: std::collections::BTreeSet<_> = first
            .iter()
            .map(|p| ((p.x * 1e6) as i64, (p.y * 1e6) as i64))
            .collect();
        assert_eq!(distinct.len(), 30, "several dots, not one");
        let edge = jittered((0.0, 1.0), "corner");
        assert!((0.0..=1.0).contains(&edge.x) && (0.0..=1.0).contains(&edge.y));
    }

    /// scipy's `gaussian_kde([[0.1, 0.4, 0.5, 0.9], [0.2, 0.3, 0.8, 0.6]])` at (0.5, 0.5) and
    /// at (0.1, 0.9), worked out by hand from its covariance and Scott's factor.
    #[test]
    fn the_density_is_scotts_kernel_density() {
        let points = [(0.1, 0.2), (0.4, 0.3), (0.5, 0.8), (0.9, 0.6)].map(|(x, y)| Point { x, y });
        let density = Density::new(&points).unwrap();
        let mean = |f: fn(&Point) -> f64| points.iter().map(f).sum::<f64>() / 4.0;
        let (mx, my) = (mean(|p| p.x), mean(|p| p.y));
        let cov = |f: fn(&Point) -> f64, g: fn(&Point) -> f64, mf: f64, mg: f64| {
            points
                .iter()
                .map(|p| (f(p) - mf) * (g(p) - mg))
                .sum::<f64>()
                / 3.0
        };
        let factor = 4f64.powf(-1.0 / 6.0).powi(2);
        let a = cov(|p| p.x, |p| p.x, mx, mx) * factor;
        let b = cov(|p| p.x, |p| p.y, mx, my) * factor;
        let c = cov(|p| p.y, |p| p.y, my, my) * factor;
        let det = a * c - b * b;
        let by_hand = |x: f64, y: f64| {
            points
                .iter()
                .map(|p| {
                    let (dx, dy) = (x - p.x, y - p.y);
                    let q = (c * dx * dx - 2.0 * b * dx * dy + a * dy * dy) / det;
                    (-0.5 * q).exp() / (std::f64::consts::TAU * det.sqrt())
                })
                .sum::<f64>()
                / 4.0
        };
        for (x, y) in [(0.5, 0.5), (0.1, 0.9)] {
            let ours = density.at(Point { x, y });
            assert!((ours - by_hand(x, y)).abs() < 1e-9, "{ours} at {x},{y}");
        }
    }

    #[test]
    fn one_place_or_one_line_still_has_a_round_kernel() {
        let same = vec![Point { x: 0.3, y: 0.3 }; 6];
        let density = Density::new(&same).unwrap();
        assert!(density.at(Point { x: 0.3, y: 0.3 }).is_finite());
        assert!(density.at(Point { x: 0.3, y: 0.3 }) > density.at(Point { x: 0.35, y: 0.3 }));
        assert!(Density::new(&same[..1]).is_none());
    }

    #[test]
    fn the_difference_is_red_where_a_group_acts_more_and_blue_where_less() {
        let mut target = cloud((0.2, 0.2), 20, "t-a");
        target.extend(cloud((0.8, 0.8), 2, "t-b"));
        let mut other = cloud((0.8, 0.8), 20, "o-b");
        other.extend(cloud((0.2, 0.2), 2, "o-a"));
        let third = cloud((0.8, 0.8), 10, "third");
        let groups = vec![target, other, third];
        let diff = differences(&groups).remove(0).unwrap();
        assert_eq!(diff.grid.len(), GRID * GRID);
        let at = |x: f64, y: f64| {
            let column = (x * (GRID - 1) as f64).round() as usize;
            let row = (y * (GRID - 1) as f64).round() as usize;
            diff.grid[row * GRID + column]
        };
        assert!(at(0.2, 0.2) > 0.0 && at(0.8, 0.8) < 0.0);
        let scaled = diff.scaled();
        let largest = scaled.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((largest - 1.0).abs() < 1e-6);
        assert_eq!(diff.at_points.len(), 22);
        assert!(diff.at_points[0] > 0.0 && diff.at_points[21] < 0.0);
    }

    #[test]
    fn a_small_group_or_a_lone_one_has_no_overlay() {
        let groups = vec![cloud((0.5, 0.5), 4, "small"), cloud((0.2, 0.2), 9, "big")];
        let found = differences(&groups);
        assert_eq!(found[0], Err(NoOverlay::Small(4)));
        // The small group counts in nobody's mean, so the big one has nothing to compare with.
        assert_eq!(found[1], Err(NoOverlay::Alone));
    }

    #[test]
    fn each_groups_difference_is_its_density_less_the_others_mean() {
        let groups = vec![
            cloud((0.2, 0.2), 12, "a"),
            cloud((0.8, 0.8), 9, "b"),
            cloud((0.5, 0.2), 7, "c"),
        ];
        let found = differences(&groups);
        let density = |points: &[Point], at: Point| Density::new(points).unwrap().at(at);
        let at = Point { x: 0.4, y: 0.6 };
        let column = (at.x * (GRID - 1) as f64).round() as usize;
        let row = (at.y * (GRID - 1) as f64).round() as usize;
        let place = Point {
            x: column as f64 / (GRID - 1) as f64,
            y: row as f64 / (GRID - 1) as f64,
        };
        let by_hand = density(&groups[1], place)
            - (density(&groups[0], place) + density(&groups[2], place)) / 2.0;
        let ours = found[1].as_ref().unwrap().grid[row * GRID + column];
        assert!((ours - by_hand).abs() < 1e-9, "{ours} against {by_hand}");
        let own = found[0].as_ref().unwrap();
        assert_eq!(own.at_points.len(), 12);
        assert!(
            own.at_points.iter().all(|gap| *gap > 0.0),
            "{:?}",
            own.at_points
        );
    }

    #[test]
    fn the_ranking_starts_where_the_group_is_most_its_own_then_spreads() {
        let points = [(0.1, 0.1), (0.12, 0.1), (0.9, 0.9), (0.5, 0.5)].map(|(x, y)| Point { x, y });
        let gaps = [1.0, 0.95, 0.5, -0.4];
        let order = farthest_first(&points, &gaps, 3);
        // The heaviest first; then the far one beats the near twin of the first.
        assert_eq!(order, vec![0, 2, 1]);
        assert_eq!(farthest_first(&points, &gaps, 10).len(), 4);
        // Under-represented actions keep a little weight, never none above zero distance.
        assert_eq!(farthest_first(&points[3..], &gaps[3..], 1), vec![0]);
    }

    #[test]
    fn the_label_list_is_by_share_against_the_rest() {
        let counted = |behavior: &str, mine, others| Counted {
            behavior: behavior.to_owned(),
            mine,
            others,
        };
        // GPT 6 Luna's five rows, 66 actions against the others' 496.
        let rows = vec![
            counted("returned its structured answer", 34, 135),
            counted("edited a source file", 3, 23),
            counted("ran the test suite", 8, 19),
            counted("checked git status or diff and read git history", 4, 2),
            counted("checked git status or diff and ran a shell command", 3, 4),
            counted("read a file once", 2, 0),
        ];
        let ranked: Vec<String> = by_share(rows, 66, 496)
            .into_iter()
            .map(|c| c.behavior)
            .collect();
        assert_eq!(
            ranked,
            [
                "checked git status or diff and read git history",
                "checked git status or diff and ran a shell command",
                "ran the test suite",
                "returned its structured answer",
                "edited a source file",
            ]
        );
    }
}
