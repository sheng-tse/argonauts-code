//! Predictions are probabilities written before a task starts, scored after it ends. Routing
//! uses the Brier skill score against the worker's own base rate, since a worker that always
//! predicts its average is calibrated and tells you nothing. The run review and `/records` show
//! reliability separately, so calibration and telling easy tasks from hard ones stay apart.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use argonauts_core::names::ModelRef;
use argonauts_core::settings::protocol::RoutingSettings;
use argonauts_core::{RunId, TaskId};

use crate::outcomes::{LogLine, Outcome, Prediction, Predictor};

pub const PASS_CHECKS_FIRST_TRY: &str = "p_pass_checks_first_try";
pub const WITHIN_TIME_CAP: &str = "p_within_time_cap";
pub const AT_MOST_TWO_TRIES: &str = "p_at_most_two_tries";
pub const PASSES_MERGE: &str = "p_passes_merge";
/// What a worker predicts about its own task; their skill is its routing score.
pub const WORKER_METRICS: [&str; 3] = [PASS_CHECKS_FIRST_TRY, WITHIN_TIME_CAP, AT_MOST_TWO_TRIES];

/// `(p - outcome)^2`, where the outcome is 1 if it happened.
pub fn brier(p: f64, happened: bool) -> f64 {
    let outcome = if happened { 1.0 } else { 0.0 };
    (p - outcome).powi(2)
}

/// `1 - Brier / Brier_reference`, where the reference always predicts `base_rate`. 0 is no
/// better than guessing the base rate, 1 is perfect, and below 0 is worse.
pub fn skill(pairs: &[(f64, bool)], base_rate: f64) -> f64 {
    let scored: f64 = pairs.iter().map(|&(p, happened)| brier(p, happened)).sum();
    let reference: f64 = pairs
        .iter()
        .map(|&(_, happened)| brier(base_rate, happened))
        .sum();
    skill_score(scored, reference)
}

/// `1 - scored / reference` over summed Brier scores.
fn skill_score(scored: f64, reference: f64) -> f64 {
    // Outcomes that never varied, all predicted by the base rate, leave nothing to tell apart,
    // so they show no skill either way.
    if reference <= f64::EPSILON {
        return 0.0;
    }
    1.0 - scored / reference
}

/// Predicted against actual, in one bin of width 0.1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bin {
    pub lower: f64,
    pub upper: f64,
    pub count: u32,
    pub mean_predicted: f64,
    pub actual_rate: f64,
}

/// The reliability table: bins of 0.1, empty bins left out.
pub fn reliability(pairs: &[(f64, bool)]) -> Vec<Bin> {
    let mut bins = [(0_u32, 0.0, 0.0); 10];
    for &(p, happened) in pairs {
        let p = p.clamp(0.0, 1.0);
        // The nudge keeps 0.3, stored a hair under, in the bin it names; 1.0 joins the last.
        let index = ((p * 10.0 + 1e-9).floor() as usize).min(9);
        let bin = &mut bins[index];
        bin.0 += 1;
        bin.1 += p;
        bin.2 += if happened { 1.0 } else { 0.0 };
    }
    bins.iter()
        .enumerate()
        .filter(|(_, (count, _, _))| *count > 0)
        .map(|(index, &(count, predicted, happened))| Bin {
            lower: index as f64 / 10.0,
            upper: (index + 1) as f64 / 10.0,
            count,
            mean_predicted: predicted / f64::from(count),
            actual_rate: happened / f64::from(count),
        })
        .collect()
}

/// Murphy's decomposition: Brier = reliability - resolution + uncertainty.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Decomposition {
    /// Calibration error; lower is better.
    pub reliability: f64,
    /// How well predictions tell easy tasks from hard ones; higher is better.
    pub resolution: f64,
    /// How hard the outcomes were to predict at all.
    pub uncertainty: f64,
}

///
/// Grouped by each distinct prediction rather than by bin, so the three terms add up to the
/// Brier score exactly.
pub fn decompose(pairs: &[(f64, bool)]) -> Decomposition {
    if pairs.is_empty() {
        return Decomposition::default();
    }
    let total = pairs.len() as f64;
    let base = pairs.iter().filter(|(_, happened)| *happened).count() as f64 / total;
    let mut sorted = pairs.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut reliability = 0.0;
    let mut resolution = 0.0;
    for group in sorted.chunk_by(|a, b| a.0.total_cmp(&b.0).is_eq()) {
        let count = group.len() as f64;
        let rate = group.iter().filter(|(_, happened)| *happened).count() as f64 / count;
        reliability += count * (group[0].0 - rate).powi(2);
        resolution += count * (rate - base).powi(2);
    }
    Decomposition {
        reliability: reliability / total,
        resolution: resolution / total,
        uncertainty: base * (1.0 - base),
    }
}

