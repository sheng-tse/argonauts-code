//! The work ledger, `~/.argonauts/usage/work/<yyyy-mm>.jsonl`, beside the usage ledger. When a
//! run ends it gets one line for each session (its role, task, tokens, tools and time), one for
//! each task (who built it and how review took it), one for each review, one for each approval
//! the judge decided, one for the run, and one for how its plan and its merge went. Stats reads it
//! with the usage ledger's conversation turns, so it says who did what per CLI, model and role
//! long after the runs are gone. Reading it costs nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use argonauts_core::api::day_format;
use argonauts_core::crew_stats::{
    CrewStats, Fallback, ModelDay, ReviewTally, StatsRole, TaskRecord, WorkRow,
};
use argonauts_core::event::ErrorCategory;
use argonauts_core::events::agent_status::{AgentStatusEvent, FallbackEvent};
use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::events::approval::{AnswerAction, ApprovalEvent};
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::events::run::{Party, RunLifecycle, RunOutcome};
use argonauts_core::events::session::{EndReason, SessionStarted};
use argonauts_core::events::task::{CheckKind, ReviewEvent, TaskEvent, TaskKind, TaskState};
use argonauts_core::events::{MergeEvent, PlanEvent, RunEvent, RunEventKind, SessionEvent};
use argonauts_core::limits::PlanUsageStatus;
use argonauts_core::model_catalog::ModelCatalog;
use argonauts_core::names::ApprovalId;
use argonauts_core::names::{AgentId, ModelRef, SessionKey};
use argonauts_core::steps::StepName;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::steps::plan::Severity;
use argonauts_core::views::writes_the_message;
use argonauts_core::{AgentEvent, RunId, Spend, TaskId, create_private_dir_all};
use serde::{Deserialize, Serialize};
use time::{Date, Duration, OffsetDateTime};

use crate::usage_ledger::{LedgerLine, local_day};
use crate::views::sessions::{RunFacts, SessionFacts, add_spend};

/// The Captain-or-crew rule that sends a request to a workflow because ultracode is on.
const ULTRACODE_RULE: &str = "ultracode";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkLine {
    /// The local day it happened: a session's start, or the run's end.
    #[serde(with = "day_format")]
    pub day: Date,
    pub repo: String,
    pub run: RunId,
    #[serde(flatten)]
    pub what: Work,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "line", rename_all = "snake_case")]
pub(crate) enum Work {
    Session {
        /// The role its start named, such as `worker` or `judge`.
        role: String,
        by: ModelRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<TaskId>,
        /// What its task was, such as `build` or `tests`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        spent: Spend,
        /// Seconds of wall time.
        took: u64,
        /// The tools it called.
        #[serde(default, skip_serializing_if = "is_zero")]
        tools: u32,
        /// The one it is the fallback for, and why.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fallback_for: Option<ModelRef>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        why: String,
        /// The plan window that had run out on the one it is the fallback for.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<String>,
        /// Why the one it is the fallback for stopped, as a fixed word.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<FallbackCause>,
    },
    Task {
        task: TaskId,
        /// The worker whose work it ended with.
        by: ModelRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        /// Review rounds that sent it back.
        sent_back: u32,
        merged: bool,
        /// A reviewer or the judge looked at it, so it fared in review one way or another.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        reviewed: bool,
        /// Its checks passed the first time they ran, before any review. `None` when none ran.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checks_first: Option<bool>,
        /// Its checks passed as they last ran.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checks_last: Option<bool>,
    },
    Review {
        by: ModelRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<TaskId>,
        /// A judge's ruling rather than a reviewer's review.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        judge: bool,
        sent_back: bool,
        /// Why it sent the work back, in the words Stats shows.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The check commands it was given with their results.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        checks: Vec<String>,
        /// One of those checks had failed. `None` when it was given none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checks_failed: Option<bool>,
        /// It ran commands of its own.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        ran_own: bool,
        /// The findings it gave, and how many of them block the work.
        #[serde(default, skip_serializing_if = "is_zero")]
        findings: u32,
        #[serde(default, skip_serializing_if = "is_zero")]
        blocking: u32,
    },
    /// An approval the judge decided in your place.
    Signoff {
        by: ModelRef,
        /// It sent the work back or stopped it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        refused: bool,
    },
    Run {
        /// The orchestrator that planned it, when one did.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<ModelRef>,
        /// Seconds from its start to its end.
        took: u64,
        /// The workflow it ran, such as `council`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow: Option<String>,
        /// Goals it set: its own, and each of its tasks'.
        #[serde(default, skip_serializing_if = "is_zero")]
        goals: u32,
        /// The Captain-or-crew rules sent it to a workflow because ultracode is on.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        ultracode: bool,
        /// Questions it settled on autopilot, in your place.
        #[serde(default, skip_serializing_if = "is_zero")]
        answered: u32,
    },
    /// How the run's plan and its merge went, beside its `Run` line.
    Outcome {
        /// Its work reached your branch.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        merged: bool,
        /// Times its plan was made again after the first.
        #[serde(default, skip_serializing_if = "is_zero")]
        replans: u32,
        /// How its merge went, when it got that far, and who merged when a model did.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        merge: Option<MergeEnd>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        merged_by: Option<ModelRef>,
        /// Its checks failed after the merge.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        checks_failed: bool,
        /// How the run ended. Older lines have none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ended: Option<RunOutcome>,
        /// Who approved its merge, and the times a merge approval sent it back.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        merge_approved_by: Option<ApprovedBy>,
        #[serde(default, skip_serializing_if = "is_zero")]
        merge_sent_back: u32,
    },
}

