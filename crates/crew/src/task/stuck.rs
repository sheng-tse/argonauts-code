//! Decisions a task waits on: a task that needs you (retry, a new worker, take over, defer, split,
//! or drop), and a task stopped for a decision because you stopped its session or its worker
//! asked for one. Every answer moves the task, so a decided approval is never asked again.

use std::collections::BTreeSet;

use argonauts_core::TaskId;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::approval::{
    AnswerAction, ApprovalDecided, ApprovalKind, ApprovalPacket, FreeText, StuckPacket,
    TakeOverReturnPacket,
};
use argonauts_core::events::run::Party;
use argonauts_core::events::session::InputPart;
use argonauts_core::events::task::{
    CheckOutcome, ReviewEvent, TaskEvent, TaskSpec, TaskState, TryCause,
};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::policy::{EvidenceDocument, EvidenceFacts, StuckFacts};
use argonauts_core::steps::plan::{GoalBrief, PlanDraft, PlanToTasks, PlanToTasksInput};

use crate::approvals::{self, ApprovalRequest, options};
use crate::divide::graph;
use crate::engine::host::{Ask, Host};
use crate::error::EngineResult;
use crate::fallback::rules;
use crate::prompts;
use crate::roles;
use crate::state::RunState;
use crate::state::tasks::TaskEntry;
use crate::task::at_start;
use crate::task::limit;
use crate::task::machine::Trigger;
use crate::task::pipeline::transition;
use crate::task::prompt;
use crate::task::recovery;
use crate::task::session::Next;
use crate::task::stopped_work;
use crate::task::support::{called, progress_of, record_of, record_task, wait_until, worker_of};
use crate::tie_breaker;
use crate::workflow::StepAt;

/// What a decision did to the task.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Decision {
    /// It moved; a following session starts as `next` says when given.
    Moved(Option<Next>),
    /// It moved while the approval was open (you reassigned it or took it over).
    MovedMeanwhile,
}

/// Opens the stuck approval for a task in `from` (`NeedsYou`, or `NeedsDecision` for
/// a stuck-style stop) and applies your answer.
pub(crate) async fn decide(
    host: &Host,
    task: &TaskId,
    at: Option<&StepAt>,
    from: TaskState,
    why: &str,
) -> EngineResult<Decision> {
    let record = record_of(host, task)?;
    let keep = stopped_work::keep_detail(host, &record).await;
    let no_other_model = recovery::other_provider(host, &record).is_none();
    // Work a stopped session left is the question's to keep or not, so it says what the session
    // did, where that work is and who else could take the task.
    let why = match keep {
        Some(_) => {
            let did = stopped_work::what_it_did(host, task)
                .unwrap_or_else(|| why.trim().trim_end_matches('.').to_owned());
            let mut why = format!("{did}. Its commit is on the task's branch.");
            if no_other_model {
                why.push_str(" No other model can retry it.");
            }
            why
        }
        None => why.to_owned(),
    };
    let mut request = request(host, &record, at, from, &why);
    request.options = Some(options::stuck(keep.as_deref(), no_other_model));
    let Some(decided) = wait_decision(host, task, from, request).await? else {
        return Ok(Decision::MovedMeanwhile);
    };
    apply_answer(host, task, at, from, &decided).await
}

/// The approval's decision, or `None` when the task left `from` first.
pub(crate) async fn wait_decision(
    host: &Host,
    task: &TaskId,
    from: TaskState,
    request: ApprovalRequest,
) -> EngineResult<Option<ApprovalDecided>> {
    let moved = |state: &RunState| state.tasks.get(task).map(|r| r.state) != Some(from);
    tokio::select! {
        decided = approvals::open_and_wait(host, request) => decided.map(Some),
        moved = wait_until(host, moved) => moved.map(|()| None),
    }
}

