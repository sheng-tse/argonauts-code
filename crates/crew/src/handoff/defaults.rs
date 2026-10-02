//! The replacements when no handoff list names any: the other installed CLIs that can take
//! work now, best track record on this kind of card first, then the most quota left. What's
//! installed and logged in changes between runs, so this is read when a card stops, never
//! written into the settings.

use std::cmp::Ordering;

use argonauts_core::events::card::CardSpec;
use argonauts_core::limits::TrackRecord;
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::handoff::FallbackEntry;
use argonauts_core::worker::WorkerKind;

use crate::engine::host::Host;
use crate::handoff::{debt, rules};

/// The other installed CLIs for `card`, best first. A CLI whose vendor is down, such as one
/// that isn't logged in, or whose quota is out, is left out, and so is every CLI of the
/// vendor that stopped.
pub fn candidates(host: &Host, card: &CardSpec, stopped: &WorkerRef) -> Vec<FallbackEntry> {
    let services = host.services();
    let task = debt::task_type(&card.kind);
    let mut ranked: Vec<(Option<f64>, Option<f64>, WorkerRef)> = Vec::new();
    for worker in services.workers.available() {
        let other_cli = worker.vendor() != stopped.vendor()
            && services
                .workers
                .get(&worker)
                .is_ok_and(|agent| agent.kind() == WorkerKind::Harness);
        if !other_cli
            || !rules::vendor_usable(host, &worker.vendor())
            || ranked.iter().any(|(_, _, seen)| *seen == worker)
        {
            continue;
        }
        let record = pass_rate(services.track_records.get(&worker, task));
        let quota = services
            .quota
            .get(&worker.vendor())
            .and_then(|reading| reading.used)
            .map(|used| (1.0 - used).clamp(0.0, 1.0));
        ranked.push((record, quota, worker));
    }
    // A stable sort, so ties keep the order the agents are listed in.
    ranked.sort_by(|(a_record, a_quota, _), (b_record, b_quota, _)| {
        descending(*a_record, *b_record).then_with(|| descending(*a_quota, *b_quota))
    });
    ranked
        .into_iter()
        .map(|(_, _, worker)| FallbackEntry::Worker(worker))
        .collect()
}

/// Whether no handoff list for the card's role and the stopped vendor names a candidate, so
/// the defaults stand in.
pub fn apply(host: &Host, card: &CardSpec, stopped: &WorkerRef) -> bool {
    rules::applied_for(host, card, stopped)
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
    fn a_record_with_no_finished_card_says_nothing() {
        assert_eq!(pass_rate(Some(TrackRecord { passed: 0, of: 0 })), None);
        assert_eq!(
            pass_rate(Some(TrackRecord { passed: 3, of: 4 })),
            Some(0.75)
        );
    }
}