/// Why a session stopped so another took its work, in words that name no account or model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FallbackCause {
    SignedOut,
    UsageLimit,
    Down,
    ModelUnavailable,
    Stuck,
    Other,
}

impl FallbackCause {
    fn of(reason: &EndReason) -> Self {
        let of_category = |category: &ErrorCategory| match category {
            ErrorCategory::Auth => Self::SignedOut,
            ErrorCategory::UsageLimit | ErrorCategory::RateLimit | ErrorCategory::Billing => {
                Self::UsageLimit
            }
            ErrorCategory::Overloaded | ErrorCategory::Server | ErrorCategory::Network => {
                Self::Down
            }
            ErrorCategory::ModelUnavailable => Self::ModelUnavailable,
            _ => Self::Other,
        };
        match reason {
            EndReason::Auth { .. } => Self::SignedOut,
            EndReason::UsageLimit { .. }
            | EndReason::RateLimited
            | EndReason::Billing { .. }
            | EndReason::LimitHit { .. } => Self::UsageLimit,
            EndReason::CliError { category, .. } => of_category(category),
            EndReason::ModelUnavailable { .. } => Self::ModelUnavailable,
            EndReason::Stuck { .. } => Self::Stuck,
            _ => Self::Other,
        }
    }
}

/// Who approved a run's merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ApprovedBy {
    You,
    Autopilot,
    /// The judge of approvals, in your place.
    Judge,
    /// Your approval rules, or a setting.
    Rules,
}

impl ApprovedBy {
    fn of(party: &Party) -> Self {
        match party {
            Party::User => Self::You,
            Party::Autopilot | Party::Declined => Self::Autopilot,
            Party::Judge { .. } => Self::Judge,
            _ => Self::Rules,
        }
    }
}

/// How a run's merge went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MergeEnd {
    /// Nothing was in conflict.
    Clean,
    /// A conflict came up and was settled.
    Settled,
    /// A conflict came up and the work never reached your branch.
    Failed,
}

/// What a run's log says of its merge.
#[derive(Default)]
struct MergeFacts {
    conflict: bool,
    /// The work reached your branch, or was offered to you to take.
    merged: bool,
    offered: bool,
    checks_failed: bool,
}

impl MergeFacts {
    fn end(&self) -> Option<MergeEnd> {
        match (self.merged || self.offered, self.conflict) {
            (true, false) => Some(MergeEnd::Clean),
            (true, true) => Some(MergeEnd::Settled),
            (false, true) => Some(MergeEnd::Failed),
            (false, false) => None,
        }
    }
}

/// The role a session's line is filed under: its own, or the merger's for the sessions that
/// settle a conflict or write the commit message, which start as a worker's.
pub(crate) fn role_of(started: &SessionStarted) -> &'static str {
    let merges =
        started.step == Some(StepName::ResolveConflict) || writes_the_message(started.step);
    if merges {
        StatsRole::MERGER
    } else {
        started.role.as_str()
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// The model a ledger line is filed under: the catalog's spelling of it, so the alias `haiku`
/// and the `claude-haiku-5-5` its CLI reports are one model in both ledgers. A model the
/// catalog doesn't know, and a CLI named alone, stay as they are.
pub(crate) fn filed_as(catalog: &ModelCatalog, worker: &ModelRef) -> ModelRef {
    match worker.model.as_ref().and_then(|_| catalog.runs(worker)) {
        Some(model) => ModelRef::new(worker.agent.clone(), Some(model)),
        None => worker.clone(),
    }
}

/// The sessions of a run in the order they started, as [`RunFacts`] lists them, with the
/// model each one's CLI said it runs. A key that starts again is a new session that says its
/// own, as when a session falls back to another CLI under the same key.
#[derive(Default)]
pub(crate) struct Reported<'a> {
    latest: BTreeMap<&'a SessionKey, usize>,
    models: Vec<Option<&'a str>>,
}

impl<'a> Reported<'a> {
    pub fn of(events: &'a [RunEvent]) -> Self {
        let mut reported = Self::default();
        for event in events {
            reported.note(event);
        }
        reported
    }

    fn note(&mut self, event: &'a RunEvent) {
        let Some(key) = &event.session else { return };
        match &event.kind {
            RunEventKind::Session(SessionEvent::Started(_)) => {
                self.latest.insert(key, self.models.len());
                self.models.push(None);
            }
            RunEventKind::Agent(AgentEvent::Init {
                model: Some(model), ..
            }) => {
                if let Some(model_of) = self.latest.get(key).and_then(|at| self.models.get_mut(*at))
                {
                    *model_of = Some(model);
                }
            }
            _ => {}
        }
    }

