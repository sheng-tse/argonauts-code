//! The `merge` step: main merged in if it moved, the final check, the merge approval with the
//! debt rule, and landing by your answer. Only tasks that passed merge: when none did, the
//! merge never opens, and the run ends saying what stopped each task.

use futures::future::join_all;

use argonauts_core::PhaseStatus;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::{
    AnswerAction, AnswerOption, ApprovalDecided, ApprovalKind, ApprovalPacket, FreeText,
};
use argonauts_core::events::run::{Party, PhaseChange, RunOutcome, RunPhase};
use argonauts_core::events::task::TaskState;
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::fallback::QualityDebt;
use argonauts_core::names::{AgentId, ApprovalId};
use argonauts_core::policy::EvidenceSubject;
use argonauts_core::settings::protocol::MergeMode;
use argonauts_core::text::clause_case;
use argonauts_core::{Note, NoteLevel, TaskId};

use crate::approvals::{self, ApprovalRequest, options};
use crate::engine::commands;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::fallback::merge_return::{self, Rerun};
use crate::fallback::{merge_rule, rules, upgrade};
use crate::merge::commit_message;
use crate::merge::final_check::{self, FinalCheck};
use crate::merge::land::{self, Landing};
use crate::merge::merge_packet::{self, MergeEvidence};
use crate::merge::your_folder;
use crate::review::Verdict;
use crate::run_review;
use crate::state::merge::MergeState;
use crate::task::pipeline;
use crate::workflow::{Flow, StepCall, StepDone};

