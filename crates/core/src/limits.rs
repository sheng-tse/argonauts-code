//! Plan usage, agent health, recovery after a stop, fallbacks, and checks, all decided in code.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use tokio::sync::{broadcast, watch};

use crate::events::run::WaitReason;
use crate::events::session::{EndReason, SessionEnd};
use crate::events::task::{CheckOutcome, TaskSpec, TaskState};
use crate::events::workflow::WorkflowEvent;
use crate::events::{AgentStatusEvent, RunEvent, RunEventKind};
use crate::fallback::CheckpointTag;
use crate::future::BoxFuture;
use crate::names::{AgentId, ModelRef, RoleName, SessionKey};
use crate::session_runner::{SandboxProfile, SessionLimits};
use crate::settings::fallback::{FallbackEntry, FallbackStrategy, Pick};
use crate::settings::limit::{LimitAmount, OnHit};
use crate::steps::StepError;
use crate::steps::task::TaskReport;
use crate::{ErrorCategory, RetryReason, RunId, TaskId, TodoItem};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanUsageReading {
    pub agent_id: AgentId,
    pub status: PlanUsageStatus,
    /// Share of the current window used, 0 to 1, where the agent reports it.
    pub used: Option<f64>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub resets_at: Option<OffsetDateTime>,
    /// Such as `five_hour` or `weekly`.
    pub window: Option<String>,
    pub source: PlanUsageSource,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    /// The agent's payload, for the raw view.
    pub detail: Option<Value>,
    /// Out of plan usage, but the running session finishes on a small allowance, as Claude Code
    /// does at a five-hour limit. Nothing falls back, and new work goes elsewhere.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wrap_up: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanUsageStatus {
    Allowed,
    Warning,
    Rejected,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanUsageSource {
    /// Claude Code's `rate_limit_event`.
    RateLimitEvent,
    /// `agy -p "/usage" --output-format json`, which spends nothing.
    UsageProbe,
    /// A usage-limit error that says when to retry (Codex exec).
    ErrorMessage,
    /// The Codex app server's rate-limit read.
    AppServer,
    /// An endpoint's HTTP headers or errors.
    Http,
    /// Claude Code's `get_usage` control request, asked before any message.
    GetUsage,
}

/// What a plan usage probe read: every window the agent reports, and its plan when it names one.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanUsageAnswer {
    pub windows: Vec<PlanUsageReading>,
    pub plan: Option<String>,
    /// The whole reading with the account it was read for, where the CLI names one.
    pub live: Option<crate::plan_usage::LiveUsage>,
    /// The CLI's own words when it answered with an error instead of its usage.
    pub failed: Option<String>,
}

impl PlanUsageAnswer {
    /// An error in place of a reading. It names no account, so it leaves every reading kept.
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            windows: Vec::new(),
            plan: None,
            live: None,
            failed: Some(message.into()),
        }
    }

    /// The window that binds soonest, which is what one number per agent shows.
    pub fn binding(&self) -> Option<&PlanUsageReading> {
        binding_reading(&self.windows)
    }
}

/// An agent's readings, the newest of each window. A reading that names no window speaks for
/// the agent as a whole, so it replaces every window, and the next windowed one replaces it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlanWindows(Vec<PlanUsageReading>);

impl PlanWindows {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn readings(&self) -> &[PlanUsageReading] {
        &self.0
    }

    /// Keeps `reading` unless its window has a newer one. Says whether it was kept.
    pub fn observe(&mut self, reading: PlanUsageReading) -> bool {
        let newer = self
            .0
            .iter()
            .all(|known| !same_window(known, &reading) || known.observed_at <= reading.observed_at);
        if newer {
            self.put(reading);
        }
        newer
    }

    /// Keeps `reading` whatever its age.
    pub fn put(&mut self, reading: PlanUsageReading) {
        if reading.window.is_none() {
            self.0.clear();
        } else {
            self.0
                .retain(|known| known.window.is_some() && known.window != reading.window);
        }
        self.0.push(reading);
    }

    /// The window that binds soonest, as [`binding_reading`] picks it.
    pub fn binding(&self) -> Option<&PlanUsageReading> {
        binding_reading(&self.0)
    }
}

