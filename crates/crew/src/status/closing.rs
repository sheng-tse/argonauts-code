//! The closing answer, the last thing a run says in the conversation, the way an agent's own CLI
//! ends a task: what changed, how it was checked, and anything left. A Captain run's words are its
//! worker's own final answer, from the task that merged. A crew run's main agent writes them from
//! the tasks' reports in one short step without tools. Code writes them when no agent can, as when
//! a run fails. Only tasks that merged are what changed; the others are what's left.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::TaskId;
use argonauts_core::agents::session_runner::Access;
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::{AnswerAction, ApprovalKind, ApprovalPacket};
use argonauts_core::runs::events::run::{
    ChangedFile, CheckLine, ClosingAnswer, DecidedAlone, KEPT_ON_ITS_BRANCH, Party, RunLifecycle,
    RunMode, RunOutcome, says_merged,
};
use argonauts_core::runs::events::session::EndReason;
use argonauts_core::runs::events::shared::{BlackboardKind, Recipient};
use argonauts_core::runs::events::task::{CheckOutcome, TaskState};
use argonauts_core::runs::events::workflow::StepEnd;
use argonauts_core::runs::hidden_git::MergedInto;
use argonauts_core::settings::Settings;
use argonauts_core::workflows::steps::StepName;
use argonauts_core::workflows::steps::task::{
    CloseRun, CloseRunInput, TaskDigest, TaskReport, assumption,
};
use argonauts_core::workflows::workflow::StepRef;

use crate::approvals::autopilot;
use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::{Ask, Host};
use crate::merge::merge_packet::merged_files_path;
use crate::prompts;
use crate::state::RunState;
use crate::state::tasks::TaskEntry;
use crate::understand::materiality::one_line;

/// The run's own records, which every run commits and nobody asked for.
const OWN_RECORDS: &str = ".argonauts/";
/// What the main agent and the checks list are given at most, so the step stays short.
const MOST_TESTS: usize = 8;
const MOST_LEFT: usize = 6;
/// What the agents did, when no merged task speaks for the run, so the sentence stays short.
const MOST_DONE: usize = 6;
/// The tasks, and the files of each, that the sentence names before it counts the rest.
const MOST_NAMED: usize = 3;

/// Writes the closing answer once the run's result is settled, after the merge, the run review, and
/// any wait to merge. Only an interruption unwinds, so the answer is written again on resume.
pub async fn record(host: &Host, outcome: RunOutcome, detail: Option<&str>) -> EngineResult<()> {
    write(host, outcome, detail, true).await
}

/// For a run that ends without finishing its workflow, as a stop or a failure does: nobody is
/// asked, so the worker's last report or code's own words close it. Nothing when the run already
/// has its answer.
pub async fn record_unasked(
    host: &Host,
    outcome: RunOutcome,
    detail: Option<&str>,
) -> EngineResult<()> {
    write(host, outcome, detail, false).await
}

async fn write(
    host: &Host,
    outcome: RunOutcome,
    detail: Option<&str>,
    ask_main_agent: bool,
) -> EngineResult<()> {
    if host.read(|state| state.run.closed) {
        return Ok(());
    }
    let mut answer = gather(host, outcome, detail).await;
    match host.read(|state| state.run.mode) {
        Some(RunMode::Crew) if !ask_main_agent => {}
        Some(RunMode::Crew) => match main_agent_answer(host, outcome, detail, &answer).await {
            // Words that call a run merged when it wasn't give way to code's.
            Ok(Some((text, _))) if contradicts(outcome, &text) => {}
            Ok(Some((text, by))) => {
                answer.text = text;
                answer.by = Some(by);
            }
            Ok(None) => {}
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            // The answer code wrote stands.
            Err(_) => {}
        },
        _ => {
            let worker = host
                .read(worker_answer)
                .filter(|(text, _)| !contradicts(outcome, text));
            if let Some((text, by)) = worker {
                answer.text = text;
                answer.by = by;
            }
        }
    }
    host.record(
        None,
        None,
        RunEventKind::Run(RunLifecycle::ClosingAnswer(Box::new(answer))),
    )?;
    Ok(())
}

/// Code's own answer, with the files, checks, and what's left filled in.
async fn gather(host: &Host, outcome: RunOutcome, detail: Option<&str>) -> ClosingAnswer {
    let (tasks, put_off, run_merged, checks, diff_path, agents, kept, committed) =
        host.read(|state| {
            let tasks: Vec<TaskEntry> = state.tasks.by_id.values().cloned().collect();
            let (checks, diff_path) = match latest_merge(state) {
                Some((checks, diff_path)) => (checks, Some(diff_path)),
                None => (tasks_checks(&tasks), None),
            };
            let kept = (outcome == RunOutcome::BranchLeft).then(|| kept_line(state, detail));
            (
                tasks,
                put_off(state),
                run_merged(state),
                checks,
                diff_path,
                Agents::of(state),
                kept,
                committed_line(state, outcome),
            )
        });
    // A run kept on its branch says so first among what's left, with why.
    let detail = kept.as_deref().or(detail);
    let mut agents = agents;
    for unchecked in &mut agents.unchecked {
        if let Some(task) = tasks.iter().find(|task| task.spec.id == unchecked.task) {
            unchecked.files = task_files(host, std::slice::from_ref(task)).await;
        }
    }
    let files = match diff_path {
        Some(path) => match merged_files(&path).await {
            Some(files) => files,
            // A merge from before the counts were kept.
            None => tokio::fs::read_to_string(&path)
                .await
                .map(|diff| files_in_diff(&diff))
                .unwrap_or_default(),
        },
        None => {
            let delivered: Vec<TaskEntry> = tasks
                .iter()
                .filter(|task| run_merged.contains(&task.spec.id))
                .cloned()
                .collect();
            task_files(host, &delivered).await
        }
    };
    let mut answer = answer_from(
        &tasks,
        &put_off,
        &run_merged,
        &checks,
        files,
        outcome,
        detail,
        &agents,
    );
    if let Some(committed) = &committed {
        answer.left = answered_by_the_commit(std::mem::take(&mut answer.left), committed);
    }
    answer.decided = host.read(|state| decided_alone(state, host.settings(), &run_merged));
    if let Some(reviews) = host.read(reviews_line) {
        answer.checks.push(reviews);
    }
    if !answer.decided.is_empty() {
        // An assumption is listed as decided, so it isn't something left.
        answer
            .left
            .retain(|line| line.lines().any(|line| assumption(line).is_none()));
    }
    answer
}

/// What the run decided in your place on autopilot, in the order it happened: what each
/// delivered task's worker says it assumed, each question a worker asked with what it got, and
/// each approval autopilot ended. Nothing off autopilot, where those came to you.
fn decided_alone(
    state: &RunState,
    settings: &Settings,
    run_merged: &BTreeSet<TaskId>,
) -> Vec<DecidedAlone> {
    if !autopilot::applies(state, settings, ApprovalKind::Question) {
        return Vec::new();
    }
    let workers_call = || "the worker's call".to_owned();
    let mut decided: Vec<DecidedAlone> = state
        .tasks
        .by_id
        .values()
        .filter(|task| run_merged.contains(&task.spec.id))
        .filter_map(|task| task.reports.last())
        .flat_map(TaskReport::assumptions)
        .map(|assumed| DecidedAlone {
            what: assumed.to_owned(),
            whose: workers_call(),
        })
        .collect();
    let board = &state.blackboard;
    let questions = board.messages.iter().filter(|message| {
        message.kind == BlackboardKind::Question && message.to == Recipient::User
    });
    for question in questions {
        let Some(answer) = board.settled(&question.id) else {
            continue;
        };
        let asked = one_line(&question.text);
        match &answer.from {
            Party::Autopilot => decided.push(DecidedAlone {
                what: asked,
                whose: workers_call(),
            }),
            Party::Agent { worker, .. } => decided.push(DecidedAlone {
                what: format!("{asked} {}", one_line(&answer.text)),
                whose: format!("{}'s answer", crate::roles::display_name(worker)),
            }),
            _ => {}
        }
    }
    decided.extend(
        autopilot::decided(state)
            .into_iter()
            .map(|what| DecidedAlone {
                what,
                whose: "autopilot's choice".to_owned(),
            }),
    );
    decided.dedup();
    decided
}

