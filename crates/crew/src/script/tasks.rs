//! The work a script's writers do. Each piece of work is a task of the engine's: a worktree and
//! branch of its own, its worker's session, a commit when the session ends, and its checks. The
//! script's own steps review it, so the engine's review stays out, and what they accepted is
//! merged into the run when the script ends.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use argonauts_core::TaskId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::session::CHILD_GROUP;
use argonauts_core::events::task::{
    CheckOutcome, ReviewEvent, TaskKind, TaskLimit, TaskSpec, TaskState,
};
use argonauts_core::labels;
use argonauts_core::names::ModelRef;
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::steps::judgment::{ReviewComment, ReviewOutput, ReviewVerdict};
use argonauts_core::steps::plan::Severity;
use argonauts_runner::{AgentCall, AgentEnd, Kept};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::bridge::reading::Change;
use super::bridge::{Bridge, lock};
use crate::engine::host::{Host, Ran};
use crate::error::{EngineError, EngineResult};
use crate::review::{Verdict, with_findings};
use crate::roles;
use crate::task::pipeline::{self, Until};
use crate::task::support::{newest_worker_session, record_of, state_of, worker_of};
use crate::task::{self, machine::Trigger, worktree};
use crate::workflow::StepAt;

/// What a call works on when it names no task and asks for no worktree of its own: the
/// request, which every such writer shares.
const THE_REQUEST: &str = "the-request";

/// How the reason starts when a call's work is skipped, as its row says it and its note says it
/// after the call's label.
pub(super) const SKIPPED: &str = "skipped, since";

/// The tasks a script's writers work on. It is kept beside the run, so a run that picks its
/// script up again still knows which tasks its reviews accepted.
pub(super) struct Book {
    entries: Mutex<Vec<Entry>>,
    kept_in: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    /// The task's name in the script, or the label of a writer with a worktree of its own.
    name: String,
    task: TaskId,
    /// Writers on one task take turns in its worktree.
    #[serde(skip)]
    turn: Arc<tokio::sync::Mutex<()>>,
    /// The newest verdict of the script's own review, when it gave one.
    accepted: Option<bool>,
    /// One of several tries the judge didn't keep.
    dropped: bool,
    /// The calls, by their place in the script, whose turn at the task is over. A run that
    /// stopped before such a call's answer was saved asks it again, and its work is done.
    #[serde(default)]
    finished: Vec<u32>,
    /// How often a writer took the task back after its review didn't accept it.
    #[serde(default)]
    sent_back: u32,
}