    /// Where the session `key` names so far stands in the order they started.
    fn now(&self, key: &SessionKey) -> Option<usize> {
        self.latest.get(key).copied()
    }

    /// The model the session at `at` ran: the one its CLI said, else the one it started with.
    pub fn ran(&self, at: usize, session: &SessionFacts) -> ModelRef {
        match self.models.get(at).copied().flatten() {
            Some(model) => ModelRef::new(session.worker.agent.clone(), Some(model.to_owned())),
            None => session.worker.clone(),
        }
    }
}

pub(crate) struct WorkLedger {
    dir: PathBuf,
    catalog: &'static ModelCatalog,
}

impl WorkLedger {
    /// The ledger in the usage ledger's folder `usage`.
    pub fn beside(usage: &Path) -> Self {
        Self {
            dir: usage.join("work"),
            catalog: argonauts_registry::models::shipped_catalog(),
        }
    }

    /// A run ended: its lines, from its whole log. A run already in this month's file isn't
    /// written again.
    pub fn run_ended(&self, repo: &Path, run: &RunId, events: &[RunEvent], at: OffsetDateTime) {
        let day = local_day(at);
        let seen = read_lines(&self.month_file(day))
            .iter()
            .any(|line| line.run == *run && matches!(line.what, Work::Run { .. }));
        if seen {
            return;
        }
        let lines = self.lines_of(repo, run, events, day);
        if let Err(err) = self.append(&lines) {
            tracing::warn!("couldn't add run {run} to the work ledger: {err}");
        }
    }

    /// Each line goes to the file of its day's month in one write, so two processes appending
    /// at once never interleave within a line.
    pub fn append(&self, lines: &[WorkLine]) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        create_private_dir_all(&self.dir)?;
        for line in lines {
            let mut text = serde_json::to_string(line).map_err(io::Error::other)?;
            text.push('\n');
            OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(self.month_file(line.day))?
                .write_all(text.as_bytes())?;
        }
        Ok(())
    }

    /// Every line from `first` on, or every line at all. Lines that don't read are skipped.
    pub fn read_since(&self, first: Option<Date>) -> Vec<WorkLine> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let first_month = first.map(month_name);
        let mut months: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                let month = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_suffix(".jsonl"));
                month.is_some_and(|month| first_month.as_deref().is_none_or(|first| month >= first))
            })
            .collect();
        months.sort();
        months
            .iter()
            .flat_map(|path| read_lines(path))
            .filter(|line| first.is_none_or(|first| line.day >= first))
            .collect()
    }

    fn month_file(&self, day: Date) -> PathBuf {
        self.dir.join(format!("{}.jsonl", month_name(day)))
    }

    /// The run's lines from its log, each model filed under the catalog's spelling.
    pub(crate) fn lines_of(
        &self,
        repo: &Path,
        run: &RunId,
        events: &[RunEvent],
        ended: Date,
    ) -> Vec<WorkLine> {
        let mut lines = lines_of(repo, run, events, ended);
        for line in &mut lines {
            self.refile(line);
        }
        lines
    }

    /// Files each model `line` names under the catalog's spelling.
    pub(crate) fn refile(&self, line: &mut WorkLine) {
        for model in line.what.models_mut() {
            *model = filed_as(self.catalog, model);
        }
    }

    /// The folder of its month files.
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Work {
    /// Every model the line names.
    fn models_mut(&mut self) -> Vec<&mut ModelRef> {
        match self {
            Work::Session {
                by, fallback_for, ..
            } => [Some(by), fallback_for.as_mut()]
                .into_iter()
                .flatten()
                .collect(),
            Work::Task { by, .. } | Work::Review { by, .. } | Work::Signoff { by, .. } => {
                vec![by]
            }
            Work::Run { by, .. } => by.as_mut().into_iter().collect(),
            Work::Outcome { merged_by, .. } => merged_by.as_mut().into_iter().collect(),
        }
    }
}

fn read_lines(path: &Path) -> Vec<WorkLine> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn month_name(day: Date) -> String {
    format!("{:04}-{:02}", day.year(), u8::from(day.month()))
}

/// What a task was, in the words Stats shows: `build`, `tests`, `docs`.
fn kind_word(kind: &TaskKind) -> &'static str {
    match kind {
        TaskKind::Captain | TaskKind::Build => "build",
        TaskKind::Interface => "interfaces",
        TaskKind::AcceptanceTests { .. } => "tests",
        TaskKind::Chore => "chores",
        TaskKind::Docs => "docs",
        TaskKind::Experiment { .. } => "experiments",
    }
}

/// The run's lines, from its log. `ended` is the local day it ended.
/// The workflow a run's first event names.
fn workflow_of(events: &[RunEvent]) -> Option<String> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Started(started)) => Some(started.workflow.clone()),
        _ => None,
    })
}

/// The goals a run set, one for the run and one for each task that had one. A goal set again,
/// as when its lines change, is still one goal.
fn goals_of(events: &[RunEvent]) -> u32 {
    let set: BTreeSet<Option<&TaskId>> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Goal(GoalEvent::Set(goal)) => Some(goal.task.as_ref()),
            _ => None,
        })
        .collect();
    set.len() as u32
}