/// The run's agents as its end tells them: how many there were, how many reported, and what
/// they did, from the steps they finished and the tasks that reported without landing.
#[derive(Debug, Default)]
struct Agents {
    total: usize,
    reported: usize,
    did: Vec<String>,
    /// What the workflows' notes said is left, such as a step you require that Claude Code's
    /// own workflow didn't have.
    left: Vec<String>,
    /// Tasks with work committed on their branch that no agent reported and no check ran on,
    /// as when a stopped session's work was committed, so the end never says nothing was done.
    unchecked: Vec<Unchecked>,
}

/// A task's committed work that nobody checked, and the files it changed on its branch.
#[derive(Debug)]
struct Unchecked {
    task: TaskId,
    files: Vec<ChangedFile>,
}

impl Agents {
    fn of(state: &RunState) -> Self {
        let sessions = state.sessions.by_key.values();
        let reported = sessions
            .clone()
            .filter(|session| {
                session
                    .end
                    .as_ref()
                    .is_some_and(|end| end.reason == EndReason::Completed)
            })
            .count();
        let run_merged = run_merged(state);
        let mut did: Vec<String> = state
            .tasks
            .by_id
            .values()
            .filter(|task| !run_merged.contains(&task.spec.id))
            .filter_map(|task| {
                let summary = task.reports.last()?.summary.trim();
                (!summary.is_empty()).then(|| format!("{}: {summary}", task.spec.title.trim()))
            })
            .collect();
        did.extend(steps_done(state));
        let left = state
            .workflows
            .by_id
            .values()
            .flat_map(|workflow| workflow.left.iter().cloned())
            .collect();
        let unchecked = state
            .tasks
            .by_id
            .values()
            .filter(|task| !run_merged.contains(&task.spec.id))
            .filter(|task| task.state != TaskState::TakenOver)
            .filter(|task| {
                !task.commits.is_empty() && task.reports.is_empty() && task.checks.is_empty()
            })
            .map(|task| Unchecked {
                task: task.spec.id.clone(),
                files: Vec::new(),
            })
            .collect();
        Self {
            total: sessions.count(),
            reported,
            did,
            left,
            unchecked,
        }
    }
}

/// What each finished step came to, in the order they finished. A loop or a nested workflow
/// only sums up the steps inside it, which speak for themselves.
fn steps_done(state: &RunState) -> Vec<String> {
    use argonauts_core::workflows::workflow::StepKind;

    let steps: Vec<_> = state
        .workflows
        .by_id
        .values()
        .flat_map(|workflow| &workflow.steps)
        .collect();
    let holds_others = |outer: &StepRef| {
        steps.iter().any(|inner| {
            inner.step.workflow == outer.workflow
                && inner.step.phase == outer.phase
                && inner.step.path.len() > outer.path.len()
                && inner.step.path.starts_with(&outer.path)
        })
    };
    let mut done: Vec<_> = steps
        .iter()
        .filter(|progress| progress.end == Some(StepEnd::Done))
        .filter(|progress| progress.kind != StepKind::Workflow && !holds_others(&progress.step))
        .filter_map(|progress| {
            let summary = progress.summary.as_deref()?.trim();
            (!summary.is_empty()).then_some((progress.ended_at, summary.to_owned()))
        })
        .collect();
    done.sort_by_key(|(at, _)| *at);
    done.into_iter().map(|(_, summary)| summary).collect()
}

/// Code's answer: what the merged tasks reported is what changed, and every other task is
/// left, with how it ended. A task put off for another reason than the run's limit says why,
/// from `put_off`.
#[allow(clippy::too_many_arguments)]
fn answer_from(
    tasks: &[TaskEntry],
    put_off: &BTreeMap<TaskId, String>,
    run_merged: &BTreeSet<TaskId>,
    checks: &[CheckOutcome],
    files: Vec<ChangedFile>,
    outcome: RunOutcome,
    detail: Option<&str>,
    agents: &Agents,
) -> ClosingAnswer {
    let reports: Vec<_> = tasks
        .iter()
        .filter(|task| run_merged.contains(&task.spec.id))
        .filter_map(|task| task.reports.last())
        .collect();
    let mut tests: Vec<String> = Vec::new();
    let mut left: Vec<String> = tasks
        .iter()
        .filter(|task| !run_merged.contains(&task.spec.id))
        .filter_map(|task| {
            not_merged(task, run_merged, put_off)
                .map(|how| format!("{} · {how}", task.spec.title.trim()))
        })
        .collect();
    for report in &reports {
        for test in &report.tests_run {
            if !tests.contains(test) {
                tests.push(test.clone());
            }
        }
        for concern in &report.concerns {
            if !left.contains(concern) {
                left.push(concern.clone());
            }
        }
    }
    // What a workflow said is left comes first: it's about what you asked for.
    for line in agents.left.iter().rev() {
        left.retain(|other| other != line);
        left.insert(0, line.clone());
    }
    if let Some(detail) = detail.map(str::trim).filter(|detail| !detail.is_empty()) {
        left.insert(0, detail.to_owned());
    }
    tests.truncate(MOST_TESTS);
    left.truncate(MOST_LEFT);
    let summaries: Vec<&str> = reports
        .iter()
        .map(|report| report.summary.trim())
        .filter(|summary| !summary.is_empty())
        .collect();
    let text = summaries.join(" ");
    let text = if text.is_empty() || contradicts(outcome, &text) {
        ending_sentence(outcome, agents)
    } else {
        text
    };
    ClosingAnswer {
        text,
        by: None,
        files,
        checks: checks
            .iter()
            .filter(|check| !check.skipped)
            .map(|check| CheckLine {
                what: check.name.clone(),
                passed: check.passed,
                result: check.summary.trim().to_owned(),
            })
            .collect(),
        tests,
        left,
        decided: Vec::new(),
    }
}

/// The tasks whose work the run delivers: the ones the newest merge question names, else the
/// ones merged. Best-of-n's losing tries, and tasks that failed or were cancelled, aren't.
fn run_merged(state: &RunState) -> BTreeSet<TaskId> {
    let named: BTreeSet<TaskId> = state
        .approvals
        .by_id
        .values()
        .filter_map(|record| match &record.opened.packet {
            ApprovalPacket::Merge(packet) => Some((record.since, packet)),
            _ => None,
        })
        .max_by_key(|(since, _)| *since)
        .map(|(_, packet)| packet.rows.iter().map(|row| row.task.clone()).collect())
        .unwrap_or_default();
    if !named.is_empty() {
        return named;
    }
    state
        .tasks
        .by_id
        .values()
        .filter(|record| record.state == TaskState::Merged)
        .map(|record| record.spec.id.clone())
        .collect()
}

/// Why each task something other than the run's limit put off was put off, as the move said:
/// "you deferred it", or "the split was refused: …".
fn put_off(state: &RunState) -> BTreeMap<TaskId, String> {
    state
        .tasks
        .by_id
        .values()
        .filter(|record| record.state == TaskState::Deferred)
        .filter_map(|record| {
            let progress = state.tasks.progress(&record.spec.id);
            let why = progress.why.trim();
            (!progress.deferred_for_limit && !why.is_empty())
                .then(|| (record.spec.id.clone(), why.to_owned()))
        })
        .collect()
}