impl Book {
    /// The book as an earlier stretch of the run left it, or an empty one.
    pub fn open(kept_in: PathBuf) -> Self {
        let entries = std::fs::read(&kept_in)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            entries: Mutex::new(entries),
            kept_in,
        }
    }

    fn keep_on_disk(&self, entries: &[Entry]) -> EngineResult<()> {
        if let Some(dir) = self.kept_in.parent() {
            create_private_dir_all(dir)?;
        }
        write_private_file(&self.kept_in, &serde_json::to_vec_pretty(entries)?)?;
        Ok(())
    }

    fn entry(&self, name: &str) -> EngineResult<Entry> {
        let mut entries = lock(&self.entries);
        if let Some(entry) = entries.iter().find(|entry| entry.name == name) {
            return Ok(entry.clone());
        }
        // Two names that start alike never share a task: the second gets `-2`, as the
        // runner's own names do.
        let base = format!("task-{}", labels::id_slug(name, 6));
        let mut id = base.clone();
        for n in 2.. {
            if !entries.iter().any(|entry| entry.task.as_str() == id) {
                break;
            }
            id = format!("{base}-{n}");
        }
        let task = TaskId::new(id).map_err(|err| EngineError::Refused(err.to_string()))?;
        let entry = Entry {
            name: name.to_owned(),
            task,
            turn: Arc::default(),
            accepted: None,
            dropped: false,
            finished: Vec::new(),
            sent_back: 0,
        };
        entries.push(entry.clone());
        self.keep_on_disk(&entries)?;
        Ok(entry)
    }

    fn known(&self, name: &str) -> Option<Entry> {
        lock(&self.entries)
            .iter()
            .find(|entry| entry.name == name)
            .cloned()
    }

    /// A call's turn at a task is over. Its work makes the task's last verdict stale.
    fn worked(&self, task: &TaskId, call: u32) -> EngineResult<()> {
        let mut entries = lock(&self.entries);
        for entry in entries.iter_mut() {
            if entry.task == *task {
                if entry.accepted == Some(false) {
                    entry.sent_back += 1;
                }
                entry.accepted = None;
                entry.finished.push(call);
            }
        }
        self.keep_on_disk(&entries)
    }

    /// The judge kept one of a step's tries, so the others are never merged.
    pub fn keep(&self, kept: &Kept) -> EngineResult<()> {
        // The tries of one step share their label up to the try's number.
        let step = |label: &str| label.rsplit_once(':').map(|(step, _)| step.to_owned());
        let Some(kept_step) = step(&kept.label) else {
            return Ok(());
        };
        let mut entries = lock(&self.entries);
        for entry in entries.iter_mut() {
            if step(&entry.name).as_deref() == Some(kept_step.as_str()) {
                entry.dropped = entry.name != kept.label;
            }
        }
        self.keep_on_disk(&entries)
    }
}

/// Who built the task the call is about, once someone has.
pub(super) fn author(bridge: &Bridge<'_>, call: &AgentCall) -> Option<ModelRef> {
    let entry = bridge.tasks.known(call.task.as_deref()?)?;
    let record = record_of(bridge.host, &entry.task).ok()?;
    Some(worker_of(bridge.host, &record))
}

/// Who built each of the script's tasks so far, once each.
pub(super) fn authors(bridge: &Bridge<'_>) -> Vec<ModelRef> {
    let tasks: Vec<TaskId> = lock(&bridge.tasks.entries)
        .iter()
        .filter(|entry| !entry.dropped)
        .map(|entry| entry.task.clone())
        .collect();
    let mut authors = Vec::new();
    for task in tasks {
        let Ok(record) = record_of(bridge.host, &task) else {
            continue;
        };
        let author = worker_of(bridge.host, &record);
        if !authors.contains(&author) {
            authors.push(author);
        }
    }
    authors
}

fn name_of(call: &AgentCall) -> String {
    match &call.task {
        Some(task) => task.clone(),
        None if call.worktree => call.label.clone(),
        None => THE_REQUEST.to_owned(),
    }
}

/// Whether a worker whose call runs in `phase` may run a workflow of its own, a level of
/// nesting `workflows.max_depth` allows: one further in than the worker, and a worker of a
/// workflow inside the run's, whose calls run in a group named for that workflow, is a level
/// in already.
fn may_nest(phase: Option<&str>, max_depth: u32) -> bool {
    let levels_in = u32::from(phase.is_some_and(|phase| phase.starts_with(CHILD_GROUP)));
    levels_in < max_depth
}