fn same_window(a: &PlanUsageReading, b: &PlanUsageReading) -> bool {
    a.window.is_none() || b.window.is_none() || a.window == b.window
}

/// The reading that binds soonest: one that refused work, else the fullest. Between two as
/// full, the later reset is the one you'd wait for, then the newer reading.
pub fn binding_reading(readings: &[PlanUsageReading]) -> Option<&PlanUsageReading> {
    readings.iter().max_by(|a, b| {
        let refused = |r: &PlanUsageReading| r.status == PlanUsageStatus::Rejected;
        let used = |r: &PlanUsageReading| r.used.unwrap_or(-1.0);
        refused(a)
            .cmp(&refused(b))
            .then(used(a).total_cmp(&used(b)))
            .then(a.resets_at.cmp(&b.resets_at))
            .then(a.observed_at.cmp(&b.observed_at))
    })
}

/// Circuit-breaker states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Healthy,
    /// New tasks go elsewhere when another agent fits.
    Degraded,
    /// Its tasks pause with work committed; new tasks go elsewhere.
    Down,
    /// One test task goes to it.
    Testing,
}

/// Why an agent reads healthy again after its test task succeeded: the one way back from an
/// outage that a check stands behind, where errors that stopped coming back say less.
pub const TEST_TASK_SUCCEEDED: &str = "the test task succeeded";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentHealth {
    pub agent_id: AgentId,
    pub state: HealthState,
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
    pub reason: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub next_test_at: Option<OffsetDateTime>,
    /// Why it's disabled for new tasks, such as an untested CLI version.
    pub disabled: Option<String>,
}

/// One model's own state on its CLI. An overload or a server error is about the model it came
/// from, so the CLI's other models stay usable while one is out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelState {
    Healthy,
    /// New work goes to another model first.
    Degraded,
    /// Out: a fallback takes its work, or the work waits for it.
    Out,
    /// One test task decides whether it's healthy or out again.
    Testing,
}

/// What took a model out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trouble {
    Overload,
    ServerErrors,
}

impl Trouble {
    /// The trouble an error of `category` is for the model it came from. `None` for an error
    /// that says nothing about one model, such as the network or the account.
    pub fn of(category: ErrorCategory) -> Option<Self> {
        match category {
            ErrorCategory::Overloaded => Some(Self::Overload),
            ErrorCategory::Server => Some(Self::ServerErrors),
            _ => None,
        }
    }

