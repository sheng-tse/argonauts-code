//! The evidence a candidate is judged on: the paired comparison with its parent, the holdout,
//! and the canary's tripwires. Every test is one-sided and set before the data comes in.

use std::collections::BTreeMap;

use rand::RngExt;
use rand::rngs::StdRng;

use argonauts_core::events::run::RunOutcome;
use argonauts_core::settings::Settings;

use super::{Thresholds, Tripwire, Verdict};
use crate::types::{RecipeRef, ReplayResult, RunRecord};

/// Bootstrap resamples. Two thousand keep the tenth percentile steady to a fraction of a point.
pub const BOOTSTRAP_RESAMPLES: usize = 2000;
/// A one-sided 90% interval's lower end is the tenth percentile.
const LOWER_QUANTILE: f64 = 0.10;
/// A "not worse" class may lose this many points at the lower end, and no more.
pub const NOT_WORSE_FLOOR: f64 = -5.0;
/// Floating sums of thirds and sixths miss whole numbers by a hair.
const EPSILON: f64 = 1e-9;

/// One task replayed under both versions.
struct Pair {
    /// Child pass rate minus parent pass rate, in points.
    gain: f64,
    parent_cost: f64,
    child_cost: f64,
    parent_rate: f64,
    child_rate: f64,
}

fn pairs(
    results: &[ReplayResult],
    parent: &RecipeRef,
    child: &RecipeRef,
    holdout: bool,
) -> Vec<Pair> {
    let mut by_task: BTreeMap<&str, (Vec<&ReplayResult>, Vec<&ReplayResult>)> = BTreeMap::new();
    for result in results.iter().filter(|result| result.holdout == holdout) {
        let entry = by_task.entry(result.task.as_str()).or_default();
        if result.recipe == *parent {
            entry.0.push(result);
        } else if result.recipe == *child {
            entry.1.push(result);
        }
    }
    let rate = |runs: &[&ReplayResult]| {
        runs.iter().filter(|run| run.passed).count() as f64 / runs.len() as f64
    };
    let cost = |runs: &[&ReplayResult]| {
        runs.iter().map(|run| run.cost_usd).sum::<f64>() / runs.len() as f64
    };
    by_task
        .into_values()
        .filter(|(old, new)| !old.is_empty() && !new.is_empty())
        .map(|(old, new)| Pair {
            gain: (rate(&new) - rate(&old)) * 100.0,
            parent_cost: cost(&old),
            child_cost: cost(&new),
            parent_rate: rate(&old),
            child_rate: rate(&new),
        })
        .collect()
}

/// Compares the candidate with its parent on the tuning set.
pub fn compare(
    results: &[ReplayResult],
    parent: &RecipeRef,
    child: &RecipeRef,
    thresholds: &Thresholds,
    rng: &mut StdRng,
) -> Verdict {
    let pairs = pairs(results, parent, child, false);
    if pairs.is_empty() {
        return Verdict {
            gain_points: 0.0,
            lower_bound: 0.0,
            cost_change: 0.0,
            passed: false,
            reasons: vec![format!(
                "no task was replayed under both {parent} and {child}"
            )],
        };
    }
    let n = pairs.len();
    let gains: Vec<f64> = pairs.iter().map(|pair| pair.gain).collect();
    let gain_points = gains.iter().sum::<f64>() / n as f64;

    // Resampling whole tasks keeps each task's two versions together, which is what makes the
    // comparison paired: a hard task drags both down and cancels out.
    let mut means: Vec<f64> = (0..BOOTSTRAP_RESAMPLES)
        .map(|_| (0..n).map(|_| gains[rng.random_range(0..n)]).sum::<f64>() / n as f64)
        .collect();
    means.sort_by(f64::total_cmp);
    let lower_bound = means[(BOOTSTRAP_RESAMPLES as f64 * LOWER_QUANTILE) as usize];

    let parent_cost: f64 = pairs.iter().map(|pair| pair.parent_cost).sum();
    let child_cost: f64 = pairs.iter().map(|pair| pair.child_cost).sum();
    let cost_change = if parent_cost > 0.0 {
        child_cost / parent_cost - 1.0
    } else if child_cost > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };

    let mut reasons = Vec::new();
    if thresholds.not_worse {
        if lower_bound <= NOT_WORSE_FLOOR + EPSILON {
            reasons.push(format!(
                "it may be worse: the lower end of its gain is {lower_bound:.1} points, not above \
                 {NOT_WORSE_FLOOR}"
            ));
        }
    } else {
        if lower_bound <= EPSILON {
            reasons.push(format!(
                "its gain isn't clear: the lower end of the 90% interval is {lower_bound:.1} \
                 points, not above 0"
            ));
        }
        if gain_points + EPSILON < thresholds.min_gain_points {
            reasons.push(format!(
                "it gains {gain_points:.1} points, short of the {} set before its replay",
                thresholds.min_gain_points
            ));
        }
    }
    if cost_change > thresholds.max_cost_increase + EPSILON {
        reasons.push(format!(
            "its cost rises {:.0}%, past the {:.0}% allowed",
            cost_change * 100.0,
            thresholds.max_cost_increase * 100.0
        ));
    }
    Verdict {
        gain_points,
        lower_bound,
        cost_change,
        passed: reasons.is_empty(),
        reasons,
    }
}