pub async fn merge_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    // Re-entered after a crash that came after landing: finish the way it already ended.
    if let Some(outcome) = host.read(|state| ended(&state.merge)) {
        return finish(host, outcome, outcome_summary(outcome));
    }
    // Tasks the approval sent back before a restart get back in first.
    run_all(host, call, &in_flight(host)).await?;
    if let Some(why) = host.read(nothing_passed) {
        return nothing_to_merge(host, why);
    }
    // Why the approval opens again. It goes into the evidence, so a new one opens.
    let mut notices: Vec<String> = Vec::new();
    let mut seen: Vec<ApprovalId> = Vec::new();
    let mut again: Option<Again> = None;
    loop {
        let answer = match answered(host, call, &seen).await? {
            Some((decided, approved)) => Answer::Given(Box::new((decided, approved))),
            None => Answer::Pending(checked(host).await?),
        };
        enter(host, RunPhase::Merge)?;
        match your_folder::merge_mode(host) {
            MergeMode::Leave => {
                return finish(host, RunOutcome::BranchLeft, "branch kept by merge.mode");
            }
            MergeMode::PullRequest => {
                return match land::pull_request_by_rule(host).await? {
                    Landing::Done(outcome) => {
                        finish(host, outcome, "pull request opened by merge.mode")
                    }
                    // Your settings chose a pull request over merging, so a run that can't
                    // open one is left on its branch, said plainly, and never merged unasked.
                    Landing::Again(why) => {
                        let note = Note {
                            level: NoteLevel::Warn,
                            text: format!("No pull request was opened: {why}."),
                        };
                        host.record(None, None, RunEventKind::Note(note))?;
                        finish(
                            host,
                            RunOutcome::BranchLeft,
                            "branch kept, no pull request could be opened",
                        )
                    }
                    Landing::Later => merge_later(host).await,
                };
            }
            MergeMode::Ask => {}
        }
        let returned = merge_return::decide(host, &call.at).await?;
        if !returned.is_empty() {
            notices.extend(rerun_returned(host, call, &returned).await?);
            continue;
        }
        let blocked = merge_rule::blocking_debt(host);
        let (decided, approved) = match answer {
            Answer::Given(given) => *given,
            Answer::Pending(check) => {
                match ask(host, call, &check, &blocked, &notices, &again, &seen).await? {
                    Ok(asked) => asked,
                    // Something changed under the question, such as a task you rewound, so it opens
                    // again on what's true now.
                    Err(why) => {
                        notices.push(why);
                        continue;
                    }
                }
            }
        };
        if seen.contains(&decided.id) {
            return Err(EngineError::Refused(
                "the merge approval could not open again".to_owned(),
            ));
        }
        seen.push(decided.id.clone());
        again = None;

        // Every way back to the approval adds a line, so its evidence is never the same as a
        // decision already acted on.
        match decided.action {
            AnswerAction::RequestChanges => {
                let tasks = requested(host, &decided);
                let note = decided
                    .text
                    .clone()
                    .unwrap_or_else(|| "changes requested at the merge approval".to_owned());
                let sent = rerun(host, call, &tasks, &note).await?;
                notices.extend(sent_back_notices("changes requested", &tasks, &sent));
            }
            AnswerAction::Wait => {
                wait_for_originals(host, &blocked).await?;
                notices.push(format!(
                    "waited for the originals of {}",
                    task_list(&blocked)
                ));
            }
            AnswerAction::UpgradeReview => {
                let Some(reviewer) = merge_rule::stronger_candidate(host, &blocked) else {
                    notices.push("no model at least as strong is free to review".to_owned());
                    continue;
                };
                let mut asked = Vec::new();
                for (task, _) in &blocked {
                    let (verdict, _) = upgrade::review(host, task, &reviewer).await?;
                    if verdict != Verdict::Approved {
                        asked.push(task.clone());
                    }
                }
                let note = "the upgrade review asked for changes";
                let sent = rerun(host, call, &asked, note).await?;
                notices.push(format!("{reviewer} reviewed {}", task_list(&blocked)));
                notices.extend(sent_back_notices(note, &asked, &sent));
            }
            AnswerAction::Merge | AnswerAction::Approve
                if !blocked.is_empty() && !merge_rule::may_merge_with_debt(host, &decided.by) =>
            {
                notices.push(format!(
                    "merging with quality debt on {} needs you, since \
                     approvals.rules.merge.allow_debt is off",
                    task_list(&blocked)
                ));
            }
            _ => match land::act(host, &decided, &approved).await? {
                Landing::Done(outcome) => {
                    return finish(host, outcome, outcome_summary(outcome));
                }
                Landing::Again(why) => {
                    // Whoever decided in your place would decide the same again and meet the
                    // same refusal, so the next question is yours alone, and where nobody is
                    // asked the run keeps its branch.
                    let yours = decided.by != Party::User;
                    if yours && approvals::autopilot::applies_now(host, ApprovalKind::Merge) {
                        return keep_unlanded(host, &why);
                    }
                    notices.push(why.clone());
                    again = Some(Again { why, yours });
                }
                Landing::Later => return merge_later(host).await,
            },
        }
    }
}

/// Why the merge question opens again after an answer that couldn't be done.
struct Again {
    why: String,
    /// The answer wasn't yours, so the question is kept from whoever gave it.
    yours: bool,
}

/// Ends the run with its work on its branch, in autopilot, when what was approved couldn't
/// land, and says why in one line.
fn keep_unlanded(host: &Host, why: &str) -> EngineResult<StepDone> {
    let branch = host
        .read(merge_packet::landing_branch_of)
        .unwrap_or_else(|| "its branch".to_owned());
    let why = why.trim().trim_end_matches('.');
    let note = Note {
        level: NoteLevel::Warn,
        text: format!(
            "Autopilot kept the run's work on {branch} instead of asking you again, since {}.",
            clause_case(why)
        ),
    };
    host.record(None, None, RunEventKind::Note(note))?;
    finish(
        host,
        RunOutcome::BranchLeft,
        "branch kept, what was approved couldn't merge",
    )
}

