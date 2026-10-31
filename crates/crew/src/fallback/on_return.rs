//! When the original agent is healthy again or its plan usage resets, `on_return` decides for each
//! task with debt: `ask` (the "original is back" question), `keep`, `review` (the original reviews
//! everything since the checkpoint, then approves it or sends it back, clearing the debt),
//! `give_back`, or `redo` (reset to the checkpoint, the fallback's work kept on a side
//! branch). If a step is running, the choice applies after it.
//!
//! A task at rest takes a new worker or a reset branch at once and goes back in line. Between
//! its session and merge it goes back to `Running`, so its checks and review never run on
//! work its new worker hasn't done. The review waits for the task's own review step, after its
//! checks pass, and runs once: its verdict clears the debt or sends the task to you, and a review
//! that couldn't happen leaves the debt to the merge rule.
//!
//! Nothing here is held in memory: what to do is worked out again from the log and the health
//! board at every safe point, so a choice survives a crash, and applying it twice is harmless.

use argonauts_core::base::names::{AgentId, ModelRef};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::{
    AnswerAction, ApprovalKind, ApprovalPacket, FreeText, OriginalReturnedPacket,
};
use argonauts_core::runs::events::run::Party;
use argonauts_core::runs::events::task::{TaskEvent, TaskState};
use argonauts_core::runs::fallback::{CheckpointTag, QualityDebt};
use argonauts_core::safety::policy::EvidenceFacts;
use argonauts_core::settings::fallback::OnReturn;
use argonauts_core::workflows::steps::StepName;
use argonauts_core::{DiffStat, Note, NoteLevel, TaskId};

use crate::approvals::options;
use crate::approvals::{self, ApprovalRequest};
use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::Host;
use crate::engine::job_fallback;
use crate::fallback::{assign, rewind, rules, upgrade};
use crate::review::Verdict;
use crate::roles::display_name;
use crate::sessions::labels;
use crate::state::RunState;
use crate::state::tasks::TaskEntry;
use crate::task::machine::Trigger;
use crate::task::{pipeline, take_over};
use crate::workflow::StepAt;

/// A task whose original worker can work again while someone else holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Returning {
    pub original: ModelRef,
    pub fallback: ModelRef,
    pub debt: Option<QualityDebt>,
    pub rule: OnReturn,
}

/// What happens to the task, once decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Keep,
    Review,
    GiveBack,
    Redo,
    TakeOver,
}

/// What applying a choice did, for the task's pipeline to go on from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    Nothing,
    /// The task moved on its own: you took it over, or the original's review sent it to you.
    Moved,
    /// The worker or the branch changed: the next session is a fresh one for the original.
    NewWorker {
        why: String,
    },
    /// The original's review asked for changes, and the task is back in `Running`.
    Changes {
        round: u32,
        comments: Vec<String>,
    },
}

impl Choice {
    pub fn from_answer(action: &AnswerAction) -> Option<Self> {
        match action {
            AnswerAction::UpgradeReview => Some(Self::Review),
            AnswerAction::GiveBack => Some(Self::GiveBack),
            AnswerAction::Redo => Some(Self::Redo),
            AnswerAction::KeepFallback => Some(Self::Keep),
            AnswerAction::TakeOver => Some(Self::TakeOver),
            _ => None,
        }
    }

    /// The setting's choice, `None` for `ask` (the question decides) and for a task without debt
    /// under any rule but `give_back`.
    pub fn from_rule(rule: OnReturn, has_debt: bool) -> Option<Self> {
        match (rule, has_debt) {
            (OnReturn::GiveBack, _) => Some(Self::GiveBack),
            (_, false) | (OnReturn::Ask, true) => None,
            (OnReturn::Keep, true) => Some(Self::Keep),
            (OnReturn::Review, true) => Some(Self::Review),
            (OnReturn::Redo, true) => Some(Self::Redo),
        }
    }
}

/// `agent_id` can take work again: each task that fell back from it is given back, asked
/// about, or left where it is with a line that says how to move it, as `on_return` says. A
/// task whose own model is still out waits for that model.
pub async fn agent_back(host: &Host, agent_id: &AgentId) -> EngineResult<()> {
    reopen_merge(host, agent_id)?;
    let who = DisplayNames::default().cli(&agent_id.0);
    originals_back(host, &who, |original| original.agent_id() == *agent_id).await
}

