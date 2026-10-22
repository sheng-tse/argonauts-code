//! The deeper map's positions: t-SNE (van der Maaten and Hinton, 2008) from the summaries'
//! vectors to two dimensions. Smaller groups are repeated, up to four times, before the fit,
//! as Hodoscope does, so the largest group doesn't decide the layout alone. A map that already
//! has positions keeps them: its old dots stay exactly where they were, and only the new ones
//! find their places among them.
//!
//! So that ten thousand actions place in seconds, the vectors are first cast to a few dozen
//! dimensions by a fixed random projection, which keeps their distances near enough, each
//! action's neighbours are looked for among the nearest actions' repeats only, and the fit's
//! repulsion is Barnes and Hut's.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::linalg::Seeded;
use super::quadtree::{QuadTree, Repulsion};

const SEED: u64 = 7;
const PERPLEXITY: f64 = 30.0;
/// How many times a smaller group may be repeated.
const OVERSAMPLE: usize = 4;
const ITERATIONS: usize = 1000;
const EXAGGERATED: usize = 250;
const EXAGGERATION: f64 = 12.0;
/// Iterations that place new dots among the old ones, which stay put.
const PLACING: usize = 250;
/// The dimensions vectors are cast to before neighbours are found.
const PROJECTED: usize = 48;
/// How often, in iterations, the fit looks whether it was asked to stop.
const STOP_EVERY: usize = 25;
/// Up to this many rows every pair's repulsion is worked out, which is quicker than a tree.
const EVERY_PAIR_UP_TO: usize = 1000;
/// How far a repeated action is moved from its original, so the two aren't one point.
const REPEAT_JITTER: f64 = 0.01;

/// Where the fit's own positions lie, so a later fit puts the old dots back exactly where
/// they were on the 0..1 map.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct Frame {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Frame {
    fn raw(&self, (x, y): (f32, f32)) -> [f64; 2] {
        [
            f64::from(self.x + x * self.width),
            f64::from(self.y + y * self.height),
        ]
    }

    fn place(&self, [x, y]: [f64; 2]) -> (f32, f32) {
        let across = |value: f64, start: f32, span: f32| {
            if span > 0.0 {
                ((value as f32 - start) / span).clamp(0.0, 1.0)
            } else {
                0.5
            }
        };
        (
            across(x, self.x, self.width),
            across(y, self.y, self.height),
        )
    }

    fn around(points: &[[f64; 2]]) -> Frame {
        let (mut low, mut high) = ([f64::MAX; 2], [f64::MIN; 2]);
        for point in points {
            for axis in 0..2 {
                low[axis] = low[axis].min(point[axis]);
                high[axis] = high[axis].max(point[axis]);
            }
        }
        Frame {
            x: low[0] as f32,
            y: low[1] as f32,
            width: (high[0] - low[0]) as f32,
            height: (high[1] - low[1]) as f32,
        }
    }
}

/// One action to place.
pub(crate) struct ToPlace<'a> {
    pub vector: &'a [f32],
    /// What it's compared by, such as its model.
    pub group: &'a str,
    /// Its place on the last map, when it had one.
    pub old: Option<(f32, f32)>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Placement {
    /// Each action's place, each axis in 0..1, in the order given.
    pub places: Vec<(f32, f32)>,
    pub frame: Frame,
}