    /// `overloaded`, `server errors`: what the model is said to have.
    pub fn words(self) -> &'static str {
        match self {
            Self::Overload => "overloaded",
            Self::ServerErrors => "server errors",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelHealth {
    pub model: ModelRef,
    pub state: ModelState,
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
    pub reason: Option<String>,
    /// What took it out, kept while it's out or under test.
    pub trouble: Option<Trouble>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub next_test_at: Option<OffsetDateTime>,
}

impl ModelHealth {
    /// A model nothing was ever said against.
    pub fn healthy(model: &ModelRef) -> Self {
        Self {
            model: model.clone(),
            state: ModelState::Healthy,
            since: OffsetDateTime::UNIX_EPOCH,
            reason: None,
            trouble: None,
            next_test_at: None,
        }
    }

    /// Whether a fallback may go to it. A model under test takes only work asked of it, as its
    /// one test task, so a batch of stopped work can't all land on it.
    pub fn takes_fallbacks(&self) -> bool {
        matches!(self.state, ModelState::Healthy | ModelState::Degraded)
    }

    /// Whether work asked of it may start on it: anything but out.
    pub fn takes_its_own_work(&self) -> bool {
        self.state != ModelState::Out
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum HealthSignal {
    Retry {
        agent_id: AgentId,
        /// The model the session runs, `None` for the agent's default.
        model: Option<String>,
        session: SessionKey,
        reason: RetryReason,
    },
    Error {
        agent_id: AgentId,
        model: Option<String>,
        session: SessionKey,
        category: ErrorCategory,
    },
    Success {
        agent_id: AgentId,
        model: Option<String>,
        session: SessionKey,
    },
    /// Unknown events piled up or the startup check failed.
    AdapterBroken { agent_id: AgentId, detail: String },
}

/// Whether the internet answers at all, as this process last found it. While it's down no
/// agent is blamed for the network, and sessions that need a provider wait for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetState {
    #[default]
    Up,
    /// No provider answered, since `since`.
    Down { since: OffsetDateTime },
}

impl NetState {
    pub fn is_down(self) -> bool {
        matches!(self, Self::Down { .. })
    }
}

/// Said in the conversation and under each workflow's header when the internet goes.
pub const NETWORK_DOWN: &str =
    "The network is down · sessions wait for it, nothing moves to another agent";
/// Said when it's back.
pub const NETWORK_BACK: &str = "The network is back · carrying on";

/// What a run's log says of the internet at `kind`: `Some(true)` where it went, `Some(false)`
/// where it's back, `None` anywhere else. Each change is a note under the workflows' headers.
pub fn network_down_in(kind: &RunEventKind) -> Option<bool> {
    let RunEventKind::Workflow(WorkflowEvent::Note(note)) = kind else {
        return None;
    };
    match note.text.as_str() {
        NETWORK_DOWN => Some(true),
        NETWORK_BACK => Some(false),
        _ => None,
    }
}

/// What a workflow's row says of a session the internet has held for `held`:
/// `waiting for the network · 2m 10s`.
pub fn waiting_for_the_network(held: Duration) -> String {
    let seconds = held.as_secs();
    let held = match (seconds / 3600, seconds / 60 % 60, seconds % 60) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, seconds) => format!("{minutes}m {seconds}s"),
        (hours, minutes, _) => format!("{hours}h {minutes}m"),
    };
    format!("waiting for the network · {held}")
}

/// Whether a session that ended for `reason` may have lost the internet: a network error, a
/// timeout, or a crash, as an offline CLI's failure often reads.
pub fn may_have_lost_the_network(reason: &EndReason) -> bool {
    match reason {
        EndReason::CliError { category, .. } => {
            matches!(category, ErrorCategory::Network | ErrorCategory::Timeout)
        }
        EndReason::Crashed { .. } => true,
        _ => false,
    }
}

/// Whether the internet took a step's session with it: the session may have lost the network,
/// and `network` is down. The main agent's own turn then fails with its CLI's reason, as Claude
/// Code's does once its retries run out, and goes to no other agent, which would meet the same
/// network.
pub fn lost_to_the_network(err: &StepError, network: NetState) -> bool {
    network.is_down()
        && matches!(err, StepError::Session(end) if may_have_lost_the_network(&end.reason))
}

/// Whether a session of `agent` on `model` runs on a server on this machine, which works
/// without the internet: a local runtime as its agent, or a model routed to one, as OpenCode's
/// `ollama/qwen3-coder:30b` is.
pub fn served_on_this_machine(agent: &str, model: Option<&str>) -> bool {
    crate::runtimes::runtime(agent).is_some()
        || model.and_then(crate::runtimes::local_route).is_some()
}

/// Plan usage across every run and the app, persisted in `~/.argonauts/agents.json`.
pub trait PlanUsageBoard: Send + Sync {
    /// Keeps the reading as its agent's for its window.
    fn observe(&self, reading: PlanUsageReading);
    /// The agent's window that binds soonest, for what wants one number per agent: the
    /// fallback, the planner, and the bottom line's chip.
    fn get(&self, agent_id: &AgentId) -> Option<PlanUsageReading>;
    /// One reading per agent, as `get` gives it.
    fn all(&self) -> Vec<PlanUsageReading>;
    /// Every window the agent reported, one reading each.
    fn windows(&self, agent_id: &AgentId) -> Vec<PlanUsageReading> {
        self.get(agent_id).into_iter().collect()
    }
    /// The account's plan, as the CLI names it.
    fn plan(&self, agent_id: &AgentId) -> Option<String> {
        let _ = agent_id;
        None
    }
    fn observe_plan(&self, agent_id: &AgentId, plan: String) {
        let _ = (agent_id, plan);
    }
    /// Keeps a live reading as its account's. A reading of another account than the last
    /// one's means the CLI was signed in again, and only the new account's numbers show.
    fn observe_live(&self, reading: crate::plan_usage::LiveUsage) {
        let _ = reading;
    }
    /// The CLI's newest live reading, for the account it was last signed in as. What shows
    /// it checks [`LiveUsage::shown_at`](crate::plan_usage::LiveUsage::shown_at) first.
    fn live(&self, agent_id: &AgentId) -> Option<crate::plan_usage::LiveUsage> {
        let _ = agent_id;
        None
    }
    /// Why this process's last live read of the CLI got no reading, in words for the screen.
    /// `None` once a reading lands.
    fn failed(&self, agent_id: &AgentId) -> Option<String> {
        let _ = agent_id;
        None
    }
    /// When this process last had cause to doubt the CLI's live reading: a read that failed,
    /// or a sign-in or sign-out since. A reading from before then may be another account's,
    /// so it no longer stands as the CLI's own.
    fn doubted_at(&self, agent_id: &AgentId) -> Option<OffsetDateTime> {
        let _ = agent_id;
        None
    }
    /// The CLI was signed in or out, so its live reading may be another account's until it
    /// is read again.
    fn doubt(&self, agent_id: &AgentId) {
        let _ = agent_id;
    }
    /// Reads plan usage without spending any, where the agent allows it (agy `/usage`).
    fn probe(&self, agent_id: &AgentId) -> BoxFuture<'_, Option<PlanUsageReading>>;
}

/// Agent health and the circuit breaker, shared like [`PlanUsageBoard`].
pub trait HealthBoard: Send + Sync {
    fn observe(&self, signal: HealthSignal);
    fn state(&self, agent_id: &AgentId) -> AgentHealth;
    fn all(&self) -> Vec<AgentHealth>;
    fn subscribe(&self) -> broadcast::Receiver<AgentStatusEvent>;
    fn test_result(&self, agent_id: &AgentId, ok: bool);
    /// One model's own health. A board that keeps none per model says healthy.
    fn model(&self, model: &ModelRef) -> ModelHealth {
        ModelHealth::healthy(model)
    }
    /// Every model of the agent the board has heard anything against, by name.
    fn models(&self, agent_id: &AgentId) -> Vec<ModelHealth> {
        let _ = agent_id;
        Vec::new()
    }
    /// Whether the internet answers. A board that never checks says it does.
    fn network(&self) -> NetState {
        NetState::Up
    }
    /// Each change of [`HealthBoard::network`]. A board that never checks sends none, so a
    /// wait on it ends at once.
    fn network_changes(&self) -> watch::Receiver<NetState> {
        watch::channel(self.network()).1
    }
    /// The internet as the latest check found it, once the check under way, if any, has said,
    /// for a session that just ended on a network error the check may still be asking about.
    fn check_network(&self) -> BoxFuture<'_, NetState> {
        let state = self.network();
        Box::pin(async move { state })
    }
}

