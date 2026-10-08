//! Cards, gates, reviews, and integration.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::run::Party;
use crate::handoff::QualityDebt;
use crate::names::WorkerRef;
use crate::policy::Violation;
use crate::settings::budget::BudgetAmount;
use crate::steps::card::{CardReport, Predictions};
use crate::steps::judgment::{AdjudicateOutput, ReviewOutput};
use crate::{CardId, CommitInfo, DiffStat};

/// One unit of work, owned by one agent session at a time, in its own worktree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardSpec {
    /// `card-001`, `test-002`, `spike-001`.
    pub id: CardId,
    pub title: String,
    pub task: String,
    pub kind: CardKind,
    /// Empty means the whole worktree (solo).
    pub owned_paths: Vec<String>,
    /// Such as frozen interfaces and its acceptance tests.
    pub read_only_paths: Vec<String>,
    /// On top of `limits.protected_paths` and `.argonauts/**`.
    pub forbidden_paths: Vec<String>,
    /// Not checked out at all (hidden tests, replays).
    pub hidden_paths: Vec<String>,
    /// Cards that must be integrated first.
    pub depends_on: Vec<CardId>,
    /// Goal criteria it serves.
    pub criteria: Vec<String>,
    pub owner: Option<WorkerRef>,
    /// Each from another model family than the owner.
    pub reviewers: Vec<WorkerRef>,
    pub budget: CardBudget,
    pub predictions: Option<Predictions>,
    /// The one-token name its labels use, such as `slugify` in `fix:slugify` and
    /// `review:slugify`: the planner's key, or the lead's item for a card built from the goal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CardKind {
    Solo,
    /// Interfaces with stubs that compile and fail "not implemented".
    Interface,
    /// Written by the reviewer of the card they test.
    AcceptanceTests {
        for_card: CardId,
    },
    Build,
    /// Integration or cleanup work.
    Chore,
    Docs,
    /// A time-boxed experiment in a sandbox that builds one option of a disputed topic.
    Spike {
        topic: String,
        option: String,
    },
}