/// One writer's turn at its task: the task's first build, or the next round on it.
pub(super) async fn work(
    bridge: &Bridge<'_>,
    call: &AgentCall,
    worker: &ModelRef,
) -> EngineResult<AgentEnd> {
    let host = bridge.host;
    let at = &bridge.call.at;
    let entry = bridge.tasks.entry(&name_of(call))?;
    // Before its own turn, so two tasks that each name the other never wait on each other.
    let depends_on = if host.read(|state| state.tasks.get(&entry.task).is_none()) {
        match merge_first(bridge, call, at).await? {
            Ok(merged) => merged,
            Err(skipped) => return Ok(AgentEnd::failed(skipped)),
        }
    } else {
        Vec::new()
    };
    let _turn = entry.turn.lock().await;
    let task = &entry.task;
    let nests = call.own_workflow
        || call
            .phase
            .as_ref()
            .is_some_and(|phase| bridge.own_workflow_in.contains(phase));
    let max_depth = host.settings().workflows.max_depth;
    if nests && may_nest(call.phase.as_deref(), max_depth) {
        host.shared().let_run_its_own_workflow(task);
    }
    let already = entry.finished.contains(&call.index);
    let rounds = host.settings().review.max_change_rounds;
    if !already && entry.accepted == Some(false) && entry.sent_back >= rounds {
        let mut end = AgentEnd::answered(Value::Null);
        end.result = Err(format!(
            "{} was sent back {} already, as many rounds of fixes as review.max_change_rounds \
             allows, so it comes to the user with the reviewers' findings",
            call.label,
            times(entry.sent_back)
        ));
        return Ok(end);
    }
    let state = if host.read(|state| state.tasks.get(task).is_none()) {
        // The brief is the call's words, under the name of whoever wrote them.
        crate::prompts::instructions::brief_written_by(host, task, bridge.author(call))?;
        task::create(host, spec(host, call, task, worker, depends_on))?;
        // A step kept going `until` a condition has its task worked as that goal from the
        // writer's first turn.
        if let Some(until) = call.until.as_deref().filter(|_| call.writes) {
            let step = call.phase.as_deref();
            let max_rounds = step.and_then(|step| bridge.max_rounds.get(&Some(step.to_owned())));
            super::goal::work_as(
                host,
                until,
                step,
                task,
                call.kept_by.as_deref(),
                max_rounds.copied(),
            )?;
        }
        pipeline::drive(host, task, at, Until::ChecksPassed).await?
    } else {
        match state_of(host, task)? {
            // This very call did its work before the run stopped, so it isn't done twice.
            state @ (TaskState::InReview | TaskState::Merged) if already => state,
            TaskState::InReview => pipeline::revise(host, task, at, &call.prompt).await?,
            // A goal's next round on work the run already merged.
            TaskState::Merged => pipeline::send_back(host, task, at, &call.prompt).await?,
            // The run stopped mid-task, and the task picks up where it was.
            _ => pipeline::drive(host, task, at, Until::ChecksPassed).await?,
        }
    };
    if !already && matches!(state, TaskState::InReview | TaskState::Merged) {
        bridge.tasks.worked(task, call.index)?;
    }
    let record = record_of(host, task)?;
    let session = host.read(|state| newest_worker_session(state, task).cloned());
    let mut end = match &session {
        Some(session) => {
            bridge.ran(call, &session.key)?;
            let done = session.end.as_ref();
            AgentEnd {
                result: Ok(Value::Null),
                model: Some(roles::display_name(&session.started.worker)),
                tokens: done.map_or(0, |end| end.spend.input_tokens + end.spend.output_tokens),
                tool_calls: done.map_or(0, |end| u32::try_from(end.tool_calls).unwrap_or(u32::MAX)),
                duration_ms: done.map_or(0, |end| end.duration_ms),
            }
        }
        None => AgentEnd::answered(Value::Null),
    };
    end.result = match state {
        TaskState::InReview | TaskState::Merged => {
            let said = record
                .reports
                .last()
                .map(|report| report.summary.trim().to_owned())
                .filter(|summary| !summary.is_empty())
                .unwrap_or_else(|| "done".to_owned());
            Ok(Value::String(said))
        }
        other => Err(format!(
            "{} stopped before its checks passed: it is {}",
            call.label,
            argonauts_core::text::state_words(&format!("{other:?}").to_lowercase())
        )),
    };
    Ok(end)
}