/// How a task that didn't land ended, for what's left. `None` for one you took over.
fn not_merged(
    task: &TaskEntry,
    run_merged: &BTreeSet<TaskId>,
    put_off: &BTreeMap<TaskId, String>,
) -> Option<String> {
    // Best of n keeps one try; the others stay on their branches.
    let try_of = |id: &TaskId| {
        id.as_str()
            .split_once("-try-")
            .map(|(task, _)| task.to_owned())
    };
    if let Some(task_id) = try_of(&task.spec.id)
        && run_merged
            .iter()
            .any(|kept| try_of(kept).as_deref() == Some(task_id.as_str()))
    {
        return Some("not kept, left on its branch".to_owned());
    }
    let words = match task.state {
        TaskState::Failed => "failed",
        TaskState::Cancelled => "dropped",
        TaskState::NeedsYou | TaskState::NeedsDecision => "waits for a decision",
        TaskState::Blocked => "blocked on a question",
        TaskState::Deferred => {
            return Some(put_off.get(&task.spec.id).map_or_else(
                || "waits for limit".to_owned(),
                |why| format!("put off: {why}"),
            ));
        }
        TaskState::Merged => "not merged",
        TaskState::Running | TaskState::Checking | TaskState::InReview | TaskState::Merging => {
            "stopped unfinished"
        }
        TaskState::Waiting | TaskState::Ready => "never started",
        TaskState::TakenOver => return None,
    };
    Some(words.to_owned())
}

/// The words a task's state has for the main agent: `merged`, else how it ended.
fn digest_state(
    task: &TaskEntry,
    run_merged: &BTreeSet<TaskId>,
    put_off: &BTreeMap<TaskId, String>,
    outcome: RunOutcome,
) -> String {
    if !run_merged.contains(&task.spec.id) {
        return not_merged(task, run_merged, put_off).unwrap_or_else(|| "taken over".to_owned());
    }
    // A task in what the run delivers is merged only once the run is: until then it is in the
    // run's own branch alone.
    match outcome {
        RunOutcome::Merged => "merged",
        RunOutcome::PullRequest => "done, waiting in the pull request",
        RunOutcome::Abandoned => "done, abandoned with the run",
        RunOutcome::BranchLeft | RunOutcome::Stopped | RunOutcome::Failed => {
            "done, kept on the run's branch"
        }
    }
    .to_owned()
}

/// What the main agent is told of each task, the merged ones first.
fn digests(state: &RunState, outcome: RunOutcome) -> Vec<TaskDigest> {
    let run_merged = run_merged(state);
    let put_off = put_off(state);
    let committed = committed_line(state, outcome);
    let mut records: Vec<&TaskEntry> = state.tasks.by_id.values().collect();
    records.sort_by_key(|record| !run_merged.contains(&record.spec.id));
    records
        .into_iter()
        .filter(|record| record.state != TaskState::TakenOver)
        .map(|record| {
            let report = record.reports.last();
            TaskDigest {
                title: record.spec.title.clone(),
                state: digest_state(record, &run_merged, &put_off, outcome),
                summary: report
                    .map(|report| report.summary.clone())
                    .unwrap_or_default(),
                tests_run: report
                    .map(|report| report.tests_run.clone())
                    .unwrap_or_default(),
                concerns: report
                    .map(|report| {
                        // A task the run branch never took keeps its work on its own branch.
                        let own = (!run_merged.contains(&record.spec.id)
                            && outcome != RunOutcome::Abandoned)
                            .then(|| record.branch.as_ref().map(|branch| committed_on(branch)))
                            .flatten();
                        match own.as_ref().or(committed.as_ref()) {
                            Some(committed) => {
                                answered_by_the_commit(report.concerns.clone(), committed)
                            }
                            None => report.concerns.clone(),
                        }
                    })
                    .unwrap_or_default(),
            }
        })
        .collect()
}

/// Whether `text` calls the run merged when it ended as `outcome`, which it may only say of a
/// run that merged.
fn contradicts(outcome: RunOutcome, text: &str) -> bool {
    outcome != RunOutcome::Merged && says_merged(text)
}

/// Where a run kept on its branch left its work, and why, in the run's own words: what it said
/// as it ended, else who kept it there by answering the merge question, and for autopilot, the
/// reason it gave.
fn kept_line(state: &RunState, detail: Option<&str>) -> String {
    let why = match detail.map(str::trim).filter(|detail| !detail.is_empty()) {
        Some(detail) => format!(": {detail}"),
        None => kept_by(state).unwrap_or_default(),
    };
    format!("{KEPT_ON_ITS_BRANCH}{why}")
}

/// Who answered the merge question with "Keep the branch", as the words after the kept line.
fn kept_by(state: &RunState) -> Option<String> {
    let record = state.approvals.last_decided(ApprovalKind::Merge, None)?;
    let decided = record.decided.as_ref()?;
    if decided.action != AnswerAction::KeepBranch {
        return None;
    }
    Some(match &decided.by {
        Party::User => ", as you chose".to_owned(),
        Party::Autopilot => match autopilot::reason(record) {
            Some(why) => format!(", as autopilot chose, since {why}"),
            None => ", as autopilot chose in your place".to_owned(),
        },
        Party::Declined => ", as your settings say when you leave the merge question".to_owned(),
        Party::Config => ", as your merge settings say".to_owned(),
        Party::Rules => ", as your approval rules say".to_owned(),
        Party::Judge { .. } => ", as the judge of approvals chose".to_owned(),
        _ => return None,
    })
}

/// Where a run's work was committed, for the line that says a task's work isn't committed
/// yet: `Committed and merged onto main · 30f0733` once the run merged, `Committed on
/// argonauts/run-1 · pull request <url>` for one waiting in its pull request, and `Committed on
/// argonauts/run-1` for one that kept its branch. `None` where no branch is left to name, as
/// for a run abandoned with its branches.
fn committed_line(state: &RunState, outcome: RunOutcome) -> Option<String> {
    if let Some(merged) = &state.merge.run_merged {
        return Some(match MergedInto::of(&merged.target) {
            MergedInto::Branch(branch) => {
                let sha: String = merged.sha.chars().take(7).collect();
                format!("Committed and merged onto {branch} · {sha}")
            }
            into @ MergedInto::YourFolder => into.heading(),
        });
    }
    if let Some(pull) = &state.merge.pull_request {
        return Some(match &pull.url {
            Some(url) => format!("{} · pull request {url}", committed_on(&pull.branch)),
            None => format!("{}, for its pull request", committed_on(&pull.branch)),
        });
    }
    match outcome {
        RunOutcome::BranchLeft | RunOutcome::Stopped | RunOutcome::Failed => {
            state.merge.branch.as_deref().map(committed_on)
        }
        RunOutcome::Merged | RunOutcome::PullRequest | RunOutcome::Abandoned => None,
    }
}

fn committed_on(branch: &str) -> String {
    format!("Committed on {branch}")
}

/// `concerns` once the run's work is committed somewhere: one that only says the work isn't
/// committed yet, which Argonauts Code does as the session ends, reads as where it was
/// committed instead.
fn answered_by_the_commit(concerns: Vec<String>, committed: &str) -> Vec<String> {
    let mut answered: Vec<String> = Vec::new();
    for concern in concerns {
        let concern = if only_uncommitted(&concern) {
            committed.to_owned()
        } else {
            concern
        };
        if !answered.contains(&concern) {
            answered.push(concern);
        }
    }
    answered
}

/// Whether a worker's concern only says its own work isn't committed, as it was told to leave
/// it: `Not committed; Argonauts Code commits on session end`. One that names something else
/// left uncommitted, such as the files a test run wrote, says what and stays.
fn only_uncommitted(concern: &str) -> bool {
    const UNCOMMITTED: &[&str] = &[
        "not committed",
        "uncommitted",
        "isn't committed",
        "aren't committed",
        "wasn't committed",
        "weren't committed",
        "not yet committed",
        "didn't commit",
        "did not commit",
    ];
    const LEFT_TO_US: &[&str] = &["argonauts", "session end", "session ends", "as instructed"];
    let concern = concern.to_lowercase().replace('\u{2019}', "'");
    UNCOMMITTED.iter().any(|said| concern.contains(said))
        && LEFT_TO_US.iter().any(|said| concern.contains(said))
}

