//! The run as a whole: its start, mode, phase, pause and stop requests, approval mode, waiting,
//! limit, agents, and outcome.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use argonauts_core::base::names::{AgentId, ApprovalId, ModelRef};
use argonauts_core::models::limits::{HealthState, ModelState, PlanUsageStatus};
use argonauts_core::runs::events::RunEvent;
use argonauts_core::runs::events::agent_status::{
    AgentStatusEvent, FallbackEvent, JobRef, JobWait, RunSpend, SpendEvent,
};
use argonauts_core::runs::events::approval::ApprovalEvent;
use argonauts_core::runs::events::run::{
    PhaseChange, ResumeCause, RunLifecycle, RunMode, RunNamed, RunOutcome, RunPhase, RunStarted,
    WaitReason,
};
use argonauts_core::runs::events::session::SessionStarted;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::{Note, TaskId};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunInfo {
    pub started: Option<Box<RunStarted>>,
    /// The run's name and description, the newest `Named`.
    #[serde(default)]
    pub named: Option<RunNamed>,
    pub mode: Option<RunMode>,
    pub phase: Option<RunPhase>,
    /// `Some(now)` while a pause is asked for and not cancelled.
    pub pause_requested: Option<bool>,
    pub paused: bool,
    pub stop_requested: bool,
    /// Set with Shift+Tab. It applies to every decision point from then on.
    pub approval_mode: Option<ApprovalMode>,
    pub waiting: Option<WaitReason>,
    pub finished: Option<RunOutcome>,
    /// The run has its closing answer.
    #[serde(default)]
    pub closed: bool,
    pub spend: Option<RunSpend>,
    /// The seq of the newest `Run::Snapshot`.
    pub last_snapshot: Option<u64>,
    /// How many times the run woke after waiting or a crash.
    pub resumes: u32,
    /// The seq of its newest `Resumed`. An agent failure from before it no longer counts
    /// against an agent, since what fixes one may have happened while the run was waiting.
    #[serde(default)]
    pub last_resumed: Option<u64>,
    /// Waits in a row because a job had nobody to fall back to, since an agent last finished
    /// a session. Each makes the wait before the next try on its own longer.
    #[serde(default)]
    pub fallback_waits: u32,
    pub health: BTreeMap<AgentId, HealthState>,
    /// Each model that isn't plainly healthy, on a CLI whose other models may be.
    #[serde(default)]
    pub model_health: BTreeMap<ModelRef, ModelState>,
    pub plan_usage: BTreeMap<AgentId, PlanUsageStatus>,
    /// When an agent whose plan usage ran out said it resets, in Unix seconds.
    #[serde(default)]
    pub plan_usage_resets: BTreeMap<AgentId, i64>,
    pub disabled: BTreeMap<AgentId, String>,
    /// Tasks waiting for an agent to come back, and until when if it said.
    pub agent_waits: BTreeMap<TaskId, AgentWait>,
    /// Jobs other than build tasks that wait for their CLI because a setting keeps them on
    /// it, until one starts again or you let it go.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_jobs: Vec<JobWait>,
    /// Jobs you let leave their CLI, each with the agent it goes to first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub freed_jobs: Vec<FreedJob>,
    /// When each approval opened, in Unix seconds, for the fallback ask timeout.
    pub opened_at: BTreeMap<ApprovalId, i64>,
    /// The first line of every `›` note in the log, with the seq of the newest, so a step that
    /// replays after a stop can tell it said why already.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub said: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentWait {
    pub agent_id: AgentId,
    /// Unix seconds.
    pub until: Option<i64>,
}

/// A job that may leave its CLI whatever `fallback.same_cli` says.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FreedJob {
    pub job: JobRef,
    /// `None` leaves the choice to the role's fallbacks.
    pub to: Option<ModelRef>,
}

impl RunInfo {
    pub fn noted(&mut self, note: &Note, seq: u64) {
        if let Some(line) = note.text.lines().next() {
            self.said.insert(line.to_owned(), seq);
        }
    }