/// Whether a fallback may go to `model` as far as health goes: its CLI is up and the model
/// itself isn't out or under test.
pub fn takes_fallbacks(board: &dyn HealthBoard, model: &ModelRef) -> bool {
    let cli = board.state(&model.agent_id()).state;
    matches!(cli, HealthState::Healthy | HealthState::Degraded)
        && board.model(model).takes_fallbacks()
}

#[derive(Debug, Clone)]
pub struct StopContext {
    pub run: RunId,
    pub task: TaskSpec,
    pub state: TaskState,
    pub role: RoleName,
    pub worker: ModelRef,
    pub end: SessionEnd,
    /// Oldest first, one line each.
    pub tries: Vec<String>,
    pub reassigned_once: bool,
    pub events: PathBuf,
    /// What the stopped agent's CLI says of its login, asked after a sign-in error: `Some(false)`
    /// when it says it's logged out, the only time a login is the fix.
    pub logged_in: Option<bool>,
}

/// What the engine does after a session stops for any reason but a clean finish.
#[derive(Debug, Clone, PartialEq)]
pub enum RecoveryAction {
    /// Treat the result as final.
    Proceed,
    ResumeSame {
        /// Such as the check output or a nudge.
        message: String,
    },
    /// Same model, fresh session, with a fallback note (context full).
    Rotate { note: Box<FallbackNote> },
    /// Another model chosen by the rules (fallback mode `auto`, retry ladder).
    Fallback {
        to: ModelRef,
        note: Box<FallbackNote>,
        /// Tag the committed work as this before the fallback starts.
        checkpoint: Option<CheckpointTag>,
    },
    /// Several models at once, each in its own worktree, sharing the task's remaining limit.
    /// `pick` says how their results combine.
    Race {
        to: Vec<ModelRef>,
        pick: Pick,
        note: Box<FallbackNote>,
        checkpoint: Option<CheckpointTag>,
    },
    /// Open a fallback approval.
    Ask(Box<crate::events::agent_status::FallbackPlan>),
    /// Keep the task in `NeedsDecision` until the agent is back.
    Wait {
        agent_id: AgentId,
        until: Option<OffsetDateTime>,
        /// The CLI that would have taken it, where `fallback.same_cli` is why it waits.
        kept_off: Option<AgentId>,
    },
    /// Apply `limits.when_reached`.
    LimitHit(OnHit),
    /// Out of tries.
    AskYou { reason: String },
    /// Nothing can continue anywhere.
    RunWaits(WaitReason),
}

