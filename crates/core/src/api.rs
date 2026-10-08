//! The app-facing conductor API: start, watch, and steer runs.
//!
//! Anything that decides or raises autonomy lives in [`Approvals`], which only the interactive
//! app holds, so no agent-facing surface can approve anything.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use tokio::sync::{mpsc, watch};

use crate::app::SettingRow;
use crate::events::RunEvent;
use crate::events::card::CardState;
use crate::events::checkpoint::AnswerAction;
use crate::events::run::{CheckedInstruction, RunMode, TriageDecision};
use crate::events::session::QueuedMessage;
use crate::events::vendor::RunBudget;
use crate::future::BoxFuture;
use crate::input::MentionTarget;
use crate::limits::QuotaReading;
use crate::names::{CheckpointId, DecisionId, MessageId, SessionKey, TurnId, WorkerRef};
use crate::required::RequiredStep;
use crate::settings::approvals::ApprovalMode;
use crate::settings::budget::BudgetAmount;
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
    /// The merged run this continues (`/follow-up`).
    pub continues: Option<RunId>,
    /// `--set key=value` overrides for this run.
    pub overrides: Vec<String>,
    /// Include uncommitted changes in the base.
    pub include_uncommitted: bool,
    pub main_agent: Option<WorkerRef>,
    /// `None` or `Supervised` only; `start` refuses anything more autonomous. Raising it goes
    /// through [`Approvals::set_approval_mode`], which needs the token.
    pub approval_mode: Option<ApprovalMode>,
    /// Set by the MCP server to the client's self-reported name. Such a run gets
    /// `mcp.run_limit` and counts against `mcp.runs_per_day`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub front_end: Option<String>,
    /// What the request said about how to run it: the ones followed and the ones not, each
    /// with its reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instructions: Vec<CheckedInstruction>,
    /// `key=value` settings at the request layer, for this run only. A safety key among them
    /// applies only when it tightens what the other layers say.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_settings: Vec<String>,
    /// Steps a setup kept for this conversation or this request alone. The run must contain
    /// them, as it must what `always.toml` and the request's own words require.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_steps: Vec<RequiredStep>,
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
    /// The run's own name once the lead or intake gave it one, such as `fix-slugify-median`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub mode: RunMode,
    pub workflow: String,
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

impl RunSummary {
    /// Not finished, and one of its cards waits on you.
    pub fn needs_you(&self) -> bool {
        self.is_open() && !self.waiting.is_empty()
    }

    /// Going, pausing, or parked: not finished, and not crashed.
    pub fn is_open(&self) -> bool {
        matches!(
            self.state,
            RunState::Running | RunState::Pausing | RunState::Parked
        )
    }
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

impl RunState {
    /// It finished, one way or another, so none of its sessions runs again unless you resume
    /// it. A parked or crashed run hasn't: resuming it starts its sessions where they were.
    pub fn ended(self) -> bool {
        matches!(
            self,
            Self::Merged
                | Self::PullRequest
                | Self::BranchLeft
                | Self::Stopped
                | Self::Failed
                | Self::Abandoned
        )
    }
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

/// The span the Usage tab adds up over. Quota is always now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsagePeriod {
    #[default]
    Today,
    /// Today and the 6 days before.
    Week,
    /// Today and the 29 days before.
    Month,
    All,
}

impl UsagePeriod {
    /// The first local day the period counts. `None` counts every day.
    pub fn first_day(self, today: Date) -> Option<Date> {
        let back = match self {
            Self::Today => 0,
            Self::Week => 6,
            Self::Month => 29,
            Self::All => return None,
        };
        Some(today.saturating_sub(time::Duration::days(back)))
    }
}

// A local day as `2026-10-04`.
time::serde::format_description!(pub day_format, Date, "[year]-[month]-[day]");

/// What the Usage tab shows for a period, from the usage ledger and the quota board, with no
/// model call.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageSummary {
    pub period: UsagePeriod,
    /// Everything the period spent. Its tokens are the tokens card: cached input, new input
    /// (input less cached), and output.
    pub total: Spend,
    /// Spend by agent and model, the largest first.
    pub by_agent: Vec<UsageShare>,
    /// Spend for each of the last 14 local days, oldest first, today last, whatever the period.
    pub history: Vec<DaySpend>,
    /// The period's runs, newest first.
    pub runs: Vec<RunSummary>,
    /// Every vendor's windows now, whatever the period.
    pub quotas: Vec<QuotaReading>,
    /// Each CLI in the crew, the main one first: its plan, its windows now, and whether it
    /// can be reached. A window that has reset since it was read is left out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plans: Vec<PlanUsage>,
    /// What this conversation spent, which no run counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<Spend>,
    /// The spend limit for a day, in dollars, when you set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend_limit: Option<f64>,
    /// The share of a plan window you let the crew use, 0 to 1, when you set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_limit: Option<f64>,
    /// How far the newest of `runs` got: their tasks and when they began.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub progress: Vec<RunProgress>,
    /// The limit of a task that has none of its own, when you set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_limit: Option<BudgetAmount>,
    /// The limits you can set, each with its value in effect and where that came from, in
    /// the order `/limits` lists them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<SettingRow>,
    /// What the crew did over the period, per CLI, model and role, for the Stats tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<crate::crew_stats::CrewStats>,
}

