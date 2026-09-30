//! The app-facing conductor API: start, watch, and steer runs.
//!
//! Anything that decides or raises autonomy lives in [`Approvals`], which only the interactive
//! app holds, so no agent-facing surface can approve anything.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::{mpsc, watch};

use crate::events::RunEvent;
use crate::events::card::CardState;
use crate::events::checkpoint::AnswerAction;
use crate::events::run::{RunMode, TriageDecision};
use crate::events::session::QueuedMessage;
use crate::events::vendor::RunBudget;
use crate::future::BoxFuture;
use crate::input::MentionTarget;
use crate::limits::QuotaReading;
use crate::names::{CheckpointId, DecisionId, MessageId, SessionKey, TurnId, WorkerRef};
use crate::settings::approvals::ApprovalMode;
use crate::steps::intake::ChangeEstimate;
use crate::worker::SteerSupport;
use crate::{CardId, CommandSpec, DiffStat, RunId, Spend};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartRun {
    pub request: String,
    pub mode: RunMode,
    /// Forced by `/solo`, `/crew`, `--solo`, or `--crew`.
    pub forced: bool,
    pub estimate: Option<ChangeEstimate>,
    pub triage: Option<TriageDecision>,
    pub chat_turn: Option<TurnId>,
    /// The merged run this continues (`/continue`).
    pub continues: Option<RunId>,
    /// `--set key=value` overrides for this run.
    pub overrides: Vec<String>,
    /// Include uncommitted changes in the base.
    pub include_uncommitted: bool,
    pub default_agent: Option<WorkerRef>,
    /// `None` or `Supervised` only; `start` refuses anything more autonomous. Raising it goes
    /// through [`Approvals::set_approval_mode`], which needs the token.
    pub approval_mode: Option<ApprovalMode>,
    /// Set by the MCP server to the client's self-reported name. Such a run gets
    /// `mcp.run_budget` and counts against `mcp.runs_per_day`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub front_end: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunHandle {
    pub run: RunId,
    /// Path to the event log.
    pub events: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub run: RunId,
    pub title: String,
    pub mode: RunMode,
    pub preset: String,
    pub state: RunState,
    /// Such as "plan checkpoint, waiting for you".
    pub stopped_at: String,
    /// Last event time.
    #[serde(with = "time::serde::rfc3339")]
    pub updated: OffsetDateTime,
    pub spent: Spend,
    pub sessions_to_resume: u32,
    pub waiting: Vec<CheckpointId>,
    /// Whether a supervisor is running it now.
    pub live: bool,
    /// Parked longer than `runs.remind_parked_days`. Parked runs never expire.
    #[serde(default)]
    pub remind: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    Pausing,
    Parked,
    /// Its supervisor died without parking. `/resume` recovers it.
    Crashed,
    Merged,
    /// A pull request was opened or suggested.
    PullRequest,
    /// Branches kept, not merged.
    BranchLeft,
    Stopped,
    Failed,
    Abandoned,
}