/// The CLI-neutral note a new agent gets. Built by code from the log, at no token cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FallbackNote {
    pub task: TaskSpec,
    pub goal_sha256: Option<String>,
    pub notes: Vec<String>,
    pub work: WorkSoFar,
    /// Files read and edited, commands run and their results, one line each.
    pub happened: Vec<String>,
    pub last_todo: Vec<TodoItem>,
    pub checks: Vec<CheckOutcome>,
    pub why_stopped: String,
    /// The previous agent's own report, if it wrote one.
    pub last_words: Option<TaskReport>,
    /// What the stopped session said last. Without a report, it is often the only word on
    /// where the work stands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    pub limit_left: Option<LimitAmount>,
    /// The old session as Markdown, written for `note+transcript` and `note+summary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<TranscriptPaths>,
    /// The summarizer's summary of that transcript, for `note+summary`. It goes into the note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Where a fallback's transcript was written. Code builds it from the task's events in the
/// run's log, so it reads the same for every agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptPaths {
    /// `<task worktree>/.argonauts/fallback/<n>/transcript.md`, the copy the note names. Every
    /// CLI can read it without extra flags, and the worktree's git exclude file keeps it
    /// out of commits.
    pub worktree: PathBuf,
    /// `runs/<run>/tasks/<task>/fallback-<n>/transcript.md`, the run's own worktree, kept for you.
    pub run: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkSoFar {
    /// `argonauts/<run>/<task>`.
    pub branch: String,
    pub commit: Option<String>,
    /// Against the task's base.
    pub stat: crate::DiffStat,
}

pub trait FallbackPlanner: Send + Sync {
    /// Valid candidates, best first: the role's rules (provider, blindness), agent health, and
    /// plan usage left. Each carries its plan usage, track record, and cost estimate.
    fn candidates(&self, query: &CandidateQuery) -> Vec<Candidate>;
    /// Built from that task's own events (the engine filters them), so its cost stays small
    /// however long the run is.
    fn note(&self, task: &TaskSpec, events: &[RunEvent]) -> FallbackNote;