fn request(
    host: &Host,
    record: &TaskEntry,
    at: Option<&StepAt>,
    from: TaskState,
    why: &str,
) -> ApprovalRequest {
    let task = record.spec.id.clone();
    let called = called(record);
    let title = match from {
        TaskState::NeedsYou => format!("{called} needs you"),
        _ => format!("{called} needs a decision"),
    };
    let packet = StuckPacket {
        task: task.clone(),
        title: record.spec.title.clone(),
        state: from,
        tries: recovery::try_lines(record),
        last_failure: why.to_owned(),
    };
    ApprovalRequest {
        at: at.cloned(),
        kind: ApprovalKind::Stuck,
        title,
        task: Some(task),
        packet: ApprovalPacket::Stuck(packet),
        facts: EvidenceFacts {
            stuck: stuck_facts(host, record),
            ..EvidenceFacts::default()
        },
        agent_outputs: Vec::new(),
        documents: documents(host, record, why),
        options: None,
        free_text: FreeText::Guidance,
        authors: vec![worker_of(host, record)],
    }
}

/// What whoever decides in your place reads: what was asked of the task, each try, the
/// worker's last report, what the reviewers found, and the checks with the output of the ones
/// that failed. A decision made on nothing can only put the task off.
fn documents(host: &Host, record: &TaskEntry, why: &str) -> Vec<EvidenceDocument> {
    let document = |label: &str, text: String| EvidenceDocument {
        label: label.to_owned(),
        text,
        path: None,
        sha256: None,
    };
    let mut documents = vec![document(
        "the task",
        format!(
            "{}\n\n{}\n\nWhy it stopped: {}",
            record.spec.title.trim(),
            record.spec.task.trim(),
            why.trim()
        ),
    )];
    let tries = recovery::try_lines(record);
    if !tries.is_empty() {
        documents.push(document("what was tried", tries.join("\n")));
    }
    if let Some(report) = record.reports.last() {
        let mut text = report.summary.trim().to_owned();
        for concern in &report.concerns {
            text.push_str(&format!("\n- {}", concern.trim()));
        }
        documents.push(document("the worker's last report", text));
    }
    let findings: Vec<String> = record
        .reviews
        .iter()
        .filter_map(|review| match review {
            ReviewEvent::Reviewed {
                reviewer,
                round,
                review,
                ..
            } => {
                let mut text = format!(
                    "Round {round}, {} said {:?}: {}",
                    roles::display_name(reviewer),
                    review.verdict,
                    review.summary.trim()
                );
                for comment in &review.comments {
                    text.push_str(&format!("\n- {}: {}", comment.path, comment.text.trim()));
                }
                Some(text)
            }
            ReviewEvent::Settled { judge, ruling, .. } => Some(format!(
                "{} settled the reviewers with {:?}: {}",
                roles::display_name(judge),
                ruling.verdict,
                ruling
                    .reasons
                    .iter()
                    .chain(&ruling.required_changes)
                    .map(|line| line.trim())
                    .collect::<Vec<_>>()
                    .join("; ")
            )),
            ReviewEvent::TestsWritten { .. } => None,
        })
        .collect();
    if !findings.is_empty() {
        documents.push(document("what the reviewers found", findings.join("\n\n")));
    }
    let mut checks = host.read(at_start::review_lines);
    for check in newest_checks(&record.checks) {
        let mut line = crate::task::checks::result_line(check);
        let tail = check.output_tail.trim();
        if !check.passed && !tail.is_empty() {
            line.push_str(&format!("\n```\n{tail}\n```"));
        }
        checks.push(line);
    }
    if !checks.is_empty() {
        documents.push(document("the checks", checks.join("\n")));
    }
    documents
}

/// Each check's newest run, in the order the checks first ran.
fn newest_checks(checks: &[CheckOutcome]) -> Vec<&CheckOutcome> {
    let mut newest: Vec<&CheckOutcome> = Vec::new();
    for check in checks {
        match newest.iter_mut().find(|kept| kept.name == check.name) {
            Some(kept) => *kept = check,
            None => newest.push(check),
        }
    }
    newest
}