/// Places `points` on the map. `frame` is the last map's, which old places are read in.
/// `None` when `stopped` said so before the fit was done; nothing is placed then.
pub(crate) fn project(
    points: &[ToPlace<'_>],
    frame: Option<Frame>,
    stopped: &dyn Fn() -> bool,
) -> Option<Placement> {
    let whole = Frame {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };
    let frame = frame.filter(|_| points.iter().any(|point| point.old.is_some()));
    let all_old = points.iter().all(|point| point.old.is_some());
    match (points.len(), frame) {
        (0, _) => {
            return Some(Placement {
                places: Vec::new(),
                frame: frame.unwrap_or(whole),
            });
        }
        (1, _) => {
            return Some(Placement {
                places: vec![points[0].old.unwrap_or((0.5, 0.5))],
                frame: frame.unwrap_or(whole),
            });
        }
        (_, Some(frame)) if all_old => {
            return Some(Placement {
                places: points.iter().filter_map(|point| point.old).collect(),
                frame,
            });
        }
        _ => {}
    }
    let mut random = Seeded::new(SEED);
    let rows = oversampled(points, &mut random);
    let affinities = affinities(&rows, points.len());
    match frame {
        Some(frame) => {
            let pinned: Vec<bool> = rows
                .iter()
                .map(|row| points[row.origin].old.is_some())
                .collect();
            let start = beside_old(points, &rows, &affinities, frame, &mut random);
            let raw = fit(&affinities, start, &pinned, PLACING, false, stopped)?;
            Some(Placement {
                places: raw[..points.len()]
                    .iter()
                    .zip(points)
                    .map(|(raw, point)| point.old.unwrap_or_else(|| frame.place(*raw)))
                    .collect(),
                frame,
            })
        }
        None => {
            let start = principal_start(&rows);
            let pinned = vec![false; rows.len()];
            let raw = fit(&affinities, start, &pinned, ITERATIONS, true, stopped)?;
            let frame = Frame::around(&raw[..points.len()]);
            Some(Placement {
                places: raw[..points.len()]
                    .iter()
                    .map(|raw| frame.place(*raw))
                    .collect(),
                frame,
            })
        }
    }
}

/// An action's vector in the fit: every action once, then the repeats of smaller groups.
struct Row {
    vector: Vec<f64>,
    origin: usize,
}

/// Every action once, in order, then the repeats of smaller groups, each vector cast down to
/// [`PROJECTED`] dimensions when it has more.
fn oversampled(points: &[ToPlace<'_>], random: &mut Seeded) -> Vec<Row> {
    let width = points.first().map_or(0, |point| point.vector.len());
    let cast = Cast::new(width);
    let mut rows: Vec<Row> = points
        .iter()
        .enumerate()
        .map(|(origin, point)| Row {
            vector: cast.of(point.vector),
            origin,
        })
        .collect();
    let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
    for (at, point) in points.iter().enumerate() {
        match groups.iter_mut().find(|(group, _)| *group == point.group) {
            Some((_, members)) => members.push(at),
            None => groups.push((point.group, vec![at])),
        }
    }
    let biggest = groups
        .iter()
        .map(|(_, members)| members.len())
        .max()
        .unwrap_or(0);
    for (_, members) in &groups {
        let extra = biggest.min(OVERSAMPLE * members.len()) - members.len();
        for _ in 0..extra {
            let origin = members[random.below(members.len())];
            let vector = rows[origin]
                .vector
                .iter()
                .map(|&x| x + REPEAT_JITTER * random.normal())
                .collect();
            rows.push(Row { vector, origin });
        }
    }
    rows
}

/// A fixed random projection to [`PROJECTED`] dimensions (Johnson and Lindenstrauss), so the
/// same vectors always cast the same way. Narrower vectors are kept as they are.
struct Cast {
    columns: Option<Vec<Vec<f64>>>,
}

impl Cast {
    fn new(width: usize) -> Cast {
        if width <= PROJECTED {
            return Cast { columns: None };
        }
        let mut random = Seeded::new(SEED + 3);
        let scale = 1.0 / (PROJECTED as f64).sqrt();
        let columns = (0..PROJECTED)
            .map(|_| (0..width).map(|_| scale * random.normal()).collect())
            .collect();
        Cast {
            columns: Some(columns),
        }
    }

    fn of(&self, vector: &[f32]) -> Vec<f64> {
        let wide: Vec<f64> = vector.iter().map(|&x| f64::from(x)).collect();
        match &self.columns {
            None => wide,
            Some(columns) => columns
                .iter()
                .map(|column| super::linalg::dot(column, &wide))
                .collect(),
        }
    }
}

/// The fit's joint probabilities, over each row's nearest neighbours: `(row, neighbour, p)`,
/// symmetric and summing to 1.
struct Affinities {
    n: usize,
    /// For each row, its neighbours and their joint probability.
    near: Vec<Vec<(usize, f64)>>,
}

/// A row's neighbours are looked for among the repeats of its own action and of the actions
/// nearest to that action, which holds every near row since a repeat sits on its original.
fn affinities(rows: &[Row], actions: usize) -> Affinities {
    let n = rows.len();
    let perplexity = PERPLEXITY.min((n as f64 - 1.0) / 3.0).max(1.0);
    let neighbours = ((3.0 * perplexity) as usize + 1).min(n - 1);
    let mut repeats: Vec<Vec<usize>> = vec![Vec::new(); actions];
    for (at, row) in rows.iter().enumerate() {
        repeats[row.origin].push(at);
    }
    let nearest_actions: Vec<Vec<usize>> = (0..actions)
        .map(|action| {
            let mut distances: Vec<(usize, f64)> = (0..actions)
                .filter(|other| *other != action)
                .map(|other| (other, squared(&rows[action].vector, &rows[other].vector)))
                .collect();
            nearest(&mut distances, neighbours);
            distances.into_iter().map(|(other, _)| other).collect()
        })
        .collect();
    let mut conditional: Vec<HashMap<usize, f64>> = Vec::with_capacity(n);
    for (i, row) in rows.iter().enumerate() {
        let near_actions =
            std::iter::once(row.origin).chain(nearest_actions[row.origin].iter().copied());
        let mut distances: Vec<(usize, f64)> = near_actions
            .flat_map(|action| repeats[action].iter().copied())
            .filter(|j| *j != i)
            .map(|j| (j, squared(&row.vector, &rows[j].vector)))
            .collect();
        nearest(&mut distances, neighbours);
        conditional.push(perplexity_row(&distances, perplexity));
    }
    let mut near: Vec<HashMap<usize, f64>> = vec![HashMap::new(); n];
    for (i, row) in conditional.iter().enumerate() {
        for (&j, &p) in row {
            let both = p + conditional[j].get(&i).copied().unwrap_or(0.0);
            let joint = (both / (2.0 * n as f64)).max(1e-12);
            near[i].insert(j, joint);
            near[j].insert(i, joint);
        }
    }
    Affinities {
        n,
        near: near
            .into_iter()
            .map(|row| {
                let mut row: Vec<(usize, f64)> = row.into_iter().collect();
                row.sort_by_key(|(j, _)| *j);
                row
            })
            .collect(),
    }
}

/// The `count` nearest of `distances`, nearest first.
fn nearest(distances: &mut Vec<(usize, f64)>, count: usize) {
    let by_distance = |a: &(usize, f64), b: &(usize, f64)| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0));
    if distances.len() > count {
        distances.select_nth_unstable_by(count, by_distance);
        distances.truncate(count);
    }
    distances.sort_by(by_distance);
}