/// Why the run has nothing to merge: no task passed. `None` once one did.
fn nothing_passed(state: &crate::state::RunState) -> Option<String> {
    let records: Vec<_> = state.tasks.by_id.values().collect();
    if records
        .iter()
        .any(|record| merge_packet::in_the_merge(record))
    {
        return None;
    }
    if records.is_empty() {
        return Some("Nothing was merged: the run made no task.".to_owned());
    }
    // The line closes the run in the conversation, so it names each task by its title.
    let stopped: Vec<String> = records
        .iter()
        .map(|record| {
            let called = crate::task::support::called(record);
            let ended = ended_as(record.state);
            let why = state.tasks.progress(&record.spec.id).why.trim();
            if why.is_empty() {
                format!("\"{called}\" {ended}")
            } else {
                format!("\"{called}\" {ended} ({why})")
            }
        })
        .collect();
    Some(format!(
        "Nothing was merged, since no task passed: {}.",
        stopped.join("; ")
    ))
}

/// How a task that never merged stands, as the closing line says it.
fn ended_as(state: TaskState) -> &'static str {
    match state {
        TaskState::Blocked => "is blocked on a question",
        TaskState::NeedsDecision => "waits for a decision",
        TaskState::Failed => "failed",
        TaskState::NeedsYou => "ran out of tries",
        TaskState::Deferred => "was deferred",
        TaskState::Cancelled => "was cancelled",
        TaskState::TakenOver => "was taken over",
        TaskState::Merged => "is an experiment, which never merges",
        _ => "never finished",
    }
}

/// Ends the run without opening the merge, saying why. Nothing lands, so nothing of
/// Argonauts Code's own is ever called merged.
fn nothing_to_merge(host: &Host, why: String) -> EngineResult<StepDone> {
    host.record(
        None,
        None,
        RunEventKind::Note(Note {
            level: NoteLevel::Warn,
            text: why.clone(),
        }),
    )?;
    Ok(StepDone {
        end: StepEnd::Done,
        summary: Some(why),
        flow: Flow::Finish(RunOutcome::Failed),
    })
}

/// You chose to merge later: the run pauses at the merge, waiting to merge, and resuming it
/// acts on the same approval and tries again.
async fn merge_later(host: &Host) -> EngineResult<StepDone> {
    let run = host.run_id();
    let text = if your_folder::is(host) {
        format!(
            "The run waits to be written into your folder, with nothing written yet. /resume \
             {run} takes it up again."
        )
    } else {
        format!(
            "The run waits to merge. Once those files are out of the way, /resume {run} merges it."
        )
    };
    host.record(
        None,
        None,
        RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text,
        }),
    )?;
    if commands::begin_pause_by(host, false, Party::User)? {
        commands::finish_pause(host).await?;
    }
    Err(EngineError::Interrupted)
}

/// Your answer, already given, or the final check the approval opens with.
enum Answer {
    Given(Box<(ApprovalDecided, EvidenceSubject)>),
    Pending(FinalCheck),
}

/// An answer given in this round that hasn't been acted on here, while the branch is still
/// where its approval showed it. A run woken by your answer acts on it without running the
/// suite or building the packet again, which could change the evidence and drop the answer.
async fn answered(
    host: &Host,
    call: &StepCall,
    seen: &[ApprovalId],
) -> EngineResult<Option<(ApprovalDecided, EvidenceSubject)>> {
    let title = title(host).await?;
    let found = approvals::decided_in_round(host, &call.at, ApprovalKind::Merge, &title)?;
    let Some((decided, Some(shown))) = found else {
        return Ok(None);
    };
    if seen.contains(&decided.id) {
        return Ok(None);
    }
    let now = merge_packet::position(host).await?.subject;
    Ok((now == shown).then_some((decided, shown)))
}

/// The final check, run again only when the branch or the target moved since the last one.
/// The run's notes and decisions are committed first, so the check and the approval cover
/// them.
pub(crate) async fn checked(host: &Host) -> EngineResult<FinalCheck> {
    run_review::notes_before_merge(host).await?;
    if let Some(check) = final_check::reusable(host).await? {
        return Ok(check);
    }
    phase(host, RunPhase::FinalCheck, PhaseStatus::Started, None)?;
    let check = final_check::run(host).await?;
    let (status, summary) = if check.passed {
        (PhaseStatus::Finished, "final check passed")
    } else {
        (PhaseStatus::Failed, "final check failed")
    };
    phase(host, RunPhase::FinalCheck, status, Some(summary))?;
    Ok(check)
}

