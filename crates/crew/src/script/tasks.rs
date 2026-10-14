//! The work a script's writers do. Each piece of work is a task of the engine's: a worktree and
//! branch of its own, its worker's session, a commit when the session ends, and its checks. The
//! script's own steps review it, so the engine's review stays out, and what they accepted is
//! merged into the run when the script ends.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{
    CardBudget, CardKind, CardSpec, CardState, GateOutcome, ReviewEvent,
};
use argonauts_core::labels;
use argonauts_core::names::WorkerRef;
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::steps::judgment::{ReviewComment, ReviewOutput, ReviewVerdict};
use argonauts_core::steps::plan::Severity;
use argonauts_runner::{AgentCall, AgentEnd, Kept};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::bridge::{Bridge, lock, shown_change};
use crate::card::pipeline::{self, Until};
use crate::card::support::{newest_owner_session, owner_of, record_of, state_of};
use crate::card::{self, machine::Trigger, worktree};
use crate::engine::host::{Host, Ran};
use crate::error::{EngineError, EngineResult};
use crate::roles;
use crate::workflow::StepAt;

/// What a call works on when it names no task and asks for no worktree of its own: the
/// request, which every such writer shares.
const THE_REQUEST: &str = "the-request";

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
    card: CardId,
    /// Writers on one task take turns in its worktree.
    #[serde(skip)]
    turn: Arc<tokio::sync::Mutex<()>>,
    /// The newest verdict of the script's own review, when it gave one.
    accepted: Option<bool>,
    /// One of several attempts the judge didn't keep.
    dropped: bool,
    /// The calls, by their place in the script, whose turn at the task is over. A run that
    /// stopped before such a call's answer was saved asks it again, and its work is done.
    #[serde(default)]
    finished: Vec<u32>,
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
            if !entries.iter().any(|entry| entry.card.as_str() == id) {
                break;
            }
            id = format!("{base}-{n}");
        }
        let card = CardId::new(id).map_err(|err| EngineError::Refused(err.to_string()))?;
        let entry = Entry {
            name: name.to_owned(),
            card,
            turn: Arc::default(),
            accepted: None,
            dropped: false,
            finished: Vec::new(),
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
    fn worked(&self, card: &CardId, call: u32) -> EngineResult<()> {
        let mut entries = lock(&self.entries);
        for entry in entries.iter_mut() {
            if entry.card == *card {
                entry.accepted = None;
                entry.finished.push(call);
            }
        }
        self.keep_on_disk(&entries)
    }

    /// The judge kept one seat of a step's attempts, so the others are never merged.
    pub fn keep(&self, kept: &Kept) -> EngineResult<()> {
        // The attempts of one step share their label up to the seat's number.
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
pub(super) fn author(bridge: &Bridge<'_>, call: &AgentCall) -> Option<WorkerRef> {
    let entry = bridge.tasks.known(call.task.as_deref()?)?;
    let record = record_of(bridge.host, &entry.card).ok()?;
    Some(owner_of(bridge.host, &record))
}

fn name_of(call: &AgentCall) -> String {
    match &call.task {
        Some(task) => task.clone(),
        None if call.worktree => call.label.clone(),
        None => THE_REQUEST.to_owned(),
    }
}

/// One writer's turn at its task: the task's first build, or the next round on it.
pub(super) async fn work(
    bridge: &Bridge<'_>,
    call: &AgentCall,
    worker: &WorkerRef,
) -> EngineResult<AgentEnd> {
    let host = bridge.host;
    let at = &bridge.call.at;
    let entry = bridge.tasks.entry(&name_of(call))?;
    let _turn = entry.turn.lock().await;
    let card = &entry.card;
    // A worker's own workflow is a level of nesting, which `workflows.max_depth` allows.
    let nests = call.own_workflow
        || call
            .phase
            .as_ref()
            .is_some_and(|phase| bridge.own_workflow_in.contains(phase));
    if nests && host.settings().workflows.max_depth > 1 {
        host.shared().let_run_its_own_workflow(card);
    }
    let already = entry.finished.contains(&call.index);
    let state = if host.read(|state| state.cards.get(card).is_none()) {
        // The brief is the call's words, under the name of whoever wrote them.
        crate::prompts::instructions::brief_written_by(host, card, bridge.author(call))?;
        card::create(host, spec(host, call, card, worker))?;
        // A step kept going `until` a condition has its task worked as that goal from the
        // writer's first turn.
        if let Some(until) = call.until.as_deref().filter(|_| call.writes) {
            let step = call.phase.as_deref();
            let max_rounds = step.and_then(|step| bridge.max_rounds.get(&Some(step.to_owned())));
            super::goal::work_as(
                host,
                until,
                step,
                card,
                call.kept_by.as_deref(),
                max_rounds.copied(),
            )?;
        }
        pipeline::drive(host, card, at, Until::GatesPassed).await?
    } else {
        match state_of(host, card)? {
            // This very call did its work before the run stopped, so it isn't done twice.
            state @ (CardState::InReview | CardState::Integrated) if already => state,
            CardState::InReview => pipeline::revise(host, card, at, &call.prompt).await?,
            // A goal's next round on work the run already merged.
            CardState::Integrated => pipeline::send_back(host, card, at, &call.prompt).await?,
            // The run stopped mid-task, and the task picks up where it was.
            _ => pipeline::drive(host, card, at, Until::GatesPassed).await?,
        }
    };
    if !already && matches!(state, CardState::InReview | CardState::Integrated) {
        bridge.tasks.worked(card, call.index)?;
    }
    let record = record_of(host, card)?;
    let session = host.read(|state| newest_owner_session(state, card).cloned());
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
        CardState::InReview | CardState::Integrated => {
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

fn spec(host: &Host, call: &AgentCall, card: &CardId, worker: &WorkerRef) -> CardSpec {
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
    CardSpec {
        id: card.clone(),
        title: call.label.clone(),
        task: call.prompt.clone(),
        kind: CardKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria,
        owner: Some(worker.clone()),
        // The script's own steps review it.
        reviewers: Vec::new(),
        budget: CardBudget::default(),
        predictions: None,
        item: Some(item),
    }
}

/// The worktree and the change of the task a reader's call is about, when the task has one.
pub(super) async fn change_of(
    bridge: &Bridge<'_>,
    call: &AgentCall,
) -> EngineResult<Option<(PathBuf, String)>> {
    let Some(entry) = call
        .task
        .as_deref()
        .and_then(|task| bridge.tasks.known(task))
    else {
        return Ok(None);
    };
    let host = bridge.host;
    if host.read(|state| state.cards.get(&entry.card).is_none()) {
        return Ok(None);
    }
    let tree = worktree::ensure(host, &entry.card).await?;
    let git = worktree::tree_git(host, &tree.path).await?;
    let change = match git.head_commit(&tree.path).await? {
        Some(head) => git.review_diff(&tree.path, &tree.base, &head).await?,
        None => String::new(),
    };
    Ok(Some((tree.path, shown_change(change))))
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
        .read(|state| state.cards.get(&entry.card).is_none())
    {
        return Ok(None);
    }
    Ok(Some(worktree::ensure(bridge.host, &entry.card).await?.path))
}

/// What the task's checks said after its last build, one line each, newest per check:
/// `tests passed: 9 passed, 0 failed`. `None` before any ran.
pub(super) fn checks_of(bridge: &Bridge<'_>, task: Option<&str>) -> Option<String> {
    let card = card_of(bridge, task)?;
    let gates = bridge
        .host
        .read(|state| state.cards.get(&card).map(|record| record.gates.clone()))?;
    checks_text(&gates)
}

/// What the checks said for every task the script's writers worked on, each line under its
/// task, for a call about no one task, such as the judge who signs off the run.
pub(super) fn all_checks(bridge: &Bridge<'_>) -> Option<String> {
    let tasks: Vec<(String, CardId)> = lock(&bridge.tasks.entries)
        .iter()
        .filter(|entry| !entry.dropped)
        .map(|entry| (entry.name.clone(), entry.card.clone()))
        .collect();
    let lines: Vec<String> = tasks
        .iter()
        .flat_map(|(name, card)| {
            let gates = bridge
                .host
                .read(|state| state.cards.get(card).map(|record| record.gates.clone()))
                .unwrap_or_default();
            let name = if name == THE_REQUEST {
                "the request"
            } else {
                name.as_str()
            };
            gate_lines(&gates)
                .into_iter()
                .map(move |line| format!("{name}: {line}"))
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "Argonauts ran these after each build, in its task's worktree:\n{}",
        lines.join("\n")
    ))
}

fn checks_text(gates: &[GateOutcome]) -> Option<String> {
    let lines = gate_lines(gates);
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "Argonauts ran these after the build, in the task's worktree:\n{}",
        lines.join("\n")
    ))
}

/// Each check's newest result in one line, such as `tests (node --test) passed: 9 pass`.
fn gate_lines(gates: &[GateOutcome]) -> Vec<String> {
    let mut newest: Vec<&GateOutcome> = Vec::new();
    for gate in gates.iter().filter(|gate| !gate.skipped) {
        match newest.iter_mut().find(|seen| seen.name == gate.name) {
            Some(seen) => *seen = gate,
            None => newest.push(gate),
        }
    }
    newest
        .iter()
        .map(|gate| {
            let result = if gate.passed { "passed" } else { "failed" };
            let ran = gate
                .command
                .as_ref()
                .map(|command| format!(" ({})", command.join(" ")))
                .unwrap_or_default();
            format!("{}{ran} {result}: {}", gate.name, gate.summary.trim())
        })
        .collect()
}

/// The card the script's `task` is worked on, once a writer took it.
pub(super) fn card_of(bridge: &Bridge<'_>, task: Option<&str>) -> Option<CardId> {
    bridge.tasks.known(task?).map(|entry| entry.card)
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
    let (Some(task), Some(accept)) = (
        call.task.as_deref(),
        answer.get("accept").and_then(Value::as_bool),
    ) else {
        return Ok(());
    };
    let (card, round) = {
        let mut entries = lock(&bridge.tasks.entries);
        let Some(entry) = entries.iter_mut().find(|entry| entry.name == task) else {
            return Ok(());
        };
        // One reviewer who asks for a change is enough to send it back.
        entry.accepted = Some(accept && entry.accepted != Some(false));
        // A review counts for the build it read, so its round is how often the task was built.
        let round = u32::try_from(entry.finished.len())
            .unwrap_or(u32::MAX)
            .max(1);
        let card = entry.card.clone();
        bridge.tasks.keep_on_disk(&entries)?;
        (card, round)
    };
    let host = bridge.host;
    if host.read(|state| state.cards.get(&card).is_none()) {
        return Ok(());
    }
    let review = ReviewEvent::Reviewed {
        reviewer: ran.worker.clone(),
        blind: false,
        round,
        review: review_of(accept, answer),
    };
    host.record(Some(&card), Some(&ran.key), RunEventKind::Review(review))?;
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
/// A task its reviews never accepted is yours to decide, and an attempt the judge didn't keep
/// stays on its branch. Returns how many tasks were merged.
pub(super) async fn finish(bridge: &Bridge<'_>, at: &StepAt) -> EngineResult<usize> {
    let host = bridge.host;
    let entries = lock(&bridge.tasks.entries).clone();
    let mut merged = 0;
    for entry in entries {
        let card = &entry.card;
        if entry.dropped || host.read(|state| state.cards.get(card).is_none()) {
            continue;
        }
        if state_of(host, card)? == CardState::InReview {
            if entry.accepted == Some(false) {
                let why = "the workflow's review never accepted it";
                pipeline::transition(host, card, Trigger::ChangesRequestedAgain, why)?;
            } else {
                let why = "the workflow's own steps accepted it";
                pipeline::transition(host, card, Trigger::Approved, why)?;
            }
        }
        if pipeline::run_card(host, card, at).await? == CardState::Integrated {
            merged += 1;
        }
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(name: &str, passed: bool, summary: &str) -> GateOutcome {
        GateOutcome {
            name: name.to_owned(),
            kind: argonauts_core::events::card::GateKind::Command,
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
    fn a_reviewer_reads_the_newest_result_of_each_check() {
        let gates = [
            gate("tests", false, "8 pass, 1 fail"),
            gate("tests", true, "9 pass, 0 fail"),
        ];
        assert_eq!(
            checks_text(&gates).as_deref(),
            Some(
                "Argonauts ran these after the build, in the task's worktree:\n\
                 tests (node --test) passed: 9 pass, 0 fail"
            )
        );
        assert_eq!(checks_text(&[]), None);
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
        assert_eq!(first.card.as_str(), "task-migrate-user-table-to-new-schema");
        assert_eq!(
            second.card.as_str(),
            "task-migrate-user-table-to-new-schema-2"
        );
        // A name seen before keeps its task.
        let again = book
            .entry("migrate-user-table-to-new-schema-step-2")
            .unwrap();
        assert_eq!(again.card, second.card);
        // A name in another script still makes a task.
        let chinese = book.entry("修復登入頁面").unwrap();
        assert_eq!(chinese.card.as_str(), "task-task");
    }
}