/// One row's conditional probabilities, with the Gaussian's width found by bisection so their
/// perplexity is `perplexity`.
fn perplexity_row(distances: &[(usize, f64)], perplexity: f64) -> HashMap<usize, f64> {
    let target = perplexity.ln();
    let (mut beta, mut low, mut high) = (1.0f64, 0.0f64, f64::INFINITY);
    let nearest = distances.first().map_or(0.0, |(_, d)| *d);
    let mut weights = vec![0.0; distances.len()];
    for _ in 0..100 {
        let mut sum = 0.0;
        for (weight, (_, d)) in weights.iter_mut().zip(distances) {
            *weight = (-(d - nearest) * beta).exp();
            sum += *weight;
        }
        let sum = sum.max(1e-300);
        let entropy: f64 = sum.ln()
            + beta
                * weights
                    .iter()
                    .zip(distances)
                    .map(|(w, (_, d))| w * (d - nearest))
                    .sum::<f64>()
                / sum;
        if (entropy - target).abs() < 1e-5 {
            break;
        }
        if entropy > target {
            low = beta;
            beta = if high.is_finite() {
                (beta + high) / 2.0
            } else {
                beta * 2.0
            };
        } else {
            high = beta;
            beta = (beta + low) / 2.0;
        }
    }
    let sum: f64 = weights.iter().sum::<f64>().max(1e-300);
    distances
        .iter()
        .zip(&weights)
        .map(|((j, _), w)| (*j, w / sum))
        .collect()
}

fn squared(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum()
}