/// What auto needs to grant one more try, by the fallback "Give it to a fallback" would pick.
fn stuck_facts(host: &Host, record: &TaskEntry) -> Option<StuckFacts> {
    let task = &record.spec.id;
    let worker = worker_of(host, record);
    let fallback_mode = rules::applied_for(host, &record.spec, &worker).mode;
    let next = recovery::other_provider(host, record);
    let next_provider = next.as_ref().map(|model| roles::provider(host, model));
    let next_name = next.map(|model| DisplayNames::default().cli(&model.agent));
    let providers_tried = recovery::providers_tried(host, record);
    let (extra_tries, fits_reserve) = host.read(|state| {
        (
            state.tasks.progress(task).extra_tries,
            limit::fits_reserve(state, record),
        )
    });
    Some(StuckFacts {
        fallback_mode,
        providers_tried,
        next_provider,
        next_name,
        extra_tries,
        fits_reserve,
    })
}

/// Moves the task by your answer. From `NeedsDecision`, the moves the table only allows from
/// `NeedsYou` ask you first.
pub(crate) async fn apply_answer(
    host: &Host,
    task: &TaskId,
    at: Option<&StepAt>,
    from: TaskState,
    decided: &ApprovalDecided,
) -> EngineResult<Decision> {
    let note = decided.text.clone().filter(|t| !t.trim().is_empty());
    let extra: Vec<_> = note
        .iter()
        .map(|text| prompt::message_part("your note", text.clone()))
        .collect();
    let to_ready = if from == TaskState::NeedsYou {
        Trigger::NewWorker
    } else {
        Trigger::Decided
    };
    let ask_you_first = |reason: &str| -> EngineResult<()> {
        if from == TaskState::NeedsDecision {
            transition(host, task, Trigger::AskYou, reason)?;
        }
        Ok(())
    };
    // Whoever answered: you, or what decided in your place.
    let who = crate::blackboard::delivery::party_label(&decided.by);
    match &decided.action {
        AnswerAction::Candidate { worker } => {
            reassign(host, task, worker.clone(), &decided.by)?;
            transition(
                host,
                task,
                to_ready,
                &format!("{who} picked {}", crate::roles::display_name(worker)),
            )?;
            Ok(fresh(
                TryCause::Reassigned,
                &format!("{who} picked a new worker"),
                extra,
            ))
        }
        AnswerAction::Fallback | AnswerAction::OtherCandidates => {
            let record = record_of(host, task)?;
            match recovery::other_provider(host, &record) {
                Some(worker) => {
                    reassign(host, task, worker.clone(), &decided.by)?;
                    transition(
                        host,
                        task,
                        to_ready,
                        &format!("fell back to {}", crate::roles::display_name(&worker)),
                    )?;
                    Ok(fresh(
                        TryCause::Fallback,
                        &format!("{who} sent it to a fallback"),
                        extra,
                    ))
                }
                None => {
                    let reason = "no other agent can take it; one more try";
                    transition(host, task, to_ready, reason)?;
                    Ok(fresh(TryCause::AfterFailure, reason, extra))
                }
            }
        }
        AnswerAction::KeepWork => {
            let reason = format!("{who} kept its work; the checks decide");
            ask_you_first(&reason)?;
            transition(host, task, Trigger::KeptWork, &reason)?;
            Ok(Decision::Moved(None))
        }
        AnswerAction::TakeOver => {
            ask_you_first("you took it over")?;
            transition(host, task, Trigger::TakeOver, "you took it over")?;
            record_task(host, task, TaskEvent::TakenOver { command: None })?;
            Ok(Decision::Moved(None))
        }
        AnswerAction::Defer | AnswerAction::Wait => {
            let reason = format!("{who} deferred it");
            ask_you_first(&reason)?;
            transition(host, task, Trigger::Defer, &reason)?;
            Ok(Decision::Moved(None))
        }
        AnswerAction::Split => {
            let how = note.clone().unwrap_or_default();
            match split(host, task, at, &how).await? {
                Ok(specs) => {
                    ask_you_first("you split it")?;
                    let ids: Vec<String> = specs.iter().map(|s| s.id.to_string()).collect();
                    let reason = format!("split into {}", ids.join(", "));
                    transition(host, task, Trigger::Split, &reason)?;
                    for spec in specs {
                        record_task(host, &spec.id.clone(), TaskEvent::Created(Box::new(spec)))?;
                    }
                    rewire_dependents(host, task, &ids)?;
                    Ok(Decision::Moved(None))
                }
                Err(why) => {
                    // A split the graph rules refuse can't be asked again from the same
                    // decided approval, so the task waits deferred with the reason.
                    ask_you_first("the split was refused")?;
                    let reason = format!("the split was refused: {why}");
                    transition(host, task, Trigger::Defer, &reason)?;
                    Ok(Decision::Moved(None))
                }
            }
        }
        AnswerAction::Cancel | AnswerAction::Stop | AnswerAction::Abandon => {
            ask_you_first("you dropped it")?;
            transition(host, task, Trigger::Drop, "you dropped it")?;
            Ok(Decision::Moved(None))
        }
        AnswerAction::AskYou if from == TaskState::NeedsDecision => {
            transition(host, task, Trigger::AskYou, "you asked to decide it")?;
            Ok(Decision::Moved(None))
        }
        _ if from == TaskState::NeedsDecision => {
            transition(host, task, to_ready, "you decided it continues")?;
            let mut parts = extra;
            if parts.is_empty() {
                parts.push(prompt::message_part(
                    "continue",
                    "You decided it continues. Carry on with the task.",
                ));
            }
            Ok(Decision::Moved(Some(Next::resume(
                TryCause::Resumed,
                parts,
            ))))
        }
        _ => {
            transition(host, task, to_ready, "you asked for another try")?;
            Ok(fresh(
                TryCause::AfterFailure,
                "you asked for another try",
                extra,
            ))
        }
    }
}

