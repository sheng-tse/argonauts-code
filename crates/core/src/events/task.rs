//! Tasks, checks, reviews, and merge.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::run::Party;
use super::session::InputPart;
use crate::fallback::QualityDebt;
use crate::names::ModelRef;
use crate::policy::Violation;
use crate::settings::limit::LimitAmount;
use crate::steps::judgment::{ReviewOutput, SettleOutput};
use crate::steps::task::{Predictions, TaskReport};
use crate::{CommitInfo, DiffStat, TaskId};

/// One unit of work, owned by one agent session at a time, in its own worktree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSpec {
    /// `task-001`, `test-002`, `experiment-001`.
    pub id: TaskId,
    pub title: String,
    pub task: String,
    pub kind: TaskKind,
    /// Empty means the whole worktree (Captain).
    pub owned_paths: Vec<String>,
    /// Such as frozen interfaces and its acceptance tests.
    pub read_only_paths: Vec<String>,
    /// On top of `limits.protected_paths` and `.argonauts/**`.
    pub forbidden_paths: Vec<String>,
    /// Not checked out at all (hidden tests, replays).
    pub hidden_paths: Vec<String>,
    /// Tasks that must be merged first.
    pub depends_on: Vec<TaskId>,
    /// Goal criteria it serves.
    pub criteria: Vec<String>,
    pub worker: Option<ModelRef>,
    /// Each from another model provider than the worker.
    pub reviewers: Vec<ModelRef>,
    pub limit: TaskLimit,
    pub predictions: Option<Predictions>,
    /// The one-token name its labels use, such as `slugify` in `fix:slugify` and
    /// `review:slugify`: the planner's key, or the main agent's item for a task built from the
    /// goal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskKind {
    Captain,
    /// Interfaces with stubs that compile and fail "not implemented".
    Interface,
    /// Written by the reviewer of the task they test.
    AcceptanceTests {
        for_task: TaskId,
    },
    Build,
    /// Merge or cleanup work.
    Chore,
    Docs,
    /// A time-boxed experiment in a sandbox that builds one option of a disputed topic.
    Experiment {
        topic: String,
        option: String,
    },
}

/// `None` fields fall back to settings.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TaskLimit {
    pub limit: Option<LimitAmount>,
    /// Wall-clock minutes.
    pub max_minutes: Option<f64>,
    pub max_tool_calls: Option<u64>,
    /// Idle minutes before the inactivity watchdog fires.
    pub inactivity_min: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Dependencies not merged yet.
    Waiting,
    /// Can start when a worker, a slot, and limit are free.
    Ready,
    Running,
    /// Waiting on an answer to a question.
    Blocked,
    /// Limit hit, agent stop, or change request. Settings or you decide.
    NeedsDecision,
    Checking,
    InReview,
    /// Merging into the run branch and running affected tests.
    Merging,
    Merged,
    /// Crashed, stuck, looping, or checks failed after the resume.
    Failed,
    /// Out of tries. You or your rules decide.
    NeedsYou,
    /// Waiting for run limit.
    Deferred,
    Cancelled,
    TakenOver,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum TaskEvent {
    Created(Box<TaskSpec>),
    State {
        /// `None` for the first state.
        from: Option<TaskState>,
        to: TaskState,
        reason: String,
    },
    Try {
        /// 1-based.
        try_number: u32,
        worker: ModelRef,
        cause: TryCause,
        /// What a try on the task's own session is told, such as the check's output or the
        /// review's comments, sent again when the run restarts before its session began. Older
        /// logs have none, and such a try is told to continue.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        told: Vec<InputPart>,
    },
    Worktree {
        path: PathBuf,
        /// `argonauts/<run>/<task>`.
        branch: String,
        base: String,
    },
    /// Argonauts Code commits the worktree after each session and on any stop.
    Committed(CommitInfo),
    Reported(Box<TaskReport>),
    Diff {
        stat: DiffStat,
        path: PathBuf,
    },
    TakenOver {
        /// Set when it opened the agent's UI on the same session.
        command: Option<Vec<String>>,
    },
    TakeOverEnded {
        changed: DiffStat,
        commit: Option<String>,
    },
    Reassigned {
        from: Option<ModelRef>,
        to: ModelRef,
        by: Party,
        /// A role rule you relaxed for this task.
        rule_override: Option<String>,
    },
    /// The task's committed state was tagged `fallback-<n>` as it fell back to another model.
    CheckpointTagged {
        n: u32,
        commit: String,
        from: ModelRef,
        /// Several for a race.
        to: Vec<ModelRef>,
        /// One line, such as "usage limit, resets at 3:45 PM".
        reason: String,
    },
    /// A fallback weaker than the original committed work since a checkpoint.
    QualityDebt(QualityDebt),
    /// A review by the original, or by a model at least as strong, cleared the debt.
    DebtCleared {
        by: ModelRef,
    },
    /// `/rewind`: the task's branch was reset to checkpoint `to`, and the work after it kept.
    Rewound {
        to: u32,
        /// The side branch that holds the replaced work.
        kept_on: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TryCause {
    First,
    /// Same session, resumed with the check output.
    CheckRetry,
    /// Same session, with the reviewer's requested changes.
    ReviewChanges,
    /// Same session, with what the check of the task's goal found still failing.
    GoalRetry,
    /// Fresh session on another agent after a failure (the retry ladder).
    AfterFailure,
    /// Usage limit, outage, or login.
    Fallback,
    /// `/reassign`.
    Reassigned,
    /// You started it over with the worker it has: a session of its own, which carries no
    /// earlier one on, with a note of the work so far.
    Restarted,
    /// Same agent, fresh session, after the context filled or the CLI lost the session.
    Rotation,
    MergeFailed,
    /// After a pause, wait, or crash.
    Resumed,
}

/// Command checks run in the task's sandbox; built-in checks run in code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub name: String,
    pub kind: CheckKind,
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
    /// Nothing ran, as when `checks.run` is empty. Nothing failed either, so `passed` holds,
    /// but no rule that needs a suite to have run counts it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
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
    /// Affected tests after a merge into the run branch.
    AffectedTests,
    /// The full suite in the final check.
    FullSuite,
    /// A done report's changed files are on the task's branch: a worker that changed nothing
    /// says so instead of naming files.
    ReportedChanges,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ReviewEvent {
    Reviewed {
        reviewer: ModelRef,
        /// Blind to other reviewers.
        blind: bool,
        /// 1-based.
        round: u32,
        review: ReviewOutput,
    },
    /// The judge settled reviewers who disagreed.
    Settled {
        judge: ModelRef,
        ruling: SettleOutput,
    },
    /// A reviewer's acceptance tests merged on a test task.
    TestsWritten {
        reviewer: ModelRef,
        for_task: TaskId,
        files: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum MergeEvent {
    BranchCreated {
        /// `argonauts/<run>/merged`.
        branch: String,
        base: String,
    },
    Merged {
        task: TaskId,
        sha: String,
    },
    /// Assigned to the interface's worker.
    Conflict {
        task: TaskId,
        files: Vec<String>,
        assigned_to: ModelRef,
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
    RunMerged {
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
    /// The plan's new packages, added with install scripts and hooks off before any build task
    /// started. Tasks branch from here.
    DependenciesAdded {
        packages: Vec<String>,
        /// The manifest and lockfile change.
        sha: String,
    },
}
