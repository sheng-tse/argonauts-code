//! The engine's API for the app: start, watch, and steer runs.
//!
//! Anything that decides or raises autonomy lives in [`Approvals`], which only the interactive
//! app holds, so no agent-facing surface can approve anything.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use tokio::sync::{mpsc, watch};

use crate::app::SettingRow;
use crate::events::RunEvent;
use crate::events::agent_status::RunSpend;
use crate::events::approval::AnswerAction;
use crate::events::run::{CaptainOrCrewDecision, CheckedInstruction, RunMode};
use crate::events::session::QueuedMessage;
use crate::events::task::TaskState;
use crate::future::BoxFuture;
use crate::input::MentionTarget;
use crate::limits::PlanUsageReading;
use crate::names::{ApprovalId, DecisionId, MessageId, ModelRef, SessionKey, TurnId};
use crate::required::RequiredStep;
use crate::session_runner::SteerSupport;
use crate::settings::approvals::ApprovalMode;
use crate::settings::limit::LimitAmount;
use crate::settings::periods::WeekStart;
use crate::steps::intake::ChangeEstimate;
use crate::{CommandSpec, DiffStat, RunId, Spend, TaskId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartRun {
    pub request: String,
    pub mode: RunMode,
    /// Forced by `/captain`, `/crew`, `--captain`, or `--crew`.
    pub forced: bool,
    pub estimate: Option<ChangeEstimate>,
    pub captain_or_crew: Option<CaptainOrCrewDecision>,
    pub conversation_turn: Option<TurnId>,
    /// The merged run this continues (`/follow-up`).
    pub continues: Option<RunId>,
    /// `--set key=value` overrides for this run.
    pub overrides: Vec<String>,
    /// Include uncommitted changes in the base.
    pub include_uncommitted: bool,
    pub main_agent: Option<ModelRef>,
    /// `None` or `Supervised` only; `start` refuses anything more autonomous. Raising it goes
    /// through [`Approvals::set_approval_mode`], which needs the token.
    pub approval_mode: Option<ApprovalMode>,
    /// Set by the MCP server to the client's self-reported name. Such a run gets
    /// `mcp.run_limit` and counts against `mcp.runs_per_day`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outside_agent: Option<String>,
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
    /// The run's own name once the main agent or intake gave it one, such as `fix-slugify-median`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub mode: RunMode,
    pub workflow: String,
    pub state: RunState,
    /// Such as "plan approval, waiting for you".
    pub stopped_at: String,
    /// Last event time.
    #[serde(with = "time::serde::rfc3339")]
    pub updated: OffsetDateTime,
    pub spent: Spend,
    pub sessions_to_resume: u32,
    pub waiting: Vec<ApprovalId>,
    /// Whether a run host is running it now.
    pub live: bool,
    /// Waiting longer than `runs.remind_waiting_days`. Waiting runs never expire.
    #[serde(default)]
    pub remind: bool,
    /// Its workflow has its shape, so its row is drawn: see [`crate::views::shape_known`]. A
    /// summary from a build before this one says nothing of it, so it shows; only `false` is
    /// written.
    #[serde(default = "shape_was_known", skip_serializing_if = "is_known")]
    pub shape_known: bool,
}

fn shape_was_known() -> bool {
    true
}

fn is_known(shape_known: &bool) -> bool {
    *shape_known
}

impl RunSummary {
    /// Not finished, and one of its questions waits on you.
    pub fn needs_you(&self) -> bool {
        self.is_open() && !self.waiting.is_empty()
    }

    /// Going, pausing, or waiting: not finished, and not crashed.
    pub fn is_open(&self) -> bool {
        matches!(
            self.state,
            RunState::Running | RunState::Pausing | RunState::Waiting
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    Pausing,
    Waiting,
    /// Its run host died without waiting. `/resume` carries it on.
    Crashed,
    /// It was asked for, and its run host never wrote its first event.
    NeverStarted,
    /// Its journal was written by another Argonauts Code, in a format this one can't read, so
    /// it can't be opened or resumed. `/runs archive` moves it out of the way.
    FromAnotherBuild,
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
    /// it. A waiting or crashed run hasn't: resuming it starts its sessions where they were.
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
    /// Turns false when the run host exits; if the log's last event then isn't `Waiting` or
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
    pub task: TaskId,
    /// The agent's own UI on the same session, in the task's worktree. `None` for an endpoint,
    /// a CLI that can't open a session in its own screen, or a task with no session yet, which
    /// take-over then says rather than opening anything.
    pub command: Option<CommandSpec>,
    pub worktree: PathBuf,
    pub agent_id: String,
}

/// Built by code from the event log, with no model call.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StatusSummary {
    pub run: Option<RunId>,
    /// The approved goal's summary and criteria.
    pub goal: Vec<String>,
    pub plan: Option<String>,
    pub decisions: Vec<(DecisionId, String)>,
    pub tasks: Vec<TaskBrief>,
    /// Newest last.
    pub recent: Vec<String>,
    pub spend: Option<RunSpend>,
    pub plan_windows: Vec<PlanUsageReading>,
    pub waiting: Vec<String>,
}

/// The span the Usage, Limits and Stats tabs add up over. Plan usage is always now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsagePeriod {
    /// What the cards above the input ask for before any numbers came. A tab opens on
    /// [`UsagePeriod::OPENS_ON`] instead.
    #[default]
    Today,
    /// This calendar week, from the day weeks start on.
    Week,
    /// This month, from its 1st.
    Month,
    All,
}

impl UsagePeriod {
    /// The period every tab opens on, and `/usage` shows when it names none.
    pub const OPENS_ON: UsagePeriod = UsagePeriod::All;

    /// The first local day the period counts, a week starting on `week_starts`. `None`
    /// counts every day.
    pub fn first_day(self, today: Date, week_starts: time::Weekday) -> Option<Date> {
        match self {
            Self::Today => Some(today),
            Self::Week => {
                let back = (7 + today.weekday().number_days_from_sunday()
                    - week_starts.number_days_from_sunday())
                    % 7;
                Some(today.saturating_sub(time::Duration::days(i64::from(back))))
            }
            Self::Month => Some(today.replace_day(1).unwrap_or(today)),
            Self::All => None,
        }
    }
}

// A local day as `2026-10-04`.
time::serde::format_description!(pub day_format, Date, "[year]-[month]-[day]");

/// What the Usage tab shows for a period, from the usage ledger and the plan usage board, with no
/// model call.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageSummary {
    pub period: UsagePeriod,
    /// The day the period's weeks start on.
    #[serde(default, skip_serializing_if = "WeekStart::is_sunday")]
    pub week_starts: WeekStart,
    /// Everything the period spent. Its tokens are the tokens mod's: cached input, new input
    /// (input less cached), and output.
    pub total: Spend,
    /// Spend by agent and model, the largest first.
    pub by_agent: Vec<UsageShare>,
    /// Spend for each of the last 14 local days, oldest first, today last, whatever the period.
    pub history: Vec<DaySpend>,
    /// The period's runs, newest first.
    pub runs: Vec<RunSummary>,
    /// Every agent's windows now, whatever the period.
    pub plan_windows: Vec<PlanUsageReading>,
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
    pub task_limit: Option<LimitAmount>,
    /// The limits you can set, each with its value in effect and where that came from, in
    /// the order `/limits` lists them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<SettingRow>,
    /// What the crew did over the period, per CLI, model and role, for the Stats tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<crate::crew_stats::CrewStats>,
    /// What ran through Argonauts Code itself over the period, from the work ledger, for the Stats
    /// tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argonauts: Option<crate::overview::ThroughArgonauts>,
    /// Every model that has run here with its tokens in all, as the Overview has them, so a
    /// model draws in the same color on every tab.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all_time_models: Vec<crate::overview::ModelTokens>,
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
    /// Every model the crew runs on it runs on a server on this machine, so there is no plan
    /// to read.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local: bool,
    /// Its windows, the shortest first. Each says when it was read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<PlanUsageReading>,
    /// Who takes its work while it's out or down, by display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_to: Option<String>,
    /// The CLI's own live reading, for the account signed in, that `windows` come from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<crate::plan_usage::LiveUsage>,
    /// A live reading is on its way.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reading: bool,
    /// Why the last live read got no reading, such as the CLI's own error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
    /// The reading kept from before while it no longer stands as the CLI's own, because a
    /// read since failed or the CLI was signed in or out since. Drawn dim, as when it was read
    /// and for whom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<crate::plan_usage::LiveUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageShare {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub agent_id: String,
    pub spent: Spend,
    /// Its CLI bills for each token, so this was charged rather than spent inside a plan.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pays_per_token: bool,
    /// It ran on a server on this machine, which charges nothing and no plan covers.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local: bool,
    /// What it spent on each day of this week that it spent anything, oldest first, whatever
    /// the period.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub this_week: Vec<DaySpend>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DaySpend {
    #[serde(with = "day_format")]
    pub day: Date,
    pub spent: Spend,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskBrief {
    pub task: TaskId,
    pub title: String,
    pub state: TaskState,
    pub worker: Option<ModelRef>,
    /// Such as "6m 12s · 38k tok" or "needs task-002 and task-003".
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiffView {
    pub task: TaskId,
    pub stat: DiffStat,
    /// The unified diff, cut at 2 MB. The full one is at `path`.
    pub text: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffScope {
    /// The whole task against its base.
    Task,
    /// The last session's changes.
    LastTurn,
}

/// The agent's own transcript lines, unparsed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawView {
    pub session: SessionKey,
    /// Oldest first, cut to the last 5,000.
    pub lines: Vec<String>,
    pub truncated: bool,
    /// Argonauts Code's raw copy.
    pub path: PathBuf,
    pub cli_transcript: Option<PathBuf>,
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
    #[error("run host: {0}")]
    RunHost(String),
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
    /// Guidance from an MCP client, recorded as `Party::OutsideAgent { client }` so the log shows
    /// where it came from. It takes a client name rather than any `Party`, so a caller can't
    /// claim to be your rules or the judge.
    fn guidance_from_outside_agent(
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
    /// `Party::OutsideAgent { client }`.
    fn pause_from_outside_agent(
        &self,
        run: &RunId,
        now: bool,
        client: String,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        let _ = client;
        self.pause(run, now)
    }
    fn cancel_pause(&self, run: &RunId) -> BoxFuture<'_, Result<(), ApiError>>;
    /// Starts a run host that rebuilds from the log and resumes sessions.
    fn resume(&self, run: &RunId) -> BoxFuture<'_, Result<(), ApiError>>;
    /// Agents wrap up and report, work is committed, and the run ends as stopped.
    fn stop(&self, run: &RunId) -> BoxFuture<'_, Result<(), ApiError>>;
    /// The session's task goes to `NeedsDecision`.
    fn stop_session(
        &self,
        run: &RunId,
        session: &SessionKey,
    ) -> BoxFuture<'_, Result<(), ApiError>>;
    /// Stops a session that has no task and starts it over with the prompt it had. Whoever
    /// waits for its answer waits for the new one.
    fn restart_session(
        &self,
        run: &RunId,
        session: &SessionKey,
    ) -> BoxFuture<'_, Result<(), ApiError>>;
    /// Moves the task with its work saved and a fallback note, to a model the role's rules
    /// allow. Overriding a rule needs [`Approvals::reassign_overriding`].
    fn reassign(
        &self,
        run: &RunId,
        task: &TaskId,
        to: ModelRef,
    ) -> BoxFuture<'_, Result<(), ApiError>>;
    /// Past, waiting, and crashed runs in this repository, newest first.
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
        task: &TaskId,
        scope: DiffScope,
    ) -> BoxFuture<'_, Result<DiffView, ApiError>>;
    fn raw(&self, run: &RunId, session: &SessionKey) -> BoxFuture<'_, Result<RawView, ApiError>>;
    /// Resets a task's branch to checkpoint `fallback-<checkpoint>`, keeping the work after it on
    /// a side branch. Merged work is refused; a new change undoes it instead.
    fn rewind(
        &self,
        run: &RunId,
        task: &TaskId,
        checkpoint: u32,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        let _ = (run, task, checkpoint);
        Box::pin(async {
            Err(ApiError::NotNow(
                "Argonauts Code can't rewind a task".to_owned(),
            ))
        })
    }
    /// Your verdict on an auto-approval from the spot-check queue. A wrong one drops that
    /// decision point back to auto preview.
    fn spot_check(
        &self,
        run: &RunId,
        id: &ApprovalId,
        correct: bool,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        let _ = (run, id, correct);
        Box::pin(async {
            Err(ApiError::NotNow(
                "Argonauts Code takes no spot checks".to_owned(),
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
                "Argonauts Code can't change a goal".to_owned(),
            ))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalAnswer {
    pub id: ApprovalId,
    /// Must be one the question offered.
    pub action: AnswerAction,
    pub text: Option<String>,
    /// The evidence hash the task showed. The engine refuses a mismatch.
    pub evidence_sha256: String,
    /// For "request changes on some tasks", the tasks to send back. Empty means every task.
    #[serde(default)]
    pub tasks: Vec<TaskId>,
}

/// Everything that decides or raises autonomy. Held only by the interactive app, which refuses
/// to run inside an Argonauts Code agent session (`ARGONAUTS_SESSION` set) or without a terminal.
/// Every call carries the run's approval token, and the run host refuses a caller that
/// descends from an agent process.
pub trait Approvals: Send + Sync {
    /// With the run waiting, appends the answer under the run lock and starts a run host.
    fn answer(&self, run: &RunId, answer: ApprovalAnswer) -> BoxFuture<'_, Result<(), ApiError>>;
    /// Hard limits and the questions that always reach you still apply.
    fn set_approval_mode(
        &self,
        run: &RunId,
        mode: ApprovalMode,
    ) -> BoxFuture<'_, Result<(), ApiError>>;
    /// Breaks one of the role's rules, such as the reviewer provider rule, for this task only.
    /// The override is logged in `Task::Reassigned`.
    fn reassign_overriding(
        &self,
        run: &RunId,
        task: &TaskId,
        to: ModelRef,
        rule: String,
    ) -> BoxFuture<'_, Result<(), ApiError>>;
    /// Pauses the task, commits its work, and returns what to run in the terminal.
    fn take_over(
        &self,
        run: &RunId,
        task: &TaskId,
    ) -> BoxFuture<'_, Result<TakeOverTicket, ApiError>>;
    /// Commits your changes and opens the return question.
    fn take_over_ended(&self, run: &RunId, task: &TaskId) -> BoxFuture<'_, Result<(), ApiError>>;
}

#[cfg(test)]
mod tests {
    use super::{RunState, RunSummary};

    /// A summary writes that its workflow's shape isn't known only while it isn't, and one that
    /// says nothing of it, as an older build's does, has its shape.
    #[test]
    fn a_summary_says_its_shape_only_while_it_is_unknown() {
        let summary = RunSummary {
            run: crate::RunId::new("run-01").unwrap(),
            title: "Fix the money rounding".to_owned(),
            name: None,
            mode: crate::events::run::RunMode::Captain,
            workflow: String::new(),
            state: RunState::Running,
            stopped_at: String::new(),
            updated: time::OffsetDateTime::UNIX_EPOCH,
            spent: crate::Spend::default(),
            sessions_to_resume: 0,
            waiting: Vec::new(),
            live: true,
            remind: false,
            shape_known: false,
        };
        let written = serde_json::to_value(&summary).unwrap();
        assert_eq!(written["shape_known"], serde_json::json!(false));
        let read: RunSummary = serde_json::from_value(written).unwrap();
        assert!(!read.shape_known);

        let shaped = RunSummary {
            shape_known: true,
            ..summary
        };
        let written = serde_json::to_value(&shaped).unwrap();
        assert!(written.get("shape_known").is_none(), "{written}");
        let read: RunSummary = serde_json::from_value(written).unwrap();
        assert!(read.shape_known);
    }

    /// A run that can still start its sessions again hasn't ended: waiting and crashed runs
    /// resume them where they were.
    #[test]
    fn only_a_finished_run_has_ended() {
        let going = [
            RunState::Running,
            RunState::Pausing,
            RunState::Waiting,
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
