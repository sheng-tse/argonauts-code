//! Every task: its spec, state, tries, worktree, commits, reports, checks, reviews, fallbacks,
//! checkpoint tags, and quality debt, plus the pipeline's own bookkeeping in `progress`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use argonauts_core::events::RunEvent;
use argonauts_core::events::agent_status::FallbackEvent;
use argonauts_core::events::run::Party;
use argonauts_core::events::task::{
    CheckOutcome, ReviewEvent, TaskEvent, TaskSpec, TaskState, TryCause,
};
use argonauts_core::fallback::QualityDebt;
use argonauts_core::limits::START_CHECKS;
use argonauts_core::names::{AgentId, ModelRef, SessionKey};
use argonauts_core::steps::task::TaskReport;
use argonauts_core::{CommitInfo, DiffStat, TaskId};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tasks {
    pub by_id: BTreeMap<TaskId, TaskEntry>,
    /// What the pipeline keeps per task beyond the record. A task without an entry has none.
    #[serde(default)]
    pub progress: BTreeMap<TaskId, TaskProgress>,
    /// The checks as they ran on the code the run started from, before anything changed.
    #[serde(default)]
    pub at_start: Vec<CheckOutcome>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskEntry {
    pub spec: TaskSpec,
    pub state: TaskState,
    /// Tries started, from 1.
    pub tries: u32,
    pub worker: Option<ModelRef>,
    pub reassigned: u32,
    pub worktree: Option<PathBuf>,
    pub branch: Option<String>,
    pub base: Option<String>,
    pub commits: Vec<CommitInfo>,
    pub reports: Vec<TaskReport>,
    pub checks: Vec<CheckOutcome>,
    pub reviews: Vec<ReviewEvent>,
    /// The `n` of the newest `fallback-<n>` tag.
    pub fallbacks: u32,
    pub debt: Option<QualityDebt>,
}

/// The pipeline's bookkeeping for one task, folded from the same events as its record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskProgress {
    pub last_cause: Option<TryCause>,
    /// `Task::Try` events so far; each one starts exactly one worker session.
    pub try_events: u32,
    /// The seq of the newest `Task::Try`, to tell whether its session has started.
    pub try_seq: Option<u64>,
    /// Deferred because the run's limit ran low, not by you, so a raised limit brings it back.
    pub deferred_for_limit: bool,
    /// Check retries by the current worker, which the ladder allows once. A pause, a resume
    /// after a stop, and a context rotation keep the count.
    pub check_retries: u32,
    /// Fresh sessions on another agent after a failure, which the ladder allows once. Fallbacks
    /// for limits or usage limits, give-backs, races, and your reassignments don't count.
    #[serde(default)]
    pub ladder_retries: u32,
    /// Times the current worker got the task back from merge.
    #[serde(default)]
    pub merge_returns: u32,
    /// Tries granted after the task needed you, each one a move from `NeedsYou` back to
    /// `Ready`. The stuck rules cap these at `max_extra_tries`.
    #[serde(default)]
    pub extra_tries: u32,
    /// Review rounds that asked for changes.
    pub change_rounds: u32,
    /// The newest saved diff against the base.
    pub diff: Option<(DiffStat, PathBuf)>,
    /// The newest fallback note written for this task.
    pub note: Option<PathBuf>,
    /// Who had the task before its newest reassignment. That agent's fallback rules say what
    /// the new worker is told.
    #[serde(default)]
    pub reassigned_from: Option<ModelRef>,
    /// Set while the task waits for its agent to come back.
    pub waiting_for: Option<AgentId>,
    /// Times it waited for its agent, which makes each next wait with no reset to wait for
    /// longer.
    #[serde(default)]
    pub agent_waits: u32,
    /// What your take-over changed and its commit, once you're done with it, until the task
    /// is taken over again.
    pub take_over_end: Option<(DiffStat, Option<String>)>,
    /// Events folded for this task, so a waiting task is looked at again whenever it changes.
    pub changes: u64,
    /// Why the task moved to its current state, as the move said.
    #[serde(default)]
    pub why: String,
    /// The session each of the record's reviews came from, in their order, so asks that
    /// outlive a relaunch are counted again from the log.
    #[serde(default)]
    pub review_sessions: Vec<Option<SessionKey>>,
}