    /// The candidates from the most specific fallback list for the task's role and the
    /// stopped agent, with the strategy and race count that apply, both from
    /// [`FallbackTable::applied`](crate::settings::fallback::FallbackTable::applied).
    fn shortlist(&self, query: &CandidateQuery) -> Shortlist {
        Shortlist {
            strategy: FallbackStrategy::First,
            race_count: 1,
            pick: Pick::default_for(query.role),
            candidates: self.candidates(query),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shortlist {
    pub strategy: FallbackStrategy,
    /// How many fallbacks take the task at once (`together`).
    pub race_count: u32,
    /// How their results combine.
    pub pick: Pick,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateQuery {
    pub task: TaskSpec,
    pub role: RoleName,
    pub stopped: ModelRef,
    /// Such as the worker's for a reviewer.
    pub exclude_providers: Vec<String>,
    pub fallback: Vec<FallbackEntry>,
    /// List every valid model, not only the fallback list.
    pub all_valid: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub worker: ModelRef,
    /// 0 to 1, where known.
    pub plan_usage_left: Option<f64>,
    /// Such as "62%", or "API key".
    pub plan_usage_text: String,
    pub track_record: Option<TrackRecord>,
    pub cost: CostEstimate,
    /// Moving to a paid API needs your OK.
    pub needs_confirm: bool,
    pub may_run_out: bool,
}

/// A track record on similar tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackRecord {
    pub passed: u32,
    pub of: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "unit", content = "amount", rename_all = "snake_case")]
pub enum CostEstimate {
    Usd(f64),
    /// Share of a subscription window.
    PlanUsage(f64),
    Free,
    Unknown,
}

/// Derived from the outcome log.
pub trait TrackRecords: Send + Sync {
    fn get(&self, worker: &ModelRef, task_type: &str) -> Option<TrackRecord>;
}

pub trait LimitsService: Send + Sync {
    fn session_limits(&self, task: Option<&TaskSpec>, role: RoleName) -> SessionLimits;
    fn after_stop<'a>(&'a self, context: &'a StopContext) -> BoxFuture<'a, RecoveryAction>;
    /// Whether every candidate for the remaining work is down or out of plan usage.
    fn all_out(&self, workers: &[ModelRef]) -> Option<WaitReason>;
}

/// What the checks run on the starting code go by in a request's and an event's `task`, in a
/// task's place: they are the run's own, made before any task exists.
pub const START_CHECKS: &str = "start";

/// The environment is an allowlist: `PATH`, `LANG`, `TERM`, `TMPDIR`, `HOME` set to a temp
/// directory, and `CI=1`. No agent key, and nothing named `*_KEY`, `*_TOKEN`, `*_SECRET`, or
/// `*_PASSWORD`, reaches a check, since tests an agent edited can print their environment.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckRequest {
    pub run: RunId,
    pub task: TaskId,
    pub name: String,
    /// An argument vector from protected config.
    pub command: Vec<String>,
    pub workdir: PathBuf,
    pub sandbox: SandboxProfile,
    pub timeout: Duration,
    /// A `checks.setup` command: runs once per worktree before the first check, outside the
    /// agent's sandbox, with network for installs from the lockfile and install scripts off
    /// where the tool allows it.
    pub setup: bool,
}

/// Fails closed when the sandbox is missing.
pub trait CheckRunner: Send + Sync {
    fn run(&self, request: CheckRequest) -> BoxFuture<'_, CheckOutcome>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn reading(window: Option<&str>, used: f64, minute: u8) -> PlanUsageReading {
        PlanUsageReading {
            agent_id: AgentId("codex".into()),
            status: PlanUsageStatus::Allowed,
            used: Some(used),
            resets_at: None,
            window: window.map(str::to_owned),
            source: PlanUsageSource::AppServer,
            observed_at: datetime!(2026-10-04 12:00 UTC) + time::Duration::minutes(minute.into()),
            detail: None,
            wrap_up: false,
        }
    }

    fn windows_of(windows: &PlanWindows) -> Vec<(Option<&str>, Option<f64>)> {
        windows
            .readings()
            .iter()
            .map(|r| (r.window.as_deref(), r.used))
            .collect()
    }

    /// A board that keeps nothing and never checks the internet.
    struct Forgetful;

    impl HealthBoard for Forgetful {
        fn observe(&self, _: HealthSignal) {}
        fn state(&self, agent_id: &AgentId) -> AgentHealth {
            AgentHealth {
                agent_id: agent_id.clone(),
                state: HealthState::Healthy,
                since: OffsetDateTime::UNIX_EPOCH,
                reason: None,
                next_test_at: None,
                disabled: None,
            }
        }
        fn all(&self) -> Vec<AgentHealth> {
            Vec::new()
        }
        fn subscribe(&self) -> broadcast::Receiver<AgentStatusEvent> {
            broadcast::channel(1).1
        }
        fn test_result(&self, _: &AgentId, _: bool) {}
    }

