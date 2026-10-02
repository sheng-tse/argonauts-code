//! Hand-written outcome lines for the records tests.

// Each test file uses a different part of this.
#![allow(dead_code)]

use time::macros::datetime;
use time::{Duration, OffsetDateTime};

use argonauts_core::names::WorkerRef;
use argonauts_core::{CardId, RunId};
use argonauts_learning::outcomes::{LogLine, Outcome, Prediction, Predictor};
use argonauts_learning::types::Assignment;

/// Every book in these tests is built as of this moment.
pub const AS_OF: OffsetDateTime = datetime!(2026-10-01 0:00 UTC);

pub fn worker(name: &str) -> WorkerRef {
    name.parse().unwrap()
}

pub fn days_ago(days: f64) -> OffsetDateTime {
    AS_OF - Duration::seconds_f64(days * 86_400.0)
}

/// Card `n` of run-01, ended `days` before `AS_OF`, chosen as the best by posterior mean.
pub fn outcome(n: u32, owner: &str, task_type: &str, first_try: bool, days: f64) -> Outcome {
    Outcome {
        run: RunId::new("run-01").unwrap(),
        card: CardId::numbered(n),
        worker: worker(owner),
        task_type: task_type.to_owned(),
        first_try,
        attempts: if first_try { 1 } else { 2 },
        integrated: true,
        spent_usd: 1.0,
        step: None,
        at: Some(days_ago(days)),
        model_id: None,
        assigned: Some(Assignment::Exploit),
        assign_p: None,
        reviewer: None,
        approved_first_review: None,
        within_time_cap: None,
        minutes: None,
        difficulty: None,
        recipe: None,
    }
}

/// `passes` first-try passes, then `fails` failures, numbered from `from`.
pub fn cards(
    from: u32,
    owner: &str,
    task_type: &str,
    passes: u32,
    fails: u32,
    days: f64,
) -> Vec<Outcome> {
    (0..passes + fails)
        .map(|i| outcome(from + i, owner, task_type, i < passes, days))
        .collect()
}

pub fn lines(outcomes: impl IntoIterator<Item = Outcome>) -> Vec<LogLine> {
    outcomes.into_iter().map(LogLine::Outcome).collect()
}

/// An owner's prediction for card `n` of run-01, made the day before `AS_OF`.
pub fn prediction(n: u32, owner: &str, task_type: &str, p_first_try: f64) -> Prediction {
    Prediction {
        run: RunId::new("run-01").unwrap(),
        card: CardId::numbered(n),
        worker: worker(owner),
        role: Predictor::Owner,
        task_type: task_type.to_owned(),
        at: days_ago(1.0),
        p_pass_gates_first_try: Some(p_first_try),
        p_within_time_cap: None,
        p_at_most_two_attempts: None,
        p_passes_integration: None,
        expected_minutes: None,
    }
}

pub fn close(a: f64, b: f64, within: f64) -> bool {
    (a - b).abs() <= within
}
