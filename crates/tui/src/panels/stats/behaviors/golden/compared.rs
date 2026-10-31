//! One group of a sample map compared with the rest the way the app works it out
//! (`app::behaviors::map`), for the drawings that show a comparison the app would send: each
//! group's density on the grid, the compared group's less the mean of the others', and the label
//! list by share.

use std::collections::BTreeMap;

use argonauts_core::behaviors::behavior_map::{BehaviorMap, BehaviorRow, Comparison, GRID};

/// Groups with fewer actions have no density of their own.
const SMALLEST: usize = 5;
/// The spread of a label's cloud, the kernel's floor.
const JITTER: f64 = 0.018;
/// How many times a group does a behavior before the list names it.
const LISTED_FROM: usize = 3;

/// `map` with `group` compared with the rest: its overlay and its label list.
pub(super) fn compared(mut map: BehaviorMap, group: usize) -> BehaviorMap {
    let points: Vec<Vec<(f64, f64)>> = (0..map.groups.len())
        .map(|at| {
            map.in_group(at)
                .map(|(_, action)| (f64::from(action.x), f64::from(action.y)))
                .collect()
        })
        .collect();
    let grids: Vec<Option<Vec<f64>>> = points.iter().map(|own| density(own)).collect();
    let Some(mine) = grids[group].clone() else {
        return map;
    };
    let others: Vec<&Vec<f64>> = grids
        .iter()
        .enumerate()
        .filter(|(at, _)| *at != group)
        .filter_map(|(_, grid)| grid.as_ref())
        .collect();
    let difference: Vec<f64> = (0..GRID * GRID)
        .map(|at| mine[at] - others.iter().map(|grid| grid[at]).sum::<f64>() / others.len() as f64)
        .collect();
    let largest = difference.iter().fold(0.0f64, |most, v| most.max(v.abs()));
    let total = map.actions.len();
    let own = map.groups[group].actions;
    map.comparisons
        .resize(map.groups.len(), Comparison::default());
    map.comparisons[group] = Comparison {
        overlay: Some(difference.iter().map(|v| (v / largest) as f32).collect()),
        why_not: None,
        rows: by_share(&map, group, own, total - own),
        mine: own,
        others: total - own,
    };
    map
}

/// A Gaussian density with Scott's factor over `points`, on the grid row by row from `y = 0`.
fn density(points: &[(f64, f64)]) -> Option<Vec<f64>> {
    if points.len() < SMALLEST {
        return None;
    }
    let count = points.len() as f64;
    let mean = |pick: fn(&(f64, f64)) -> f64| points.iter().map(pick).sum::<f64>() / count;
    let (mean_x, mean_y) = (mean(|p| p.0), mean(|p| p.1));
    let spread = |f: &dyn Fn(f64, f64) -> f64| {
        points
            .iter()
            .map(|p| f(p.0 - mean_x, p.1 - mean_y))
            .sum::<f64>()
    };
    let factor = count.powf(-1.0 / 3.0) / (count - 1.0);
    let floor = JITTER * JITTER * count.powf(-1.0 / 3.0);
    let a = (spread(&|x, _| x * x) * factor).max(floor);
    let c = (spread(&|_, y| y * y) * factor).max(floor);
    let limit = 0.999 * (a * c).sqrt();
    let b = (spread(&|x, y| x * y) * factor).clamp(-limit, limit);
    let det = a * c - b * b;
    let scale = 1.0 / (std::f64::consts::TAU * det.sqrt() * count);
    let step = 1.0 / (GRID - 1) as f64;
    let at = |x: f64, y: f64| {
        let sum: f64 = points
            .iter()
            .map(|p| {
                let (dx, dy) = (x - p.0, y - p.1);
                (-0.5 * (c * dx * dx - 2.0 * b * dx * dy + a * dy * dy) / det).exp()
            })
            .sum();
        sum * scale
    };
    Some(
        (0..GRID)
            .flat_map(|row| (0..GRID).map(move |column| (column, row)))
            .map(|(column, row)| at(column as f64 * step, row as f64 * step))
            .collect(),
    )
}

/// Each behavior `group` did often enough, by how much more of its share it is than of the
/// others', the others' count smoothed by one.
fn by_share(map: &BehaviorMap, group: usize, mine: usize, others: usize) -> Vec<BehaviorRow> {
    let mut counts: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for action in &map.actions {
        let count = counts.entry(action.behavior.as_str()).or_default();
        match action.group == group {
            true => count.0 += 1,
            false => count.1 += 1,
        }
    }
    let (all_mine, all_others) = (mine.max(1) as f64, others.max(1) as f64);
    let lift = |(m, o): (usize, usize)| (m as f64 / all_mine) / ((o as f64 + 1.0) / all_others);
    let mut listed: Vec<(&str, (usize, usize))> = counts
        .into_iter()
        .filter(|(_, (m, _))| *m >= LISTED_FROM)
        .collect();
    listed.sort_by(|a, b| {
        lift(b.1)
            .total_cmp(&lift(a.1))
            .then(b.1.0.cmp(&a.1.0))
            .then_with(|| a.0.cmp(b.0))
    });
    listed
        .into_iter()
        .map(|(behavior, (m, o))| BehaviorRow {
            behavior: behavior.to_owned(),
            label: None,
            mine: m,
            others: o,
            lit: map
                .actions
                .iter()
                .enumerate()
                .filter(|(_, action)| action.behavior == behavior)
                .map(|(at, _)| at)
                .collect(),
            new: false,
        })
        .collect()
}