fn spec(
    host: &Host,
    call: &AgentCall,
    task: &TaskId,
    worker: &ModelRef,
    depends_on: Vec<TaskId>,
) -> TaskSpec {
    let criteria = host.read(|state| {
        state
            .goal
            .approved
            .as_ref()
            .map(|goal| goal.criteria.iter().map(|c| c.id.clone()).collect())
            .unwrap_or_default()
    });
    let item = call
        .task
        .clone()
        .unwrap_or_else(|| labels::slug(&call.label, 3));
    TaskSpec {
        id: task.clone(),
        title: call.label.clone(),
        task: call.prompt.clone(),
        kind: TaskKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on,
        criteria,
        worker: Some(worker.clone()),
        // The script's own steps review it.
        reviewers: Vec::new(),
        limit: TaskLimit::default(),
        predictions: None,
        item: Some(item),
    }
}

/// The worktree and the change of the task a reader's call is about, when the task has one.
pub(super) async fn change_of(
    bridge: &Bridge<'_>,
    call: &AgentCall,
) -> EngineResult<Option<(PathBuf, Change)>> {
    let Some(entry) = call
        .task
        .as_deref()
        .and_then(|task| bridge.tasks.known(task))
    else {
        return Ok(None);
    };
    let host = bridge.host;
    if host.read(|state| state.tasks.get(&entry.task).is_none()) {
        return Ok(None);
    }
    let tree = worktree::ensure(host, &entry.task).await?;
    let git = worktree::tree_git(host, &tree.path).await?;
    let change = match git.head_commit(&tree.path).await? {
        Some(head) => git.review_diff(&tree.path, &tree.base, &head).await?,
        None => String::new(),
    };
    Ok(Some((tree.path, Change::of_one(change))))
}

/// A task of the script's with committed work: its worktree, its newest commit and its change.
struct Changed {
    name: String,
    tree: worktree::TaskTree,
    head: String,
    change: String,
}

/// Every task with a change so far, in the order the script made them.
async fn changed_tasks(bridge: &Bridge<'_>) -> EngineResult<Vec<Changed>> {
    let entries: Vec<(String, TaskId)> = lock(&bridge.tasks.entries)
        .iter()
        .filter(|entry| !entry.dropped)
        .map(|entry| (entry.name.clone(), entry.task.clone()))
        .collect();
    let host = bridge.host;
    let mut changed = Vec::new();
    for (name, task) in entries {
        if host.read(|state| state.tasks.get(&task).is_none()) {
            continue;
        }
        let tree = worktree::ensure(host, &task).await?;
        let git = worktree::tree_git(host, &tree.path).await?;
        let Some(head) = git.head_commit(&tree.path).await? else {
            continue;
        };
        let change = git.review_diff(&tree.path, &tree.base, &head).await?;
        if !change.trim().is_empty() {
            changed.push(Changed {
                name,
                tree,
                head,
                change,
            });
        }
    }
    Ok(changed)
}

/// What a reviewer or a judge about no one task reads: every task's change so far, each under
/// its name, as they will merge into the run. `None` before any task has a change.
pub(super) async fn combined_change(bridge: &Bridge<'_>) -> EngineResult<Option<Change>> {
    let changes: Vec<(String, String)> = changed_tasks(bridge)
        .await?
        .into_iter()
        .map(|changed| {
            let name = if changed.name == THE_REQUEST {
                "the request".to_owned()
            } else {
                changed.name
            };
            (name, changed.change)
        })
        .collect();
    Ok(Change::of_tasks(changes))
}

/// The folder a reader or a check about no one task works in, so the files it reads and the
/// tests it runs hold the change it judges.
pub(super) struct WholeTree {
    pub(super) path: PathBuf,
    /// Made for this one use, so it goes when the use is over.
    made: bool,
}

impl WholeTree {
    /// Takes away a worktree made for the use. A task's own worktree stays.
    pub(super) async fn done(self, bridge: &Bridge<'_>) {
        if self.made {
            remove_tree(bridge.host, &self.path).await;
        }
    }
}

/// What a reader or a check about no one task works on.
pub(super) enum Whole {
    /// No task has a change yet.
    Nothing,
    Tree(WholeTree),
    /// The tasks' changes don't merge cleanly, so no one folder holds them all. Says which
    /// tasks and files.
    Conflict(String),
}