/// You're done with a task you took over: let the worker continue, told what changed, or keep
/// it, so its checks and review run on your work.
pub(crate) async fn take_over_return(
    host: &Host,
    task: &TaskId,
    at: Option<&StepAt>,
) -> EngineResult<Decision> {
    let record = record_of(host, task)?;
    let Some((changed, commit)) = progress_of(host, task).take_over_end else {
        return Ok(Decision::MovedMeanwhile);
    };
    let worker = worker_of(host, &record);
    let packet = TakeOverReturnPacket {
        task: task.clone(),
        worker: worker.clone(),
        changed,
        commit: commit.clone(),
    };
    let request = ApprovalRequest {
        at: at.cloned(),
        kind: ApprovalKind::TakeOverReturn,
        title: format!("{task} is back from you · {}", record.spec.title),
        task: Some(task.clone()),
        packet: ApprovalPacket::TakeOverReturn(packet),
        facts: EvidenceFacts::default(),
        agent_outputs: Vec::new(),
        documents: Vec::new(),
        options: None,
        free_text: FreeText::Guidance,
        authors: Vec::new(),
    };
    let Some(decided) = wait_decision(host, task, TaskState::TakenOver, request).await? else {
        return Ok(Decision::MovedMeanwhile);
    };
    match &decided.action {
        AnswerAction::LetContinue | AnswerAction::GiveBack => {
            transition(
                host,
                task,
                Trigger::GiveBack,
                &format!("you gave it back to {worker}"),
            )?;
            let commit = commit.map_or_else(String::new, |sha| format!(" in commit {sha}"));
            let mut text = format!(
                "The user took this task over while you were paused and changed {} files \
                 (+{} -{}){commit}. Read what changed before you go on, then continue the \
                 task.",
                changed.files_changed, changed.insertions, changed.deletions
            );
            if let Some(note) = decided.text.as_deref().filter(|t| !t.trim().is_empty()) {
                text.push_str(&format!("\n\nTheir note: {note}"));
            }
            let parts = vec![prompt::message_part("your changes", text)];
            Ok(Decision::Moved(Some(Next::resume(
                TryCause::Resumed,
                parts,
            ))))
        }
        // Back from your own session, keeping the task says it's done: the work is checked as
        // yours.
        _ => {
            transition(
                host,
                task,
                Trigger::SaidDone,
                "you kept it, so its checks run",
            )?;
            Ok(Decision::Moved(None))
        }
    }
}