/// Builds the evidence and waits for the approval's decision, or why it was withdrawn.
async fn ask(
    host: &Host,
    call: &StepCall,
    check: &FinalCheck,
    blocked: &[(TaskId, QualityDebt)],
    notices: &[String],
    again: &Option<Again>,
    seen: &[ApprovalId],
) -> EngineResult<Result<(ApprovalDecided, EvidenceSubject), String>> {
    let mut evidence = merge_packet::build(host, check).await?;
    // The merger writes the change's message while the question is open, and what that
    // costs is no new evidence: asked again after a pause, the question keeps the spend it
    // was first asked with, so an answer given to it still holds.
    match host.read(asked_with) {
        Some(spent) => {
            evidence.packet.spent = spent;
            evidence.facts.spent = spent;
        }
        // Answered while this question was being built, so that answer is the one to act on.
        None => {
            if let Some(answer) = answered(host, call, seen).await? {
                return Ok(Ok(answer));
            }
        }
    }
    let Some(approved) = evidence.subject().cloned() else {
        return Err(EngineError::Refused(
            "the merge has nothing to approve".to_owned(),
        ));
    };
    let mut request = request(host, call, title(host).await?, evidence, blocked, notices);
    if let (Some(again), ApprovalPacket::Merge(packet)) = (again, &mut request.packet) {
        packet.again = Some(again.why.clone());
    }
    let opened = match again {
        Some(again) if again.yours => approvals::open_yours(host, request, again.why.clone())?,
        _ => approvals::open(host, request)?,
    };
    // The merger writes the change's message while you decide, so it's ready to land with.
    // An answer given sooner doesn't wait for it: the session is stopped, and the change lands
    // with the message plain code writes.
    let writing = commit_message::Writing::default();
    let written = commit_message::write(host, &writing);
    let deciding = opened.decided_or_withdrawn(host);
    tokio::pin!(written, deciding);
    let decided = tokio::select! {
        written = &mut written => {
            written?;
            deciding.await
        }
        decided = &mut deciding => {
            writing.stop(host);
            written.await?;
            decided
        }
    };
    Ok(decided?.map(|decided| (decided, approved)))
}

/// What the run had spent when the merge question still open was asked.
fn asked_with(state: &crate::state::RunState) -> Option<argonauts_core::Spend> {
    let open = state.approvals.open_for(ApprovalKind::Merge, None)?;
    match &open.opened.packet {
        ApprovalPacket::Merge(packet) => Some(packet.spent),
        _ => None,
    }
}

/// What the merge question asks, such as "Merge this run into main?". The screen shows it, so
/// it names the target and leaves the run's id to the detail views.
async fn title(host: &Host) -> EngineResult<String> {
    if your_folder::is(host) {
        return Ok(your_folder::TITLE.to_owned());
    }
    Ok(match final_check::lands_on(host).await? {
        Some(target) => format!("Merge this run into {target}?"),
        None => "Merge this run?".to_owned(),
    })
}

fn request(
    host: &Host,
    call: &StepCall,
    title: String,
    evidence: MergeEvidence,
    blocked: &[(TaskId, QualityDebt)],
    notices: &[String],
) -> ApprovalRequest {
    let authors = evidence
        .packet
        .rows
        .iter()
        .filter_map(|row| row.worker.clone())
        .collect();
    let mut agent_outputs = evidence.agent_outputs;
    agent_outputs.extend(notices.iter().cloned());
    let options = merge_options(host, blocked);
    ApprovalRequest {
        at: Some(call.at.clone()),
        kind: ApprovalKind::Merge,
        title,
        task: None,
        packet: ApprovalPacket::Merge(evidence.packet),
        facts: evidence.facts,
        agent_outputs,
        documents: evidence.documents,
        options,
        free_text: FreeText::Guidance,
        authors,
    }
}

