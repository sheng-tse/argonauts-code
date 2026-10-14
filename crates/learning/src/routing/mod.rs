//! The router's arithmetic: one utility per candidate from its track record, Thompson draws
//! over the posteriors, the probability each choice had, and the guardrails on exploring.
//!
//! The engine keeps the rules that remove candidates (the review ring, blindness, plan usage,
//! agent health, caps, cost ceilings) and records which rule removed whom; this module ranks
//! what's left. Predictions are one term of the utility and never change a task's path.

use rand::rngs::StdRng;
use serde::{Deserialize, Serialize};

use argonauts_core::TaskId;
use argonauts_core::names::ModelRef;
use argonauts_core::settings::protocol::{RoutingSettings, RoutingWeights};

use crate::records::Record;
use crate::types::Assignment;

/// `weights.pass_rate * pass + weights.approval_rate * approval + weights.prediction *
/// prediction - weights.cost * cost_ratio - weights.time * time_ratio`, with the two rates
/// either drawn or taken at their means.
pub fn utility(record: &Record, pass: f64, approval: f64, weights: &RoutingWeights) -> f64 {
    weights.pass_rate * pass
        + weights.approval_rate * approval
        + weights.prediction * record.prediction
        - weights.cost * record.cost_ratio
        - weights.time * record.time_ratio
}

/// One candidate's place in a draw.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ranked {
    pub worker: ModelRef,
    /// The utility with this draw's rates.
    pub sampled: f64,
    /// The utility at the posterior means.
    pub mean: f64,
}

/// One draw over the eligible candidates, best first.
/// A tie keeps the order of `eligible`.
pub fn rank(eligible: &[Record], weights: &RoutingWeights, rng: &mut StdRng) -> Vec<Ranked> {
    let mut ranked: Vec<Ranked> = eligible
        .iter()
        .map(|record| Ranked {
            worker: record.worker.clone(),
            sampled: sampled_utility(record, weights, rng),
            mean: utility(record, record.pass.mean(), record.approval.mean(), weights),
        })
        .collect();
    ranked.sort_by(|a, b| b.sampled.total_cmp(&a.sampled));
    ranked
}

fn sampled_utility(record: &Record, weights: &RoutingWeights, rng: &mut StdRng) -> f64 {
    let pass = record.pass.sample(rng);
    let approval = record.approval.sample(rng);
    utility(record, pass, approval, weights)
}

/// The probability that `chosen` would rank first, estimated with `draws` more seeded draws.
/// Logged with the choice, so comparisons can weigh outcomes by how likely each worker was.
pub fn choice_probability(
    eligible: &[Record],
    chosen: &ModelRef,
    weights: &RoutingWeights,
    rng: &mut StdRng,
    draws: u32,
) -> f64 {
    if !eligible.iter().any(|record| &record.worker == chosen) {
        return 0.0;
    }
    if eligible.len() == 1 {
        return 1.0;
    }
    let draws = draws.max(1);
    let mut first = 0_u32;
    for _ in 0..draws {
        let mut best: Option<(f64, &ModelRef)> = None;
        for record in eligible {
            let sampled = sampled_utility(record, weights, rng);
            if best.is_none_or(|(top, _)| sampled > top) {
                best = Some((sampled, &record.worker));
            }
        }
        if best.is_some_and(|(_, worker)| worker == chosen) {
            first += 1;
        }
    }
    f64::from(first) / f64::from(draws)
}

/// What the run has routed so far, for the cap on exploring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunSoFar {
    /// Tasks routed before this one.
    pub routed: u32,
    /// Of those, the ones that went to anyone but the best by posterior mean.
    pub explored: u32,
}

/// Whether this task may go to anyone but the best by posterior mean. Never on a label in
/// `no_explore_labels` or a task that touches protected paths, never for a fallback or retry,
/// and never past `max_explore_share` of the run's tasks.
pub fn may_explore(
    labels: &[String],
    touches_protected: bool,
    reason: Option<Assignment>,
    so_far: RunSoFar,
    settings: &RoutingSettings,
) -> bool {
    let high_stakes = labels.iter().any(|label| {
        settings
            .no_explore_labels
            .iter()
            .any(|stake| stake.eq_ignore_ascii_case(label.trim()))
    });
    let inherited = matches!(reason, Some(Assignment::Fallback | Assignment::Retry));
    if high_stakes || touches_protected || inherited {
        return false;
    }
    // Counting this task, the share stays within the cap at every point of the run, so a short
    // run explores less rather than more.
    let explored = f64::from(so_far.explored) + 1.0;
    let routed = f64::from(so_far.routed) + 1.0;
    explored <= settings.max_explore_share * routed + 1e-9
}

/// A candidate the engine considered, and the rule that removed it if one did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Considered {
    pub worker: ModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampled: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean: Option<f64>,
}

/// One routing decision, logged so any assignment can be explained later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Draw {
    pub task: TaskId,
    pub task_type: String,
    pub considered: Vec<Considered>,
    pub chosen: ModelRef,
    pub reason: Assignment,
    pub probability: f64,
}