/// One model is back while its CLI went on working: the same for the tasks that fell back
/// from that model, wherever they went, another model of its own CLI included.
pub async fn model_back(host: &Host, model: &ModelRef) -> EngineResult<()> {
    let who = DisplayNames::default().worker(model);
    originals_back(host, &who, |original| original == model).await
}

/// Acts on every task whose original `is_back` says returned. `who` names what came back.
async fn originals_back(
    host: &Host,
    who: &str,
    is_back: impl Fn(&ModelRef) -> bool,
) -> EngineResult<()> {
    let tasks: Vec<TaskEntry> = host.read(|state| state.tasks.by_id.values().cloned().collect());
    for record in tasks {
        let Some(back) = returning(host, &record) else {
            continue;
        };
        if !is_back(&back.original) {
            continue;
        }
        let task = record.spec.id.clone();
        match (&back.debt, back.rule) {
            (None, OnReturn::GiveBack) => {}
            (None, _) => {
                notice(host, &task, &back, who)?;
                continue;
            }
            (Some(debt), OnReturn::Ask) => {
                ask_in_background(host, &task, debt.since);
                continue;
            }
            _ => {}
        }
        if idle(host, &task) {
            apply_pending(host, &task).await?;
        }
    }
    Ok(())
}

/// A merge question whose debt waits on `agent` offered to wait for it, which is moot now. It's
/// withdrawn, so the merge step decides again with the original back.
fn reopen_merge(host: &Host, agent_id: &AgentId) -> EngineResult<()> {
    let waiting = host.read(|state| {
        let open = state.approvals.open_for(ApprovalKind::Merge, None)?;
        let ApprovalPacket::Merge(packet) = &open.opened.packet else {
            return None;
        };
        let waits = packet
            .rows
            .iter()
            .flat_map(|row| &row.debt)
            .any(|debt| debt.needs.agent_id() == *agent_id);
        waits.then(|| open.opened.id.clone())
    });
    match waiting {
        Some(id) => approvals::withdraw(
            host,
            &id,
            &format!("{} is back", DisplayNames::default().cli(&agent_id.0)),
        ),
        None => Ok(()),
    }
}

/// Called by the task pipeline at each safe point between steps, and for a task at rest when
/// the choice is made: applies a return choice decided for this task while a step ran.
pub async fn apply_pending(host: &Host, task: &TaskId) -> EngineResult<Applied> {
    let record = rules::record(host, task)?;
    let Some(back) = returning(host, &record) else {
        return Ok(Applied::Nothing);
    };
    let (choice, by) = match (
        &back.debt,
        Choice::from_rule(back.rule, back.debt.is_some()),
    ) {
        (Some(debt), None) if back.rule == OnReturn::Ask => {
            match host.read(|state| answered(state, task, debt.since)) {
                Some(answer) => answer,
                None => return Ok(Applied::Nothing),
            }
        }
        (_, Some(choice)) => (choice, Party::Rules),
        (_, None) => return Ok(Applied::Nothing),
    };
    act(host, &record, &back, choice, by).await
}

/// The original of a task someone else holds, when it can work again. Tasks already in the
/// run branch are left to the merge approval, in `merge_return`.
pub fn returning(host: &Host, record: &TaskEntry) -> Option<Returning> {
    if matches!(
        record.state,
        TaskState::Merging | TaskState::Merged | TaskState::Cancelled | TaskState::TakenOver
    ) {
        return None;
    }
    let fallback = rules::worker(record)?;
    // A task planned without a worker, such as the Captain task, is the main agent's.
    let original = record
        .debt
        .as_ref()
        .map(|debt| debt.needs.clone())
        .or_else(|| record.spec.worker.clone())
        .unwrap_or_else(|| host.settings().crew.main.clone());
    // Another model of the original's own CLI is a fallback too. The original is back once
    // its CLI takes work and the model itself is neither out nor under test.
    if original == fallback || !rules::takes_work(host, &original) {
        return None;
    }
    Some(Returning {
        rule: rules::applied_for(host, &record.spec, &original).on_return,
        original,
        fallback,
        debt: record.debt.clone(),
    })
}