/// A first fit starts on the rows' two principal directions, scaled small, as scikit-learn's
/// does, so the same vectors always give the same map.
fn principal_start(rows: &[Row]) -> Vec<[f64; 2]> {
    let width = rows[0].vector.len();
    let n = rows.len() as f64;
    let mean: Vec<f64> = (0..width)
        .map(|c| rows.iter().map(|row| row.vector[c]).sum::<f64>() / n)
        .collect();
    let centred: Vec<Vec<f64>> = rows
        .iter()
        .map(|row| row.vector.iter().zip(&mean).map(|(x, m)| x - m).collect())
        .collect();
    let mut random = Seeded::new(SEED + 1);
    let mut directions: Vec<Vec<f64>> = Vec::new();
    for _ in 0..2 {
        let mut v: Vec<f64> = (0..width).map(|_| random.normal()).collect();
        for _ in 0..60 {
            // v = Xᵀ X v, kept apart from the directions already found.
            let along: Vec<f64> = centred
                .iter()
                .map(|row| super::linalg::dot(row, &v))
                .collect();
            let mut next = vec![0.0; width];
            for (row, a) in centred.iter().zip(&along) {
                for (out, x) in next.iter_mut().zip(row) {
                    *out += a * x;
                }
            }
            for found in &directions {
                let overlap = super::linalg::dot(found, &next);
                next.iter_mut()
                    .zip(found)
                    .for_each(|(x, f)| *x -= overlap * f);
            }
            let norm = super::linalg::dot(&next, &next).sqrt();
            if norm < 1e-12 {
                break;
            }
            v = next.into_iter().map(|x| x / norm).collect();
        }
        directions.push(v);
    }
    let mut start: Vec<[f64; 2]> = centred
        .iter()
        .map(|row| {
            [
                super::linalg::dot(row, &directions[0]),
                super::linalg::dot(row, &directions[1]),
            ]
        })
        .collect();
    let deviation = (start.iter().map(|p| p[0] * p[0]).sum::<f64>() / n).sqrt();
    let scale = if deviation > 1e-12 {
        1e-4 / deviation
    } else {
        1e-4
    };
    let mut jitter = Seeded::new(SEED + 2);
    for point in &mut start {
        point[0] = point[0] * scale + 1e-6 * jitter.normal();
        point[1] = point[1] * scale + 1e-6 * jitter.normal();
    }
    start
}