/// The merge question offers wait, a stronger review, and merge anyway while debt blocks.
fn merge_options(host: &Host, blocked: &[(TaskId, QualityDebt)]) -> Option<Vec<AnswerOption>> {
    if blocked.is_empty() {
        return your_folder::merge_options(host);
    }
    let stronger = merge_rule::stronger_candidate(host, blocked);
    let options = options::merge_with_debt(stronger.as_ref());
    Some(your_folder::without_branch_answers(host, options))
}

/// "Request changes on some tasks": the merged tasks in the answer, or every one when it
/// names none.
fn requested(host: &Host, decided: &ApprovalDecided) -> Vec<TaskId> {
    let merged: Vec<TaskId> = host.read(|state| {
        state
            .tasks
            .by_id
            .values()
            .filter(|record| {
                record.state == TaskState::Merged && merge_packet::in_the_merge(record)
            })
            .map(|record| record.spec.id.clone())
            .collect()
    });
    if decided.tasks.is_empty() {
        return merged;
    }
    decided
        .tasks
        .iter()
        .filter(|task| merged.contains(task))
        .cloned()
        .collect()
}

/// Sends the merged ones of `tasks` back with `note` and runs them to merge again.
/// The rest stay as they are. Returns the tasks sent back.
async fn rerun(
    host: &Host,
    call: &StepCall,
    tasks: &[TaskId],
    note: &str,
) -> EngineResult<Vec<TaskId>> {
    let merged: Vec<TaskId> = host.read(|state| {
        tasks
            .iter()
            .filter(|task| {
                state
                    .tasks
                    .get(task)
                    .is_some_and(|record| record.state == TaskState::Merged)
            })
            .cloned()
            .collect()
    });
    let runs = merged
        .iter()
        .map(|task| pipeline::send_back(host, task, &call.at, note));
    for result in join_all(runs).await {
        result?;
    }
    Ok(merged)
}

/// Runs the tasks whose original came back to merge again. Returns what happened, for
/// the merge question.
async fn rerun_returned(
    host: &Host,
    call: &StepCall,
    returned: &[(TaskId, Rerun)],
) -> EngineResult<Vec<String>> {
    let runs = returned.iter().map(|(task, rerun)| async move {
        match rerun {
            Rerun::Changes => pipeline::send_back(host, task, &call.at, CHANGES_ASKED).await,
            Rerun::NewWorker { why } => pipeline::give_back(host, task, &call.at, why).await,
            Rerun::TakeOver => pipeline::take_over_back(host, task, &call.at).await,
        }
    });
    for result in join_all(runs).await {
        result?;
    }
    Ok(returned
        .iter()
        .map(|(task, rerun)| match rerun {
            Rerun::Changes => format!("{CHANGES_ASKED} on {task}"),
            Rerun::NewWorker { why } => format!("{task}: {why}"),
            Rerun::TakeOver => format!("you took {task} over"),
        })
        .collect())
}

const CHANGES_ASKED: &str = "the original's review asked for changes";

/// Tasks in the merge between `Running` and `Merging`, or taken over from it. Build ends
/// with none, so these were sent back from the approval before a restart.
fn in_flight(host: &Host) -> Vec<TaskId> {
    host.read(|state| {
        state
            .tasks
            .by_id
            .values()
            .filter(|record| {
                merge_packet::in_the_merge(record)
                    && matches!(
                        record.state,
                        TaskState::Running
                            | TaskState::Checking
                            | TaskState::InReview
                            | TaskState::Merging
                            | TaskState::TakenOver
                    )
            })
            .map(|record| record.spec.id.clone())
            .collect()
    })
}

async fn run_all(host: &Host, call: &StepCall, tasks: &[TaskId]) -> EngineResult<()> {
    let runs = tasks
        .iter()
        .map(|task| pipeline::run_task_back(host, task, &call.at));
    for result in join_all(runs).await {
        result?;
    }
    Ok(())
}