/// Your answer on this task's "original is back" question for the debt that starts at `since`.
pub fn answered(state: &RunState, task: &TaskId, since: u32) -> Option<(Choice, Party)> {
    let record = state
        .approvals
        .last_decided(ApprovalKind::OriginalReturned, Some(task))?;
    let ApprovalPacket::OriginalReturned(packet) = &record.opened.packet else {
        return None;
    };
    let decided = record.decided.as_ref()?;
    if packet.since != since {
        return None;
    }
    Some((Choice::from_answer(&decided.action)?, decided.by.clone()))
}

fn idle(host: &Host, task: &TaskId) -> bool {
    host.read(|state| {
        state
            .tasks
            .get(task)
            .is_some_and(|record| rules::at_rest(record.state))
            && !rules::session_running(state, task)
    })
}

async fn act(
    host: &Host,
    record: &TaskEntry,
    back: &Returning,
    choice: Choice,
    by: Party,
) -> EngineResult<Applied> {
    let task = &record.spec.id;
    match choice {
        Choice::Keep => Ok(Applied::Nothing),
        Choice::Review => review(host, record, back).await,
        Choice::GiveBack => {
            let since = back
                .debt
                .as_ref()
                .map_or(record.fallbacks, |debt| debt.since);
            let why = give_back_why(back, since);
            give_back(host, record, back, &why, by).await?;
            Ok(Applied::NewWorker { why })
        }
        Choice::Redo => {
            let since = back.debt.as_ref().map_or(0, |debt| debt.since);
            let kept_on = rewind::reset_to(host, record, since).await?;
            host.record(
                Some(task),
                None,
                RunEventKind::Task(TaskEvent::Rewound { to: since, kept_on }),
            )?;
            // Folding `Rewound` clears the debt: the work it was for is off the branch.
            let why = redo_why(back, since);
            let record = rules::record(host, task)?;
            give_back(host, &record, back, &why, by).await?;
            Ok(Applied::NewWorker { why })
        }
        // Only a running task or one that needs you can be taken over; anywhere else the choice
        // waits for the task to get there.
        Choice::TakeOver if matches!(record.state, TaskState::Running | TaskState::NeedsYou) => {
            take_over::take_over(host, task)
                .await
                .map_err(EngineError::Refused)?;
            Ok(Applied::Moved)
        }
        Choice::TakeOver => Ok(Applied::Nothing),
    }
}

/// The original reviews the task once its checks passed, unless its review is already the
/// newest. After an approval the task's own review still runs; changes go back to the worker,
/// and a rejection sends the task to you.
async fn review(host: &Host, record: &TaskEntry, back: &Returning) -> EngineResult<Applied> {
    let task = &record.spec.id;
    if record.state != TaskState::InReview || upgrade::reviewed_last(record, &back.original) {
        return Ok(Applied::Nothing);
    }
    let round = upgrade::next_round(&record.reviews);
    let (verdict, _) = upgrade::review(host, task, &back.original).await?;
    let reviewed = host.read(|state| {
        state
            .tasks
            .get(task)
            .is_some_and(|record| upgrade::reviewed_last(record, &back.original))
    });
    let change_rounds = host.read(|state| state.tasks.progress(task).change_rounds);
    match verdict {
        Verdict::Approved => Ok(Applied::Nothing),
        Verdict::ChangesRequested { comments }
            if change_rounds < host.settings().review.max_change_rounds =>
        {
            let reason = format!("{}'s review asked for changes", back.original);
            pipeline::transition(host, task, Trigger::ChangesRequested, &reason)?;
            Ok(Applied::Changes { round, comments })
        }
        verdict @ Verdict::ChangesRequested { .. } => {
            let what = format!(
                "{}'s review asked for changes again after {change_rounds} rounds",
                back.original
            );
            let reason = crate::review::with_findings(what, &verdict);
            pipeline::transition(host, task, Trigger::ChangesRequestedAgain, &reason)?;
            Ok(Applied::Moved)
        }
        // The review didn't happen: the task's own review goes on, and the debt stays for
        // the merge rule.
        Verdict::NeedsYou { .. } if !reviewed => Ok(Applied::Nothing),
        Verdict::NeedsYou { reasons } => {
            let reason = format!(
                "{} rejected the work: {}",
                back.original,
                reasons.join("; ")
            );
            pipeline::transition(host, task, Trigger::ChangesRequestedAgain, &reason)?;
            Ok(Applied::Moved)
        }
    }
}