/// A Captain run's worker already said it all in its last report: its answer, else its summary.
/// The task that merged speaks, never a losing try; with none merged, the last to report.
fn worker_answer(state: &RunState) -> Option<(String, Option<ModelRef>)> {
    let run_merged = run_merged(state);
    let reported = |record: &&TaskEntry| !record.reports.is_empty();
    let record = state
        .tasks
        .by_id
        .values()
        .filter(reported)
        .find(|record| run_merged.contains(&record.spec.id))
        .or_else(|| {
            run_merged
                .is_empty()
                .then(|| state.tasks.by_id.values().rev().find(reported))
                .flatten()
        })?;
    let report = record.reports.last()?;
    let text = [&report.answer, &report.summary]
        .into_iter()
        .map(|text| text.trim())
        .find(|text| !text.is_empty())?;
    let by = record.worker.clone().or_else(|| record.spec.worker.clone());
    Some((text.to_owned(), by))
}

/// The main agent, the agent you talk to, writes a crew run's answer from what code gathered.
/// `None` when no task reported, so there's nothing for it to say beyond code's words.
async fn main_agent_answer(
    host: &Host,
    outcome: RunOutcome,
    detail: Option<&str>,
    gathered: &ClosingAnswer,
) -> EngineResult<Option<(String, ModelRef)>> {
    let (request, tasks, asked, needed_you) = host.read(|state| {
        let request = state
            .run
            .started
            .as_ref()
            .map(|started| started.request.clone())
            .unwrap_or_default();
        (
            request,
            digests(state, outcome),
            asked(state),
            needed_you(state),
        )
    });
    if tasks.iter().all(|task| task.summary.trim().is_empty()) {
        return Ok(None);
    }
    let mut ending = ending_words(outcome).to_owned();
    // A run kept on its branch tells why as its answer's own line does.
    if let Some(kept) = gathered.kept() {
        ending = lowercase_first(kept);
    } else if let Some(detail) = detail.filter(|detail| !detail.trim().is_empty()) {
        ending.push_str(": ");
        ending.push_str(detail.trim());
    }
    let input = CloseRunInput {
        request,
        ending,
        tasks,
        files: gathered
            .files
            .iter()
            .map(|file| format!("{} +{} -{}", file.path, file.insertions, file.deletions))
            .collect(),
        checks: gathered
            .checks
            .iter()
            .map(|check| {
                let verdict = if check.passed { "passed" } else { "failed" };
                match check.result.as_str() {
                    "" => format!("{}: {verdict}", check.what),
                    result => format!("{}: {verdict}, {result}", check.what),
                }
            })
            .collect(),
        asked,
        needed_you,
        decided_on_autopilot: gathered
            .decided
            .iter()
            .map(|decided| format!("{} ({})", decided.what, decided.whose))
            .collect(),
    };
    let worker = host.settings().crew.main.clone();
    let mut ask = Ask::new(
        None,
        RoleName::Main,
        worker,
        prompts::divide::CLOSE_RUN,
        input,
    );
    ask.access = Access::NoTools;
    let answered = host.ask::<CloseRun>(ask).await?;
    let text = answered.output.answer.trim().to_owned();
    Ok((!text.is_empty()).then_some((text, answered.worker)))
}

/// Each reviewer and sign-off that answered, in order, with what it said.
fn asked(state: &RunState) -> Vec<String> {
    state
        .sessions
        .in_order()
        .into_iter()
        .filter(|session| matches!(session.started.role, RoleName::Reviewer | RoleName::Judge))
        // The judge settling reviewers or deciding an approval is neither.
        .filter(|session| {
            !matches!(
                session.started.step,
                Some(StepName::Settle | StepName::ModelJudge)
            )
        })
        .filter_map(|session| {
            let answer = session.end.as_ref()?.structured.as_ref()?;
            let accepted = match answer.get("accept").and_then(serde_json::Value::as_bool) {
                Some(accepted) => accepted,
                None => answer.get("verdict")?.as_str()? == "approve",
            };
            let said = if accepted { "accepted" } else { "sent it back" };
            let who = crate::roles::display_name(&session.started.worker);
            Some(match session.started.label.as_deref() {
                Some(label) => format!("{label} ({who}): {said}"),
                None => format!("{who}: {said}"),
            })
        })
        .collect()
}

/// Who reviewed, for whom, and what each said, from the reviews themselves, so a fallback is
/// named as one whatever a worker's report says: "Codex asked for changes · Claude Code, in place
/// of Codex, accepted it". `None` when nothing was reviewed.
fn reviews_line(state: &RunState) -> Option<CheckLine> {
    use argonauts_core::runs::events::task::ReviewEvent;
    use argonauts_core::workflows::steps::judgment::ReviewVerdict;
    let in_place_of = |reviewer: &ModelRef| {
        state.sessions.by_key.values().find_map(|session| {
            let reviews = matches!(session.started.role, RoleName::Reviewer | RoleName::Judge);
            let asked = session.asked.as_ref()?;
            (reviews && session.started.worker == *reviewer && asked != reviewer)
                .then(|| asked.clone())
        })
    };
    let mut said: Vec<String> = Vec::new();
    let mut accepted = None;
    for task in state.tasks.by_id.values() {
        for review in &task.reviews {
            let ReviewEvent::Reviewed {
                reviewer, review, ..
            } = review
            else {
                continue;
            };
            let mut who = crate::roles::display_name(reviewer);
            if let Some(asked) = in_place_of(reviewer) {
                who = format!("{who}, in place of {}", crate::roles::display_name(&asked));
            }
            let yes = review.verdict == ReviewVerdict::Approve;
            let line = format!(
                "{who} {}",
                if yes {
                    "accepted it"
                } else {
                    "asked for changes"
                }
            );
            if said.last() != Some(&line) {
                said.push(line);
            }
            accepted = Some(yes);
        }
    }
    Some(CheckLine {
        what: "reviews".to_owned(),
        passed: accepted?,
        result: said.join(" · "),
    })
}