/// Whether the Captain-or-crew rules sent the run to a workflow because ultracode is on.
fn ultracode_of(events: &[RunEvent]) -> bool {
    events.iter().any(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Started(started)) => started
            .captain_or_crew
            .as_ref()
            .is_some_and(|captain_or_crew| captain_or_crew.rule == ULTRACODE_RULE),
        _ => false,
    })
}

fn lines_of(repo: &Path, run: &RunId, events: &[RunEvent], ended: Date) -> Vec<WorkLine> {
    let facts = RunFacts::of(events);
    let mut kinds: BTreeMap<TaskId, &'static str> = BTreeMap::new();
    let mut merged: BTreeSet<TaskId> = BTreeSet::new();
    // Each task's review rounds that sent it back.
    let mut sent_back: BTreeMap<TaskId, BTreeSet<u32>> = BTreeMap::new();
    let mut merge = MergeFacts::default();
    let mut plans = 0u32;
    // Who stopped on a task, and why, until its fallback starts.
    let mut stopped: BTreeMap<TaskId, (String, Option<String>, FallbackCause)> = BTreeMap::new();
    // How each task's last session to stop early stopped, and each session's task from its
    // start, for an end that names only the session.
    let mut ended_early: BTreeMap<TaskId, EndReason> = BTreeMap::new();
    let mut task_of: BTreeMap<&SessionKey, &TaskId> = BTreeMap::new();
    // Each task's checks the first time they ran, until its first review, and whether they passed.
    let mut first_checks: BTreeMap<TaskId, bool> = BTreeMap::new();
    let mut first_done: BTreeSet<TaskId> = BTreeSet::new();
    let mut run_end = None;
    let mut merge_approvals: BTreeSet<ApprovalId> = BTreeSet::new();
    let mut merge_approved_by = None;
    let mut merge_sent_back = 0u32;
    // Each CLI's plan window that has run out, by its last reading.
    let mut out_of: BTreeMap<AgentId, String> = BTreeMap::new();
    // Each task's check commands as they last ran, and whether each passed.
    let mut checks: BTreeMap<TaskId, BTreeMap<String, bool>> = BTreeMap::new();
    // The model each session's CLI said it runs, which the line is filed under.
    let mut reported = Reported::default();
    // Each review line made in a session the log names, by where that session started, so
    // the review is the session's own and not whichever session last ran its worker.
    let mut made_in: BTreeMap<usize, usize> = BTreeMap::new();
    // The tasks a reviewer or the judge looked at.
    let mut looked_at: BTreeSet<TaskId> = BTreeSet::new();
    let line = |day: Date, what: Work| WorkLine {
        day,
        repo: repo.display().to_string(),
        run: run.clone(),
        what,
    };
    let mut lines = Vec::new();
    for event in events {
        let task = event.task_id.as_ref();
        reported.note(event);
        if let RunEventKind::Review(ReviewEvent::Reviewed { .. } | ReviewEvent::Settled { .. }) =
            &event.kind
        {
            looked_at.extend(task.cloned());
            if let Some(at) = event.session.as_ref().and_then(|key| reported.now(key)) {
                made_in.insert(lines.len(), at);
            }
        }
        match &event.kind {
            RunEventKind::Task(TaskEvent::Created(spec)) => {
                kinds.insert(spec.id.clone(), kind_word(&spec.kind));
            }
            RunEventKind::Task(TaskEvent::State {
                to: TaskState::Merged,
                ..
            }) => {
                merged.extend(task.cloned());
            }
            RunEventKind::Check(check) if check.kind == CheckKind::Command && !check.skipped => {
                if let (Some(task), Some(command)) = (task, &check.command) {
                    checks
                        .entry(task.clone())
                        .or_default()
                        .insert(command.join(" "), check.passed);
                    if !first_done.contains(task) {
                        let first = first_checks.entry(task.clone()).or_insert(true);
                        *first &= check.passed;
                    }
                }
            }
            RunEventKind::Session(SessionEvent::Started(_)) => {
                if let (Some(key), Some(task)) = (&event.session, task) {
                    task_of.insert(key, task);
                }
            }
            RunEventKind::Session(SessionEvent::Ended(end))
                if end.reason != EndReason::Completed =>
            {
                let of_session = event.session.as_ref().and_then(|key| task_of.get(key));
                if let Some(task) = task.or(of_session.copied()) {
                    ended_early.insert(task.clone(), end.reason.clone());
                }
            }
            RunEventKind::Run(RunLifecycle::Finished { outcome, .. }) => run_end = Some(*outcome),
            RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if opened.kind == ApprovalKind::Merge =>
            {
                merge_approvals.insert(opened.id.clone());
            }
            RunEventKind::Review(ReviewEvent::Reviewed {
                reviewer,
                round,
                review,
                ..
            }) => {
                let back = review.verdict != ReviewVerdict::Approve;
                let given = task.and_then(|task| checks.get(task));
                let failed = given.map(|given| given.values().any(|passed| !passed));
                let test_named = review.comments.iter().any(|comment| {
                    comment.text.to_lowercase().contains("test") || comment.path.contains("test")
                });
                if back && let Some(task) = task {
                    sent_back.entry(task.clone()).or_default().insert(*round);
                }
                // The checks that ran before a review are the first try's; later ones answer it.
                if let Some(task) = task {
                    first_done.insert(task.clone());
                }
                let blocking = review
                    .comments
                    .iter()
                    .filter(|comment| comment.severity == Severity::High)
                    .count();
                let what = Work::Review {
                    by: reviewer.clone(),
                    task: task.cloned(),
                    judge: false,
                    sent_back: back,
                    reason: back.then(|| sent_back_for(review.verdict, failed, test_named)),
                    checks: given
                        .map(|given| given.keys().cloned().collect())
                        .unwrap_or_default(),
                    checks_failed: failed,
                    ran_own: false,
                    findings: review.comments.len() as u32,
                    blocking: blocking as u32,
                };
                lines.push(line(local_day(event.ts), what));
            }
            RunEventKind::Review(ReviewEvent::Settled {
                judge: settled_by,
                ruling,
            }) => {
                let back = ruling.verdict != ReviewVerdict::Approve;
                let given = task.and_then(|task| checks.get(task));
                let failed = given.map(|given| given.values().any(|passed| !passed));
                let what = Work::Review {
                    by: settled_by.clone(),
                    task: task.cloned(),
                    judge: true,
                    sent_back: back,
                    reason: back.then(|| sent_back_for(ruling.verdict, failed, false)),
                    checks: given
                        .map(|given| given.keys().cloned().collect())
                        .unwrap_or_default(),
                    checks_failed: failed,
                    ran_own: false,
                    findings: 0,
                    blocking: 0,
                };
                lines.push(line(local_day(event.ts), what));
            }
            RunEventKind::AgentStatus(AgentStatusEvent::PlanUsage(reading)) => {
                match &reading.window {
                    Some(window) if reading.status == PlanUsageStatus::Rejected => {
                        out_of.insert(reading.agent_id.clone(), window.clone());
                    }
                    _ if reading.status == PlanUsageStatus::Allowed => {
                        out_of.remove(&reading.agent_id);
                    }
                    _ => {}
                }
            }
            RunEventKind::Fallback(FallbackEvent::Needed(plan)) => {
                let window = out_of.get(&plan.stopped.agent_id()).cloned();
                let cause = ended_early
                    .get(&plan.task)
                    .map_or(FallbackCause::Other, FallbackCause::of);
                stopped.insert(plan.task.clone(), (plan.reason.clone(), window, cause));
            }
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                if merge_approvals.contains(&decided.id) {
                    if refuses(&decided.action) {
                        merge_sent_back += 1;
                    } else {
                        merge_approved_by = Some(ApprovedBy::of(&decided.by));
                    }
                }
                if let Party::Judge { model } = &decided.by {
                    let what = Work::Signoff {
                        by: model.clone(),
                        refused: refuses(&decided.action),
                    };
                    lines.push(line(local_day(event.ts), what));
                }
            }
            RunEventKind::Plan(PlanEvent::Orchestrated { .. }) => plans += 1,
            RunEventKind::Merge(MergeEvent::Conflict { .. }) => merge.conflict = true,
            RunEventKind::Merge(MergeEvent::RunMerged { .. }) => merge.merged = true,
            RunEventKind::Merge(
                MergeEvent::PullRequest { .. } | MergeEvent::WaitingToMerge { .. },
            ) => merge.offered = true,
            RunEventKind::Merge(MergeEvent::FinalCheck { passed }) => {
                merge.checks_failed = !passed;
            }
            _ => {}
        }
    }

    // A line is filed under the model the session's CLI said it ran, which an alias in the
    // settings, such as `haiku`, may not name the way the catalog does.
    let ran_on: Vec<ModelRef> = (facts.sessions.iter().enumerate())
        .map(|(at, session)| reported.ran(at, session))
        .collect();
    let ran_shell = |session: &SessionFacts| session.tools.iter().any(|tool| is_shell(&tool.name));
    // A review the log doesn't tie to a session names the reviewer as its step asked; the
    // session that ran that worker knows the model, and whether it ran commands of its own.
    let ran: BTreeMap<&ModelRef, &ModelRef> = (facts.sessions.iter().zip(&ran_on))
        .map(|(session, worker)| (&session.started.worker, worker))
        .collect();
    let own_commands: BTreeSet<(&TaskId, &ModelRef)> = facts
        .sessions
        .iter()
        .filter(|session| {
            let role = StatsRole::counted_as(role_of(&session.started));
            matches!(role, StatsRole::Reviewer | StatsRole::Judge) && ran_shell(session)
        })
        .filter_map(|session| Some((session.task.as_ref()?, &session.started.worker)))
        .collect();
    for (at, review) in lines.iter_mut().enumerate() {
        let session = made_in
            .get(&at)
            .and_then(|at| Some((facts.sessions.get(*at)?, ran_on.get(*at)?)));
        let Work::Review {
            by,
            task,
            judge,
            sent_back,
            ran_own,
            ..
        } = &mut review.what
        else {
            continue;
        };
        match session {
            // A judge's verdict on the work, rather than its ruling between a worker and a
            // reviewer, is one of its sign-offs.
            Some((session, worker))
                if !*judge
                    && StatsRole::counted_as(role_of(&session.started)) == StatsRole::Judge =>
            {
                review.what = Work::Signoff {
                    by: worker.clone(),
                    refused: *sent_back,
                };
            }
            Some((session, worker)) => {
                *by = worker.clone();
                *ran_own = ran_shell(session);
            }
            None => {
                *ran_own = task
                    .as_ref()
                    .is_some_and(|task| own_commands.contains(&(task, &*by)));
                if let Some(worker) = ran.get(&*by) {
                    *by = (*worker).clone();
                }
            }
        }
    }

    let mut builders: BTreeMap<TaskId, ModelRef> = BTreeMap::new();
    let mut orchestrator = None;
    let mut merged_by = None;
    for (session, worker) in facts.sessions.iter().zip(ran_on) {
        let role = role_of(&session.started);
        let crew_role = StatsRole::counted_as(role);
        if crew_role == StatsRole::Orchestrator && orchestrator.is_none() {
            orchestrator = Some(worker.clone());
        }
        if crew_role == StatsRole::Merger {
            merged_by.get_or_insert_with(|| worker.clone());
            merge.conflict |= session.started.step == Some(StepName::ResolveConflict);
        }
        let kind = session.task.as_ref().and_then(|task| kinds.get(task));
        if crew_role == StatsRole::Worker
            && let Some(task) = &session.task
        {
            builders.insert(task.clone(), worker.clone());
        }
        let ended_at = session.ended.as_ref().map(|end| {
            session.started_at + Duration::milliseconds(end.duration_ms.min(i64::MAX as u64) as i64)
        });
        let until = ended_at.or(facts.finished_at).or(facts.last_ts);
        let took = until.map_or(0, |until| {
            (until - session.started_at).whole_seconds().max(0)
        });
        let (why, window, cause) = match (&session.fallback_for, &session.task) {
            (Some(_), Some(task)) => match stopped.get(task) {
                Some((why, window, cause)) => (why.clone(), window.clone(), Some(*cause)),
                None => (String::new(), None, Some(FallbackCause::Other)),
            },
            _ => (String::new(), None, None),
        };
        let what = Work::Session {
            role: role.to_owned(),
            by: worker,
            task: session.task.clone(),
            kind: kind.map(|kind| (*kind).to_owned()),
            spent: session.spent(),
            took: took as u64,
            tools: session.tools.len() as u32,
            fallback_for: session.fallback_for.clone(),
            why,
            window,
            cause,
        };
        lines.push(line(local_day(session.started_at), what));
    }

    for (task, by) in builders {
        let checks_last = checks
            .get(&task)
            .map(|given| given.values().all(|passed| *passed));
        let what = Work::Task {
            kind: kinds.get(&task).map(|kind| (*kind).to_owned()),
            sent_back: sent_back.get(&task).map_or(0, |rounds| rounds.len() as u32),
            merged: merged.contains(&task),
            reviewed: looked_at.contains(&task),
            checks_first: first_checks.get(&task).copied(),
            checks_last,
            task,
            by,
        };
        lines.push(line(ended, what));
    }

    let started = events.first().map(|event| event.ts);
    let finished = facts.finished_at.or(facts.last_ts);
    let took = match (started, finished) {
        (Some(started), Some(finished)) => (finished - started).whole_seconds().max(0) as u64,
        _ => 0,
    };
    let answered = events
        .iter()
        .filter(|event| {
            matches!(
                &event.kind,
                RunEventKind::Approval(ApprovalEvent::Decided(decided))
                    if decided.by == Party::Autopilot
            )
        })
        .count() as u32;
    lines.push(line(
        ended,
        Work::Run {
            by: orchestrator,
            took,
            workflow: workflow_of(events).filter(|name| !name.is_empty()),
            goals: goals_of(events),
            ultracode: ultracode_of(events),
            answered,
        },
    ));
    lines.push(line(
        ended,
        Work::Outcome {
            merged: merge.merged,
            replans: plans.saturating_sub(1),
            merge: merge.end(),
            merged_by,
            checks_failed: merge.checks_failed,
            ended: run_end,
            merge_approved_by,
            merge_sent_back,
        },
    ));
    lines
}