/// `None` fields fall back to settings.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CardBudget {
    pub limit: Option<BudgetAmount>,
    /// Wall-clock minutes.
    pub max_minutes: Option<f64>,
    pub max_tool_calls: Option<u64>,
    /// Idle minutes before the inactivity watchdog fires.
    pub inactivity_min: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardState {
    /// Dependencies not integrated yet.
    Waiting,
    /// Can start when an owner, a slot, and budget are free.
    Ready,
    Running,
    /// Waiting on an answer to a question.
    Blocked,
    /// Budget hit, vendor stop, or change request. Settings or you decide.
    NeedsDecision,
    Gating,
    InReview,
    /// Merging into the integration branch and running affected tests.
    Integrating,
    Integrated,
    /// Crashed, stuck, looping, or gates failed after the resume.
    Failed,
    /// Out of attempts. You or your rules decide.
    Escalated,
    /// Waiting for run budget.
    Deferred,
    Cancelled,
    TakenOver,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CardEvent {
    Created(Box<CardSpec>),
    State {
        /// `None` for the first state.
        from: Option<CardState>,
        to: CardState,
        reason: String,
    },
    Attempt {
        /// 1-based.
        attempt: u32,
        worker: WorkerRef,
        cause: AttemptCause,
    },
    Worktree {
        path: PathBuf,
        /// `argonauts/<run>/<card>`.
        branch: String,
        base: String,
    },
    /// The conductor commits the worktree after each session and on any stop.
    Committed(CommitInfo),
    Reported(Box<CardReport>),
    Diff {
        stat: DiffStat,
        path: PathBuf,
    },
    TakenOver {
        /// Set when it opened the vendor's UI on the same session.
        command: Option<Vec<String>>,
    },
    TakeOverEnded {
        changed: DiffStat,
        commit: Option<String>,
    },
    Reassigned {
        from: Option<WorkerRef>,
        to: WorkerRef,
        by: Party,
        /// A role rule you relaxed for this card.
        rule_override: Option<String>,
    },
    /// The card's committed state was tagged `handoff-<n>` as another worker took over.
    CheckpointTagged {
        n: u32,
        commit: String,
        from: WorkerRef,
        /// Several for a race.
        to: Vec<WorkerRef>,
        /// One line, such as "usage limit, resets at 3:45 PM".
        reason: String,
    },
    /// A replacement weaker than the original committed work since a checkpoint.
    QualityDebt(QualityDebt),
    /// A review by the original, or by a model at least as strong, cleared the debt.
    DebtCleared {
        by: WorkerRef,
    },
    /// `/rewind`: the card's branch was reset to checkpoint `to`, and the work after it kept.
    Reverted {
        to: u32,
        /// The side branch that holds the replaced work.
        kept_on: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptCause {
    First,
    /// Same session, resumed with the gate output.
    GateRetry,
    /// Same session, with the reviewer's requested changes.
    ReviewChanges,
    /// Same session, with what the check of the task's goal found still failing.
    GoalRetry,
    /// Fresh session on another agent after a failure (the retry ladder).
    AfterFailure,
    /// Usage limit, outage, or login.
    Handoff,
    /// `/reassign`.
    Reassigned,
    /// Same agent, fresh session, after the context filled.
    Rotation,
    IntegrationFailed,
    /// After a pause, park, or crash.
    Resumed,
}

/// Command gates run in the card's sandbox; built-in checks run in code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateOutcome {
    pub name: String,
    pub kind: GateKind,
    pub passed: bool,
    pub command: Option<Vec<String>>,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    /// One line, such as "41 passed, 1 failed".
    pub summary: String,
    pub output_tail: String,
    pub violations: Vec<Violation>,
    /// Whether it ran inside the OS sandbox.
    pub sandboxed: bool,
    /// Nothing ran, as when `gates.run` is empty. Nothing failed either, so `passed` holds,
    /// but no rule that needs a suite to have run counts it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    Command,
    /// No protected or forbidden path touched.
    ProtectedPaths,
    SecretScan,
    /// No deleted tests, skip markers, or test config changes.
    TestTamper,
    /// New dependencies only from the allowlist; no lockfile edits.
    Dependencies,
    /// Every acceptance test fails against the interface stubs.
    StubCheck,
    /// Affected tests after a merge into the integration branch.
    AffectedTests,
    /// The full suite in the final check.
    FullSuite,
    /// A done report's changed files are on the card's branch: an owner that changed nothing
    /// says so instead of naming files.
    ReportedChanges,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ReviewEvent {
    Reviewed {
        reviewer: WorkerRef,
        /// Blind to other reviewers.
        blind: bool,
        /// 1-based.
        round: u32,
        review: ReviewOutput,
    },
    Adjudicated {
        adjudicator: WorkerRef,
        ruling: AdjudicateOutput,
    },
    /// A reviewer's acceptance tests landed on a test card.
    TestsWritten {
        reviewer: WorkerRef,
        for_card: CardId,
        files: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum IntegrationEvent {
    BranchCreated {
        /// `argonauts/<run>/integration`.
        branch: String,
        base: String,
    },
    Merged {
        card: CardId,
        sha: String,
    },
    /// Assigned to the interface's owner.
    Conflict {
        card: CardId,
        files: Vec<String>,
        assigned_to: WorkerRef,
    },
    /// `main` moved since the run started.
    MainMoved {
        from: String,
        to: String,
        commits: u32,
    },
    MainMergedIn {
        sha: String,
    },
    FinalCheck {
        /// The full suite and end-to-end checks.
        passed: bool,
    },
    Landed {
        target: String,
        sha: String,
        /// Whether a branch of yours moved to `sha`. False when the run's work was only
        /// written into your files.
        fast_forward: bool,
    },
    /// The merge into your checkout waits on your files, which git won't overwrite: untracked
    /// ones where the run's files go, and uncommitted edits to files it changes. A folder ends
    /// in `/`. Empty when git refused for a reason of its own.
    WaitingToMerge {
        target: String,
        in_the_way: Vec<String>,
    },
    /// A pull request was opened (`url`), or only the command to open one was suggested.
    PullRequest {
        branch: String,
        url: Option<String>,
        command: Option<Vec<String>>,
    },
    /// Branches are kept.
    Abandoned {
        branches: Vec<String>,
    },
    /// The plan's new packages, added with install scripts and hooks off before any build card
    /// started. Cards branch from here.
    DependenciesAdded {
        packages: Vec<String>,
        /// The manifest and lockfile change.
        sha: String,
    },
}