fn sent_back_notices(what: &str, asked: &[TaskId], sent: &[TaskId]) -> Vec<String> {
    let kept: Vec<TaskId> = asked
        .iter()
        .filter(|task| !sent.contains(task))
        .cloned()
        .collect();
    let mut notices = Vec::new();
    if !sent.is_empty() {
        notices.push(format!("{what} on {}", ids(sent)));
    }
    if !kept.is_empty() {
        notices.push(format!(
            "{what} on {}, which isn't in the run branch, so it stays as it is",
            ids(&kept)
        ));
    }
    notices
}

/// Waits until every blocked task's original can work again.
async fn wait_for_originals(host: &Host, blocked: &[(TaskId, QualityDebt)]) -> EngineResult<()> {
    let agent_ids: Vec<AgentId> = blocked
        .iter()
        .map(|(_, debt)| debt.needs.agent_id())
        .collect();
    let mut events = host.services().health.subscribe();
    loop {
        if agent_ids
            .iter()
            .all(|agent_id| rules::agent_usable(host, agent_id))
        {
            return Ok(());
        }
        tokio::select! {
            () = host.interrupted().cancelled() => return Err(EngineError::Interrupted),
            event = events.recv() => {
                if let Err(tokio::sync::broadcast::error::RecvError::Closed) = event {
                    return Err(EngineError::Refused(
                        "agent health stopped reporting while the merge waited".to_owned(),
                    ));
                }
            }
        }
    }
}

/// Records the move into `phase` unless the log already has the run there, as it does when
/// a waiting run wakes.
fn enter(host: &Host, phase_now: RunPhase) -> EngineResult<()> {
    if host.read(|state| state.run.phase) == Some(phase_now) {
        return Ok(());
    }
    phase(host, phase_now, PhaseStatus::Started, None)
}

fn phase(
    host: &Host,
    phase: RunPhase,
    status: PhaseStatus,
    summary: Option<&str>,
) -> EngineResult<()> {
    host.record(
        None,
        None,
        RunEventKind::Phase(PhaseChange {
            phase,
            status,
            summary: summary.map(str::to_owned),
        }),
    )?;
    Ok(())
}

fn finish(host: &Host, outcome: RunOutcome, summary: &str) -> EngineResult<StepDone> {
    phase(host, RunPhase::Merge, PhaseStatus::Finished, Some(summary))?;
    Ok(StepDone {
        end: StepEnd::Done,
        summary: Some(summary.to_owned()),
        flow: Flow::Finish(outcome),
    })
}

/// How the run already merged, if it did.
pub fn ended(merge: &MergeState) -> Option<RunOutcome> {
    if merge.run_merged.is_some() {
        Some(RunOutcome::Merged)
    } else if merge.pull_request.is_some() {
        Some(RunOutcome::PullRequest)
    } else if merge.abandoned.is_some() {
        Some(RunOutcome::Abandoned)
    } else {
        None
    }
}

fn outcome_summary(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Merged => "merged",
        RunOutcome::PullRequest => "pull request",
        RunOutcome::BranchLeft => "branch kept",
        RunOutcome::Abandoned => "abandoned, its work removed",
        RunOutcome::Stopped => "stopped",
        RunOutcome::Failed => "failed",
    }
}

fn task_list(blocked: &[(TaskId, QualityDebt)]) -> String {
    let tasks: Vec<TaskId> = blocked.iter().map(|(task, _)| task.clone()).collect();
    ids(&tasks)
}

fn ids(tasks: &[TaskId]) -> String {
    tasks
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::merge::RunMerged;

    #[test]
    fn a_run_that_merged_ends_the_same_way_again() {
        let mut merge = MergeState::default();
        assert_eq!(ended(&merge), None);
        merge.abandoned = Some(Vec::new());
        assert_eq!(ended(&merge), Some(RunOutcome::Abandoned));
        merge.run_merged = Some(RunMerged {
            target: "main".to_owned(),
            sha: "h1".to_owned(),
            fast_forward: true,
        });
        assert_eq!(ended(&merge), Some(RunOutcome::Merged));
    }
}
