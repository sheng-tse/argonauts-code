//! The quota and health boards, told on a task of their own. A board may lock and sync a file
//! every supervisor shares, which must hold up neither the session nor the async runtime. What
//! queues up while a board is busy goes to it in one batch, only the newest quota reading of each
//! vendor included, and the vendor's health goes out when the batch changed it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::events::VendorEvent;
use argonauts_core::limits::{HealthSignal, QuotaReading, VendorHealth};
use tokio::sync::{mpsc, oneshot};

use super::context::Context;
use super::outbox::Outbox;

/// How long the end waits for the boards to take what's queued.
const SETTLE_WAIT: Duration = Duration::from_secs(10);

pub struct Boards {
    jobs: mpsc::UnboundedSender<Job>,
}

enum Job {
    Quota(QuotaReading),
    Health(HealthSignal),
    Settled(oneshot::Sender<()>),
}

impl Boards {
    pub fn start(ctx: Arc<Context>, out: Outbox) -> Self {
        let (jobs, queue) = mpsc::unbounded_channel();
        tokio::spawn(run(ctx, out, queue));
        Self { jobs }
    }

    pub fn quota(&self, reading: QuotaReading) {
        // Fails only once the task is gone, and it lives as long as this sender.
        let _ = self.jobs.send(Job::Quota(reading));
    }

    pub fn health(&self, signal: HealthSignal) {
        let _ = self.jobs.send(Job::Health(signal));
    }

    /// Waits until the boards have taken everything sent so far. Says whether they did in time.
    pub async fn settle(&self) -> bool {
        let (done, settled) = oneshot::channel();
        if self.jobs.send(Job::Settled(done)).is_err() {
            return false;
        }
        matches!(tokio::time::timeout(SETTLE_WAIT, settled).await, Ok(Ok(())))
    }
}

async fn run(ctx: Arc<Context>, out: Outbox, mut queue: mpsc::UnboundedReceiver<Job>) {
    // The vendor's health before this session's first signal, read then.
    let mut known: Option<VendorHealth> = None;
    while let Some(first) = queue.recv().await {
        let mut batch = vec![first];
        while let Ok(job) = queue.try_recv() {
            batch.push(job);
        }
        let mut settled = Vec::new();
        let mut quota: BTreeMap<String, QuotaReading> = BTreeMap::new();
        let mut signals = Vec::new();
        for job in batch {
            match job {
                Job::Quota(reading) => keep_newest(&mut quota, reading),
                Job::Health(signal) => signals.push(signal),
                Job::Settled(done) => settled.push(done),
            }
        }
        if !quota.is_empty() || !signals.is_empty() {
            let task_ctx = ctx.clone();
            let before = known.take();
            let told = tokio::task::spawn_blocking(move || {
                tell(&task_ctx, quota.into_values(), signals, before)
            })
            .await;
            match told {
                Ok((health, events)) => {
                    for event in events {
                        out.vendor(event);
                    }
                    known = health;
                }
                Err(err) => out.notice(format!("could not update the vendor boards: {err}")),
            }
        }
        for done in settled {
            let _ = done.send(());
        }
    }
}

/// A later reading replaces an earlier one, as the quota board does itself.
fn keep_newest(quota: &mut BTreeMap<String, QuotaReading>, reading: QuotaReading) {
    let newer = quota
        .get(&reading.vendor.0)
        .is_none_or(|old| old.observed_at <= reading.observed_at);
    if newer {
        quota.insert(reading.vendor.0.clone(), reading);
    }
}

/// Blocks while the boards lock and write their file. Returns the vendor's health after the
/// signals, when there were any, and what changed.
fn tell(
    ctx: &Context,
    quota: impl Iterator<Item = QuotaReading>,
    signals: Vec<HealthSignal>,
    before: Option<VendorHealth>,
) -> (Option<VendorHealth>, Vec<VendorEvent>) {
    for reading in quota {
        ctx.quota.observe(reading);
    }
    if signals.is_empty() {
        return (before, Vec::new());
    }
    let before = before.unwrap_or_else(|| ctx.health.state(&ctx.vendor));
    for signal in signals {
        ctx.health.observe(signal);
    }
    let health = ctx.health.state(&ctx.vendor);
    let changed = health.state != before.state || health.reason != before.reason;
    let newly_disabled = health.disabled.is_some() && health.disabled != before.disabled;
    let mut events = Vec::new();
    if changed || newly_disabled {
        events.push(VendorEvent::Health(health.clone()));
    }
    if newly_disabled && let Some(reason) = health.disabled.clone() {
        events.push(VendorEvent::Disabled {
            vendor: ctx.vendor.clone(),
            reason,
        });
    }
    (Some(health), events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::limits::{QuotaSource, QuotaStatus};
    use argonauts_core::names::VendorId;
    use time::OffsetDateTime;

    fn reading(vendor: &str, at: i64) -> QuotaReading {
        QuotaReading {
            vendor: VendorId(vendor.into()),
            status: QuotaStatus::Allowed,
            used: None,
            resets_at: None,
            window: None,
            source: QuotaSource::RateLimitEvent,
            observed_at: OffsetDateTime::from_unix_timestamp(at).unwrap(),
            detail: None,
            wrap_up: false,
        }
    }

    #[test]
    fn only_the_newest_reading_of_each_vendor_is_kept() {
        let mut quota = BTreeMap::new();
        for (vendor, at) in [("a", 2), ("a", 1), ("b", 5), ("a", 3)] {
            keep_newest(&mut quota, reading(vendor, at));
        }
        let kept: Vec<i64> = quota
            .values()
            .map(|reading| reading.observed_at.unix_timestamp())
            .collect();
        assert_eq!(kept, [3, 5]);
    }
}
