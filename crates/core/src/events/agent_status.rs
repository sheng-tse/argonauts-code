//! Fallbacks, agent plan usage and health, and limits.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::run::Party;
use crate::limits::{AgentHealth, Candidate, ModelHealth, PlanUsageReading};
use crate::names::{AgentId, ModelRef, RoleName};
use crate::settings::fallback::{FallbackContext, FallbackMode, FallbackStrategy};
use crate::settings::limit::{LimitAmount, LimitLineKind};
use crate::steps::StepName;
use crate::workflow::StepRef;
use crate::{DiffStat, LimitDimension, Spend, SpendStatus, TaskId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum FallbackEvent {
    /// A fallback is needed; the work so far is committed.
    Needed(FallbackPlan),
    /// The CLI-neutral note was built from the log, at no token cost.
    Note {
        task: TaskId,
        path: PathBuf,
        sha256: String,
    },
    FellBack {
        task: TaskId,
        to: ModelRef,
        mode: FallbackMode,
        by: Party,
        /// What the fallback is told, which the transcript line names.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<FallbackContext>,
    },
    Waiting {
        task: TaskId,
        agent_id: AgentId,
        #[serde(with = "time::serde::rfc3339::option")]
        until: Option<OffsetDateTime>,
        /// The CLI that would have taken the task, had `fallback.same_cli` not kept it where
        /// it was.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kept_off: Option<AgentId>,
    },
    /// A job that isn't a build task, such as a review, has nobody to take it: its CLI is out
    /// and `fallback.same_cli` keeps it there, so the run waits for the CLI.
    JobWaiting(JobWait),
    /// A waiting job may leave its CLI: you answered the question about the CLI that way.
    JobFreed {
        job: JobRef,
        /// The agent it goes to first. `None` leaves the choice to the role's fallbacks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<ModelRef>,
        by: Party,
    },
}

/// A job that isn't a build task: a review, a judge's ruling, or a step of the main agent's
/// own. Where it sits in its workflow, the role that does it, and the task it is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<StepRef>,
    /// Which round of the step, 1 the first time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
    pub role: RoleName,
    /// What its session does, where the role has more than one kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub does: Option<StepName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
}

/// A job that waits for its CLI, and who could have taken it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobWait {
    pub job: JobRef,
    /// The agent the job was asked of.
    pub stopped: ModelRef,
    /// Who could take it on another CLI, best first. For a job its builder's provider keeps
    /// waiting, everyone you could still let take it, the role's own fallbacks first.
    pub elsewhere: Vec<ModelRef>,
    /// What the job is called: its task's title, or its role in words.
    pub title: String,
    /// What keeps it from those agents. A record without one was kept by `same_cli`.
    #[serde(default, skip_serializing_if = "JobKeeper::is_same_cli")]
    pub kept_by: JobKeeper,
}

/// What keeps a waiting job from every agent that could take it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "snake_case")]
pub enum JobKeeper {
    /// `fallback.same_cli`: only the asked CLI's own models may take it.
    #[default]
    SameCli,
    /// A review or a ruling never falls back to its builder's own provider by itself, and
    /// every agent left is of that provider.
    BuildersProvider {
        /// Whose work it reviews or judges.
        builders: Vec<ModelRef>,
        /// Who stopped and why, in one clause.
        why: String,
    },
}

impl JobKeeper {
    pub fn is_same_cli(&self) -> bool {
        *self == Self::SameCli
    }
}

/// Who stopped, why, and who could continue. Also the fallback approval's question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FallbackPlan {
    pub task: TaskId,
    pub title: String,
    pub stopped: ModelRef,
    /// One line, such as "usage limit, resets in 2h 10m".
    pub reason: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub resets_at: Option<OffsetDateTime>,
    pub work: DiffStat,
    pub checks_ran: bool,
    /// Valid candidates, best first. Only ones the role's rules allow.
    pub candidates: Vec<Candidate>,
    pub mode: FallbackMode,
    /// For the task's strategy line, such as "Claude Code's 3 tasks will be spread across
    /// Codex and agy".
    #[serde(default, skip_serializing_if = "is_first")]
    pub strategy: FallbackStrategy,
    /// Tries at once for a race.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub race_count: u32,
    /// Other tasks of the stopped agent that the same strategy moves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_moving: Vec<TaskId>,
}

fn is_first(strategy: &FallbackStrategy) -> bool {
    *strategy == FallbackStrategy::First
}

fn one() -> u32 {
    1
}

fn is_one(n: &u32) -> bool {
    *n == 1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentStatusEvent {
    PlanUsage(PlanUsageReading),
    /// An agent's circuit-breaker state changed.
    Health(AgentHealth),
    /// One model's own state changed, while its CLI's other models go on.
    Model(ModelHealth),
    /// An agent is disabled for new tasks, such as after a broken CLI update.
    Disabled {
        agent_id: AgentId,
        /// Why, with the suggested pin.
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SpendEvent {
    /// Throttled to once a second.
    Run(RunSpend),
    /// A task hit a limit; its work is committed and `limits.when_reached` decides.
    Hit {
        task: TaskId,
        dimension: LimitDimension,
        status: SpendStatus,
    },
    /// A raised limit for a task, from you or from the reserve by your rules.
    Extended {
        task: TaskId,
        amount: LimitAmount,
        by: Party,
    },
    /// Tasks deferred until spend frees up.
    Deferred { tasks: Vec<TaskId> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSpend {
    /// Sessions and the narrator are excluded.
    pub spent: Spend,
    /// `None` when limits are off.
    pub limit: Option<LimitAmount>,
    pub lines: Vec<LineUse>,
    pub reserve_left: Option<LimitAmount>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LineUse {
    pub line: LimitLineKind,
    pub allocated: LimitAmount,
    /// In the same unit as `allocated`.
    pub used: f64,
}