    pub fn apply(&mut self, event: &RunLifecycle, envelope: &RunEvent) {
        match event {
            RunLifecycle::Started(started) => {
                self.mode = Some(started.mode);
                self.started = Some(started.clone());
            }
            RunLifecycle::Named(named) => self.named = Some(named.clone()),
            RunLifecycle::Resumed(resumed) => {
                self.resumes += 1;
                self.last_resumed = Some(envelope.seq);
                // A crash that cut a pause short keeps the pause: only a waiting run's resume
                // ends it. Reopening a stopped run ends its stop.
                let reopened = self.finished.take().is_some();
                if reopened {
                    self.closed = false;
                }
                if self.waiting.is_some() || reopened {
                    self.paused = false;
                    self.pause_requested = None;
                    self.stop_requested = false;
                }
                self.waiting = None;
                if resumed.cause == ResumeCause::AgentBack {
                    self.agent_waits.clear();
                }
            }
            RunLifecycle::PauseRequested { now, .. } => self.pause_requested = Some(*now),
            RunLifecycle::ApprovalModeChanged { mode, .. } => self.approval_mode = Some(*mode),
            RunLifecycle::PauseCancelled => {
                self.pause_requested = None;
                self.paused = false;
            }
            RunLifecycle::Paused { .. } => {
                self.pause_requested = None;
                self.paused = true;
            }
            RunLifecycle::StopRequested { .. } => self.stop_requested = true,
            RunLifecycle::Waiting { reason, .. } => {
                if matches!(reason, WaitReason::NoFallback { .. }) {
                    self.fallback_waits += 1;
                }
                self.waiting = Some(reason.clone());
            }
            RunLifecycle::Snapshot(_) => self.last_snapshot = Some(envelope.seq),
            RunLifecycle::ClosingAnswer(_) => self.closed = true,
            RunLifecycle::Finished { outcome, .. } => self.finished = Some(*outcome),
        }
    }

    pub fn apply_phase(&mut self, event: &PhaseChange, envelope: &RunEvent) {
        let _ = envelope;
        self.phase = Some(event.phase);
    }

    pub fn apply_agent(&mut self, event: &AgentStatusEvent, envelope: &RunEvent) {
        let _ = envelope;
        match event {
            AgentStatusEvent::PlanUsage(reading) => {
                self.plan_usage
                    .insert(reading.agent_id.clone(), reading.status);
                match reading.resets_at {
                    Some(at) if reading.status == PlanUsageStatus::Rejected => {
                        self.plan_usage_resets
                            .insert(reading.agent_id.clone(), at.unix_timestamp());
                    }
                    _ => {
                        self.plan_usage_resets.remove(&reading.agent_id);
                    }
                }
            }
            AgentStatusEvent::Health(health) => {
                self.health.insert(health.agent_id.clone(), health.state);
                match &health.disabled {
                    Some(reason) => self
                        .disabled
                        .insert(health.agent_id.clone(), reason.clone()),
                    None => self.disabled.remove(&health.agent_id),
                };
            }
            AgentStatusEvent::Disabled { agent_id, reason } => {
                self.disabled.insert(agent_id.clone(), reason.clone());
            }
            AgentStatusEvent::Model(health) => {
                if health.state == ModelState::Healthy {
                    self.model_health.remove(&health.model);
                } else {
                    self.model_health.insert(health.model.clone(), health.state);
                }
            }
        }
    }

    pub fn apply_limit(&mut self, event: &SpendEvent, envelope: &RunEvent) {
        let _ = envelope;
        if let SpendEvent::Run(limit) = event {
            self.spend = Some(limit.clone());
        }
    }

    /// Which tasks wait for an agent. Their fallbacks are the task fold's; this is for waiting.
    pub fn apply_fallback(&mut self, event: &FallbackEvent, envelope: &RunEvent) {
        let _ = envelope;
        match event {
            FallbackEvent::Waiting {
                task,
                agent_id,
                until,
                ..
            } => {
                let wait = AgentWait {
                    agent_id: agent_id.clone(),
                    until: until.map(OffsetDateTime::unix_timestamp),
                };
                self.agent_waits.insert(task.clone(), wait);
            }
            FallbackEvent::FellBack { task, .. } => {
                self.agent_waits.remove(task);
            }
            FallbackEvent::JobWaiting(wait) => {
                self.waiting_jobs.retain(|known| known.job != wait.job);
                self.waiting_jobs.push(wait.clone());
            }
            FallbackEvent::JobFreed { job, to, .. } => {
                self.waiting_jobs.retain(|known| known.job != *job);
                self.freed_jobs.retain(|known| known.job != *job);
                self.freed_jobs.push(FreedJob {
                    job: job.clone(),
                    to: to.clone(),
                });
            }
            FallbackEvent::Needed(_) | FallbackEvent::Note { .. } => {}
        }
    }