/// One worker's scores on one predicted quantity, such as `p_pass_checks_first_try`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    pub worker: ModelRef,
    pub metric: String,
    pub count: u32,
    pub brier: f64,
    pub skill: f64,
    pub mean_predicted: f64,
    pub actual_rate: f64,
    /// The average prediction runs more than `routing.overconfident_margin` above the actual
    /// rate, over at least `routing.overconfident_after` predictions.
    pub overconfident: bool,
    pub decomposition: Decomposition,
    pub bins: Vec<Bin>,
}

/// Pairs every prediction with its task's outcome and scores each worker and metric. A
/// reviewer's `p_passes_merge` scores its review accuracy.
///
/// A worker's prediction pairs with its own outcome for the task, so a fallback's result never
/// scores the worker it replaced. A worker's base rate is its own, per task type.
pub fn calibration(lines: &[LogLine], settings: &RoutingSettings) -> Vec<Calibration> {
    let mut outcomes: BTreeMap<(&RunId, &TaskId), Vec<&Outcome>> = BTreeMap::new();
    // A prediction stated again for the same task replaces the first.
    let mut stated: BTreeMap<(&RunId, &TaskId, &ModelRef, bool), &Prediction> = BTreeMap::new();
    for line in lines {
        match line {
            LogLine::Outcome(outcome) => outcomes
                .entry((&outcome.run, &outcome.task))
                .or_default()
                .push(outcome),
            LogLine::Prediction(prediction) => {
                let reviewer = prediction.role == Predictor::Reviewer;
                stated.insert(
                    (
                        &prediction.run,
                        &prediction.task,
                        &prediction.worker,
                        reviewer,
                    ),
                    prediction,
                );
            }
            _ => {}
        }
    }

    let mut scored: BTreeMap<(&ModelRef, &'static str), Vec<Scored<'_>>> = BTreeMap::new();
    for prediction in stated.values() {
        let Some(task) = outcomes.get(&(&prediction.run, &prediction.task)) else {
            continue;
        };
        let mut pairs = Vec::new();
        if let Some(own) = task
            .iter()
            .rev()
            .find(|outcome| outcome.worker == prediction.worker)
        {
            pairs.extend([
                (
                    PASS_CHECKS_FIRST_TRY,
                    prediction.p_pass_checks_first_try,
                    *own,
                    Some(own.first_try),
                ),
                (
                    WITHIN_TIME_CAP,
                    prediction.p_within_time_cap,
                    *own,
                    own.within_time_cap,
                ),
                (
                    AT_MOST_TWO_TRIES,
                    prediction.p_at_most_two_tries,
                    *own,
                    Some(own.tries <= 2),
                ),
            ]);
        }
        if let Some(last) = task.last() {
            pairs.push((
                PASSES_MERGE,
                prediction.p_passes_merge,
                *last,
                Some(last.merged),
            ));
        }
        for (metric, p, outcome, happened) in pairs {
            if let (Some(p), Some(happened)) = (p, happened) {
                scored
                    .entry((&prediction.worker, metric))
                    .or_default()
                    .push(Scored {
                        p,
                        happened,
                        task_type: &outcome.task_type,
                    });
            }
        }
    }

    scored
        .into_iter()
        .map(|((worker, metric), scored)| score_one(worker, metric, &scored, settings))
        .collect()
}

/// One prediction against what happened.
struct Scored<'a> {
    p: f64,
    happened: bool,
    task_type: &'a str,
}

fn score_one(
    worker: &ModelRef,
    metric: &str,
    scored: &[Scored<'_>],
    settings: &RoutingSettings,
) -> Calibration {
    let pairs: Vec<(f64, bool)> = scored.iter().map(|s| (s.p, s.happened)).collect();
    let total = pairs.len() as f64;
    let mut base: BTreeMap<&str, (f64, f64)> = BTreeMap::new();
    for s in scored {
        let rate = base.entry(s.task_type).or_default();
        rate.0 += if s.happened { 1.0 } else { 0.0 };
        rate.1 += 1.0;
    }
    let brier_sum: f64 = pairs.iter().map(|&(p, happened)| brier(p, happened)).sum();
    let reference: f64 = scored
        .iter()
        .map(|s| {
            let (happened, of) = base[s.task_type];
            brier(happened / of, s.happened)
        })
        .sum();
    let mean_predicted = pairs.iter().map(|(p, _)| p).sum::<f64>() / total;
    let actual_rate = pairs.iter().filter(|(_, happened)| *happened).count() as f64 / total;
    let count = u32::try_from(pairs.len()).unwrap_or(u32::MAX);
    Calibration {
        worker: worker.clone(),
        metric: metric.to_owned(),
        count,
        brier: brier_sum / total,
        skill: skill_score(brier_sum, reference),
        mean_predicted,
        actual_rate,
        overconfident: count >= settings.overconfident_after
            && mean_predicted - actual_rate > settings.overconfident_margin,
        decomposition: decompose(&pairs),
        bins: reliability(&pairs),
    }
}

/// A point estimate's error, `|ln(actual / predicted)|`. Kept out of routing.
pub fn point_error(actual: f64, predicted: f64) -> f64 {
    (actual / predicted).ln().abs()
}