/// Whether the judge's answer to an approval turned the work down.
fn refuses(action: &AnswerAction) -> bool {
    matches!(
        action,
        AnswerAction::SendBack
            | AnswerAction::Stop
            | AnswerAction::RequestChanges
            | AnswerAction::Abandon
    )
}

/// Why a review sent the work back, in the words Stats shows: a check it was given had
/// failed, it found the work wrong, or it asked for a test; anything else is changes asked for.
fn sent_back_for(verdict: ReviewVerdict, checks_failed: Option<bool>, test_named: bool) -> String {
    let why = if checks_failed == Some(true) {
        "a check failed"
    } else if verdict == ReviewVerdict::Reject {
        "didn't do the task"
    } else if test_named {
        "a test missing"
    } else {
        "changes asked for"
    };
    why.to_owned()
}

/// A tool that runs commands in a shell, whichever CLI calls it.
fn is_shell(tool: &str) -> bool {
    let tool = tool.to_ascii_lowercase();
    ["bash", "shell", "exec", "command"]
        .iter()
        .any(|word| tool.contains(word))
}

/// Stats for the days from `first` on, or every day: the work ledger's lines with the usage
/// ledger's conversation turns, whose answers the main role gave and whose conversations are
/// its work. A run the work ledger missed still counts its tokens, as work no role is known
/// for. Each day of the period keeps what each model did and used in each role, for the charts.
pub(crate) fn stats(ledger: &WorkLedger, first: Option<Date>, usage: &[LedgerLine]) -> CrewStats {
    let lines = ledger.read_since(first);
    let in_period = |day: Date| first.is_none_or(|first| day >= first);
    let recorded: BTreeSet<&RunId> = lines.iter().map(|line| &line.run).collect();

    let mut stats = CrewStats::default();
    let mut rows: BTreeMap<RowKey, WorkRow> = BTreeMap::new();
    let mut days: BTreeMap<DayKey, (Spend, u32)> = BTreeMap::new();
    // Why each task was sent back, as each review that did so said, in the order they came.
    let mut returned: BTreeMap<(&RunId, &TaskId), Vec<String>> = BTreeMap::new();
    for line in &lines {
        if let Work::Review {
            task: Some(task),
            judge: false,
            sent_back: true,
            reason: Some(why),
            ..
        } = &line.what
        {
            returned
                .entry((&line.run, task))
                .or_default()
                .push(why.clone());
        }
    }

    for line in &lines {
        if !in_period(line.day) {
            continue;
        }
        match &line.what {
            Work::Session {
                role,
                by,
                kind,
                spent,
                took,
                tools,
                fallback_for,
                why,
                window,
                ..
            } => {
                let crew_role = StatsRole::counted_as(role);
                add_spend(
                    &mut day_of(&mut days, line.day, by, Some(crew_role)).0,
                    spent,
                );
                let kind = kind.clone().filter(|_| crew_role == StatsRole::Worker);
                let row = row_of(&mut rows, by, crew_role, kind);
                add_spend(&mut row.spent, spent);
                row.took_secs += took;
                row.sessions += 1;
                row.tool_calls += tools;
                if let Some(from) = fallback_for {
                    stats.fallbacks.push(Fallback {
                        agent: by.agent.clone(),
                        model: by.model.clone(),
                        from_agent: from.agent.clone(),
                        from_model: from.model.clone(),
                        why: why.clone(),
                        window: window.clone(),
                    });
                }
            }
            Work::Task {
                task,
                by,
                kind,
                sent_back,
                merged,
                reviewed,
                ..
            } => {
                day_of(&mut days, line.day, by, Some(StatsRole::Worker)).1 += 1;
                row_of(&mut rows, by, StatsRole::Worker, kind.clone()).done += 1;
                stats.tasks.push(TaskRecord {
                    agent: by.agent.clone(),
                    model: by.model.clone(),
                    kind: kind.clone(),
                    sent_back: *sent_back,
                    reasons: returned.remove(&(&line.run, task)).unwrap_or_default(),
                    merged: *merged,
                    reviewed: *reviewed,
                });
            }
            Work::Review {
                by,
                judge,
                sent_back,
                reason,
                checks,
                checks_failed,
                ran_own,
                ..
            } => {
                // A judge's ruling between a worker and a reviewer isn't one of its sign-offs:
                // it counts with the runs' outcomes, and its row keeps only why it ruled.
                let role = if *judge {
                    StatsRole::Judge
                } else {
                    StatsRole::Reviewer
                };
                let row = row_of(&mut rows, by, role, None);
                if *judge && *sent_back {
                    stats.outcomes.sided_with_reviewer += 1;
                } else if *judge {
                    stats.outcomes.sided_with_worker += 1;
                } else {
                    row.done += 1;
                    row.sent_back += u32::from(*sent_back);
                    day_of(&mut days, line.day, by, Some(role)).1 += 1;
                }
                let mut tally = ReviewTally {
                    checks: checks.clone(),
                    ran_own: u32::from(*ran_own),
                    ..ReviewTally::default()
                };
                match checks_failed {
                    Some(true) => tally.failed = 1,
                    Some(false) => tally.passed = 1,
                    None => {}
                }
                if let Some(why) = reason.as_ref().filter(|_| *sent_back) {
                    tally.reasons.insert(why.clone(), 1);
                }
                row.review.add(&tally);
            }
            Work::Signoff { by, refused } => {
                day_of(&mut days, line.day, by, Some(StatsRole::Judge)).1 += 1;
                let row = row_of(&mut rows, by, StatsRole::Judge, None);
                row.done += 1;
                row.sent_back += u32::from(*refused);
            }
            Work::Run { by, took, .. } => {
                stats.workflows += 1;
                stats.took_secs += took;
                if let Some(by) = by {
                    day_of(&mut days, line.day, by, Some(StatsRole::Orchestrator)).1 += 1;
                    row_of(&mut rows, by, StatsRole::Orchestrator, None).done += 1;
                }
            }
            Work::Outcome {
                merged,
                replans,
                merge,
                merged_by,
                checks_failed,
                ..
            } => {
                let outcomes = &mut stats.outcomes;
                outcomes.runs += 1;
                outcomes.runs_merged += u32::from(*merged);
                outcomes.runs_replanned += u32::from(*replans > 0);
                match merge {
                    Some(MergeEnd::Clean) => outcomes.merges_clean += 1,
                    Some(MergeEnd::Settled) => outcomes.merges_settled += 1,
                    Some(MergeEnd::Failed) => outcomes.merges_failed += 1,
                    None => {}
                }
                outcomes.checks_failed += u32::from(merge.is_some() && *checks_failed);
                if let (Some(by), Some(_)) = (merged_by, merge) {
                    day_of(&mut days, line.day, by, Some(StatsRole::Merger)).1 += 1;
                    row_of(&mut rows, by, StatsRole::Merger, None).done += 1;
                }
            }
        }
    }

    // The conversations the main agent held, by who answered in them. A turn filed with
    // none is a conversation of its own.
    let mut held: BTreeSet<(&str, &str, &str, Option<&str>)> = BTreeSet::new();
    for line in usage {
        let unrecorded = line.run.as_ref().is_some_and(|run| !recorded.contains(run));
        if line.run.is_some() && !unrecorded || !in_period(line.day) {
            continue;
        }
        let by = ModelRef::new(line.agent.clone(), line.model.clone());
        let role = if unrecorded {
            StatsRole::Worker
        } else {
            StatsRole::Main
        };
        let begun = !unrecorded
            && line.conversation.as_deref().is_none_or(|conversation| {
                held.insert((
                    line.repo.as_str(),
                    conversation,
                    line.agent.as_str(),
                    line.model.as_deref(),
                ))
            });
        let day = day_of(&mut days, line.day, &by, (!unrecorded).then_some(role));
        add_spend(&mut day.0, &line.spent());
        day.1 += u32::from(begun);
        let row = row_of(&mut rows, &by, role, None);
        add_spend(&mut row.spent, &line.spent());
        row.done += u32::from(begun);
        if !unrecorded {
            row.turns += 1;
            row.took_secs += line.took;
        }
    }

    stats.work = rows.into_values().collect();
    stats.days = days
        .into_iter()
        .filter(|(_, (spent, done))| spent.total_tokens() > 0 || *done > 0)
        .map(|((day, agent, model, role), (spent, done))| ModelDay {
            day,
            agent,
            model,
            role,
            tokens: spent.total_tokens(),
            cost_usd: spent.cost_usd,
            done,
        })
        .collect();
    stats
}

