//! The run engine, driven by the per-run run host process.
//!
//! Every step is idempotent, so a replayed log never merges twice or sends a message twice.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::api::{
    ApprovalAnswer, DiffScope, DiffView, RawView, StatusSummary, SteerAck, TakeOverTicket,
};
use crate::clock::Clock;
use crate::events::goal::GoalAction;
use crate::events::run::{RunOutcome, WaitReason};
use crate::events::session::QueuedMessage;
use crate::events::{AgentStatusEvent, RunEvent, RunEventKind};
use crate::future::BoxFuture;
use crate::input::MentionTarget;
use crate::journal::JournalError;
use crate::limits::{
    CheckRunner, FallbackPlanner, HealthBoard, LimitsService, PlanUsageBoard, TrackRecords,
};
use crate::names::{ApprovalId, MessageId, ModelRef, SessionKey, WorkflowId};
use crate::policy::ApprovalPolicy;
use crate::registry::ModelRegistry;
use crate::session_runner::RunnerDirectory;
use crate::settings::Settings;
use crate::settings::approvals::ApprovalMode;
use crate::steps::StepRunner;
use crate::workflow::Workflow;
use crate::{RepoId, RunId, TaskId};

/// Appends to the run's log and fans each event out to subscribers.
pub trait EventSink: Send + Sync {
    /// Returns the event with its `seq` and timestamp filled in.
    fn record(
        &self,
        task: Option<&TaskId>,
        session: Option<&SessionKey>,
        source: &str,
        kind: RunEventKind,
    ) -> Result<RunEvent, JournalError>;
}

pub struct EngineContext {
    pub run: RunId,
    pub repo: RepoContext,
    /// Read from the main checkout at run start.
    pub settings: Arc<Settings>,
    pub settings_sha256: String,
    /// From the latest `Run::Snapshot` when there is one, else from `seq` 0.
    /// A new run holds only the `RunStarted` the run host wrote.
    pub history: Vec<RunEvent>,
    pub sink: Arc<dyn EventSink>,
    pub inbox: mpsc::Receiver<EngineCommand>,
    pub agent_events: broadcast::Receiver<AgentStatusEvent>,
    pub services: Services,
    /// Cancelled on SIGTERM or run host exit. Stop every session after its tool call, commit,
    /// record `Waiting`, and return.
    pub shutdown: CancellationToken,
    /// You asked to carry on a run whose log ends `Finished(Stopped)`: the engine records
    /// `Resumed` and goes on from its last committed state. Without it, an engine over a
    /// finished run only answers what it can and returns at once.
    pub reopen: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RepoContext {
    pub id: RepoId,
    pub root: PathBuf,
    /// `~/.argonauts/state/<repo>/runs/<run>`
    pub run_dir: PathBuf,
    /// Task worktrees go under `~/.argonauts/worktrees/<repo>/<run>`.
    pub worktrees: PathBuf,
    /// `<worktrees>/base`, detached at the run's base commit, where every read-only session
    /// works. The main checkout, with its uncommitted files, is only for conversation and the
    /// estimate.
    pub base_worktree: PathBuf,
}

impl RepoContext {
    /// `~/.argonauts/state/<repo>`, which holds the run's folder.
    pub fn state_dir(&self) -> &Path {
        self.run_dir
            .parent()
            .and_then(Path::parent)
            .unwrap_or(&self.run_dir)
    }