/// How far a run got, for the workflows card. It sits beside [`RunSummary`], which the run
/// list builds for every run without reading its tasks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunProgress {
    pub run: RunId,
    /// Tasks merged into the run, of those it still means to do or did.
    pub tasks_done: u32,
    pub tasks: u32,
    /// When it was asked for. With the summary's `updated` it says how long the run took.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub started: Option<OffsetDateTime>,
}

/// One CLI's plan usage, as the Usage tab and the plan usage card show it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PlanUsage {
    /// The CLI's id, such as `claude-code`, which picks its color.
    pub agent: String,
    /// Such as `Claude Code`.
    pub name: String,
    /// The plan as the CLI names it, such as `Max`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// The main agent runs on it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_use: bool,
    /// It can't be reached now.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub down: bool,
    /// It bills for each token, so there is no plan to track.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pays_per_token: bool,
    /// Its windows, the shortest first. Each says when it was read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<QuotaReading>,
    /// Who takes its work while it's out or down, by display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageShare {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub vendor: String,
    pub spent: Spend,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DaySpend {
    #[serde(with = "day_format")]
    pub day: Date,
    pub spent: Spend,
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
    #[error("run {0} is merged; /follow-up builds on it")]
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
    /// Delivers after the current tool call, or with `now` at once where the CLI can hand it to
    /// the model mid-call (Claude Code's send now).
    fn steer(
        &self,
        run: &RunId,
        session: &SessionKey,
        text: String,
        now: bool,
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
    /// Resets a card's branch to checkpoint `handoff-<checkpoint>`, keeping the work after it on
    /// a side branch. Merged work is refused; a new change undoes it instead.
    fn revert(
        &self,
        run: &RunId,
        card: &CardId,
        checkpoint: u32,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        let _ = (run, card, checkpoint);
        Box::pin(async {
            Err(ApiError::NotNow(
                "this conductor can't revert a task".to_owned(),
            ))
        })
    }
    /// Your verdict on an auto-approval from the spot-check queue. A wrong one drops that
    /// decision point back to shadow.
    fn spot_check(
        &self,
        run: &RunId,
        id: &CheckpointId,
        correct: bool,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        let _ = (run, id, correct);
        Box::pin(async {
            Err(ApiError::NotNow(
                "this conductor takes no spot checks".to_owned(),
            ))
        })
    }
    /// Clears the run's goal, turns its workflow into one, or changes an assumption.
    fn goal(
        &self,
        run: &RunId,
        action: crate::events::goal::GoalAction,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        let _ = (run, action);
        Box::pin(async {
            Err(ApiError::NotNow(
                "this conductor can't change a goal".to_owned(),
            ))
        })
    }
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

#[cfg(test)]
mod tests {
    use super::RunState;

    /// A run that can still start its sessions again hasn't ended: parked and crashed runs
    /// resume them where they were.
    #[test]
    fn only_a_finished_run_has_ended() {
        let going = [
            RunState::Running,
            RunState::Pausing,
            RunState::Parked,
            RunState::Crashed,
        ];
        assert!(going.iter().all(|state| !state.ended()));
        let finished = [
            RunState::Merged,
            RunState::PullRequest,
            RunState::BranchLeft,
            RunState::Stopped,
            RunState::Failed,
            RunState::Abandoned,
        ];
        assert!(finished.iter().all(|state| state.ended()));
    }
}
