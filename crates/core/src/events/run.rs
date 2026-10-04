//! Run lifecycle and phases.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::names::{CheckpointId, SessionKey, TurnId, VendorId, WorkerRef};
use crate::settings::approvals::ApprovalMode;
use crate::settings::protocol::SoloStyle;
use crate::settings::workflows::RunStyle;
use crate::steps::intake::{ChangeEstimate, InstructionKind};
use crate::{CardId, PhaseStatus, RunId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RunLifecycle {
    /// The first event of every run.
    Started(Box<RunStarted>),
    /// The run's name and description, from the lead's brief, or from intake for a solo run
    /// or a template that runs as written. Every view shows these, never a template's name.
    Named(RunNamed),
    /// A new supervisor rebuilt the run from the log and carried on.
    Resumed(RunResumed),
    /// Esc at the top level. Cancelled by Enter until the safe point is reached. A second Esc
    /// sends `now`: sessions stop at once and their work is committed.
    PauseRequested {
        by: Party,
        /// Stop now instead of at the next safe point.
        now: bool,
    },
    /// Shift+Tab changed the approval mode for every decision point of this run.
    ApprovalModeChanged {
        mode: ApprovalMode,
        /// Always you.
        by: Party,
    },
    /// You pressed Enter before the safe point.
    PauseCancelled,
    /// The safe point was reached: no new sessions, running ones stopped after their tool call,
    /// worktrees committed. A `Parked` event follows.
    Paused {
        sessions_stopped: u32,
        committed: Vec<CardId>,
    },
    /// `/stop`: agents wrap up and report, work is committed, the run ends as stopped.
    StopRequested { by: Party },
    /// The supervisor exited with nothing running and nothing spending.
    Parked {
        reason: ParkReason,
        /// Checkpoints waiting for you, if that's the reason.
        waiting_on: Vec<CheckpointId>,
    },
    /// Everything a resume or a reattach needs, so it can start here instead of at `seq` 0.
    /// The engine writes one every `supervisor.snapshot_every` events.
    Snapshot(Box<RunSnapshot>),
    /// How the run ends in the conversation, as an agent's own CLI ends a task: what changed,
    /// how it was checked, and what's left. Just before `Finished`.
    ClosingAnswer(Box<ClosingAnswer>),
    /// The last event of a run, unless it ended `Stopped` and you reopened it with `/resume`,
    /// when a `Resumed` follows.
    Finished {
        outcome: RunOutcome,
        detail: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunNamed {
    /// Two to four task words joined with `-`, such as `fix-slugify-median`.
    pub name: String,
    /// One line, the task in plain words. In a goal run, the objective.
    pub description: String,
    pub style: RunStyle,
    /// The example the workflow started from, shown once at the end of the start item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub based_on: Option<String>,
    /// The lead's done-when list, which the final check judges. Empty when the lead wrote
    /// none, and then the approved goal's criteria are judged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub done_when: Vec<crate::events::goal::Criterion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSnapshot {
    /// The last event this snapshot covers.
    pub upto: u64,
    /// The engine's own state, versioned and read only by the engine. It holds everything
    /// replay would rebuild, including the id counters, so ids never repeat.
    pub engine: Value,
    /// For views that redraw without replaying.
    pub summary: crate::api::StatusSummary,
    pub open_checkpoints: Vec<crate::events::checkpoint::CheckpointOpened>,
    pub running: Vec<SessionKey>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunStarted {
    /// Exactly what you typed.
    pub request: String,
    pub title: String,
    pub mode: RunMode,
    /// Whether `/solo`, `/crew`, `--solo`, or `--crew` forced the mode.
    pub forced: bool,
    pub preset: String,
    /// The default agent's estimate, unless the mode was forced.
    pub estimate: Option<ChangeEstimate>,
    pub triage: Option<TriageDecision>,
    pub chat_turn: Option<TurnId>,
    /// The merged run this one continues (`/follow-up`).
    pub continues: Option<RunId>,
    /// Absolute path of the repository.
    pub repo_root: PathBuf,
    pub base_branch: Option<String>,
    pub base_commit: String,
    /// Uncommitted paths left out (the default), or included when you asked.
    pub uncommitted: Vec<String>,
    pub uncommitted_included: bool,
    pub settings_sha256: String,
    pub settings: Value,
    /// Every `--set` and where each key came from, for the ablation record.
    pub overrides: Vec<OverrideRecord>,
    /// Keyed by adapter id.
    pub harness_versions: BTreeMap<String, String>,
    /// Argonauts version.
    pub version: String,
    /// The MCP client that asked for the run, by the name it reported for itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub front_end: Option<String>,
    /// What the request said about how to run it, and what came of each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instructions: Vec<CheckedInstruction>,
}

/// One instruction from a request, after code checked it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckedInstruction {
    /// The request's own words for it.
    pub says: String,
    pub kind: InstructionKind,
    #[serde(flatten)]
    pub outcome: InstructionOutcome,
}

impl CheckedInstruction {
    pub fn followed(&self) -> bool {
        matches!(self.outcome, InstructionOutcome::Followed { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum InstructionOutcome {
    /// Applied, as the Triage line says it: "Codex coordinates". `note` says what of it couldn't
    /// be, such as "Codex has no model named opus".
    Followed {
        said: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Not followed, and why, as the Triage line says it.
    NotFollowed { why: String },
    /// Its words aren't in your message, so the model wrote it, not you. Only Ctrl+O lists it.
    Dropped,
}

/// A run's last word to you, in the main agent's voice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClosingAnswer {
    /// The agent's own words: a solo owner's answer, or the lead's from the cards' reports.
    /// Code writes it when no agent could.
    pub text: String,
    /// Who wrote `text`. `None` when code did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<WorkerRef>,
    /// What changed, with lines added and removed. The run's own records are left out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ChangedFile>,
    /// How it was checked: the gates Argonauts ran, each with its result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckLine>,
    /// The tests the agents say they ran, in their words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<String>,
    /// Anything left or worth knowing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub insertions: u32,
    pub deletions: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckLine {
    /// The gate's name.
    pub what: String,
    pub passed: bool,
    /// Such as "6 passed". Empty when there's nothing more to say.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub result: String,
}

/// One setting that differs from the built-in default, and the layer that set it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverrideRecord {
    /// Dotted key.
    pub key: String,
    /// Value as TOML text.
    pub value: String,
    /// `global`, `project`, `preset`, or `flag`.
    pub from: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    /// One card for the default agent.
    Solo,
    /// Goal check, planning, dividing, card pipelines.
    Crew,
}

/// The triage rules' answer, computed by code from the estimate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriageDecision {
    pub mode: RunMode,
    /// Solo first, then crew if the solo card escalates.
    pub escalate_to_crew: bool,
    /// The rule that fired, such as `solo_max_files` or `recipe:api-feature`.
    pub rule: String,
    /// One line for you, such as "at most 3 files, and tests exist". Empty when your words
    /// chose the mode.
    pub reason: String,
    /// Where a solo run works: in your checkout, or checked in a copy. `None` for the crew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solo: Option<SoloStyle>,
}

impl TriageDecision {
    /// The rule that fires when your words chose the mode.
    pub const AS_YOU_ASKED: &'static str = "request";

    /// Your words chose the mode, not the rules.
    pub fn as_you_asked(&self) -> bool {
        self.rule == Self::AS_YOU_ASKED
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunResumed {
    pub cause: ResumeCause,
    pub settings_changed: bool,
    pub harness_versions_changed: Vec<String>,
    /// Commits `main` moved by, if it moved. The final check runs again before the merge.
    pub main_moved_by: Option<u32>,
    pub sessions: Vec<ResumedSession>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeCause {
    /// Enter or `/resume`.
    User,
    /// Your answer to a checkpoint.
    Answer,
    /// A vendor came back, or the time a parked run waited for passed.
    VendorBack,
    /// A handoff checkpoint's `ask_timeout` passed, so the role's fallback applies.
    AskTimeout,
    Crash,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResumedSession {
    /// The new session key.
    pub session: SessionKey,
    pub card: Option<CardId>,
    /// The vendor session id, when resumed by id.
    pub vendor_session: Option<String>,
    /// Resumed with every launch flag passed again, or restarted fresh with a handoff packet.
    pub resumed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "why", rename_all = "snake_case")]
pub enum ParkReason {
    /// A checkpoint waits for you and no session is running.
    Checkpoint,
    Paused,
    /// Every vendor is down or out of quota. It resumes when one returns.
    AllVendorsOut {
        /// The earliest known reset.
        #[serde(with = "time::serde::rfc3339::option")]
        until: Option<OffsetDateTime>,
    },
    /// Its remaining cards wait for one vendor (handoff mode `wait`).
    WaitingForVendor {
        vendor: VendorId,
        #[serde(with = "time::serde::rfc3339::option")]
        until: Option<OffsetDateTime>,
    },
    /// A step's agent stopped for a vendor reason, and no other agent could take its role.
    NoStandIn {
        /// Which role, in words: `nobody could take over as the lead`.
        detail: String,
        /// When it tries again: the agent's reset, else a retry after a wait that grows with
        /// each try. `None` once it stopped trying on its own.
        #[serde(with = "time::serde::rfc3339::option")]
        until: Option<OffsetDateTime>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Merged,
    /// A pull request was opened or suggested.
    PullRequest,
    /// Branches kept for later.
    BranchLeft,
    /// You abandoned it at the merge checkpoint.
    Abandoned,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "party", rename_all = "snake_case")]
pub enum Party {
    /// You, interactively.
    User,
    /// Your approval rules.
    Rules,
    Approver {
        model: WorkerRef,
    },
    /// A setting, such as `merge.mode = "leave"`.
    Config,
    /// Full auto, in your place: nothing waits for you there, so what your rules and the
    /// approver couldn't settle takes its safe option.
    FullAuto,
    /// By a fixed rule.
    Conductor,
    Agent {
        session: SessionKey,
        worker: WorkerRef,
    },
    /// An MCP client such as a chat front end, by the name it reported for itself. It can guide
    /// and pause, never approve.
    FrontEnd {
        client: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseChange {
    pub phase: RunPhase,
    pub status: PhaseStatus,
    /// The one-line summary the collapsed step shows, such as "3 agents restated the goal".
    pub summary: Option<String>,
}

/// Phases of a run. Solo runs use Goal, Build, FinalCheck, Merge, Retro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    /// Restatements, differs, questions, the goal checkpoint.
    Goal,
    /// Drafts, map, critique, revision, spikes, judging.
    Planning,
    /// Task graph, owners, review ring, predictions, budget lines, plan checkpoint.
    Dividing,
    /// Interface card and acceptance-test cards.
    Contracts,
    /// Card pipelines.
    Build,
    /// Bring in main, full suite, end-to-end checks.
    FinalCheck,
    /// The merge checkpoint and landing.
    Merge,
    /// Predictions scored, memory, recipe candidate.
    Retro,
}