fn fresh(cause: TryCause, why: &str, extra: Vec<InputPart>) -> Decision {
    Decision::Moved(Some(Next::Fresh {
        cause,
        why: why.to_owned(),
        note: None,
        extra,
    }))
}

fn reassign(host: &Host, task: &TaskId, to: ModelRef, by: &Party) -> EngineResult<()> {
    let record = record_of(host, task)?;
    let event = TaskEvent::Reassigned {
        from: Some(worker_of(host, &record)),
        to,
        by: by.clone(),
        rule_override: None,
    };
    record_task(host, task, event)?;
    Ok(())
}

/// Your note on how to split the task becomes new tasks through the `PlanToTasks` step and the
/// graph validator. `Err` carries the validator's reasons.
async fn split(
    host: &Host,
    task: &TaskId,
    at: Option<&StepAt>,
    how: &str,
) -> EngineResult<Result<Vec<TaskSpec>, String>> {
    let record = record_of(host, task)?;
    let (goal, decisions, tie_breaker, next_number) = host.read(|state| {
        (
            state.goal.approved.clone(),
            state.plan.decisions.clone(),
            tie_breaker::tie_breaker(state, host.settings()),
            next_task_number(state),
        )
    });
    let settings = host.settings();
    let brief = match &goal {
        Some(goal) => GoalBrief {
            summary: goal.summary.clone(),
            criteria: goal
                .criteria
                .iter()
                .map(|c| format!("{}: {}", c.id, c.text))
                .collect(),
            assumptions: goal.assumptions.iter().map(|a| a.text.clone()).collect(),
            sha256: goal.sha256.clone(),
        },
        None => GoalBrief {
            summary: record.spec.task.clone(),
            criteria: Vec::new(),
            assumptions: Vec::new(),
            sha256: String::new(),
        },
    };
    let mut forbidden = vec![".argonauts/**".to_owned()];
    forbidden.extend(settings.limits.protected_paths.iter().cloned());
    let input = PlanToTasksInput {
        goal: brief,
        plan: PlanDraft {
            summary: format!(
                "Split {task} ({}) as the user says: {how}",
                record.spec.title
            ),
            approach: format!(
                "The task:\n\n{}\n\nIts paths: {}\n\nHow to split it: {how}",
                record.spec.task,
                record.spec.owned_paths.join(", ")
            ),
            pieces: Vec::new(),
            choices: Vec::new(),
            risks: Vec::new(),
            test_strategy: String::new(),
            new_dependencies: Vec::new(),
        },
        decisions: decisions
            .iter()
            .map(|d| format!("{}: {}", d.title, d.choice))
            .collect(),
        forbidden_paths: forbidden,
        max_tasks: settings.divide.max_tasks,
    };
    let worker = tie_breaker.unwrap_or_else(|| settings.crew.main.clone());
    let mut ask = Ask::new(
        at.cloned(),
        RoleName::TieBreaker,
        worker,
        prompts::divide::PLAN_TO_TASKS,
        input,
    );
    ask.task = Some(task.clone());
    ask.parts.extend(at_start::part(host).await?);
    let answered = host.ask::<PlanToTasks>(ask).await?;
    let specs = match graph::validate(host.run_id(), &answered.output, settings) {
        Ok(specs) => specs,
        Err(reasons) => return Ok(Err(reasons.join("; "))),
    };
    Ok(Ok(renumbered(specs, &record.spec, next_number)))
}