/// Progress of a task nothing has happened to yet.
static NO_PROGRESS: TaskProgress = TaskProgress {
    last_cause: None,
    try_events: 0,
    try_seq: None,
    deferred_for_limit: false,
    check_retries: 0,
    ladder_retries: 0,
    merge_returns: 0,
    extra_tries: 0,
    change_rounds: 0,
    diff: None,
    note: None,
    reassigned_from: None,
    waiting_for: None,
    agent_waits: 0,
    take_over_end: None,
    changes: 0,
    why: String::new(),
    review_sessions: Vec::new(),
};

impl TaskEntry {
    pub fn new(spec: TaskSpec) -> Self {
        Self {
            worker: spec.worker.clone(),
            spec,
            state: TaskState::Waiting,
            tries: 0,
            reassigned: 0,
            worktree: None,
            branch: None,
            base: None,
            commits: Vec::new(),
            reports: Vec::new(),
            checks: Vec::new(),
            reviews: Vec::new(),
            fallbacks: 0,
            debt: None,
        }
    }

    /// Whether this commit is already recorded.
    pub fn has_commit(&self, sha: &str) -> bool {
        self.commits
            .iter()
            .any(|commit| commit.sha.as_deref() == Some(sha))
    }

    pub fn newest_report(&self) -> Option<&TaskReport> {
        self.reports.last()
    }
}

impl Tasks {
    pub fn apply(&mut self, event: &TaskEvent, envelope: &RunEvent) {
        if let TaskEvent::Created(spec) = event {
            let id = spec.id.clone();
            self.by_id
                .entry(id.clone())
                .and_modify(|record| record.spec = (**spec).clone())
                .or_insert_with(|| TaskEntry::new((**spec).clone()));
            self.progress.entry(id).or_default().changes += 1;
            return;
        }
        let Some(task) = envelope.task_id.as_ref() else {
            return;
        };
        let Some(record) = self.by_id.get_mut(task) else {
            return;
        };
        let progress = self.progress.entry(task.clone()).or_default();
        progress.changes += 1;
        match event {
            TaskEvent::Created(_) => {}
            TaskEvent::State { from, to, reason } => {
                record.state = *to;
                reason.clone_into(&mut progress.why);
                progress.deferred_for_limit =
                    *to == TaskState::Deferred && *from == Some(TaskState::Ready);
                if *from == Some(TaskState::Merging) && *to == TaskState::Running {
                    progress.merge_returns += 1;
                }
                if *from == Some(TaskState::NeedsYou) && *to == TaskState::Ready {
                    progress.extra_tries += 1;
                }
            }
            TaskEvent::Try {
                try_number,
                worker,
                cause,
            } => {
                record.tries = record.tries.max(*try_number);
                record.worker = Some(worker.clone());
                progress.last_cause = Some(*cause);
                progress.waiting_for = None;
                match cause {
                    TryCause::CheckRetry => progress.check_retries += 1,
                    TryCause::ReviewChanges => {
                        progress.change_rounds += 1;
                        progress.check_retries = 0;
                    }
                    TryCause::MergeFailed | TryCause::Rotation | TryCause::Resumed => {}
                    // A new try at the task's goal gets its own check retry.
                    TryCause::First
                    | TryCause::GoalRetry
                    | TryCause::AfterFailure
                    | TryCause::Fallback
                    | TryCause::Reassigned
                    | TryCause::Restarted => progress.check_retries = 0,
                }
                progress.try_events += 1;
                progress.try_seq = Some(envelope.seq);
            }
            TaskEvent::Worktree { path, branch, base } => {
                record.worktree = Some(path.clone());
                record.branch = Some(branch.clone());
                record.base = Some(base.clone());
            }
            TaskEvent::Committed(commit) => {
                let known = commit
                    .sha
                    .as_deref()
                    .is_some_and(|sha| record.has_commit(sha));
                if !known {
                    record.commits.push(commit.clone());
                }
            }
            TaskEvent::Reported(report) => record.reports.push((**report).clone()),
            TaskEvent::Diff { stat, path } => progress.diff = Some((*stat, path.clone())),
            TaskEvent::TakenOver { .. } => {
                progress.take_over_end = None;
                progress.merge_returns = 0;
            }
            TaskEvent::TakeOverEnded { changed, commit } => {
                progress.take_over_end = Some((*changed, commit.clone()));
            }
            TaskEvent::Reassigned { from, to, by, .. } => {
                // Only Argonauts Code or your rules move a failed task on their own, and that
                // is the ladder's retry.
                if record.state == TaskState::Failed
                    && matches!(by, Party::ArgonautsCode | Party::Rules)
                {
                    progress.ladder_retries += 1;
                }
                progress.reassigned_from = from
                    .clone()
                    .or_else(|| record.worker.clone())
                    .or_else(|| record.spec.worker.clone());
                record.worker = Some(to.clone());
                record.reassigned += 1;
                progress.check_retries = 0;
                progress.merge_returns = 0;
            }
            TaskEvent::CheckpointTagged { n, .. } => record.fallbacks = record.fallbacks.max(*n),
            TaskEvent::QualityDebt(debt) => record.debt = Some(debt.clone()),
            TaskEvent::DebtCleared { .. } => record.debt = None,
            TaskEvent::Rewound { to, .. } => {
                // Work after checkpoint `to` is gone from the branch, and so is its debt.
                if record.debt.as_ref().is_some_and(|debt| debt.since >= *to) {
                    record.debt = None;
                }
            }
        }
    }