/// The run's whole change on disk: the worktree of the one task with a change, else a fresh
/// worktree off the run with every changed task merged in, named for the call (`index`).
pub(super) async fn whole_tree(bridge: &Bridge<'_>, index: u32) -> EngineResult<Whole> {
    let changed = changed_tasks(bridge).await?;
    let (first, rest) = match changed.as_slice() {
        [] => return Ok(Whole::Nothing),
        [only] => {
            return Ok(Whole::Tree(WholeTree {
                path: only.tree.path.clone(),
                made: false,
            }));
        }
        [first, rest @ ..] => (first, rest),
    };
    let host = bridge.host;
    let path = host.repo().worktrees.join(format!("whole-change-{index}"));
    // A folder left by a run that stopped mid-call is made again from the newest work.
    remove_tree(host, &path).await;
    host.git()
        .add_detached_worktree(&host.repo().root, &path, &first.head)
        .await?;
    let tree = WholeTree { path, made: true };
    let git = worktree::tree_git(host, &tree.path).await?;
    let mut taken = vec![first.name.as_str()];
    for task in rest {
        let message = format!("Take in {}", task.name);
        let merged = git.merge_no_ff(&tree.path, &task.head, &message).await;
        match merged {
            Ok(crate::git::MergeOutcome::Merged(_)) => taken.push(&task.name),
            Ok(crate::git::MergeOutcome::Conflict(files)) => {
                tree.done(bridge).await;
                return Ok(Whole::Conflict(format!(
                    "{} and {} don't merge cleanly: {}",
                    taken.join(", "),
                    task.name,
                    files.join(", ")
                )));
            }
            Err(err) => {
                tree.done(bridge).await;
                return Err(err.into());
            }
        }
    }
    Ok(Whole::Tree(tree))
}

async fn remove_tree(host: &Host, path: &std::path::Path) {
    if host
        .git()
        .remove_worktree(&host.repo().root, path)
        .await
        .is_err()
    {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// The task's worktree, once it has one.
pub(super) async fn tree_of(
    bridge: &Bridge<'_>,
    task: Option<&str>,
) -> EngineResult<Option<PathBuf>> {
    let Some(entry) = task.and_then(|task| bridge.tasks.known(task)) else {
        return Ok(None);
    };
    if bridge
        .host
        .read(|state| state.tasks.get(&entry.task).is_none())
    {
        return Ok(None);
    }
    Ok(Some(worktree::ensure(bridge.host, &entry.task).await?.path))
}

/// What the task's checks said after its last build, one line each, newest per check:
/// `tests passed: 9 passed, 0 failed`. `None` before any ran.
pub(super) fn checks_of(bridge: &Bridge<'_>, name: Option<&str>) -> Option<String> {
    let task = task_of(bridge, name)?;
    let checks = bridge
        .host
        .read(|state| state.tasks.get(&task).map(|record| record.checks.clone()))?;
    checks_text(&checks)
}

/// What the checks said for every task the script's writers worked on, each line under its
/// task, for a call about no one task, such as the judge who signs off the run.
pub(super) fn all_checks(bridge: &Bridge<'_>) -> Option<String> {
    let entries: Vec<(String, TaskId)> = lock(&bridge.tasks.entries)
        .iter()
        .filter(|entry| !entry.dropped)
        .map(|entry| (entry.name.clone(), entry.task.clone()))
        .collect();
    let lines: Vec<String> = entries
        .iter()
        .flat_map(|(name, task)| {
            let checks = bridge
                .host
                .read(|state| state.tasks.get(task).map(|record| record.checks.clone()))
                .unwrap_or_default();
            let name = if name == THE_REQUEST {
                "the request"
            } else {
                name.as_str()
            };
            check_lines(&checks)
                .into_iter()
                .map(move |line| format!("{name}: {line}"))
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "Argonauts Code ran these after each build, in its task's worktree:\n{}",
        lines.join("\n")
    ))
}

fn checks_text(checks: &[CheckOutcome]) -> Option<String> {
    let lines = check_lines(checks);
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "Argonauts Code ran these after the build, in the task's worktree:\n{}",
        lines.join("\n")
    ))
}