    /// The hidden history `root` works with when it has no git of its own. Git calls for
    /// `root` must name it, since nothing in the folder points there.
    pub fn hidden_git(&self) -> Option<PathBuf> {
        crate::hidden_git::of(&self.root, self.state_dir())
    }
}

/// Trait objects so tests can swap in fakes.
#[derive(Clone)]
pub struct Services {
    pub workers: Arc<dyn RunnerDirectory>,
    pub steps: Arc<dyn StepRunner>,
    /// Approvals and hard limits.
    pub policy: Arc<dyn ApprovalPolicy>,
    /// Session limits and recovery decisions.
    pub limits: Arc<dyn LimitsService>,
    pub fallback: Arc<dyn FallbackPlanner>,
    pub plan_usage: Arc<dyn PlanUsageBoard>,
    pub health: Arc<dyn HealthBoard>,
    pub checks: Arc<dyn CheckRunner>,
    pub registry: Arc<dyn ModelRegistry>,
    pub track_records: Arc<dyn TrackRecords>,
    pub clock: Arc<dyn Clock>,
}

pub enum EngineCommand {
    Answer {
        answer: ApprovalAnswer,
        /// Err says why it was refused, such as an unknown action or changed evidence.
        reply: oneshot::Sender<Result<(), String>>,
    },
    Steer {
        session: SessionKey,
        text: String,
        /// Hand it to the model now, where the session's CLI can.
        now: bool,
        reply: oneshot::Sender<Result<SteerAck, String>>,
    },
    Mention {
        target: MentionTarget,
        text: String,
        reply: oneshot::Sender<Result<MessageId, String>>,
    },
    /// For every working agent's next turn.
    Guidance {
        text: String,
        reply: oneshot::Sender<Result<MessageId, String>>,
    },
    /// Take undelivered messages back.
    TakeBack {
        messages: Vec<MessageId>,
        reply: oneshot::Sender<Vec<QueuedMessage>>,
    },
    /// Pause at the next safe point, or stop the agents now.
    Pause {
        now: bool,
    },
    StopSession {
        session: SessionKey,
        /// Starts it over with the prompt it had once it has stopped.
        again: bool,
    },
    /// Applies to every decision point of this run from now on.
    ApprovalMode {
        mode: ApprovalMode,
    },
    CancelPause,
    Stop,
    Reassign {
        task: TaskId,
        to: ModelRef,
        /// Already checked against the approval token.
        rule_override: Option<String>,
        /// Err names the rule it breaks.
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Pause a task for take-over.
    TakeOver {
        task: TaskId,
        reply: oneshot::Sender<Result<TakeOverTicket, String>>,
    },
    TakeOverEnded {
        task: TaskId,
    },
    Status {
        reply: oneshot::Sender<StatusSummary>,
    },
    Diff {
        task: TaskId,
        scope: DiffScope,
        reply: oneshot::Sender<Result<DiffView, String>>,
    },
    Raw {
        session: SessionKey,
        reply: oneshot::Sender<Result<RawView, String>>,
    },
    /// Reset a task's branch to checkpoint `fallback-<checkpoint>`, keeping the work after it on
    /// a side branch. Refused for merged work, which a new change undoes instead.
    Rewind {
        task: TaskId,
        checkpoint: u32,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Your verdict on one of this run's auto-approvals, passed to the policy's
    /// `spot_checked`.
    SpotCheck {
        id: ApprovalId,
        correct: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Clear the goal, turn the workflow into one, or change an assumption.
    Goal {
        action: GoalAction,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// A change to the running workflow from the next phase on. It goes through the
    /// `change_request` approval, so the reply is that approval's id, or the validator's
    /// errors.
    ProposeWorkflowChange {
        /// A nested workflow to change. `None` is the run's own.
        target: Option<WorkflowId>,
        workflow: Box<Workflow>,
        /// One line for the approval, such as "add a security review before merge".
        reason: String,
        reply: oneshot::Sender<Result<ApprovalId, String>>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum EngineExit {
    /// `RunFinished` is already logged.
    Finished(RunOutcome),
    /// Nothing can happen until something outside changes; `Waiting` is logged.
    Waiting(WaitReason),
    /// The engine itself failed, such as an unwritable log. The next run host recovers.
    Crashed(String),
}

/// One per run per run host process.
pub trait RunEngine: Send {
    fn run(self: Box<Self>, context: EngineContext) -> BoxFuture<'static, EngineExit>;
}