/// The validator numbers tasks from 1; the split's tasks take the numbers after the run's
/// highest, keep the split task's dependencies, worker, and reviewers, and point at each other
/// by their new ids.
fn renumbered(specs: Vec<TaskSpec>, split: &TaskSpec, first: u32) -> Vec<TaskSpec> {
    let renames: Vec<(TaskId, TaskId)> = specs
        .iter()
        .zip(first..)
        .map(|(spec, n)| (spec.id.clone(), TaskId::numbered(n)))
        .collect();
    let rename = |id: &TaskId| {
        renames
            .iter()
            .find(|(old, _)| old == id)
            .map(|(_, new)| new.clone())
    };
    specs
        .into_iter()
        .map(|mut spec| {
            spec.id = rename(&spec.id).unwrap_or(spec.id);
            let mut deps: Vec<TaskId> = spec.depends_on.iter().filter_map(rename).collect();
            deps.extend(split.depends_on.iter().cloned());
            deps.sort();
            deps.dedup();
            spec.depends_on = deps;
            if spec.worker.is_none() {
                spec.worker = split.worker.clone();
            }
            if spec.reviewers.is_empty() {
                spec.reviewers = split.reviewers.clone();
            }
            spec
        })
        .collect()
}

/// Tasks that waited on the split task now wait on every task it was split into.
fn rewire_dependents(host: &Host, split: &TaskId, new_ids: &[String]) -> EngineResult<()> {
    let dependents: Vec<TaskSpec> = host.read(|state| {
        state
            .tasks
            .by_id
            .values()
            .filter(|r| r.spec.depends_on.contains(split))
            .map(|r| r.spec.clone())
            .collect()
    });
    let new: Vec<TaskId> = new_ids
        .iter()
        .filter_map(|id| TaskId::new(id.as_str()).ok())
        .collect();
    for mut spec in dependents {
        let mut deps: BTreeSet<TaskId> = spec.depends_on.drain(..).filter(|d| d != split).collect();
        deps.extend(new.iter().cloned());
        spec.depends_on = deps.into_iter().collect();
        let id = spec.id.clone();
        record_task(host, &id, TaskEvent::Created(Box::new(spec)))?;
    }
    Ok(())
}

/// One past the highest `task-NNN` number in the run.
fn next_task_number(state: &RunState) -> u32 {
    state
        .tasks
        .by_id
        .keys()
        .filter_map(|id| id.as_str().strip_prefix("task-")?.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::task_spec;

    #[test]
    fn split_tasks_take_new_numbers_and_the_old_dependencies() {
        let mut split = task_spec("task-002", &["task-001"], Some("codex"));
        split.reviewers = vec!["agy".parse().unwrap()];
        let a = task_spec("task-001", &[], None);
        let b = task_spec("task-002", &["task-001"], None);
        let tasks = renumbered(vec![a, b], &split, 7);
        let ids: Vec<&str> = tasks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["task-007", "task-008"]);
        assert_eq!(tasks[0].depends_on, [TaskId::new("task-001").unwrap()]);
        assert_eq!(
            tasks[1].depends_on,
            [
                TaskId::new("task-001").unwrap(),
                TaskId::new("task-007").unwrap()
            ]
        );
        assert_eq!(tasks[1].worker, Some("codex".parse().unwrap()));
        assert_eq!(tasks[1].reviewers, split.reviewers);
    }

    #[test]
    fn new_task_numbers_follow_the_highest() {
        let mut state = RunState::default();
        for id in ["task-003", "task-010", "experiment-001"] {
            let spec = task_spec(id, &[], None);
            state
                .tasks
                .by_id
                .insert(spec.id.clone(), TaskEntry::new(spec));
        }
        assert_eq!(next_task_number(&state), 11);
        assert_eq!(next_task_number(&RunState::default()), 1);
    }
}