/// Each check's newest result in one line, such as `tests (node --test) passed: 9 pass`.
fn check_lines(checks: &[CheckOutcome]) -> Vec<String> {
    let mut newest: Vec<&CheckOutcome> = Vec::new();
    for check in checks.iter().filter(|check| !check.skipped) {
        match newest.iter_mut().find(|seen| seen.name == check.name) {
            Some(seen) => *seen = check,
            None => newest.push(check),
        }
    }
    newest
        .into_iter()
        .map(crate::task::checks::result_line)
        .collect()
}

/// The task the script's `task` is worked on, once a writer took it.
pub(super) fn task_of(bridge: &Bridge<'_>, task: Option<&str>) -> Option<TaskId> {
    bridge.tasks.known(task?).map(|entry| entry.task)
}

/// A reader's answer about a task that says whether it accepts it is the script's verdict. It
/// is recorded as a review of the task under the name of whoever gave it, so the merge
/// approval shows who approved what and your merge rules can weigh it.
pub(super) fn judged(
    bridge: &Bridge<'_>,
    call: &AgentCall,
    ran: &Ran,
    answer: &Value,
) -> EngineResult<()> {
    let Some(accept) = answer.get("accept").and_then(Value::as_bool) else {
        return Ok(());
    };
    // A call about no one task, as a run-wide sign-off is, judged the run's whole change, so
    // its verdict is every task's.
    let judged: Vec<(TaskId, u32)> = {
        let mut entries = lock(&bridge.tasks.entries);
        let mut judged = Vec::new();
        for entry in entries
            .iter_mut()
            .filter(|entry| match call.task.as_deref() {
                Some(name) => entry.name == name,
                None => !entry.dropped,
            })
        {
            // One reviewer who asks for a change is enough to send it back.
            entry.accepted = Some(accept && entry.accepted != Some(false));
            // A review counts for the build it read, so its round is how often it was built.
            let round = u32::try_from(entry.finished.len())
                .unwrap_or(u32::MAX)
                .max(1);
            judged.push((entry.task.clone(), round));
        }
        if judged.is_empty() {
            return Ok(());
        }
        bridge.tasks.keep_on_disk(&entries)?;
        judged
    };
    let host = bridge.host;
    for (task, round) in judged {
        if host.read(|state| state.tasks.get(&task).is_none()) {
            continue;
        }
        let review = ReviewEvent::Reviewed {
            reviewer: ran.worker.clone(),
            blind: false,
            round,
            review: review_of(accept, answer),
        };
        host.record(Some(&task), Some(&ran.key), RunEventKind::Review(review))?;
    }
    Ok(())
}

/// A script's verdict in the shape of a review: what it found is its comments.
fn review_of(accept: bool, answer: &Value) -> ReviewOutput {
    let findings: Vec<&str> = answer
        .get("findings")
        .and_then(Value::as_array)
        .map(|findings| findings.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let summary = answer
        .get("summary")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| match (accept, findings.len()) {
            (true, _) => "Accepted".to_owned(),
            (false, 0) => "Not accepted".to_owned(),
            (false, 1) => "Not accepted: 1 finding".to_owned(),
            (false, many) => format!("Not accepted: {many} findings"),
        });
    ReviewOutput {
        verdict: if accept {
            ReviewVerdict::Approve
        } else {
            ReviewVerdict::RequestChanges
        },
        summary,
        comments: findings
            .into_iter()
            .map(|text| ReviewComment {
                path: String::new(),
                line: None,
                // What keeps a task from being accepted blocks it.
                severity: if accept {
                    Severity::Low
                } else {
                    Severity::High
                },
                text: text.to_owned(),
            })
            .collect(),
        tamper_suspected: false,
    }
}

