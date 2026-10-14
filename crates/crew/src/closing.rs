//! The closing answer, the last thing a run says in the conversation, the way an agent's own CLI
//! ends a task: what changed, how it was checked, and anything left. A Captain run's words are its
//! worker's own final answer, from the task that merged. A crew run's main agent writes them from
//! the tasks' reports in one short step without tools. Code writes them when no agent can, as when
//! a run fails. Only tasks that merged are what changed; the others are what's left.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::TaskId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::{ApprovalKind, ApprovalPacket};
use argonauts_core::events::run::{
    ChangedFile, CheckLine, ClosingAnswer, DecidedAlone, Party, RunLifecycle, RunMode, RunOutcome,
};
use argonauts_core::events::session::EndReason;
use argonauts_core::events::shared::{BlackboardKind, Recipient};
use argonauts_core::events::task::{CheckOutcome, TaskState};
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::session_runner::Access;
use argonauts_core::settings::Settings;
use argonauts_core::steps::StepName;
use argonauts_core::steps::task::{CloseRun, CloseRunInput, TaskDigest, TaskReport, assumption};
use argonauts_core::workflow::StepRef;

use crate::approvals::autopilot;
use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
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
            if let Some((text, by)) = host.read(worker_answer) {
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
    let (tasks, run_merged, checks, diff_path, agents) = host.read(|state| {
        let tasks: Vec<TaskEntry> = state.tasks.by_id.values().cloned().collect();
        let (checks, diff_path) = match latest_merge(state) {
            Some((checks, diff_path)) => (checks, Some(diff_path)),
            None => (tasks_checks(&tasks), None),
        };
        (
            tasks,
            run_merged(state),
            checks,
            diff_path,
            Agents::of(state),
        )
    });
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
        &run_merged,
        &checks,
        files,
        outcome,
        detail,
        &agents,
    );
    answer.decided = host.read(|state| decided_alone(state, host.settings(), &run_merged));
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
        Self {
            total: sessions.count(),
            reported,
            did,
            left,
        }
    }
}

/// What each finished step came to, in the order they finished. A loop or a nested workflow
/// only sums up the steps inside it, which speak for themselves.
fn steps_done(state: &RunState) -> Vec<String> {
    use argonauts_core::workflow::StepKind;

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
/// left, with how it ended.
fn answer_from(
    tasks: &[TaskEntry],
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
            not_merged(task, run_merged).map(|how| format!("{} · {how}", task.spec.title.trim()))
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
    let text = if summaries.is_empty() {
        ending_sentence(outcome, agents)
    } else {
        summaries.join(" ")
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

/// How a task that didn't land ended, for what's left. `None` for one you took over.
fn not_merged(task: &TaskEntry, run_merged: &BTreeSet<TaskId>) -> Option<&'static str> {
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
        return Some("not kept, left on its branch");
    }
    Some(match task.state {
        TaskState::Failed => "failed",
        TaskState::Cancelled => "dropped",
        TaskState::NeedsYou | TaskState::NeedsDecision => "waits for a decision",
        TaskState::Blocked => "blocked on a question",
        TaskState::Deferred => "waits for limit",
        TaskState::Merged => "not merged",
        TaskState::Running | TaskState::Checking | TaskState::InReview | TaskState::Merging => {
            "stopped unfinished"
        }
        TaskState::Waiting | TaskState::Ready => "never started",
        TaskState::TakenOver => return None,
    })
}

/// The words a task's state has for the main agent: `merged`, else how it ended.
fn digest_state(task: &TaskEntry, run_merged: &BTreeSet<TaskId>) -> String {
    if run_merged.contains(&task.spec.id) {
        "merged".to_owned()
    } else {
        not_merged(task, run_merged)
            .unwrap_or("taken over")
            .to_owned()
    }
}

/// What the main agent is told of each task, the merged ones first.
fn digests(state: &RunState) -> Vec<TaskDigest> {
    let run_merged = run_merged(state);
    let mut records: Vec<&TaskEntry> = state.tasks.by_id.values().collect();
    records.sort_by_key(|record| !run_merged.contains(&record.spec.id));
    records
        .into_iter()
        .filter(|record| record.state != TaskState::TakenOver)
        .map(|record| {
            let report = record.reports.last();
            TaskDigest {
                title: record.spec.title.clone(),
                state: digest_state(record, &run_merged),
                summary: report
                    .map(|report| report.summary.clone())
                    .unwrap_or_default(),
                tests_run: report
                    .map(|report| report.tests_run.clone())
                    .unwrap_or_default(),
                concerns: report
                    .map(|report| report.concerns.clone())
                    .unwrap_or_default(),
            }
        })
        .collect()
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
        (request, digests(state), asked(state), needed_you(state))
    });
    if tasks.iter().all(|task| task.summary.trim().is_empty()) {
        return Ok(None);
    }
    let mut ending = ending_words(outcome).to_owned();
    if let Some(detail) = detail.filter(|detail| !detail.trim().is_empty()) {
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

/// How many times the run came to you: each approval you decided, and each still waiting.
fn needed_you(state: &RunState) -> u32 {
    let count = state
        .approvals
        .by_id
        .values()
        .filter(|record| match &record.decided {
            Some(decided) => decided.by == argonauts_core::events::run::Party::User,
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
        RunOutcome::BranchLeft => "left on its branches, not merged",
        RunOutcome::Abandoned => "abandoned at the merge, nothing merged",
        RunOutcome::Stopped => "stopped before it finished",
        RunOutcome::Failed => "failed",
    }
}

/// Code's words when no task that merged speaks for the run: how many of its agents reported
/// and what they did, so a run that failed late never reads as one that did nothing.
fn ending_sentence(outcome: RunOutcome, agents: &Agents) -> String {
    if agents.reported == 0 {
        return nobody_reported(outcome);
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
            format!("The run's work is on its branches, not merged, after {who} reported")
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

/// Code's words when no agent reported anything.
fn nobody_reported(outcome: RunOutcome) -> String {
    match outcome {
        RunOutcome::Merged => "The run merged.",
        RunOutcome::PullRequest => "The run's work waits in a pull request.",
        RunOutcome::BranchLeft => "The run's work is on its branches, not merged.",
        RunOutcome::Abandoned => "The run was abandoned, so nothing merged.",
        RunOutcome::Stopped => "The run stopped before any agent reported.",
        RunOutcome::Failed => "The run failed before any agent reported.",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::steps::task::{ReportStatus, TaskReport};

    fn task(id: &str, title: &str, state: TaskState, said: &str) -> TaskEntry {
        let mut spec = crate::testutil::task_spec(id, &[], Some("claude-code/opus"));
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
        let told: Vec<(String, String)> = digests(&state)
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
        use argonauts_core::events::workflow::{
            StepFinished, StepStarted, WorkflowEvent, WorkflowStarted,
        };
        use argonauts_core::names::WorkflowId;
        use argonauts_core::workflow::{StandardPhase, StepKind};
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
            state.apply(&crate::testutil::run_event(
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
        use argonauts_core::events::workflow::{RunNote, WorkflowEvent, WorkflowStarted};
        use argonauts_core::names::WorkflowId;
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
            state.apply(&crate::testutil::run_event(seq, None, None, kind));
        }
        let tasks = vec![task("task-001", "Add due dates", TaskState::Merged, "DUE")];
        let run_merged: BTreeSet<TaskId> = [TaskId::new("task-001").unwrap()].into();
        let answer = answer_from(
            &tasks,
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
