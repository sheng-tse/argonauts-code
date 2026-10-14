use std::collections::BTreeMap;

use argonauts_core::limits::{PlanUsageReading, PlanUsageStatus};

/// Keeps the newest reading per agent and window.
pub(super) fn record(
    plan_windows: &mut BTreeMap<String, Vec<PlanUsageReading>>,
    reading: &PlanUsageReading,
) {
    let readings = plan_windows.entry(reading.agent_id.0.clone()).or_default();
    match readings
        .iter_mut()
        .find(|known| known.window == reading.window)
    {
        Some(known) if known.observed_at <= reading.observed_at => *known = reading.clone(),
        Some(_) => {}
        None => readings.push(reading.clone()),
    }
}

/// The window that binds soonest: one that refused work, else the fullest, else the newest.
/// Only each window's newest reading counts, since an older one may predate a reset.
pub fn binding_reading(readings: &[PlanUsageReading]) -> Option<&PlanUsageReading> {
    let newest = |reading: &&PlanUsageReading| {
        !readings
            .iter()
            .any(|other| other.window == reading.window && other.observed_at > reading.observed_at)
    };
    readings.iter().filter(newest).max_by(|a, b| {
        let refused = |r: &PlanUsageReading| r.status == PlanUsageStatus::Rejected;
        let used = |r: &PlanUsageReading| r.used.unwrap_or(-1.0);
        refused(a)
            .cmp(&refused(b))
            .then(used(a).total_cmp(&used(b)))
            .then(a.observed_at.cmp(&b.observed_at))
    })
}