/// How many times the run came to you: each approval you decided, Esc's "leave it to the
/// agent" included, and each still waiting.
fn needed_you(state: &RunState) -> u32 {
    use argonauts_core::runs::events::run::Party;
    let count = state
        .approvals
        .by_id
        .values()
        .filter(|record| match &record.decided {
            Some(decided) => matches!(decided.by, Party::User | Party::Declined),
            None => record.withdrawn.is_none(),
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// The merge question's checks and where its diff was written, from the newest one put up.
fn latest_merge(state: &RunState) -> Option<(Vec<CheckOutcome>, std::path::PathBuf)> {
    state
        .approvals
        .by_id
        .values()
        .filter_map(|record| match &record.opened.packet {
            ApprovalPacket::Merge(packet) => Some((record.since, packet)),
            _ => None,
        })
        .max_by_key(|(since, _)| *since)
        .map(|(_, packet)| (packet.checks.clone(), packet.diff_path.clone()))
}

/// Each check's latest result over every task, for a run that never reached the merge.
fn tasks_checks(tasks: &[TaskEntry]) -> Vec<CheckOutcome> {
    let mut latest: BTreeMap<String, CheckOutcome> = BTreeMap::new();
    for check in tasks.iter().flat_map(|task| &task.checks) {
        latest.insert(check.name.clone(), check.clone());
    }
    latest.into_values().collect()
}

/// What the tasks changed on their branches, for a run that never reached the merge.
async fn task_files(host: &Host, tasks: &[TaskEntry]) -> Vec<ChangedFile> {
    let root = host.repo().root.clone();
    let mut files: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for task in tasks {
        let (Some(base), Some(branch)) = (&task.base, &task.branch) else {
            continue;
        };
        let Ok(changed) = host.git().diff_files(&root, base, branch).await else {
            continue;
        };
        for (path, added, removed) in changed {
            let counts = files.entry(path).or_default();
            counts.0 += added;
            counts.1 += removed;
        }
    }
    files
        .into_iter()
        .filter(|(path, _)| !path.starts_with(OWN_RECORDS))
        .map(|(path, (insertions, deletions))| ChangedFile {
            path,
            insertions,
            deletions,
        })
        .collect()
}

/// Each file the merge changed, as `git diff --numstat` counted it when the merge was
/// readied. The run's own records are left out.
async fn merged_files(diff_path: &std::path::Path) -> Option<Vec<ChangedFile>> {
    let bytes = tokio::fs::read(merged_files_path(diff_path)).await.ok()?;
    let mut files: Vec<ChangedFile> = serde_json::from_slice(&bytes).ok()?;
    files.retain(|file| !file.path.starts_with(OWN_RECORDS));
    Some(files)
}

/// Each file of a unified diff, as `git diff` writes it, with its lines added and removed. The
/// run's own records are left out.
fn files_in_diff(diff: &str) -> Vec<ChangedFile> {
    let mut files: Vec<ChangedFile> = Vec::new();
    let mut in_hunk = false;
    for line in diff.lines() {
        if let Some(names) = line.strip_prefix("diff --git ") {
            in_hunk = false;
            // `a/<path> b/<path>`; the b side names a renamed or new file as it is now.
            let path = names.rsplit_once(" b/").map_or(names, |(_, path)| path);
            files.push(ChangedFile {
                path: path.to_owned(),
                insertions: 0,
                deletions: 0,
            });
        } else if line.starts_with("@@") {
            in_hunk = true;
        } else if in_hunk && let Some(file) = files.last_mut() {
            if line.starts_with('+') {
                file.insertions += 1;
            } else if line.starts_with('-') {
                file.deletions += 1;
            }
        }
    }
    files.retain(|file| !file.path.starts_with(OWN_RECORDS));
    files
}

/// For the main agent: how the run ended.
fn ending_words(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Merged => "merged into your branch",
        RunOutcome::PullRequest => "waiting in a pull request",
        RunOutcome::BranchLeft => "kept on its branch, not merged",
        RunOutcome::Abandoned => "abandoned at the merge, nothing merged",
        RunOutcome::Stopped => "stopped before it finished",
        RunOutcome::Failed => "failed",
    }
}

/// Code's words when no task that merged speaks for the run: how many of its agents reported
/// and what they did, so a run that failed late never reads as one that did nothing.
fn ending_sentence(outcome: RunOutcome, agents: &Agents) -> String {
    if agents.reported == 0 {
        return match unchecked_words(outcome, &agents.unchecked) {
            Some(words) => words,
            None => nobody_reported(outcome),
        };
    }
    let who = match (agents.reported, agents.total) {
        (1, 1) => "its one agent".to_owned(),
        (reported, total) if reported >= total => format!("all {reported} of its agents"),
        (1, total) => format!("1 of its {total} agents"),
        (reported, total) => format!("{reported} of its {total} agents"),
    };
    let mut sentence = match outcome {
        RunOutcome::Merged => format!("The run merged after {who} reported"),
        RunOutcome::PullRequest => {
            format!("The run's work waits in a pull request after {who} reported")
        }
        RunOutcome::BranchLeft => {
            format!("The run's work is kept on its branch, not merged, after {who} reported")
        }
        RunOutcome::Abandoned => {
            format!("The run was abandoned after {who} reported, so nothing merged")
        }
        RunOutcome::Stopped => format!("The run stopped after {who} reported"),
        RunOutcome::Failed => format!("The run failed after {who} reported"),
    };
    if agents.did.is_empty() {
        sentence.push('.');
        return sentence;
    }
    let mut did: Vec<String> = agents
        .did
        .iter()
        .take(MOST_DONE)
        .map(|what| lowercase_first(what.trim().trim_end_matches('.')))
        .collect();
    let more = agents.did.len().saturating_sub(MOST_DONE);
    if more > 0 {
        did.push(format!("and {more} more"));
    }
    format!("{sentence}: {}.", did.join("; "))
}

/// `the main agent answered` for `The main agent answered`, leaving a name such as `GPT` as it is.
fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(first), Some(second)) if first.is_uppercase() && second.is_lowercase() => {
            first.to_lowercase().chain(text.chars().skip(1)).collect()
        }
        _ => text.to_owned(),
    }
}

/// A run that stopped or failed with work committed that nobody reported or checked says
/// where it is: `Stopped before anything merged. task-001 has work no one has checked, on its
/// branch (slug.py +13 −1).` `None` when there's none, or for a run that got further.
fn unchecked_words(outcome: RunOutcome, unchecked: &[Unchecked]) -> Option<String> {
    let ended = match outcome {
        RunOutcome::Stopped => "Stopped",
        RunOutcome::Failed => "Failed",
        _ => return None,
    };
    let named = |task: &Unchecked| match files_words(&task.files) {
        Some(files) => format!("{} ({files})", task.task),
        None => task.task.to_string(),
    };
    let whose = match unchecked {
        [] => return None,
        [one] => {
            let files =
                files_words(&one.files).map_or_else(String::new, |files| format!(" ({files})"));
            return Some(format!(
                "{ended} before anything merged. {} has work no one has checked, on its \
                 branch{files}.",
                one.task
            ));
        }
        many => {
            let mut names: Vec<String> = many.iter().take(MOST_NAMED).map(named).collect();
            let more = many.len().saturating_sub(MOST_NAMED);
            if more > 0 {
                names.push(format!("{more} more"));
            }
            let last = names.pop().unwrap_or_default();
            format!("{} and {last}", names.join(", "))
        }
    };
    Some(format!(
        "{ended} before anything merged. {whose} have work no one has checked, on their branches."
    ))
}

/// The files a task changed with their lines, such as `slug.py +13 −1`, the first few named.
fn files_words(files: &[ChangedFile]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let mut named: Vec<String> = files
        .iter()
        .take(MOST_NAMED)
        .map(|file| {
            format!(
                "{} +{} \u{2212}{}",
                file.path, file.insertions, file.deletions
            )
        })
        .collect();
    let more = files.len().saturating_sub(MOST_NAMED);
    if more > 0 {
        let files = if more == 1 { "file" } else { "files" };
        named.push(format!("{more} more {files}"));
    }
    let last = named.pop().unwrap_or_default();
    Some(match named.as_slice() {
        [] => last,
        rest => format!("{} and {last}", rest.join(", ")),
    })
}