/// The script is over: what its reviews accepted, or nobody reviewed, is merged into the run.
/// A task its reviews never accepted is yours to decide, and a try the judge didn't keep
/// stays on its branch. Returns how many tasks were merged.
pub(super) async fn finish(bridge: &Bridge<'_>, at: &StepAt) -> EngineResult<usize> {
    let host = bridge.host;
    let entries = lock(&bridge.tasks.entries).clone();
    let mut merged = 0;
    for entry in entries {
        if entry.dropped || host.read(|state| state.tasks.get(&entry.task).is_none()) {
            continue;
        }
        if settle(bridge, &entry, at).await? == TaskState::Merged {
            merged += 1;
        }
    }
    Ok(merged)
}

/// Takes the script's verdict on a built task to its end: accepted work merges into the run,
/// and work the reviewers didn't accept goes to its next round or to you.
async fn settle(bridge: &Bridge<'_>, entry: &Entry, at: &StepAt) -> EngineResult<TaskState> {
    let host = bridge.host;
    let task = &entry.task;
    if state_of(host, task)? == TaskState::InReview {
        if entry.accepted == Some(false) {
            let what = match entry.sent_back {
                0 => "the workflow's reviewers didn't accept it".to_owned(),
                n => format!(
                    "the workflow's reviewers didn't accept it after {}",
                    rounds_of_fixes(n)
                ),
            };
            let why = with_findings(what, &newest_findings(host, task));
            pipeline::transition(host, task, Trigger::ChangesRequestedAgain, &why)?;
        } else {
            let why = "the workflow's own steps accepted it";
            pipeline::transition(host, task, Trigger::Approved, why)?;
        }
    }
    pipeline::run_task(host, task, at).await
}

/// Merges into the run each task the call's task comes after, once the script accepted it,
/// so the task's worktree starts from that work and its change leaves it out. The tasks
/// merged, or why the call is skipped: a task it comes after that nobody accepted. A name
/// the script never gave a task waits for nothing.
async fn merge_first(
    bridge: &Bridge<'_>,
    call: &AgentCall,
    at: &StepAt,
) -> EngineResult<Result<Vec<TaskId>, String>> {
    let host = bridge.host;
    let mut merged = Vec::new();
    for name in &call.after {
        let Some(entry) = bridge.tasks.known(name) else {
            continue;
        };
        // Its writer may still be at work on it.
        let turn = entry.turn.clone();
        let _turn = turn.lock().await;
        let Some(entry) = bridge.tasks.known(name) else {
            continue;
        };
        let built = host.read(|state| state.tasks.get(&entry.task).is_some());
        let why = match built.then(|| state_of(host, &entry.task)).transpose()? {
            Some(TaskState::Merged) => None,
            Some(TaskState::InReview) if !entry.dropped && entry.accepted != Some(false) => {
                unmerged_why(settle(bridge, &entry, at).await?)
            }
            _ => Some("which wasn't accepted"),
        };
        if let Some(why) = why {
            return Ok(Err(format!("{SKIPPED} it comes after {name}, {why}")));
        }
        merged.push(entry.task.clone());
    }
    Ok(Ok(merged))
}

/// Why an accepted task the call comes after isn't in the run, by where its merge left it.
/// `None` once it merged.
fn unmerged_why(state: TaskState) -> Option<&'static str> {
    match state {
        TaskState::Merged => None,
        TaskState::NeedsYou | TaskState::NeedsDecision | TaskState::Blocked => {
            Some("which couldn't merge into the run and waits for you")
        }
        _ => Some("which couldn't merge into the run"),
    }
}

