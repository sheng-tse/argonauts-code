//! The original back while its task waits at the merge approval, already in the run's branch with
//! debt the merge rule enforces. `on_return` decides as it does for a task still being worked on:
//! `ask` opens the "original is back" question in place of the merge question, whose offer to wait
//! for the original is moot now. The merge rule has the original review the task first when the
//! rule is `keep`, when you chose to wait for it at the merge question, when the task is already
//! back with it, and when the log has no outage on it. Keeping the fallback's work from the
//! "original is back" question leaves the debt to the merge question.

use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::{AnswerAction, ApprovalKind};
use argonauts_core::runs::events::run::Party;
use argonauts_core::runs::events::task::TaskEvent;
use argonauts_core::runs::fallback::QualityDebt;
use argonauts_core::{Note, NoteLevel, TaskId};

use crate::approvals;
use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::Host;
use crate::fallback::on_return::{self, Choice, Returning};
use crate::fallback::{merge_rule, rewind, rules, upgrade};
use crate::review::Verdict;
use crate::state::tasks::TaskEntry;
use crate::workflow::StepAt;

/// How a task whose original is back runs to merge again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rerun {
    /// The original's review asked for changes.
    Changes,
    /// The original takes it, starting fresh with why: from the run's branch, or from the
    /// checkpoint for a redo.
    NewWorker { why: String },
    /// You took it over.
    TakeOver,
}

/// Decides and acts for each task whose debt blocks the merge and whose original is back,
/// asking you first when the rule is `ask`. Returns the tasks to run to merge again.
pub async fn decide(host: &Host, at: &StepAt) -> EngineResult<Vec<(TaskId, Rerun)>> {
    let mut reruns = Vec::new();
    for (task, debt) in merge_rule::blocking_debt(host) {
        if !rules::agent_usable(host, &debt.needs.agent_id()) {
            continue;
        }
        let record = rules::record(host, &task)?;
        if upgrade::reviewed_last(&record, &debt.needs) {
            continue;
        }
        let Some((choice, back, by)) = choice(host, &record, &debt, at).await? else {
            continue;
        };
        if let Some(rerun) = act(host, &record, &back, &debt, choice, by).await? {
            reruns.push((task, rerun));
        }
    }
    Ok(reruns)
}

/// What happens to the task, and who decided it. `None` when its question was withdrawn.
async fn choice(
    host: &Host,
    record: &TaskEntry,
    debt: &QualityDebt,
    at: &StepAt,
) -> EngineResult<Option<(Choice, Returning, Party)>> {
    let task = &record.spec.id;
    let fallback = rules::worker(record).unwrap_or_else(|| debt.by.clone());
    let back = Returning {
        rule: rules::applied_for(host, &record.spec, &debt.needs).on_return,
        original: debt.needs.clone(),
        fallback,
        debt: Some(debt.clone()),
    };
    // An original the log has no outage on, such as one whose plan usage reads fine again, and one
    // that holds the task again leave only the merge rule's review.
    let came_back = host.read(|state| state.run.agent_out(&back.original.agent_id()));
    let decided = if !came_back || back.fallback.agent_id() == back.original.agent_id() {
        Some((Choice::Review, Party::Rules))
    } else if let Some(choice) = Choice::from_rule(back.rule, true) {
        let choice = if choice == Choice::Keep {
            Choice::Review
        } else {
            choice
        };
        Some((choice, Party::Rules))
    } else if let Some(answer) = host.read(|state| on_return::answered(state, task, debt.since)) {
        Some(answer)
    } else if waited_for_originals(host) {
        Some((Choice::Review, Party::Rules))
    } else {
        None
    };
    match decided {
        Some((choice, by)) => Ok(Some((choice, back, by))),
        None => ask(host, record, back, debt, at).await,
    }
}

/// Your newest answer at the merge question was to wait for the originals.
fn waited_for_originals(host: &Host) -> bool {
    host.read(|state| {
        state
            .approvals
            .last_decided(ApprovalKind::Merge, None)
            .and_then(|record| record.decided.as_ref())
            .is_some_and(|decided| decided.action == AnswerAction::Wait)
    })
}

/// Opens the "original is back" question in place of the merge question and waits for your answer.
async fn ask(
    host: &Host,
    record: &TaskEntry,
    back: Returning,
    debt: &QualityDebt,
    at: &StepAt,
) -> EngineResult<Option<(Choice, Returning, Party)>> {
    let merge_approval = host.read(|state| {
        state
            .approvals
            .open_for(ApprovalKind::Merge, None)
            .map(|record| record.opened.id.clone())
    });
    if let Some(id) = merge_approval {
        let reason = format!(
            "{} is back",
            DisplayNames::default().cli(&back.original.agent_id().0)
        );
        approvals::withdraw(host, &id, &reason)?;
    }
    let request = on_return::request(host, record, &back, debt, Some(at.clone())).await?;
    let Ok(decided) = approvals::open(host, request)?
        .decided_or_withdrawn(host)
        .await?
    else {
        return Ok(None);
    };
    Ok(Choice::from_answer(&decided.action).map(|choice| (choice, back, decided.by)))
}

/// Applies the choice to the merged task. Returns how it runs again, if it does.
async fn act(
    host: &Host,
    record: &TaskEntry,
    back: &Returning,
    debt: &QualityDebt,
    choice: Choice,
    by: Party,
) -> EngineResult<Option<Rerun>> {
    let task = &record.spec.id;
    match choice {
        Choice::Keep => Ok(None),
        Choice::Review => {
            let (verdict, _) = upgrade::review(host, task, &back.original).await?;
            Ok(matches!(verdict, Verdict::ChangesRequested { .. }).then_some(Rerun::Changes))
        }
        Choice::GiveBack => {
            let why = on_return::give_back_why(back, debt.since);
            on_return::reassign(host, record, back, &why, by).await?;
            Ok(Some(Rerun::NewWorker { why }))
        }
        Choice::Redo => {
            let kept_on = match rewind::reset_to(host, record, debt.since).await {
                Ok(kept_on) => kept_on,
                // Nothing changed, so the debt stays for the merge question.
                Err(EngineError::Refused(why)) => {
                    let text = format!(
                        "{task} couldn't be redone from fallback-{}: {why}",
                        debt.since
                    );
                    host.record(
                        Some(task),
                        None,
                        RunEventKind::Note(Note {
                            level: NoteLevel::Warn,
                            text,
                        }),
                    )?;
                    return Ok(None);
                }
                Err(err) => return Err(err),
            };
            host.record(
                Some(task),
                None,
                RunEventKind::Task(TaskEvent::Rewound {
                    to: debt.since,
                    kept_on,
                }),
            )?;
            let why = on_return::redo_why(back, debt.since);
            let record = rules::record(host, task)?;
            on_return::reassign(host, &record, back, &why, by).await?;
            Ok(Some(Rerun::NewWorker { why }))
        }
        Choice::TakeOver => Ok(Some(Rerun::TakeOver)),
    }
}