/// The log from `from_seq`, then live events. Closes after the run's last event once it has
/// finished.
pub struct EventStream {
    pub events: mpsc::Receiver<RunEvent>,
    /// The last `seq` in the log when the stream opened. Events up to it are history, so the
    /// app sends `AppEvent::ReplayDone` after them and rings no bell for them.
    pub replay_until: Option<u64>,
    /// Turns false when the supervisor exits; if the log's last event then isn't `Parked` or
    /// `Finished`, the run crashed.
    pub live: watch::Receiver<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SteerAck {
    pub session: SessionKey,
    pub how: SteerSupport,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TakeOverTicket {
    /// Paused with its work committed.
    pub card: CardId,
    /// The vendor's own UI on the same session, in the card's worktree. `None` for endpoints or
    /// a vendor out of quota; you then work in the worktree with your own tools.
    pub command: Option<CommandSpec>,
    pub worktree: PathBuf,
    pub vendor: String,
}

/// Built by code from the event log, with no model call.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StatusSummary {
    pub run: Option<RunId>,
    /// The approved goal's summary and criteria.
    pub goal: Vec<String>,
    pub plan: Option<String>,
    pub decisions: Vec<(DecisionId, String)>,
    pub cards: Vec<CardBrief>,
    /// Newest last.
    pub recent: Vec<String>,
    pub budget: Option<RunBudget>,
    pub quotas: Vec<QuotaReading>,
    pub waiting: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardBrief {
    pub card: CardId,
    pub title: String,
    pub state: CardState,
    pub owner: Option<WorkerRef>,
    /// Such as "6m 12s · 38k tok" or "needs card-002 and card-003".
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiffView {
    pub card: CardId,
    pub stat: DiffStat,
    /// The unified diff, cut at 2 MB. The full one is at `path`.
    pub text: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffScope {
    /// The whole card against its base.
    Card,
    /// The last session's changes.
    LastTurn,
}

/// The vendor's own transcript lines, unparsed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawView {
    pub session: SessionKey,
    /// Oldest first, cut to the last 5,000.
    pub lines: Vec<String>,
    pub truncated: bool,
    /// Argonauts' raw copy.
    pub path: PathBuf,
    pub vendor_transcript: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    #[error("no run {0}")]
    NoRun(RunId),
    /// The run can't do that now, such as steering a finished session.
    #[error("{0}")]
    NotNow(String),
    #[error("run {0} is merged; /continue starts a follow-up run")]
    Merged(RunId),
    #[error("refused: {0}")]
    Refused(String),
    #[error("supervisor: {0}")]
    Supervisor(String),
}

/// Start, watch, and steer runs in one repository.
pub trait ConductorApi: Send + Sync {
    /// Returns once `RunStarted` is logged.
    fn start(&self, request: StartRun) -> BoxFuture<'_, Result<RunHandle, ApiError>>;
    fn subscribe(&self, run: &RunId, from_seq: u64)
    -> BoxFuture<'_, Result<EventStream, ApiError>>;
    /// Replays from the latest `Run::Snapshot` (or `seq` 0 when there is none), so a long run
    /// redraws without replaying every event.
    fn reattach(&self, run: &RunId) -> BoxFuture<'_, Result<EventStream, ApiError>> {
        self.subscribe(run, 0)
    }
    /// Delivers after the current tool call.
    fn steer(
        &self,
        run: &RunId,
        session: &SessionKey,
        text: String,
    ) -> BoxFuture<'_, Result<SteerAck, ApiError>>;
    /// Queued for the target's next turn.
    fn mention(
        &self,
        run: &RunId,
        target: MentionTarget,
        text: String,
    ) -> BoxFuture<'_, Result<MessageId, ApiError>>;
    /// Queued for every working agent's next turn.
    fn guidance(&self, run: &RunId, text: String) -> BoxFuture<'_, Result<MessageId, ApiError>>;
    /// Guidance from an MCP client, recorded as `Party::FrontEnd { client }` so the log shows
    /// where it came from. It takes a client name rather than any `Party`, so a caller can't
    /// claim to be your rules or the approver.
    fn guidance_from_front_end(
        &self,
        run: &RunId,
        text: String,
        client: String,
    ) -> BoxFuture<'_, Result<MessageId, ApiError>> {
        let _ = client;
        self.guidance(run, text)
    }
    /// Delivered messages are skipped.
    fn take_back(
        &self,
        run: &RunId,
        messages: Vec<MessageId>,
    ) -> BoxFuture<'_, Result<Vec<QueuedMessage>, ApiError>>;
    /// Pauses at the next safe point, or with `now` stops the agents at once and commits.
    fn pause(&self, run: &RunId, now: bool) -> BoxFuture<'_, Result<(), ApiError>>;
    /// A pause from an MCP client, recorded in `PauseRequested.by` as
    /// `Party::FrontEnd { client }`.
    fn pause_from_front_end(
        &self,
        run: &RunId,
        now: bool,
        client: String,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        let _ = client;
        self.pause(run, now)
    }
    fn cancel_pause(&self, run: &RunId) -> BoxFuture<'_, Result<(), ApiError>>;
    /// Starts a supervisor that rebuilds from the log and resumes sessions.
    fn resume(&self, run: &RunId) -> BoxFuture<'_, Result<(), ApiError>>;
    /// Agents wrap up and report, work is committed, and the run ends as stopped.
    fn stop(&self, run: &RunId) -> BoxFuture<'_, Result<(), ApiError>>;
    /// The session's card goes to `NeedsDecision`.
    fn stop_session(
        &self,
        run: &RunId,
        session: &SessionKey,
    ) -> BoxFuture<'_, Result<(), ApiError>>;
    /// Moves the card with its work saved and a handoff packet, to a worker the role's rules
    /// allow. Overriding a rule needs [`Approvals::reassign_overriding`].
    fn reassign(
        &self,
        run: &RunId,
        card: &CardId,
        to: WorkerRef,
    ) -> BoxFuture<'_, Result<(), ApiError>>;
    /// Past, parked, and crashed runs in this repository, newest first.
    fn list(&self) -> BoxFuture<'_, Result<Vec<RunSummary>, ApiError>>;
    /// A follow-up run that inherits the merged run's goal, decisions, and notes.
    fn continue_run(
        &self,
        merged: &RunId,
        request: StartRun,
    ) -> BoxFuture<'_, Result<RunHandle, ApiError>>;
    fn status(&self, run: &RunId) -> BoxFuture<'_, Result<StatusSummary, ApiError>>;
    fn diff(
        &self,
        run: &RunId,
        card: &CardId,
        scope: DiffScope,
    ) -> BoxFuture<'_, Result<DiffView, ApiError>>;
    fn raw(&self, run: &RunId, session: &SessionKey) -> BoxFuture<'_, Result<RawView, ApiError>>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointAnswer {
    pub id: CheckpointId,
    /// Must be one the card offered.
    pub action: AnswerAction,
    pub text: Option<String>,
    /// The evidence hash the card showed. The engine refuses a mismatch.
    pub evidence_sha256: String,
    /// For "request changes on some cards", the cards to send back. Empty means every card.
    #[serde(default)]
    pub cards: Vec<CardId>,
}

/// Everything that decides or raises autonomy. Held only by the interactive app, which refuses
/// to run inside an Argonauts agent session (`ARGONAUTS_SESSION` set) or without a terminal.
/// Every call carries the run's approval token, and the supervisor refuses a caller that
/// descends from an agent process.
pub trait Approvals: Send + Sync {
    /// With the run parked, appends the answer under the run lock and starts a supervisor.
    fn answer(&self, run: &RunId, answer: CheckpointAnswer) -> BoxFuture<'_, Result<(), ApiError>>;
    /// Hard limits and the questions that always reach you still apply.
    fn set_approval_mode(
        &self,
        run: &RunId,
        mode: ApprovalMode,
    ) -> BoxFuture<'_, Result<(), ApiError>>;
    /// Breaks one of the role's rules, such as the reviewer family rule, for this card only.
    /// The override is logged in `Card::Reassigned`.
    fn reassign_overriding(
        &self,
        run: &RunId,
        card: &CardId,
        to: WorkerRef,
        rule: String,
    ) -> BoxFuture<'_, Result<(), ApiError>>;
    /// Pauses the card, commits its work, and returns what to run in the terminal.
    fn take_over(
        &self,
        run: &RunId,
        card: &CardId,
    ) -> BoxFuture<'_, Result<TakeOverTicket, ApiError>>;
    /// Commits your changes and opens the return card.
    fn take_over_ended(&self, run: &RunId, card: &CardId) -> BoxFuture<'_, Result<(), ApiError>>;
}
