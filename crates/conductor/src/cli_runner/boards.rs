//! The plan usage and health boards, told on a task of their own. A board may lock and sync a file
//! every run host shares, which must hold up neither the session nor the async runtime. What
//! queues up while a board is busy goes to it in one batch, only the newest plan usage reading of
//! each agent's window included, and the agent's health goes out when the batch changed it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::events::AgentStatusEvent;
use argonauts_core::limits::{AgentHealth, HealthSignal, PlanUsageReading};
use tokio::sync::{mpsc, oneshot};

use super::context::Context;
use super::outbox::Outbox;

/// How long the end waits for the boards to take what's queued.
const SETTLE_WAIT: Duration = Duration::from_secs(10);

pub struct Boards {
    jobs: mpsc::UnboundedSender<Job>,
}

enum Job {
    PlanUsage(PlanUsageReading),
    Plan(String),
    Health(HealthSignal),
    Settled(oneshot::Sender<()>),
}

impl Boards {
    pub fn start(ctx: Arc<Context>, out: Outbox) -> Self {
        let (jobs, queue) = mpsc::unbounded_channel();
        tokio::spawn(run(ctx, out, queue));
        Self { jobs }
    }

    pub fn plan_usage(&self, reading: PlanUsageReading) {
        // Fails only once the task is gone, and it lives as long as this sender.
        let _ = self.jobs.send(Job::PlanUsage(reading));
    }

    /// The account's plan, as the CLI named it.
    pub fn plan(&self, plan: String) {
        let _ = self.jobs.send(Job::Plan(plan));
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
    // The agent's health before this session's first signal, read then.
    let mut known: Option<AgentHealth> = None;
    while let Some(first) = queue.recv().await {
        let mut batch = vec![first];
        while let Ok(job) = queue.try_recv() {
            batch.push(job);
        }
        let mut settled = Vec::new();
        let mut plan_usage: BTreeMap<WindowKey, PlanUsageReading> = BTreeMap::new();
        let mut plan = None;
        let mut signals = Vec::new();
        for job in batch {
            match job {
                Job::PlanUsage(reading) => keep_newest(&mut plan_usage, reading),
                Job::Plan(named) => plan = Some(named),
                Job::Health(signal) => signals.push(signal),
                Job::Settled(done) => settled.push(done),
            }
        }
        if !plan_usage.is_empty() || plan.is_some() || !signals.is_empty() {
            let task_ctx = ctx.clone();
            let before = known.take();
            let told = tokio::task::spawn_blocking(move || {
                if let Some(plan) = plan {
                    task_ctx.plan_usage.observe_plan(&task_ctx.agent_id, plan);
                }
                tell(&task_ctx, plan_usage.into_values(), signals, before)
            })
            .await;
            match told {
                Ok((health, events)) => {
                    for event in events {
                        out.agent_id(event);
                    }
                    known = health;
                }
                Err(err) => out.notice(format!("could not update the agent boards: {err}")),
            }
        }
        for done in settled {
            let _ = done.send(());
        }
    }
}

/// An agent and one of its windows.
type WindowKey = (String, Option<String>);

/// A later reading of a window replaces an earlier one, as the plan usage board does itself.
fn keep_newest(plan_usage: &mut BTreeMap<WindowKey, PlanUsageReading>, reading: PlanUsageReading) {
    let key = (reading.agent_id.0.clone(), reading.window.clone());
    let newer = plan_usage
        .get(&key)
        .is_none_or(|old| old.observed_at <= reading.observed_at);
    if newer {
        plan_usage.insert(key, reading);
    }
}

/// Blocks while the boards lock and write their file. Returns the agent's health after the
/// signals, when there were any, and what changed.
fn tell(
    ctx: &Context,
    plan_usage: impl Iterator<Item = PlanUsageReading>,
    signals: Vec<HealthSignal>,
    before: Option<AgentHealth>,
) -> (Option<AgentHealth>, Vec<AgentStatusEvent>) {
    for reading in plan_usage {
        ctx.plan_usage.observe(reading);
    }
    if signals.is_empty() {
        return (before, Vec::new());
    }
    let before = before.unwrap_or_else(|| ctx.health.state(&ctx.agent_id));
    for signal in signals {
        ctx.health.observe(signal);
    }
    let health = ctx.health.state(&ctx.agent_id);
    let changed = health.state != before.state || health.reason != before.reason;
    let newly_disabled = health.disabled.is_some() && health.disabled != before.disabled;
    let mut events = Vec::new();
    if changed || newly_disabled {
        events.push(AgentStatusEvent::Health(health.clone()));
    }
    if newly_disabled && let Some(reason) = health.disabled.clone() {
        events.push(AgentStatusEvent::Disabled {
            agent_id: ctx.agent_id.clone(),
            reason,
        });
    }
    (Some(health), events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::limits::{PlanUsageSource, PlanUsageStatus};
    use argonauts_core::names::AgentId;
    use time::OffsetDateTime;

    fn reading(agent_id: &str, at: i64) -> PlanUsageReading {
        windowed(agent_id, None, at)
    }

    fn windowed(agent_id: &str, window: Option<&str>, at: i64) -> PlanUsageReading {
        PlanUsageReading {
            agent_id: AgentId(agent_id.into()),
            status: PlanUsageStatus::Allowed,
            used: None,
            resets_at: None,
            window: window.map(str::to_owned),
            source: PlanUsageSource::RateLimitEvent,
            observed_at: OffsetDateTime::from_unix_timestamp(at).unwrap(),
            detail: None,
            wrap_up: false,
        }
    }

    #[test]
    fn only_the_newest_reading_of_each_agent_is_kept() {
        let mut plan_usage = BTreeMap::new();
        for (agent_id, at) in [("a", 2), ("a", 1), ("b", 5), ("a", 3)] {
            keep_newest(&mut plan_usage, reading(agent_id, at));
        }
        let kept: Vec<i64> = plan_usage
            .values()
            .map(|reading| reading.observed_at.unix_timestamp())
            .collect();
        assert_eq!(kept, [3, 5]);
    }

    #[test]
    fn each_window_of_an_agent_keeps_its_own_newest_reading() {
        let mut plan_usage = BTreeMap::new();
        for (window, at) in [("five_hour", 1), ("seven_day", 1), ("five_hour", 2)] {
            keep_newest(&mut plan_usage, windowed("claude-code", Some(window), at));
        }
        let kept: Vec<(Option<&str>, i64)> = plan_usage
            .values()
            .map(|r| (r.window.as_deref(), r.observed_at.unix_timestamp()))
            .collect();
        assert_eq!(kept, [(Some("five_hour"), 2), (Some("seven_day"), 1)]);
    }
}
