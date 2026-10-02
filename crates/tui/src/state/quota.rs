use std::collections::BTreeMap;

use argonauts_core::limits::{QuotaReading, QuotaStatus};

/// Keeps the newest reading per vendor and window.
pub(super) fn record(quotas: &mut BTreeMap<String, Vec<QuotaReading>>, reading: &QuotaReading) {
    let readings = quotas.entry(reading.vendor.0.clone()).or_default();
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
pub fn binding_reading(readings: &[QuotaReading]) -> Option<&QuotaReading> {
    let newest = |reading: &&QuotaReading| {
        !readings
            .iter()
            .any(|other| other.window == reading.window && other.observed_at > reading.observed_at)
    };
    readings.iter().filter(newest).max_by(|a, b| {
        let refused = |r: &QuotaReading| r.status == QuotaStatus::Rejected;
        let used = |r: &QuotaReading| r.used.unwrap_or(-1.0);
        refused(a)
            .cmp(&refused(b))
            .then(used(a).total_cmp(&used(b)))
            .then(a.observed_at.cmp(&b.observed_at))
    })
}