    pub fn apply_check(&mut self, event: &CheckOutcome, envelope: &RunEvent) {
        let scope = envelope.task_id.as_ref().map(TaskId::as_str);
        if scope == Some(START_CHECKS) {
            self.at_start.push(event.clone());
            return;
        }
        if let Some(record) = self.record_mut(envelope) {
            record.checks.push(event.clone());
            self.touch(envelope);
        }
    }

    pub fn apply_review(&mut self, event: &ReviewEvent, envelope: &RunEvent) {
        if let Some(record) = self.record_mut(envelope) {
            record.reviews.push(event.clone());
            self.touch(envelope);
            if let Some(task) = envelope.task_id.as_ref() {
                self.progress
                    .entry(task.clone())
                    .or_default()
                    .review_sessions
                    .push(envelope.session.clone());
            }
        }
    }

    pub fn apply_fallback(&mut self, event: &FallbackEvent, _envelope: &RunEvent) {
        let task = match event {
            FallbackEvent::Needed(plan) => &plan.task,
            FallbackEvent::Note { task, .. }
            | FallbackEvent::FellBack { task, .. }
            | FallbackEvent::Waiting { task, .. } => task,
            // A job that isn't the task's build leaves the task as it is.
            FallbackEvent::JobWaiting(_) | FallbackEvent::JobFreed { .. } => return,
        };
        let Some(record) = self.by_id.get_mut(task) else {
            return;
        };
        let progress = self.progress.entry(task.clone()).or_default();
        progress.changes += 1;
        match event {
            FallbackEvent::Needed(_)
            | FallbackEvent::JobWaiting(_)
            | FallbackEvent::JobFreed { .. } => {}
            FallbackEvent::Note { path, .. } => progress.note = Some(path.clone()),
            FallbackEvent::FellBack { to, .. } => {
                record.worker = Some(to.clone());
                progress.waiting_for = None;
            }
            FallbackEvent::Waiting { agent_id, .. } => {
                progress.waiting_for = Some(agent_id.clone());
                progress.agent_waits += 1;
            }
        }
    }

    pub fn get(&self, task: &TaskId) -> Option<&TaskEntry> {
        self.by_id.get(task)
    }

    /// The pipeline's bookkeeping for `task`, empty when nothing has happened to it yet.
    pub fn progress(&self, task: &TaskId) -> &TaskProgress {
        self.progress.get(task).unwrap_or(&NO_PROGRESS)
    }

    /// Tasks in `state`, in id order.
    pub fn in_state(&self, state: TaskState) -> Vec<TaskId> {
        self.by_id
            .values()
            .filter(|record| record.state == state)
            .map(|record| record.spec.id.clone())
            .collect()
    }

