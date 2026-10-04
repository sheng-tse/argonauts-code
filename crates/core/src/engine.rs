//! The run engine, driven by the per-run supervisor process.
//!
//! Every step is idempotent, so a replayed log never merges twice or sends a message twice.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::api::{
    CheckpointAnswer, DiffScope, DiffView, RawView, StatusSummary, SteerAck, TakeOverTicket,
};
use crate::clock::Clock;
use crate::events::goal::GoalAction;
use crate::events::run::{ParkReason, RunOutcome};
use crate::events::session::QueuedMessage;
use crate::events::{RunEvent, RunEventKind, VendorEvent};
use crate::future::BoxFuture;
use crate::input::MentionTarget;
use crate::journal::JournalError;
use crate::limits::{
    GateRunner, HandoffPlanner, HealthBoard, LimitsService, QuotaBoard, TrackRecords,
};
use crate::names::{CheckpointId, MessageId, SessionKey, WorkerRef, WorkflowId};
use crate::policy::ApprovalPolicy;
use crate::registry::ModelRegistry;
use crate::settings::Settings;
use crate::settings::approvals::ApprovalMode;
use crate::steps::StepRunner;
use crate::worker::WorkerDirectory;
use crate::workflow::Workflow;
use crate::{CardId, RepoId, RunId};

/// Appends to the run's log and fans each event out to subscribers.
pub trait EventSink: Send + Sync {
    /// Returns the event with its `seq` and timestamp filled in.
    fn record(
        &self,
        card: Option<&CardId>,
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
    /// A new run holds only the `RunStarted` the supervisor wrote.
    pub history: Vec<RunEvent>,
    pub sink: Arc<dyn EventSink>,
    pub inbox: mpsc::Receiver<EngineCommand>,
    pub vendor_events: broadcast::Receiver<VendorEvent>,
    pub services: Services,
    /// Cancelled on SIGTERM or supervisor exit. Stop every session after its tool call, commit,
    /// record `Parked`, and return.
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
    /// Card worktrees go under `~/.argonauts/worktrees/<repo>/<run>`.
    pub worktrees: PathBuf,
    /// `<worktrees>/base`, detached at the run's base commit, where every read-only session
    /// works. The main checkout, with its uncommitted files, is only for chat and triage.
    pub base_worktree: PathBuf,
}

/// Trait objects so tests can swap in fakes.
#[derive(Clone)]
pub struct Services {
    pub workers: Arc<dyn WorkerDirectory>,
    pub steps: Arc<dyn StepRunner>,
    /// Approvals and hard limits.
    pub policy: Arc<dyn ApprovalPolicy>,
    /// Session limits and recovery decisions.
    pub limits: Arc<dyn LimitsService>,
    pub handoff: Arc<dyn HandoffPlanner>,
    pub quota: Arc<dyn QuotaBoard>,
    pub health: Arc<dyn HealthBoard>,
    pub gates: Arc<dyn GateRunner>,
    pub registry: Arc<dyn ModelRegistry>,
    pub track_records: Arc<dyn TrackRecords>,
    pub clock: Arc<dyn Clock>,
}

pub enum EngineCommand {
    Answer {
        answer: CheckpointAnswer,
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
    },
    /// Applies to every decision point of this run from now on.
    ApprovalMode {
        mode: ApprovalMode,
    },
    CancelPause,
    Stop,
    Reassign {
        card: CardId,
        to: WorkerRef,
        /// Already checked against the approval token.
        rule_override: Option<String>,
        /// Err names the rule it breaks.
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Pause a card for take-over.
    TakeOver {
        card: CardId,
        reply: oneshot::Sender<Result<TakeOverTicket, String>>,
    },
    TakeOverEnded {
        card: CardId,
    },
    Status {
        reply: oneshot::Sender<StatusSummary>,
    },
    Diff {
        card: CardId,
        scope: DiffScope,
        reply: oneshot::Sender<Result<DiffView, String>>,
    },
    Raw {
        session: SessionKey,
        reply: oneshot::Sender<Result<RawView, String>>,
    },
    /// Reset a card's branch to checkpoint `handoff-<checkpoint>`, keeping the work after it on
    /// a side branch. Refused for merged work, which a new change undoes instead.
    Revert {
        card: CardId,
        checkpoint: u32,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Your verdict on one of this run's auto-approvals, passed to the policy's
    /// `spot_checked`.
    SpotCheck {
        id: CheckpointId,
        correct: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Clear the goal, turn the workflow into one, or change an assumption.
    Goal {
        action: GoalAction,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// A change to the running workflow from the next phase on. It goes through the
    /// `change_request` checkpoint, so the reply is that checkpoint's id, or the validator's
    /// errors.
    ProposeWorkflowChange {
        /// A nested workflow to change. `None` is the run's own.
        target: Option<WorkflowId>,
        workflow: Box<Workflow>,
        /// One line for the checkpoint, such as "add a security review before merge".
        reason: String,
        reply: oneshot::Sender<Result<CheckpointId, String>>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum EngineExit {
    /// `RunFinished` is already logged.
    Finished(RunOutcome),
    /// Nothing can happen until something outside changes; `Parked` is logged.
    Parked(ParkReason),
    /// The engine itself failed, such as an unwritable log. The next supervisor recovers.
    Crashed(String),
}

/// One per run per supervisor process.
pub trait RunEngine: Send {
    fn run(self: Box<Self>, context: EngineContext) -> BoxFuture<'static, EngineExit>;
}