pub(crate) fn give_back_why(back: &Returning, since: u32) -> String {
    format!(
        "{} is back and continues the task from its current state. {} worked on it since \
         fallback-{since}.",
        back.original, back.fallback
    )
}

pub(crate) fn redo_why(back: &Returning, since: u32) -> String {
    format!(
        "{} is back and redoes the task from fallback-{since}. The work {} did after it is \
         kept on a side branch.",
        back.original, back.fallback
    )
}

async fn give_back(
    host: &Host,
    record: &TaskEntry,
    back: &Returning,
    why: &str,
    by: Party,
) -> EngineResult<()> {
    reassign(host, record, back, why, by).await?;
    rules::back_in_line(host, &record.spec.id, &format!("{} is back", back.original))
}

/// Gives the task to its original, with a note that says why.
pub(crate) async fn reassign(
    host: &Host,
    record: &TaskEntry,
    back: &Returning,
    why: &str,
    by: Party,
) -> EngineResult<()> {
    let task = &record.spec.id;
    let mut note = host.services().fallback.note(&record.spec, &[]);
    note.why_stopped = why.to_owned();
    assign::write_note(host, task, &note).await?;
    host.record(
        Some(task),
        None,
        RunEventKind::Task(TaskEvent::Reassigned {
            from: Some(back.fallback.clone()),
            to: back.original.clone(),
            by,
            rule_override: None,
        }),
    )?;
    say_in_the_view(host, task, &back.original)
}

/// The task's workflow says the return in one line as well. Its view shows the newest line
/// about it under its header, which would otherwise still read as if the fallback held the
/// task.
fn say_in_the_view(host: &Host, task: &TaskId, original: &ModelRef) -> EngineResult<()> {
    let (label, step) = host.read(|state| {
        let built_in = state
            .sessions
            .by_key
            .values()
            .filter(|session| session.task.as_ref() == Some(task))
            .filter(|session| session.started.step == Some(StepName::Build))
            .max_by_key(|session| session.since)
            .and_then(|session| session.started.workflow_step.clone());
        let step = built_in.or_else(|| rules::current_step(state).map(|at| at.step));
        (labels::task_label(state, task), step)
    });
    // A run with no step under way has no workflow's view to say it in.
    let Some(step) = step else {
        return Ok(());
    };
    job_fallback::say_in_the_view(host, &step.workflow, back_in_the_view(original, &label))
}

/// What a workflow's view says under its header once a task is its first worker's again, such
/// as `Opus 5.5 on Claude Code is back · build:due-flag carries on with it`.
fn back_in_the_view(original: &ModelRef, task: &str) -> String {
    format!(
        "{} is back · {task} carries on with it",
        display_name(original)
    )
}

fn notice(host: &Host, task: &TaskId, back: &Returning, who: &str) -> EngineResult<()> {
    let text = format!(
        "{who} is back · {task} stays with {} · /reassign {task} {} moves it back",
        back.fallback, back.original
    );
    host.record(
        Some(task),
        None,
        RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text,
        }),
    )?;
    Ok(())
}

/// Opens the "original is back" question without holding up the caller, which is the engine's
/// loop. Your answer applies at once when the task is at rest, else at its next safe point.
fn ask_in_background(host: &Host, task: &TaskId, since: u32) {
    let already = host.read(|state| {
        state.approvals.by_id.values().any(|record| {
            record.withdrawn.is_none()
                && record.opened.task.as_ref() == Some(task)
                && matches!(
                    &record.opened.packet,
                    ApprovalPacket::OriginalReturned(packet) if packet.since == since
                )
        })
    });
    if already {
        return;
    }
    let host = host.clone();
    let task = task.clone();
    tokio::spawn(async move {
        let result = tokio::select! {
            () = host.interrupted().cancelled() => Ok(()),
            result = ask(&host, &task) => result,
        };
        if let Err(err) = result {
            let text = format!("The \"original is back\" question for {task} failed: {err}");
            let note = RunEventKind::Note(Note {
                level: NoteLevel::Error,
                text,
            });
            if let Err(err) = host.record(Some(&task), None, note) {
                tracing::error!(%task, %err, "could not record a failed return question");
            }
        }
    });
}