/// Code's words when no agent reported anything.
fn nobody_reported(outcome: RunOutcome) -> String {
    match outcome {
        RunOutcome::Merged => "The run merged.",
        RunOutcome::PullRequest => "The run's work waits in a pull request.",
        RunOutcome::BranchLeft => "The run's work is kept on its branch, not merged.",
        RunOutcome::Abandoned => "The run was abandoned, so nothing merged.",
        RunOutcome::Stopped => "The run stopped before any agent reported.",
        RunOutcome::Failed => "The run failed before any agent reported.",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::workflows::steps::task::{ReportStatus, TaskReport};

    fn task(id: &str, title: &str, state: TaskState, said: &str) -> TaskEntry {
        let mut spec = crate::testing::task_spec(id, &[], Some("claude-code/opus"));
        spec.title = title.to_owned();
        let mut record = TaskEntry::new(spec);
        record.state = state;
        record.reports.push(TaskReport {
            status: ReportStatus::Done,
            summary: format!("{said} summary"),
            files_changed: Vec::new(),
            tests_run: vec![format!("{said} tests")],
            concerns: vec![format!("{said} concern")],
            questions: Vec::new(),
            change_requests: Vec::new(),
            memory_proposals: Vec::new(),
            answer: format!("{said} answer"),
        });
        record
    }

    fn state_of(tasks: Vec<TaskEntry>) -> RunState {
        let mut state = RunState::default();
        for record in tasks {
            state.tasks.by_id.insert(record.spec.id.clone(), record);
        }
        state
    }

    /// "Defer it" on a task that kept failing says who put it off; only the run's limit reads
    /// as waiting for it.
    #[test]
    fn a_task_put_off_says_why() {
        let tasks = vec![
            task("task-001", "Add the flag", TaskState::Deferred, "FLAG"),
            task("task-002", "Write the docs", TaskState::Deferred, "DOCS"),
        ];
        let mut state = state_of(tasks.clone());
        let progress = &mut state.tasks.progress;
        let stuck = progress.entry(TaskId::numbered(1)).or_default();
        stuck.why = "autopilot deferred it".to_owned();
        let low = progress.entry(TaskId::numbered(2)).or_default();
        low.why = "the run's limit is near".to_owned();
        low.deferred_for_limit = true;
        let answer = answer_from(
            &tasks,
            &put_off(&state),
            &run_merged(&state),
            &[],
            Vec::new(),
            RunOutcome::Failed,
            None,
            &Agents::default(),
        );
        assert_eq!(
            answer.left[..2],
            [
                "Add the flag · put off: autopilot deferred it",
                "Write the docs · waits for limit"
            ]
        );
    }

    /// What a task's report says the main agent is told: its word that its work isn't committed
    /// yet names where it was, the run's branch for a task the run branch took, and its own for
    /// one it didn't.
    #[test]
    fn a_tasks_digest_names_the_branch_its_work_was_committed_on() {
        let uncommitted = "Not committed; Argonauts Code commits on session end";
        let mut tasks = vec![
            task("task-001", "Add the flag", TaskState::Merged, "FLAG"),
            task("task-002", "Write the docs", TaskState::Cancelled, "DOCS"),
        ];
        for (record, branch) in tasks
            .iter_mut()
            .zip(["argonauts/run-01/task-001", "argonauts/run-01/task-002"])
        {
            record.branch = Some(branch.to_owned());
            record.reports[0].concerns = vec![uncommitted.to_owned()];
        }
        let mut state = state_of(tasks);
        state.merge.branch = Some("argonauts/run-01".to_owned());
        let concerns = |outcome: RunOutcome| -> Vec<Vec<String>> {
            digests(&state, outcome)
                .into_iter()
                .map(|digest| digest.concerns)
                .collect()
        };
        assert_eq!(
            concerns(RunOutcome::BranchLeft),
            [
                vec!["Committed on argonauts/run-01".to_owned()],
                vec!["Committed on argonauts/run-01/task-002".to_owned()]
            ]
        );
        assert_eq!(
            concerns(RunOutcome::Abandoned),
            [vec![uncommitted.to_owned()], vec![uncommitted.to_owned()]]
        );
    }

    #[test]
    fn only_a_task_that_merged_is_what_changed() {
        // A crew run whose second task was dropped: its claims are left, never what changed.
        let tasks = vec![
            task("task-001", "Add the flag", TaskState::Merged, "FLAG"),
            task("task-002", "Write the docs", TaskState::Cancelled, "DOCS"),
        ];
        let state = state_of(tasks.clone());
        let run_merged = run_merged(&state);
        let answer = answer_from(
            &tasks,
            &BTreeMap::new(),
            &run_merged,
            &[],
            Vec::new(),
            RunOutcome::Merged,
            None,
            &Agents::default(),
        );
        assert_eq!(answer.text, "FLAG summary");
        assert_eq!(answer.tests, ["FLAG tests"]);
        assert_eq!(answer.left, ["Write the docs · dropped", "FLAG concern"]);
        let told: Vec<(String, String)> = digests(&state, RunOutcome::Merged)
            .into_iter()
            .map(|digest| (digest.title, digest.state))
            .collect();
        assert_eq!(
            told,
            [
                ("Add the flag".to_owned(), "merged".to_owned()),
                ("Write the docs".to_owned(), "dropped".to_owned())
            ]
        );
    }

    #[test]
    fn best_of_n_speaks_for_the_try_it_kept() {
        let mut state = state_of(vec![
            task("task-001-try-1", "Fix (try 1)", TaskState::Merged, "FIRST"),
            task(
                "task-001-try-2",
                "Fix (try 2)",
                TaskState::Checking,
                "SECOND",
            ),
        ]);
        let (text, _) = worker_answer(&state).unwrap();
        assert_eq!(text, "FIRST answer");
        let tasks: Vec<TaskEntry> = state.tasks.by_id.values().cloned().collect();
        let answer = answer_from(
            &tasks,
            &put_off(&state),
            &run_merged(&state),
            &[],
            Vec::new(),
            RunOutcome::Merged,
            None,
            &Agents::default(),
        );
        assert_eq!(answer.left[0], "Fix (try 2) · not kept, left on its branch");
        // With nothing merged, as in a failed run, the last to report still speaks.
        for record in state.tasks.by_id.values_mut() {
            record.state = TaskState::Failed;
        }
        assert_eq!(worker_answer(&state).unwrap().0, "SECOND answer");
    }

    #[test]
    fn a_diff_gives_each_file_its_lines_without_the_runs_own_records() {
        let diff = "\
diff --git a/invoice/money.py b/invoice/money.py
index 1111111..2222222 100644
--- a/invoice/money.py
+++ b/invoice/money.py
@@ -1,4 +1,5 @@
-def parse(text):
+def parse(text: str) -> int:
+    text = text.strip()
--- a line that starts with dashes
 unchanged
diff --git a/.argonauts/goal/run-1.md b/.argonauts/goal/run-1.md
new file mode 100644
--- /dev/null
+++ b/.argonauts/goal/run-1.md
@@ -0,0 +1 @@
+The goal.
diff --git a/my notes.md b/my notes.md
--- a/my notes.md
+++ b/my notes.md
@@ -3 +3 @@
-old
+new
";
        let files = files_in_diff(diff);
        assert_eq!(
            files,
            vec![
                ChangedFile {
                    path: "invoice/money.py".into(),
                    insertions: 2,
                    deletions: 2,
                },
                ChangedFile {
                    path: "my notes.md".into(),
                    insertions: 1,
                    deletions: 1,
                },
            ]
        );
    }

    #[tokio::test]
    async fn the_merges_own_counts_list_each_file_and_leave_the_runs_records_out() {
        let dir = tempfile::tempdir().unwrap();
        let diff_path = dir.path().join("merge").join("abc123.diff");
        assert_eq!(merged_files(&diff_path).await, None, "a merge from before");
        std::fs::create_dir_all(diff_path.parent().unwrap()).unwrap();
        let counted = vec![
            ChangedFile {
                path: "src/export/csv.rs".into(),
                insertions: 64,
                deletions: 6,
            },
            ChangedFile {
                path: ".argonauts/goal.md".into(),
                insertions: 3,
                deletions: 0,
            },
            ChangedFile {
                path: "assets/logo.png".into(),
                insertions: 0,
                deletions: 0,
            },
        ];
        std::fs::write(
            merged_files_path(&diff_path),
            serde_json::to_vec(&counted).unwrap(),
        )
        .unwrap();
        let files = merged_files(&diff_path).await.unwrap();
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["src/export/csv.rs", "assets/logo.png"]);
        assert_eq!((files[0].insertions, files[0].deletions), (64, 6));
    }

    /// A run shaped like an orchestrator that failed late: the main agent designed its workflow,
    /// three agents read the request back, two compared the readings, the main agent answered the
    /// questions, and the main agent's next design session ended early.
    fn failed_late() -> RunState {
        use argonauts_core::base::names::WorkflowId;
        use argonauts_core::runs::events::workflow::{
            StepFinished, StepStarted, WorkflowEvent, WorkflowStarted,
        };
        use argonauts_core::workflows::workflow::{StandardPhase, StepKind};
        use serde_json::json;

        let id = WorkflowId::numbered(1);
        let workflow = serde_json::from_value(json!({
            "name": "add-due-dates",
            "phases": [{"name": "Understand", "standard": "understand", "steps": [
                {"kind": "loop_until", "until": "understood", "max": 2, "steps": [
                    {"kind": "read"}, {"kind": "compare"}, {"kind": "approval"},
                ]},
            ]}],
        }))
        .unwrap();
        let step = |path: &[u32]| StepRef {
            workflow: id.clone(),
            phase: 0,
            path: path.to_vec(),
        };
        let started = |path: &[u32], kind| {
            RunEventKind::Workflow(WorkflowEvent::StepStarted(StepStarted {
                step: step(path),
                kind,
                standard: StandardPhase::Understand,
                round: 1,
            }))
        };
        let finished = |path: &[u32], kind, summary: &str| {
            RunEventKind::Workflow(WorkflowEvent::StepFinished(StepFinished {
                step: step(path),
                kind,
                standard: StandardPhase::Understand,
                round: 1,
                end: StepEnd::Done,
                summary: Some(summary.to_owned()),
            }))
        };
        let session = |event: serde_json::Value| -> RunEventKind {
            serde_json::from_value(json!({"type": "session", "data": event})).unwrap()
        };
        let began = |worker: &str, role: &str| {
            session(json!({
                "event": "started", "worker": worker, "agent_id": worker, "provider": "openai",
                "kind": "cli", "role": role, "step": null, "access": "read_only",
                "lifetime": "task", "resumed_from": null, "follow_key": null,
            }))
        };
        let ended = |why: serde_json::Value| {
            session(json!({
                "event": "ended", "cli_session": null, "exit_code": 0, "signal": null,
                "duration_ms": 1000, "reason": why, "tool_calls": 0,
                "spend": serde_json::to_value(argonauts_core::Spend::default()).unwrap(),
                "result_text": null, "structured": null, "last_message": null,
                "raw_log": "/tmp/raw.jsonl",
            }))
        };
        let done = json!({"why": "completed"});
        let mut events = vec![(
            None,
            RunEventKind::Workflow(WorkflowEvent::Started(Box::new(WorkflowStarted {
                id: id.clone(),
                from: None,
                version: "v1".to_owned(),
                parent: None,
                workflow,
            }))),
        )];
        let mut sessions = 0;
        let mut agent = |events: &mut Vec<(Option<String>, RunEventKind)>, why| {
            sessions += 1;
            let key = format!("s-{sessions:03}");
            events.push((Some(key.clone()), began("codex", "reader")));
            events.push((Some(key), ended(why)));
        };
        agent(&mut events, done.clone());
        events.push((None, started(&[0], StepKind::LoopUntil)));
        events.push((None, started(&[0, 0], StepKind::Read)));
        for _ in 0..3 {
            agent(&mut events, done.clone());
        }
        events.push((
            None,
            finished(&[0, 0], StepKind::Read, "3 agents read the request back"),
        ));
        events.push((None, started(&[0, 1], StepKind::Compare)));
        for _ in 0..2 {
            agent(&mut events, done.clone());
        }
        events.push((
            None,
            finished(&[0, 1], StepKind::Compare, "5 questions, 2 assumptions"),
        ));
        events.push((None, started(&[0, 2], StepKind::Approval)));
        agent(&mut events, done.clone());
        events.push((
            None,
            finished(
                &[0, 2],
                StepKind::Approval,
                "The main agent answered 5 questions, and the goal is approved.",
            ),
        ));
        events.push((
            None,
            finished(&[0], StepKind::LoopUntil, "understood after 1 round"),
        ));
        agent(
            &mut events,
            json!({"why": "auth", "message": "workspace routing discovery unauthorized (401)"}),
        );
        let mut state = RunState::default();
        for (seq, (session, kind)) in events.into_iter().enumerate() {
            let seq = u64::try_from(seq).unwrap();
            state.apply(&crate::testing::run_event(
                seq,
                None,
                session.as_deref(),
                kind,
            ));
        }
        state
    }

    /// What Claude Code's own workflow lacked of the steps you require is said when it's found,
    /// and the closing answer lists it first among what's left. Other notes stay out.
    #[test]
    fn what_a_workflow_left_undone_comes_first_in_whats_left() {
        use argonauts_core::base::names::WorkflowId;
        use argonauts_core::runs::events::workflow::{RunNote, WorkflowEvent, WorkflowStarted};
        use serde_json::json;

        let id = WorkflowId::numbered(1);
        let workflow = serde_json::from_value(json!({
            "name": "add-due-dates",
            "phases": [{"name": "Build", "standard": "do", "steps": [{"kind": "build"}]}],
        }))
        .unwrap();
        let note = |text: &str, left: bool| {
            RunEventKind::Workflow(WorkflowEvent::Note(RunNote {
                text: text.to_owned(),
                workflow: Some(id.clone()),
                left,
            }))
        };
        let lacked = "It had no step for \"Benchmark\", which you require.";
        let events = [
            RunEventKind::Workflow(WorkflowEvent::Started(Box::new(WorkflowStarted {
                id: id.clone(),
                from: None,
                version: "v1".to_owned(),
                parent: None,
                workflow,
            }))),
            note("Build · Claude Code's own workflow", false),
            note(lacked, true),
        ];
        let mut state = RunState::default();
        for (seq, kind) in (0..).zip(events) {
            state.apply(&crate::testing::run_event(seq, None, None, kind));
        }
        let tasks = vec![task("task-001", "Add due dates", TaskState::Merged, "DUE")];
        let run_merged: BTreeSet<TaskId> = [TaskId::new("task-001").unwrap()].into();
        let answer = answer_from(
            &tasks,
            &BTreeMap::new(),
            &run_merged,
            &[],
            Vec::new(),
            RunOutcome::Merged,
            None,
            &Agents::of(&state),
        );
        assert_eq!(answer.left, [lacked, "DUE concern"]);
    }

    /// A run that failed after its agents did their work says how many reported and what they
    /// did, never that it failed before any agent reported. A loop sums up its steps, so it
    /// isn't told twice.
    #[test]
    fn a_run_that_failed_late_counts_its_agents_and_what_they_did() {
        let state = failed_late();
        let agents = Agents::of(&state);
        assert_eq!((agents.reported, agents.total), (7, 8));
        let answer = answer_from(
            &[],
            &BTreeMap::new(),
            &BTreeSet::new(),
            &[],
            Vec::new(),
            RunOutcome::Failed,
            Some("the main agent's design session ended early"),
            &agents,
        );
        assert_eq!(
            answer.text,
            "The run failed after 7 of its 8 agents reported: 3 agents read the request back; 5 \
             questions, 2 assumptions; the main agent answered 5 questions, and the goal is approved."
        );
        assert_eq!(answer.left, ["the main agent's design session ended early"]);
        assert!(!answer.text.contains("before any agent reported"));

        let none = Agents::default();
        assert_eq!(
            ending_sentence(RunOutcome::Failed, &none),
            "The run failed before any agent reported."
        );
        let all = Agents {
            total: 2,
            reported: 2,
            ..Agents::default()
        };
        assert_eq!(
            ending_sentence(RunOutcome::Stopped, &all),
            "The run stopped after all 2 of its agents reported."
        );
    }

    /// The local Qwen run: its stuck session's fix was committed, nobody reported and no check
    /// ran, and the end said the run "stopped before any agent reported". It names the work.
    #[test]
    fn a_stopped_run_names_the_work_it_committed() {
        let mut record = task("task-001", "Fix slugify", TaskState::Running, "qwen");
        record.reports.clear();
        record.commits.push(argonauts_core::CommitInfo {
            branch: "argonauts/run-01/task-001".into(),
            sha: Some("58721bae".into()),
            message: "task-001: work from s-001".into(),
            stat: argonauts_core::DiffStat {
                files_changed: 1,
                insertions: 13,
                deletions: 1,
            },
            by: None,
        });
        let mut agents = Agents::of(&state_of(vec![record.clone()]));
        assert_eq!(agents.reported, 0);
        agents.unchecked[0].files = vec![ChangedFile {
            path: "slug.py".into(),
            insertions: 13,
            deletions: 1,
        }];
        assert_eq!(
            ending_sentence(RunOutcome::Stopped, &agents),
            "Stopped before anything merged. task-001 has work no one has checked, on its \
             branch (slug.py +13 \u{2212}1)."
        );
        assert_eq!(
            ending_sentence(RunOutcome::Merged, &agents),
            "The run merged.",
            "a run that merged says so"
        );

        // Work a check already ran on, or that an agent reported, isn't unchecked.
        let mut checked = record.clone();
        checked.checks.push(CheckOutcome {
            name: "tests".into(),
            kind: argonauts_core::runs::events::task::CheckKind::Command,
            passed: false,
            command: None,
            exit_code: Some(1),
            duration_ms: 40,
            summary: "1 failed".into(),
            output_tail: String::new(),
            violations: Vec::new(),
            sandboxed: true,
            skipped: false,
        });
        assert!(Agents::of(&state_of(vec![checked])).unchecked.is_empty());

        let second = |id: &str| Unchecked {
            task: TaskId::new(id).unwrap(),
            files: Vec::new(),
        };
        let mut two = Agents::of(&state_of(vec![record]));
        two.unchecked.push(second("task-002"));
        two.unchecked[0].files = (1..=4)
            .map(|n| ChangedFile {
                path: format!("f{n}.py"),
                insertions: n,
                deletions: 0,
            })
            .collect();
        assert_eq!(
            ending_sentence(RunOutcome::Failed, &two),
            "Failed before anything merged. task-001 (f1.py +1 \u{2212}0, f2.py +2 \u{2212}0, \
             f3.py +3 \u{2212}0 and 1 more file) and task-002 have work no one has checked, on \
             their branches."
        );
    }

    /// The merge question answered with "Keep the branch", by `by`, with `reasons` from the
    /// policy that weighed it.
    fn kept_by_answer(state: &mut RunState, by: Party, reasons: &[&str]) {
        use argonauts_core::base::names::ApprovalId;
        use argonauts_core::runs::events::approval::{
            ApprovalDecided, ApprovalOpened, FreeText, PolicyCheck,
        };
        use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
        use argonauts_core::settings::asking::WhenDeclined;

        let id = ApprovalId::numbered(1);
        let opened = ApprovalOpened {
            id: id.clone(),
            kind: ApprovalKind::Merge,
            title: "Merge this run into main?".to_owned(),
            task: None,
            packet: ApprovalPacket::WorkflowPromotion(serde_json::json!({})),
            evidence_sha256: String::new(),
            mode: ApprovalMode::Autopilot,
            decide_with: DecideWith::Rules,
            when_declined: WhenDeclined::Decide,
            options: Vec::new(),
            free_text: FreeText::None,
            needs_you: false,
        };
        let check = PolicyCheck {
            id: id.clone(),
            violations: Vec::new(),
            verdict: None,
            by: None,
            judge: None,
            reasons: reasons.iter().map(|why| (*why).to_owned()).collect(),
            auto_preview: false,
        };
        let decided = ApprovalDecided {
            id: id.clone(),
            action: AnswerAction::KeepBranch,
            text: None,
            by,
            evidence_sha256: String::new(),
            auto_preview: None,
            tasks: Vec::new(),
        };
        let record = crate::state::approvals::ApprovalRecord {
            opened,
            since: 1,
            checks: vec![check],
            decided: Some(decided),
            withdrawn: None,
            spot_check: false,
        };
        state.approvals.by_id.insert(id, record);
    }

    /// A run kept on its branch never reads as merged: its task is done on the run's branch as
    /// the main agent is told, code's words never call it merged, and its first line of what's
    /// left says it was kept there and why, as whoever kept it said.
    #[test]
    fn a_run_kept_on_its_branch_says_so_and_why() {
        let mut flag = task("task-001", "Add the flag", TaskState::Merged, "FLAG");
        flag.reports[0].summary = "The flag is merged and documented.".to_owned();
        let tasks = vec![flag];
        let mut state = state_of(tasks.clone());
        let told: Vec<String> = digests(&state, RunOutcome::BranchLeft)
            .into_iter()
            .map(|digest| digest.state)
            .collect();
        assert_eq!(told, ["done, kept on the run's branch"]);

        kept_by_answer(&mut state, Party::User, &[]);
        let kept = kept_line(&state, None);
        assert_eq!(kept, "Kept on its branch, not merged, as you chose");
        let answer = answer_from(
            &tasks,
            &BTreeMap::new(),
            &run_merged(&state),
            &[],
            Vec::new(),
            RunOutcome::BranchLeft,
            Some(&kept),
            &Agents::default(),
        );
        assert_eq!(answer.kept(), Some(kept.as_str()));
        assert_eq!(answer.left[0], kept);
        assert!(!says_merged(&answer.text), "{}", answer.text);

        kept_by_answer(
            &mut state,
            Party::Autopilot,
            &["The review stayed within Claude. Autopilot keeps such a merge for you."],
        );
        assert_eq!(
            kept_line(&state, None),
            "Kept on its branch, not merged, as autopilot chose, since the review stayed \
             within Claude"
        );
        // What the run said as it ended comes first.
        assert_eq!(
            kept_line(&state, Some("branch kept by merge.mode")),
            "Kept on its branch, not merged: branch kept by merge.mode"
        );
        assert!(contradicts(RunOutcome::BranchLeft, "The fix is merged."));
        assert!(!contradicts(RunOutcome::Merged, "The fix is merged."));
    }

    /// Once the run merged, a worker's word that its work isn't committed yet reads as the
    /// commit the merge made; anything else left uncommitted stays, saying what. A run that kept
    /// its branch or waits in a pull request names the branch its work is committed on.
    #[test]
    fn a_runs_left_line_names_where_its_work_was_committed() {
        use crate::state::merge::{PullRequest, RunMerged};

        let mut state = RunState::default();
        assert_eq!(committed_line(&state, RunOutcome::BranchLeft), None);
        state.merge.branch = Some("argonauts/run-01".to_owned());
        for outcome in [
            RunOutcome::BranchLeft,
            RunOutcome::Stopped,
            RunOutcome::Failed,
        ] {
            assert_eq!(
                committed_line(&state, outcome).as_deref(),
                Some("Committed on argonauts/run-01"),
                "{outcome:?}"
            );
        }
        assert_eq!(committed_line(&state, RunOutcome::Abandoned), None);
        state.merge.pull_request = Some(PullRequest {
            branch: "argonauts/run-01".to_owned(),
            url: Some("https://github.com/you/app/pull/7".to_owned()),
            command: None,
        });
        assert_eq!(
            committed_line(&state, RunOutcome::PullRequest).as_deref(),
            Some("Committed on argonauts/run-01 · pull request https://github.com/you/app/pull/7")
        );
        state.merge.run_merged = Some(RunMerged {
            target: "main".to_owned(),
            sha: "30f0733c9d".to_owned(),
            fast_forward: true,
        });
        let merge_commit = committed_line(&state, RunOutcome::Merged).expect("the merge");
        assert_eq!(merge_commit, "Committed and merged onto main · 30f0733");
        let stays = "Untracked invoice/__pycache__/ was left by the test runs and should not be \
                     committed.";
        let concerns = vec![
            "Not committed; Argonauts Code commits on session end".to_owned(),
            stays.to_owned(),
            "Changes are left uncommitted, as instructed.".to_owned(),
        ];
        assert_eq!(
            answered_by_the_commit(concerns, &merge_commit),
            [merge_commit.clone(), stays.to_owned()]
        );
    }

    #[test]
    fn every_ending_has_words_for_the_main_agent_and_for_code() {
        for outcome in [
            RunOutcome::Merged,
            RunOutcome::PullRequest,
            RunOutcome::BranchLeft,
            RunOutcome::Abandoned,
            RunOutcome::Stopped,
            RunOutcome::Failed,
        ] {
            assert!(!ending_words(outcome).is_empty());
            assert!(ending_sentence(outcome, &Agents::default()).ends_with('.'));
        }
    }
}