    #[tokio::test]
    async fn a_board_that_never_checks_says_the_internet_is_there() {
        let board = Forgetful;
        assert_eq!(board.network(), NetState::Up);
        assert_eq!(board.check_network().await, NetState::Up);
        // Nothing will ever change, so a wait for a change ends at once.
        assert!(board.network_changes().changed().await.is_err());
    }

    #[test]
    fn only_a_session_the_network_ended_while_it_is_down_was_lost_to_it() {
        let ended = |reason| {
            StepError::Session(Box::new(SessionEnd {
                cli_session: None,
                exit_code: Some(1),
                signal: None,
                duration_ms: 180_000,
                reason,
                spend: crate::Spend::default(),
                tool_calls: 0,
                result_text: None,
                structured: None,
                last_message: None,
                raw_log: None,
            }))
        };
        let unreachable = || {
            ended(EndReason::CliError {
                category: ErrorCategory::Network,
                message: "API Error: Unable to connect to API. Check your internet connection"
                    .to_owned(),
            })
        };
        let down = NetState::Down {
            since: OffsetDateTime::UNIX_EPOCH,
        };
        assert!(lost_to_the_network(&unreachable(), down));
        assert!(!lost_to_the_network(&unreachable(), NetState::Up));
        let limited = ended(EndReason::RateLimited);
        assert!(!lost_to_the_network(&limited, down));
    }

    #[test]
    fn a_runs_log_says_when_the_network_went_and_came_back() {
        use crate::events::workflow::RunNote;
        let said = |text: &str| {
            network_down_in(&RunEventKind::Workflow(WorkflowEvent::Note(RunNote {
                text: text.to_owned(),
                workflow: None,
                left: false,
            })))
        };
        assert_eq!(said(NETWORK_DOWN), Some(true));
        assert_eq!(said(NETWORK_BACK), Some(false));
        assert_eq!(said("Gemma4 E2B is up next"), None);
        let elsewhere = RunEventKind::Note(crate::Note {
            level: crate::NoteLevel::Info,
            text: NETWORK_DOWN.to_owned(),
        });
        assert_eq!(network_down_in(&elsewhere), None);
    }

    #[test]
    fn a_held_row_says_how_long_the_network_has_held_it() {
        let held = |seconds| waiting_for_the_network(Duration::from_secs(seconds));
        assert_eq!(held(9), "waiting for the network · 9s");
        assert_eq!(held(130), "waiting for the network · 2m 10s");
        assert_eq!(held(3 * 3600 + 125), "waiting for the network · 3h 2m");
    }

    #[test]
    fn each_window_keeps_its_newest_reading() {
        let mut windows = PlanWindows::default();
        assert!(windows.observe(reading(Some("five_hour"), 0.5, 1)));
        assert!(windows.observe(reading(Some("weekly"), 0.4, 1)));
        assert!(windows.observe(reading(Some("five_hour"), 0.6, 2)));
        assert!(!windows.observe(reading(Some("five_hour"), 0.1, 0)));
        assert_eq!(
            windows_of(&windows),
            [(Some("weekly"), Some(0.4)), (Some("five_hour"), Some(0.6))]
        );
        assert_eq!(windows.binding().unwrap().used, Some(0.6));
    }

    #[test]
    fn a_reading_with_no_window_speaks_for_the_whole_agent() {
        let mut windows = PlanWindows::default();
        windows.observe(reading(Some("five_hour"), 0.5, 1));
        windows.observe(reading(Some("weekly"), 0.4, 1));
        windows.observe(reading(None, 1.0, 2));
        assert_eq!(windows_of(&windows), [(None, Some(1.0))]);
        windows.observe(reading(Some("weekly"), 0.2, 3));
        assert_eq!(windows_of(&windows), [(Some("weekly"), Some(0.2))]);
    }

    #[test]
    fn a_refused_window_binds_before_a_fuller_one() {
        let mut refused = reading(Some("weekly"), 0.3, 1);
        refused.status = PlanUsageStatus::Rejected;
        let readings = [reading(Some("five_hour"), 0.9, 1), refused];
        assert_eq!(
            binding_reading(&readings).unwrap().window.as_deref(),
            Some("weekly")
        );
    }
}