    fn record_mut(&mut self, envelope: &RunEvent) -> Option<&mut TaskEntry> {
        let task = envelope.task_id.as_ref()?;
        self.by_id.get_mut(task)
    }

    fn touch(&mut self, envelope: &RunEvent) {
        if let Some(task) = envelope.task_id.as_ref() {
            self.progress.entry(task.clone()).or_default().changes += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::RunEventKind;
    use argonauts_core::events::task::CheckKind;
    use argonauts_core::settings::fallback::FallbackMode;

    use super::*;
    use crate::testutil::{run_event, task_spec};

    fn worker(name: &str) -> ModelRef {
        name.parse().unwrap()
    }

    fn task(kind: TaskEvent) -> RunEventKind {
        RunEventKind::Task(kind)
    }

    fn fold(events: Vec<RunEvent>) -> Tasks {
        let mut tasks = Tasks::default();
        for event in &events {
            match &event.kind {
                RunEventKind::Task(e) => tasks.apply(e, event),
                RunEventKind::Check(e) => tasks.apply_check(e, event),
                RunEventKind::Review(e) => tasks.apply_review(e, event),
                RunEventKind::Fallback(e) => tasks.apply_fallback(e, event),
                _ => {}
            }
        }
        tasks
    }

    fn id(task: &str) -> TaskId {
        TaskId::new(task).unwrap()
    }

    #[test]
    fn created_tasks_wait_with_their_planned_worker() {
        let spec = task_spec("task-001", &[], Some("claude-code/opus"));
        let tasks = fold(vec![run_event(
            1,
            Some("task-001"),
            None,
            task(TaskEvent::Created(Box::new(spec.clone()))),
        )]);
        let record = tasks.get(&id("task-001")).unwrap();
        assert_eq!(record.state, TaskState::Waiting);
        assert_eq!(record.worker, Some(worker("claude-code/opus")));
        assert_eq!(record.spec, spec);
        assert_eq!(tasks.in_state(TaskState::Waiting), [id("task-001")]);
    }

    #[test]
    fn states_tries_worktrees_and_commits_fold() {
        let c = Some("task-001");
        let commit = CommitInfo {
            branch: "argonauts/run-01/task-001".into(),
            sha: Some("abc".into()),
            message: "task-001 s-001".into(),
            stat: DiffStat::default(),
            by: Some(worker("codex")),
        };
        let tasks = fold(vec![
            run_event(
                1,
                c,
                None,
                task(TaskEvent::Created(Box::new(task_spec(
                    "task-001",
                    &[],
                    None,
                )))),
            ),
            run_event(
                2,
                c,
                None,
                task(TaskEvent::State {
                    from: None,
                    to: TaskState::Ready,
                    reason: "ready".into(),
                }),
            ),
            run_event(
                3,
                c,
                None,
                task(TaskEvent::Worktree {
                    path: "/w/task-001".into(),
                    branch: "argonauts/run-01/task-001".into(),
                    base: "base".into(),
                }),
            ),
            run_event(
                4,
                c,
                Some("s-001"),
                task(TaskEvent::Try {
                    try_number: 1,
                    worker: worker("codex"),
                    cause: TryCause::First,
                }),
            ),
            run_event(
                5,
                c,
                Some("s-001"),
                task(TaskEvent::Committed(commit.clone())),
            ),
            run_event(
                6,
                c,
                Some("s-001"),
                task(TaskEvent::Committed(commit.clone())),
            ),
            run_event(
                7,
                c,
                None,
                task(TaskEvent::Diff {
                    stat: DiffStat {
                        files_changed: 1,
                        insertions: 2,
                        deletions: 0,
                    },
                    path: "/d/1.diff".into(),
                }),
            ),
            run_event(
                8,
                c,
                Some("s-001"),
                task(TaskEvent::Try {
                    try_number: 1,
                    worker: worker("codex"),
                    cause: TryCause::CheckRetry,
                }),
            ),
            run_event(
                9,
                c,
                Some("s-001"),
                task(TaskEvent::Try {
                    try_number: 1,
                    worker: worker("codex"),
                    cause: TryCause::ReviewChanges,
                }),
            ),
        ]);
        let record = tasks.get(&id("task-001")).unwrap();
        let progress = tasks.progress(&id("task-001"));
        assert_eq!(record.state, TaskState::Ready);
        assert_eq!(record.tries, 1);
        assert_eq!(record.worker, Some(worker("codex")));
        assert_eq!(progress.try_events, 3);
        assert_eq!(progress.try_seq, Some(9));
        assert_eq!(record.worktree, Some(PathBuf::from("/w/task-001")));
        assert_eq!(record.base.as_deref(), Some("base"));
        assert_eq!(record.commits, [commit]);
        assert_eq!(progress.diff.as_ref().unwrap().0.insertions, 2);
        assert_eq!(progress.change_rounds, 1);
        assert_eq!(
            progress.check_retries, 0,
            "a review round starts the checks ladder over"
        );
        assert_eq!(progress.last_cause, Some(TryCause::ReviewChanges));
    }

    #[test]
    fn only_a_ready_task_deferred_waits_for_limit() {
        let c = Some("task-001");
        let state = |seq, from, to| {
            run_event(
                seq,
                c,
                None,
                task(TaskEvent::State {
                    from: Some(from),
                    to,
                    reason: String::new(),
                }),
            )
        };
        let created = run_event(
            1,
            c,
            None,
            task(TaskEvent::Created(Box::new(task_spec(
                "task-001",
                &[],
                None,
            )))),
        );
        let tasks = fold(vec![
            created.clone(),
            state(2, TaskState::Waiting, TaskState::Ready),
            state(3, TaskState::Ready, TaskState::Deferred),
        ]);
        assert!(tasks.progress(&id("task-001")).deferred_for_limit);
        let tasks = fold(vec![
            created,
            state(2, TaskState::Running, TaskState::NeedsYou),
            state(3, TaskState::NeedsYou, TaskState::Deferred),
        ]);
        assert!(!tasks.progress(&id("task-001")).deferred_for_limit);
    }

    #[test]
    fn each_move_out_of_stuck_back_in_line_is_an_extra_try() {
        let c = Some("task-001");
        let state = |seq, from, to| {
            run_event(
                seq,
                c,
                None,
                task(TaskEvent::State {
                    from: Some(from),
                    to,
                    reason: String::new(),
                }),
            )
        };
        let created = run_event(
            1,
            c,
            None,
            task(TaskEvent::Created(Box::new(task_spec(
                "task-001",
                &[],
                None,
            )))),
        );
        let tasks = fold(vec![
            created,
            state(2, TaskState::Running, TaskState::NeedsYou),
            state(3, TaskState::NeedsYou, TaskState::Ready),
            state(4, TaskState::Ready, TaskState::Running),
            state(5, TaskState::Running, TaskState::NeedsDecision),
            state(6, TaskState::NeedsDecision, TaskState::Ready),
            state(7, TaskState::Running, TaskState::NeedsYou),
            state(8, TaskState::NeedsYou, TaskState::Ready),
        ]);
        assert_eq!(tasks.progress(&id("task-001")).extra_tries, 2);
    }

    #[test]
    fn check_retries_reset_on_a_fresh_session() {
        let c = Some("task-001");
        let try_event = |seq, n, cause| {
            run_event(
                seq,
                c,
                None,
                task(TaskEvent::Try {
                    try_number: n,
                    worker: worker("codex"),
                    cause,
                }),
            )
        };
        let tasks = fold(vec![
            run_event(
                1,
                c,
                None,
                task(TaskEvent::Created(Box::new(task_spec(
                    "task-001",
                    &[],
                    None,
                )))),
            ),
            try_event(2, 1, TryCause::First),
            try_event(3, 1, TryCause::CheckRetry),
            try_event(4, 1, TryCause::Resumed),
            try_event(5, 1, TryCause::Rotation),
        ]);
        assert_eq!(
            tasks.progress(&id("task-001")).check_retries,
            1,
            "a resume or a rotation of the same worker keeps the checks retry used"
        );
        let tasks = fold(vec![
            run_event(
                1,
                c,
                None,
                task(TaskEvent::Created(Box::new(task_spec(
                    "task-001",
                    &[],
                    None,
                )))),
            ),
            try_event(2, 1, TryCause::First),
            try_event(3, 1, TryCause::CheckRetry),
            run_event(
                4,
                c,
                None,
                task(TaskEvent::Reassigned {
                    from: Some(worker("codex")),
                    to: worker("agy"),
                    by: Party::ArgonautsCode,
                    rule_override: None,
                }),
            ),
            try_event(5, 2, TryCause::AfterFailure),
        ]);
        let record = tasks.get(&id("task-001")).unwrap();
        assert_eq!(tasks.progress(&id("task-001")).check_retries, 0);
        assert_eq!(record.reassigned, 1);
        assert_eq!(record.tries, 2);
    }

    #[test]
    fn only_an_automatic_move_of_a_failed_task_is_a_ladder_retry() {
        let c = Some("task-001");
        let state = |seq, from, to| {
            run_event(
                seq,
                c,
                None,
                task(TaskEvent::State {
                    from: Some(from),
                    to,
                    reason: String::new(),
                }),
            )
        };
        let reassigned = |seq, to: &str, by| {
            run_event(
                seq,
                c,
                None,
                task(TaskEvent::Reassigned {
                    from: None,
                    to: worker(to),
                    by,
                    rule_override: None,
                }),
            )
        };
        let created = run_event(
            1,
            c,
            None,
            task(TaskEvent::Created(Box::new(task_spec(
                "task-001",
                &[],
                None,
            )))),
        );
        let tasks = fold(vec![
            created.clone(),
            state(2, TaskState::Running, TaskState::NeedsDecision),
            reassigned(3, "agy", Party::ArgonautsCode),
            state(4, TaskState::NeedsDecision, TaskState::Ready),
            state(5, TaskState::Running, TaskState::Failed),
            reassigned(6, "codex", Party::User),
        ]);
        let record = tasks.get(&id("task-001")).unwrap();
        assert_eq!(record.reassigned, 2);
        assert_eq!(tasks.progress(&id("task-001")).ladder_retries, 0);

        let tasks = fold(vec![
            created,
            state(2, TaskState::Running, TaskState::Failed),
            reassigned(3, "agy", Party::Rules),
            state(4, TaskState::Failed, TaskState::Ready),
        ]);
        assert_eq!(tasks.progress(&id("task-001")).ladder_retries, 1);
    }

    #[test]
    fn merge_returns_count_per_worker() {
        let c = Some("task-001");
        let back = |seq| {
            run_event(
                seq,
                c,
                None,
                task(TaskEvent::State {
                    from: Some(TaskState::Merging),
                    to: TaskState::Running,
                    reason: String::new(),
                }),
            )
        };
        let created = run_event(
            1,
            c,
            None,
            task(TaskEvent::Created(Box::new(task_spec(
                "task-001",
                &[],
                None,
            )))),
        );
        let tasks = fold(vec![created.clone(), back(2), back(3)]);
        assert_eq!(tasks.progress(&id("task-001")).merge_returns, 2);
        let reassigned = run_event(
            4,
            c,
            None,
            task(TaskEvent::Reassigned {
                from: None,
                to: worker("agy"),
                by: Party::User,
                rule_override: None,
            }),
        );
        let tasks = fold(vec![created, back(2), back(3), reassigned]);
        assert_eq!(tasks.progress(&id("task-001")).merge_returns, 0);
    }

    #[test]
    fn checks_reviews_and_reports_are_kept_per_task() {
        let c = Some("task-002");
        let check = CheckOutcome {
            name: "test".into(),
            kind: CheckKind::Command,
            passed: false,
            command: Some(vec!["cargo".into(), "test".into()]),
            exit_code: Some(101),
            duration_ms: 10,
            summary: "1 failed".into(),
            output_tail: "boom".into(),
            violations: Vec::new(),
            sandboxed: true,
            skipped: false,
        };
        let written = ReviewEvent::TestsWritten {
            reviewer: worker("agy"),
            for_task: id("task-001"),
            files: vec!["tests/a.rs".into()],
        };
        let tasks = fold(vec![
            run_event(
                1,
                c,
                None,
                task(TaskEvent::Created(Box::new(task_spec(
                    "task-002",
                    &[],
                    None,
                )))),
            ),
            run_event(2, c, None, RunEventKind::Check(check.clone())),
            run_event(3, c, None, RunEventKind::Review(written.clone())),
            run_event(4, None, None, RunEventKind::Check(check.clone())),
            run_event(
                5,
                Some("task-404"),
                None,
                RunEventKind::Check(check.clone()),
            ),
        ]);
        let record = tasks.get(&id("task-002")).unwrap();
        assert_eq!(record.checks, [check]);
        assert_eq!(record.reviews, [written]);
        assert_eq!(tasks.by_id.len(), 1);
        assert_eq!(tasks.progress(&id("task-002")).changes, 3);
        assert_eq!(
            tasks.progress(&id("task-404")),
            &TaskProgress::default(),
            "a task the log never named has no progress"
        );
    }

    #[test]
    fn fallback_checkpoints_debt_and_rewind_fold() {
        let c = Some("task-001");
        let debt = QualityDebt {
            since: 2,
            by: worker("agy"),
            needs: worker("claude-code"),
        };
        let base = vec![
            run_event(
                1,
                c,
                None,
                task(TaskEvent::Created(Box::new(task_spec(
                    "task-001",
                    &[],
                    Some("claude-code"),
                )))),
            ),
            run_event(
                2,
                c,
                None,
                task(TaskEvent::CheckpointTagged {
                    n: 1,
                    commit: "a".into(),
                    from: worker("claude-code"),
                    to: vec![worker("codex")],
                    reason: "usage limit".into(),
                }),
            ),
            run_event(
                3,
                c,
                None,
                task(TaskEvent::CheckpointTagged {
                    n: 2,
                    commit: "b".into(),
                    from: worker("codex"),
                    to: vec![worker("agy")],
                    reason: "usage limit".into(),
                }),
            ),
            run_event(4, c, None, task(TaskEvent::QualityDebt(debt.clone()))),
            run_event(
                5,
                None,
                None,
                RunEventKind::Fallback(FallbackEvent::Note {
                    task: id("task-001"),
                    path: "/p/note.md".into(),
                    sha256: "00".into(),
                }),
            ),
            run_event(
                6,
                None,
                None,
                RunEventKind::Fallback(FallbackEvent::Waiting {
                    task: id("task-001"),
                    agent_id: AgentId("claude-code".into()),
                    until: None,
                    kept_off: None,
                }),
            ),
        ];
        let tasks = fold(base.clone());
        let record = tasks.get(&id("task-001")).unwrap();
        assert_eq!(record.fallbacks, 2);
        assert_eq!(record.debt, Some(debt.clone()));
        assert_eq!(
            tasks.progress(&id("task-001")).note,
            Some(PathBuf::from("/p/note.md"))
        );
        assert_eq!(
            tasks.progress(&id("task-001")).waiting_for,
            Some(AgentId("claude-code".into()))
        );

        let mut reassigned = base.clone();
        reassigned.push(run_event(
            7,
            None,
            None,
            RunEventKind::Fallback(FallbackEvent::FellBack {
                task: id("task-001"),
                to: worker("agy"),
                mode: FallbackMode::Auto,
                by: Party::Rules,
                context: None,
            }),
        ));
        reassigned.push(run_event(
            8,
            c,
            None,
            task(TaskEvent::DebtCleared {
                by: worker("claude-code"),
            }),
        ));
        let tasks = fold(reassigned);
        let record = tasks.get(&id("task-001")).unwrap();
        assert_eq!(record.worker, Some(worker("agy")));
        assert_eq!(tasks.progress(&id("task-001")).waiting_for, None);
        assert_eq!(record.debt, None);

        let mut rewound = base.clone();
        rewound.push(run_event(
            7,
            c,
            None,
            task(TaskEvent::Rewound {
                to: 3,
                kept_on: "x".into(),
            }),
        ));
        assert_eq!(
            fold(rewound).get(&id("task-001")).unwrap().debt,
            Some(debt.clone())
        );
        let mut rewound = base;
        rewound.push(run_event(
            7,
            c,
            None,
            task(TaskEvent::Rewound {
                to: 2,
                kept_on: "x".into(),
            }),
        ));
        assert_eq!(fold(rewound).get(&id("task-001")).unwrap().debt, None);
    }
}