    /// A job whose session started no longer waits for its CLI, whichever round it is in.
    pub fn job_started(&mut self, started: &SessionStarted, task: Option<&TaskId>) {
        self.waiting_jobs.retain(|known| {
            let job = &known.job;
            !(job.role == started.role
                && job.does == started.step
                && job.task.as_ref() == task
                && job.step == started.workflow_step)
        });
    }

    /// The agent `job` goes to first when you let it leave its CLI: `Some(None)` when you left
    /// the choice to its role's fallbacks, `None` when it stays.
    pub fn freed(&self, job: &JobRef) -> Option<Option<&ModelRef>> {
        self.freed_jobs
            .iter()
            .find(|known| known.job == *job)
            .map(|known| known.to.as_ref())
    }

    pub fn apply_approval(&mut self, event: &ApprovalEvent, envelope: &RunEvent) {
        match event {
            ApprovalEvent::Opened(opened) => {
                self.opened_at
                    .insert(opened.id.clone(), envelope.ts.unix_timestamp());
            }
            ApprovalEvent::Decided(decided) => {
                self.opened_at.remove(&decided.id);
            }
            ApprovalEvent::Withdrawn { id, .. } => {
                self.opened_at.remove(id);
            }
            ApprovalEvent::Checked(_) | ApprovalEvent::SpotCheckQueued { .. } => {}
        }
    }

    /// A task with work again no longer waits for its agent.
    pub fn task_active(&mut self, task: &TaskId) {
        self.agent_waits.remove(task);
    }

    /// Whether this agent was out, by health or plan usage, as far as the log says.
    pub fn agent_out(&self, agent_id: &AgentId) -> bool {
        self.health_out(agent_id)
            || self.plan_usage.get(agent_id) == Some(&PlanUsageStatus::Rejected)
    }

    /// Whether it's still out at `now`: plan usage that ran out counts until the reset it
    /// reported has passed, as the fallback planner reads it.
    pub fn agent_out_at(&self, agent_id: &AgentId, now: OffsetDateTime) -> bool {
        self.health_out(agent_id) || self.plan_usage_out_at(agent_id, now)
    }

    fn health_out(&self, agent_id: &AgentId) -> bool {
        matches!(
            self.health.get(agent_id),
            Some(HealthState::Down | HealthState::Degraded)
        )
    }

    fn plan_usage_out_at(&self, agent_id: &AgentId, now: OffsetDateTime) -> bool {
        self.plan_usage.get(agent_id) == Some(&PlanUsageStatus::Rejected)
            && self
                .plan_usage_resets
                .get(agent_id)
                .is_none_or(|at| *at > now.unix_timestamp())
    }

    /// Agents whose plan usage ran out and has reset since, and that are out no other way.
    pub fn plan_usage_reset_passed(&self, now: OffsetDateTime) -> Vec<AgentId> {
        self.plan_usage_resets
            .iter()
            .filter(|(agent_id, at)| {
                **at <= now.unix_timestamp()
                    && self.plan_usage.get(*agent_id) == Some(&PlanUsageStatus::Rejected)
                    && !self.health_out(agent_id)
            })
            .map(|(agent_id, _)| agent_id.clone())
            .collect()
    }

    /// The soonest reset still to come of plan usage that ran out, when one was reported.
    pub fn next_plan_usage_reset(&self, now: OffsetDateTime) -> Option<i64> {
        self.plan_usage_resets
            .iter()
            .filter(|(agent_id, at)| {
                self.plan_usage.get(*agent_id) == Some(&PlanUsageStatus::Rejected)
                    && **at > now.unix_timestamp()
            })
            .map(|(_, at)| *at)
            .min()
    }
}
