//! The fallbacks when no fallback list names any: the installed CLIs' other models that can
//! take work now, best track record on this kind of task first, then the most plan usage left.
//! What's installed and logged in changes between runs, so this is read when a task stops, never
//! written into the settings.

use std::cmp::Ordering;

use argonauts_core::events::task::TaskSpec;
use argonauts_core::limits::TrackRecord;
use argonauts_core::names::ModelRef;
use argonauts_core::session_runner::RunnerKind;
use argonauts_core::settings::fallback::FallbackEntry;

use crate::engine::host::Host;
use crate::fallback::{debt, rules};

/// Every other model an installed CLI offers for `task`, best first. A CLI that is down, such
/// as one that isn't logged in, or whose plan usage is out, is left out, and so is a model that
/// is out itself. The stopped CLI's other models stay in: the recovery rules drop them when
/// the CLI stopped as a whole, and keep them when only its model did.
pub fn candidates(host: &Host, task: &TaskSpec, stopped: &ModelRef) -> Vec<FallbackEntry> {
    let services = host.services();
    let task_type = debt::task_type(&task.kind);
    let mut ranked: Vec<(Option<f64>, Option<f64>, ModelRef)> = Vec::new();
    for worker in services.workers.available() {
        let cli = services
            .workers
            .get(&worker)
            .is_ok_and(|agent| agent.kind() == RunnerKind::Cli);
        if worker == *stopped
            || !cli
            || !rules::takes_work(host, &worker)
            || ranked.iter().any(|(_, _, seen)| *seen == worker)
        {
            continue;
        }
        let record = pass_rate(services.track_records.get(&worker, task_type));
        let plan_usage = services
            .plan_usage
            .get(&worker.agent_id())
            .and_then(|reading| reading.used)
            .map(|used| (1.0 - used).clamp(0.0, 1.0));
        ranked.push((record, plan_usage, worker));
    }
    // A stable sort, so ties keep the order the agents are listed in.
    ranked.sort_by(|(a_record, a_plan_usage, _), (b_record, b_plan_usage, _)| {
        descending(*a_record, *b_record).then_with(|| descending(*a_plan_usage, *b_plan_usage))
    });
    ranked
        .into_iter()
        .map(|(_, _, worker)| FallbackEntry::Model(worker))
        .collect()
}

/// Whether no fallback list for the task's role and the stopped agent names a candidate, so
/// the defaults apply.
pub fn apply(host: &Host, task: &TaskSpec, stopped: &ModelRef) -> bool {
    rules::applied_for(host, task, stopped)
        .candidates
        .is_empty()
}

fn pass_rate(record: Option<TrackRecord>) -> Option<f64> {
    record
        .filter(|record| record.of > 0)
        .map(|record| f64::from(record.passed) / f64::from(record.of))
}

/// Higher first, and an unknown after every known value.
fn descending(a: Option<f64>, b: Option<f64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.total_cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values_come_first_and_highest_first() {
        let mut values = vec![None, Some(0.2), Some(0.9), None, Some(0.5)];
        values.sort_by(|a, b| descending(*a, *b));
        assert_eq!(values, [Some(0.9), Some(0.5), Some(0.2), None, None]);
    }

    #[test]
    fn a_record_with_no_finished_task_says_nothing() {
        assert_eq!(pass_rate(Some(TrackRecord { passed: 0, of: 0 })), None);
        assert_eq!(
            pass_rate(Some(TrackRecord { passed: 3, of: 4 })),
            Some(0.75)
        );
    }
}