/// What the task's newest round of reviews asked for, in the reviewers' words: their findings,
/// or the summary of one who gave none.
fn newest_findings(host: &Host, task: &TaskId) -> Verdict {
    let reviews = record_of(host, task)
        .map(|record| record.reviews)
        .unwrap_or_default();
    let reviewed = || {
        reviews.iter().filter_map(|event| match event {
            ReviewEvent::Reviewed { round, review, .. } => Some((*round, review)),
            _ => None,
        })
    };
    let newest = reviewed().map(|(round, _)| round).max();
    let comments = reviewed()
        .filter(|(round, review)| {
            Some(*round) == newest && review.verdict != ReviewVerdict::Approve
        })
        .flat_map(|(_, review)| {
            if review.comments.is_empty() {
                vec![review.summary.clone()]
            } else {
                review.comments.iter().map(|c| c.text.clone()).collect()
            }
        })
        .collect();
    Verdict::ChangesRequested { comments }
}

fn times(n: u32) -> String {
    match n {
        1 => "once".to_owned(),
        2 => "twice".to_owned(),
        n => format!("{n} times"),
    }
}

fn rounds_of_fixes(n: u32) -> String {
    match n {
        1 => "1 round of fixes".to_owned(),
        n => format!("{n} rounds of fixes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, passed: bool, summary: &str) -> CheckOutcome {
        CheckOutcome {
            name: name.to_owned(),
            kind: argonauts_core::events::task::CheckKind::Command,
            passed,
            command: Some(vec!["node".to_owned(), "--test".to_owned()]),
            exit_code: Some(i32::from(!passed)),
            duration_ms: 900,
            summary: summary.to_owned(),
            output_tail: String::new(),
            violations: Vec::new(),
            sandboxed: true,
            skipped: false,
        }
    }

    #[test]
    fn an_accepted_task_that_didnt_merge_says_why_its_dependents_wait() {
        assert_eq!(unmerged_why(TaskState::Merged), None);
        assert_eq!(
            unmerged_why(TaskState::NeedsYou),
            Some("which couldn't merge into the run and waits for you")
        );
        assert_eq!(
            unmerged_why(TaskState::Failed),
            Some("which couldn't merge into the run")
        );
    }

    #[test]
    fn a_reviewer_reads_the_newest_result_of_each_check() {
        let checks = [
            check("tests", false, "8 pass, 1 fail"),
            check("tests", true, "9 pass, 0 fail"),
        ];
        assert_eq!(
            checks_text(&checks).as_deref(),
            Some(
                "Argonauts Code ran these after the build, in the task's worktree:\n\
                 tests (node --test) passed: 9 pass, 0 fail"
            )
        );
        assert_eq!(checks_text(&[]), None);
    }

    /// A worker's own workflow is a level inside the run, and a worker already inside a
    /// workflow inside the run's starts without one unless two levels are allowed.
    #[test]
    fn a_worker_runs_its_own_workflow_only_as_deep_as_the_settings_allow() {
        assert!(may_nest(Some("Build"), 1));
        assert!(may_nest(None, 1));
        assert!(!may_nest(Some("Build"), 0));
        assert!(!may_nest(Some("▸ lint"), 1));
        assert!(may_nest(Some("▸ lint"), 2));
    }

    #[test]
    fn two_names_that_start_alike_get_tasks_of_their_own() {
        let dir = tempfile::tempdir().unwrap();
        let book = Book::open(dir.path().join("tasks.json"));
        let first = book
            .entry("migrate-user-table-to-new-schema-step-1")
            .unwrap();
        let second = book
            .entry("migrate-user-table-to-new-schema-step-2")
            .unwrap();
        assert_eq!(first.task.as_str(), "task-migrate-user-table-to-new-schema");
        assert_eq!(
            second.task.as_str(),
            "task-migrate-user-table-to-new-schema-2"
        );
        // A name seen before keeps its task.
        let again = book
            .entry("migrate-user-table-to-new-schema-step-2")
            .unwrap();
        assert_eq!(again.task, second.task);
        // A name in another script still makes a task.
        let chinese = book.entry("修復登入頁面").unwrap();
        assert_eq!(chinese.task.as_str(), "task-task");
    }
}