async fn ask(host: &Host, task: &TaskId) -> EngineResult<()> {
    let record = rules::record(host, task)?;
    let Some(back) = returning(host, &record) else {
        return Ok(());
    };
    let Some(debt) = back.debt.clone() else {
        return Ok(());
    };
    let at = host.read(rules::current_step);
    let request = request(host, &record, &back, &debt, at).await?;
    // Withdrawn, the debt went with the work it was for, or the merge approval asks instead.
    if approvals::open(host, request)?
        .decided_or_withdrawn(host)
        .await?
        .is_err()
    {
        return Ok(());
    }
    if idle(host, task) {
        apply_pending(host, task).await?;
    }
    Ok(())
}

/// The "original is back" question for the debt.
pub(crate) async fn request(
    host: &Host,
    record: &TaskEntry,
    back: &Returning,
    debt: &QualityDebt,
    at: Option<StepAt>,
) -> EngineResult<ApprovalRequest> {
    let task = &record.spec.id;
    let packet = packet(host, record, back, debt).await?;
    let summary = format!(
        "{} commits by {} since fallback-{}, {} files changed",
        packet.commits, back.fallback, debt.since, packet.changed.files_changed
    );
    Ok(ApprovalRequest {
        at,
        kind: ApprovalKind::OriginalReturned,
        title: format!(
            "{} is back · {}",
            DisplayNames::default().cli(&back.original.agent_id().0),
            host.read(|state| crate::sessions::labels::task_label(state, task))
        ),
        task: Some(task.clone()),
        packet: ApprovalPacket::OriginalReturned(packet),
        facts: EvidenceFacts::default(),
        agent_outputs: vec![summary],
        documents: Vec::new(),
        options: Some(options::original_returned(&back.original, &back.fallback)),
        free_text: FreeText::None,
        authors: vec![back.fallback.clone()],
    })
}

/// The task's work since the checkpoint the debt starts at.
async fn packet(
    host: &Host,
    record: &TaskEntry,
    back: &Returning,
    debt: &QualityDebt,
) -> EngineResult<OriginalReturnedPacket> {
    let task = &record.spec.id;
    let git = host.git();
    let root = host.repo().root.clone();
    let since_commit = if debt.since == 0 {
        record.base.clone()
    } else {
        let tag = CheckpointTag {
            run: host.run_id().clone(),
            task: task.clone(),
            n: debt.since,
        };
        git.tag_target(&root, &tag.tag_name()).await?
    };
    let (commits, changed) = match (since_commit, &record.branch) {
        (Some(from), Some(branch)) => {
            let head = git.resolve(&root, branch).await?;
            let commits = git.log_messages(&root, &format!("{from}..{head}")).await?;
            let changed = git.diff_stat(&root, &from, &head).await?;
            (u32::try_from(commits.len()).unwrap_or(u32::MAX), changed)
        }
        _ => (0, DiffStat::default()),
    };
    Ok(OriginalReturnedPacket {
        task: task.clone(),
        title: record.spec.title.clone(),
        original: back.original.clone(),
        fallback: back.fallback.clone(),
        since: debt.since,
        commits,
        changed,
        tests_passed: record.checks.last().map(|check| check.passed),
        debt: vec![debt.clone()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_and_answers_map_to_choices() {
        assert_eq!(
            Choice::from_rule(OnReturn::GiveBack, false),
            Some(Choice::GiveBack)
        );
        assert_eq!(Choice::from_rule(OnReturn::Review, false), None);
        assert_eq!(Choice::from_rule(OnReturn::Ask, true), None);
        assert_eq!(Choice::from_rule(OnReturn::Keep, true), Some(Choice::Keep));
        assert_eq!(Choice::from_rule(OnReturn::Redo, true), Some(Choice::Redo));
        assert_eq!(
            Choice::from_answer(&AnswerAction::UpgradeReview),
            Some(Choice::Review)
        );
        assert_eq!(
            Choice::from_answer(&AnswerAction::KeepFallback),
            Some(Choice::Keep)
        );
        assert_eq!(Choice::from_answer(&AnswerAction::ShowDiff), None);
    }

    #[test]
    fn a_return_reads_in_the_view_as_the_fallback_did() {
        let opus: ModelRef = "claude-code/opus".parse().expect("a model");
        assert_eq!(
            back_in_the_view(&opus, "build:due-flag"),
            format!(
                "{} is back · build:due-flag carries on with it",
                display_name(&opus)
            )
        );
    }
}