/// Files each record that names no model under the model its CLI runs when none is named,
/// so a turn recorded without one counts as that model's rather than as the CLI's.
pub(crate) fn name_unnamed_models(stats: &mut CrewStats, defaults: &BTreeMap<String, String>) {
    let named = |agent: &str, model: &mut Option<String>| {
        if model.is_none() {
            model.clone_from(&defaults.get(agent).cloned());
        }
    };
    for row in &mut stats.work {
        named(&row.agent, &mut row.model);
    }
    for task in &mut stats.tasks {
        named(&task.agent, &mut task.model);
    }
    for day in &mut stats.days {
        named(&day.agent, &mut day.model);
    }
    for fallback in &mut stats.fallbacks {
        named(&fallback.agent, &mut fallback.model);
        named(&fallback.from_agent, &mut fallback.from_model);
    }
}

type DayKey = (Date, String, Option<String>, Option<StatsRole>);

/// The tokens and the pieces of work of one model in one role on one day.
fn day_of<'a>(
    days: &'a mut BTreeMap<DayKey, (Spend, u32)>,
    day: Date,
    by: &ModelRef,
    role: Option<StatsRole>,
) -> &'a mut (Spend, u32) {
    let key = (day, by.agent.clone(), by.model.clone(), role);
    days.entry(key).or_default()
}

type RowKey = (String, Option<String>, StatsRole, Option<String>);

fn row_of<'a>(
    rows: &'a mut BTreeMap<RowKey, WorkRow>,
    by: &ModelRef,
    role: StatsRole,
    kind: Option<String>,
) -> &'a mut WorkRow {
    let key = (by.agent.clone(), by.model.clone(), role, kind.clone());
    rows.entry(key).or_insert_with(|| WorkRow {
        agent: by.agent.clone(),
        model: by.model.clone(),
        role,
        kind,
        spent: Spend::default(),
        done: 0,
        sent_back: 0,
        took_secs: 0,
        sessions: 0,
        tool_calls: 0,
        turns: 0,
        review: ReviewTally::default(),
    })
}

#[cfg(test)]
#[path = "work_ledger_tests.rs"]
mod tests;