/// Old rows start where they were; a new one starts on its nearest old neighbour, a little
/// off it, or near the middle when none of its neighbours is old.
fn beside_old(
    points: &[ToPlace<'_>],
    rows: &[Row],
    affinities: &Affinities,
    frame: Frame,
    random: &mut Seeded,
) -> Vec<[f64; 2]> {
    let spread = f64::from(frame.width.max(frame.height)) * 0.01;
    let middle = frame.raw((0.5, 0.5));
    rows.iter()
        .enumerate()
        .map(|(at, row)| {
            if let Some(old) = points[row.origin].old {
                return frame.raw(old);
            }
            let anchor = affinities.near[at]
                .iter()
                .filter_map(|(j, p)| Some((points[rows[*j].origin].old?, *p)))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map_or(middle, |(old, _)| frame.raw(old));
            [
                anchor[0] + spread * random.normal(),
                anchor[1] + spread * random.normal(),
            ]
        })
        .collect()
}

/// What every other row does to each row: by every pair for a small map, else by the tree.
fn repulsions(y: &[[f64; 2]]) -> Vec<Repulsion> {
    if y.len() > EVERY_PAIR_UP_TO {
        let tree = QuadTree::of(y);
        let mut stack = Vec::new();
        return (0..y.len())
            .map(|i| tree.repulsion(i, &mut stack))
            .collect();
    }
    y.iter()
        .enumerate()
        .map(|(i, &[x, z])| {
            let mut out = Repulsion::default();
            for (j, &[other_x, other_z]) in y.iter().enumerate() {
                if i == j {
                    continue;
                }
                let (dx, dz) = (x - other_x, z - other_z);
                let kernel = 1.0 / (1.0 + dx * dx + dz * dz);
                out.kernels += kernel;
                out.push[0] += kernel * kernel * dx;
                out.push[1] += kernel * kernel * dz;
            }
            out
        })
        .collect()
}

/// Gradient descent with momentum and a gain for each axis of each row, as the paper and
/// scikit-learn do, moving only the rows not `pinned`. `None` once `stopped` says so.
fn fit(
    affinities: &Affinities,
    mut y: Vec<[f64; 2]>,
    pinned: &[bool],
    iterations: usize,
    exaggerate: bool,
    stopped: &dyn Fn() -> bool,
) -> Option<Vec<[f64; 2]>> {
    let n = affinities.n;
    let rate = (n as f64 / EXAGGERATION / 4.0).max(50.0);
    let mut update = vec![[0.0f64; 2]; n];
    let mut gains = vec![[1.0f64; 2]; n];
    let moving: Vec<usize> = (0..n).filter(|&i| !pinned[i]).collect();
    if moving.is_empty() {
        return Some(y);
    }
    for step in 0..iterations {
        if step % STOP_EVERY == 0 && stopped() {
            return None;
        }
        let early = exaggerate && step < EXAGGERATED;
        let exaggeration = if early { EXAGGERATION } else { 1.0 };
        let momentum = if early || (!exaggerate && step < 100) {
            0.5
        } else {
            0.8
        };
        // Z, the sum of every pair's kernel, which the repulsion is divided by: a pinned row
        // has its share of it too.
        let repulsions = repulsions(&y);
        let z = repulsions
            .iter()
            .map(|repulsion| repulsion.kernels)
            .sum::<f64>()
            .max(1e-300);
        let mut gradient = vec![[0.0f64; 2]; n];
        for &i in &moving {
            let mut g = [0.0f64; 2];
            for (j, p) in &affinities.near[i] {
                let dx = y[i][0] - y[*j][0];
                let dy = y[i][1] - y[*j][1];
                let w = 1.0 / (1.0 + dx * dx + dy * dy);
                g[0] += exaggeration * p * w * dx;
                g[1] += exaggeration * p * w * dy;
            }
            g[0] -= repulsions[i].push[0] / z;
            g[1] -= repulsions[i].push[1] / z;
            gradient[i] = [4.0 * g[0], 4.0 * g[1]];
        }
        for &i in &moving {
            for axis in 0..2 {
                let same_way = (gradient[i][axis] > 0.0) == (update[i][axis] > 0.0);
                gains[i][axis] = if same_way {
                    (gains[i][axis] * 0.8).max(0.01)
                } else {
                    gains[i][axis] + 0.2
                };
                update[i][axis] =
                    momentum * update[i][axis] - rate * gains[i][axis] * gradient[i][axis];
                y[i][axis] += update[i][axis];
            }
        }
    }
    Some(y)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three tight clusters of `each` points in 8 dimensions.
    fn clusters(each: usize) -> Vec<Vec<f32>> {
        let mut random = Seeded::new(3);
        (0..3 * each)
            .map(|at| {
                let cluster = at / each;
                (0..8)
                    .map(|c| {
                        let centre = if c == cluster { 10.0 } else { 0.0 };
                        centre + 0.3 * random.normal() as f32
                    })
                    .collect()
            })
            .collect()
    }

    fn mean_distance(
        places: &[(f32, f32)],
        a: std::ops::Range<usize>,
        b: std::ops::Range<usize>,
    ) -> f32 {
        let mut sum = 0.0;
        let mut count = 0.0;
        for i in a {
            for j in b.clone() {
                if i != j {
                    sum += ((places[i].0 - places[j].0).powi(2)
                        + (places[i].1 - places[j].1).powi(2))
                    .sqrt();
                    count += 1.0;
                }
            }
        }
        sum / count
    }

    #[test]
    fn alike_vectors_land_together_and_apart_from_the_rest() {
        let vectors = clusters(12);
        let points: Vec<ToPlace> = vectors
            .iter()
            .map(|vector| ToPlace {
                vector,
                group: "Haiku 5.5",
                old: None,
            })
            .collect();
        let placed = project(&points, None, &|| false).unwrap();
        assert_eq!(placed.places.len(), 36);
        assert!(
            placed
                .places
                .iter()
                .all(|(x, y)| (0.0..=1.0).contains(x) && (0.0..=1.0).contains(y))
        );
        let within = mean_distance(&placed.places, 0..12, 0..12);
        let across = mean_distance(&placed.places, 0..12, 12..24);
        assert!(within * 3.0 < across, "{within} {across}");
        assert_eq!(
            project(&points, None, &|| false).unwrap(),
            placed,
            "the same vectors, the same map"
        );
    }

    #[test]
    fn the_projection_keeps_old_positions() {
        let vectors = clusters(10);
        let first: Vec<ToPlace> = vectors[..20]
            .iter()
            .map(|vector| ToPlace {
                vector,
                group: "GPT 6 Luna",
                old: None,
            })
            .collect();
        let before = project(&first, None, &|| false).unwrap();
        let again: Vec<ToPlace> = vectors
            .iter()
            .enumerate()
            .map(|(at, vector)| ToPlace {
                vector,
                group: "GPT 6 Luna",
                old: before.places.get(at).copied(),
            })
            .collect();
        let after = project(&again, Some(before.frame), &|| false).unwrap();
        assert_eq!(&after.places[..20], &before.places[..], "old dots stay put");
        assert_eq!(after.frame, before.frame);
        // The new cluster finds a place of its own, away from the old two.
        let new = mean_distance(&after.places, 20..30, 20..30);
        let to_old = mean_distance(&after.places, 20..30, 0..20);
        assert!(new < to_old, "{new} {to_old}");
    }

    #[test]
    fn a_smaller_group_is_repeated_up_to_four_times() {
        let vectors: Vec<Vec<f32>> = (0..30).map(|at| vec![at as f32, 0.0]).collect();
        let points: Vec<ToPlace> = vectors
            .iter()
            .enumerate()
            .map(|(at, vector)| ToPlace {
                vector,
                group: if at < 3 {
                    "Gemini 3.8 Flash"
                } else if at < 10 {
                    "Haiku 4.5"
                } else {
                    "Haiku 5.5"
                },
                old: None,
            })
            .collect();
        let rows = oversampled(&points, &mut Seeded::new(SEED));
        let count = |range: std::ops::Range<usize>| {
            rows.iter()
                .filter(|row| range.contains(&row.origin))
                .count()
        };
        assert_eq!(count(0..3), 12, "three actions count as four times three");
        assert_eq!(
            count(3..10),
            20,
            "seven actions are raised to the largest group's twenty"
        );
        assert_eq!(count(10..30), 20);
    }

    #[test]
    fn a_fit_asked_to_stop_places_nothing() {
        let vectors = clusters(6);
        let points: Vec<ToPlace> = vectors
            .iter()
            .map(|vector| ToPlace {
                vector,
                group: "Haiku 5.5",
                old: None,
            })
            .collect();
        assert_eq!(project(&points, None, &|| true), None);
    }

    #[test]
    fn wide_vectors_are_cast_down_and_still_land_by_likeness() {
        let mut random = Seeded::new(5);
        let vectors: Vec<Vec<f32>> = (0..60)
            .map(|at| {
                (0..300)
                    .map(|c| {
                        let centre = if c % 3 == at / 20 { 4.0 } else { 0.0 };
                        centre + 0.2 * random.normal() as f32
                    })
                    .collect()
            })
            .collect();
        let points: Vec<ToPlace> = vectors
            .iter()
            .map(|vector| ToPlace {
                vector,
                group: "Haiku 5.5",
                old: None,
            })
            .collect();
        let placed = project(&points, None, &|| false).unwrap();
        let within = mean_distance(&placed.places, 0..20, 0..20);
        let across = mean_distance(&placed.places, 0..20, 20..40);
        assert!(within * 3.0 < across, "{within} {across}");
    }

    #[test]
    fn a_map_with_nothing_new_is_kept_as_it_was() {
        let vectors = clusters(4);
        let points: Vec<ToPlace> = vectors
            .iter()
            .enumerate()
            .map(|(at, vector)| ToPlace {
                vector,
                group: "Haiku 5.5",
                old: Some((at as f32 / 12.0, 0.5)),
            })
            .collect();
        let frame = Frame {
            x: -3.0,
            y: -3.0,
            width: 6.0,
            height: 6.0,
        };
        let placed = project(&points, Some(frame), &|| true).unwrap();
        assert_eq!(placed.frame, frame);
        assert_eq!(placed.places[5], (5.0 / 12.0, 0.5));
    }

    #[test]
    fn one_action_or_none_needs_no_fit() {
        assert!(project(&[], None, &|| false).unwrap().places.is_empty());
        let vector = vec![1.0];
        let one = [ToPlace {
            vector: &vector,
            group: "Haiku 5.5",
            old: Some((0.2, 0.7)),
        }];
        assert_eq!(
            project(&one, None, &|| false).unwrap().places,
            vec![(0.2, 0.7)]
        );
    }
}