/// No regression on the holdout, which reports only pass or fail. A candidate with no holdout
/// result was never checked for overfitting, so it isn't ok.
pub fn holdout_ok(results: &[ReplayResult], parent: &RecipeRef, child: &RecipeRef) -> bool {
    if holdout_tasks(results, parent, child) == 0 {
        return false;
    }
    let (old, new) = holdout_passes(results, parent, child);
    new + EPSILON >= old
}

/// Holdout tasks both versions ran.
pub fn holdout_tasks(results: &[ReplayResult], parent: &RecipeRef, child: &RecipeRef) -> usize {
    pairs(results, parent, child, true).len()
}

/// Pass rates summed over the holdout tasks both versions ran, parent's first.
pub fn holdout_passes(
    results: &[ReplayResult],
    parent: &RecipeRef,
    child: &RecipeRef,
) -> (f64, f64) {
    let pairs = pairs(results, parent, child, true);
    (
        pairs.iter().map(|pair| pair.parent_rate).sum(),
        pairs.iter().map(|pair| pair.child_rate).sum(),
    )
}

/// The tripwires a canary run sets off, against the parent's last runs.
pub fn tripwires(
    canary: &[RunRecord],
    baseline: &[RunRecord],
    reversals: u32,
    settings: &Settings,
) -> Vec<Tripwire> {
    let p = settings.evolution.tripwire_p;
    let mut trips = Vec::new();
    if canary
        .iter()
        .any(|run| run.outcome == RunOutcome::Abandoned)
    {
        trips.push(Tripwire::Rejected);
    }
    if reversals > 0 {
        trips.push(Tripwire::Reverted);
    }
    // With none of the parent's runs to compare with, only you can trip a canary.
    if canary.is_empty() || baseline.is_empty() {
        return trips;
    }
    let n = canary.len() as f64;
    let m = baseline.len() as f64;

    // Half a pseudo-count keeps a clean baseline from making one escalation impossible, which
    // would trip on any escalation at all.
    let base_escalations: u64 = baseline.iter().map(|run| u64::from(run.escalations)).sum();
    let rate = (base_escalations as f64 + 0.5) / (m + 1.0);
    let escalations: u64 = canary.iter().map(|run| u64::from(run.escalations)).sum();
    if escalations > 0 && poisson_at_least(escalations, rate * n) < p {
        trips.push(Tripwire::Escalations);
    }

    let base_failures = baseline
        .iter()
        .filter(|run| !run.final_checks_passed)
        .count();
    let failure_rate = (base_failures as f64 + 0.5) / (m + 1.0);
    let failures = canary.iter().filter(|run| !run.final_checks_passed).count();
    if failures > 0 && binomial_at_least(failures, canary.len(), failure_rate) < p {
        trips.push(Tripwire::FailedFinalChecks);
    }

    if baseline.len() >= 2 {
        let mean = baseline.iter().map(|run| run.cost_usd).sum::<f64>() / m;
        let variance = baseline
            .iter()
            .map(|run| (run.cost_usd - mean).powi(2))
            .sum::<f64>()
            / (m - 1.0);
        // A baseline of identical costs would make any extra cent significant, so the spread
        // is taken as at least a twentieth of the mean.
        let spread = variance.sqrt().max(mean.abs() * 0.05).max(EPSILON);
        let canary_mean = canary.iter().map(|run| run.cost_usd).sum::<f64>() / n;
        let z = (canary_mean - mean) / (spread / n.sqrt());
        if z > 0.0 && normal_upper_tail(z) < p {
            trips.push(Tripwire::Cost);
        }
    }
    trips
}

/// P(X >= k) for a Poisson count with mean `lambda`.
fn poisson_at_least(k: u64, lambda: f64) -> f64 {
    let mut term = (-lambda).exp();
    let mut below = 0.0;
    for i in 0..k {
        below += term;
        term *= lambda / (i + 1) as f64;
    }
    (1.0 - below).max(0.0)
}

/// P(X >= k) for X ~ Binomial(n, q).
fn binomial_at_least(k: usize, n: usize, q: f64) -> f64 {
    let mut term = (1.0 - q).powi(n as i32);
    let mut below = 0.0;
    for i in 0..k.min(n + 1) {
        below += term;
        term *= (n - i) as f64 / (i + 1) as f64 * q / (1.0 - q);
    }
    (1.0 - below).max(0.0)
}

/// P(Z > z) for a standard normal.
fn normal_upper_tail(z: f64) -> f64 {
    0.5 * erfc(z / std::f64::consts::SQRT_2)
}

/// The complementary error function, to about seven digits (Numerical Recipes' `erfcc`).
fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let poly = -1.265_512_23
        + t * (1.000_023_68
            + t * (0.374_091_96
                + t * (0.096_784_18
                    + t * (-0.186_288_06
                        + t * (0.278_868_07
                            + t * (-1.135_203_98
                                + t * (1.488_515_87 + t * (-0.822_152_23 + t * 0.170_872_77))))))));
    let r = t * (-z * z + poly).exp();
    if x >= 0.0 { r } else { 2.0 - r }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tails() {
        assert!((normal_upper_tail(1.644_853_6) - 0.05).abs() < 1e-5);
        assert!((normal_upper_tail(0.0) - 0.5).abs() < 1e-7);
        assert!((poisson_at_least(1, 0.05) - (1.0 - (-0.05f64).exp())).abs() < 1e-12);
        assert!((binomial_at_least(2, 2, 0.5) - 0.25).abs() < 1e-12);
        assert!((binomial_at_least(0, 3, 0.2) - 1.0).abs() < 1e-12);
    }
}
