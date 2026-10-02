//! The original back while its card waits at the merge checkpoint, already in the run's
//! branch with debt the merge rule enforces. `on_return` decides as it does for a card still
//! being worked on: `ask` opens the "original is back" card in place of the merge card, whose
//! offer to wait for the original is moot now. The merge rule has the original review the
//! card first when the rule is `keep`, when you chose to wait for it at the merge card, when
//! the card is already back with it, and when the log has no outage on it. Keeping the
//! replacement's work from the "original is back" card leaves the debt to the merge card.

use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::CardEvent;
use argonauts_core::events::checkpoint::{AnswerAction, CheckpointKind};
use argonauts_core::events::run::Party;
use argonauts_core::handoff::QualityDebt;
use argonauts_core::{CardId, Note, NoteLevel};

use crate::checkpoints;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::handoff::on_return::{self, Choice, Returning, vendor_label};
use crate::handoff::{merge_rule, revert, rules, upgrade};
use crate::review::Verdict;
use crate::state::cards::CardRecord;
use crate::workflow::StepAt;

/// How a card whose original is back runs to integration again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rerun {
    /// The original's review asked for changes.
    Changes,
    /// The original takes it, starting fresh with why: from the run's branch, or from the
    /// checkpoint for a redo.
    NewOwner { why: String },
    /// You took it over.
    TakeOver,
}

/// Decides and acts for each card whose debt blocks the merge and whose original is back,
/// asking you first when the rule is `ask`. Returns the cards to run to integration again.
pub async fn decide(host: &Host, at: &StepAt) -> EngineResult<Vec<(CardId, Rerun)>> {
    let mut reruns = Vec::new();
    for (card, debt) in merge_rule::blocking_debt(host) {
        if !rules::vendor_usable(host, &debt.needs.vendor()) {
            continue;
        }
        let record = rules::record(host, &card)?;
        if upgrade::reviewed_last(&record, &debt.needs) {
            continue;
        }
        let Some((choice, back, by)) = choice(host, &record, &debt, at).await? else {
            continue;
        };
        if let Some(rerun) = act(host, &record, &back, &debt, choice, by).await? {
            reruns.push((card, rerun));
        }
    }
    Ok(reruns)
}

/// What happens to the card, and who decided it. `None` when its card was withdrawn.
async fn choice(
    host: &Host,
    record: &CardRecord,
    debt: &QualityDebt,
    at: &StepAt,
) -> EngineResult<Option<(Choice, Returning, Party)>> {
    let card = &record.spec.id;
    let replacement = rules::owner(record).unwrap_or_else(|| debt.by.clone());
    let back = Returning {
        rule: rules::applied_for(host, &record.spec, &debt.needs).on_return,
        original: debt.needs.clone(),
        replacement,
        debt: Some(debt.clone()),
    };
    // An original the log has no outage on, such as one whose quota reads fine again, and one
    // that holds the card again leave only the merge rule's review.
    let came_back = host.read(|state| state.run.vendor_out(&back.original.vendor()));
    let decided = if !came_back || back.replacement.vendor() == back.original.vendor() {
        Some((Choice::Review, Party::Rules))
    } else if let Some(choice) = Choice::from_rule(back.rule, true) {
        let choice = if choice == Choice::Keep {
            Choice::Review
        } else {
            choice
        };
        Some((choice, Party::Rules))
    } else if let Some(answer) = host.read(|state| on_return::answered(state, card, debt.since)) {
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

/// Your newest answer at the merge card was to wait for the originals.
fn waited_for_originals(host: &Host) -> bool {
    host.read(|state| {
        state
            .checkpoints
            .last_decided(CheckpointKind::Merge, None)
            .and_then(|record| record.decided.as_ref())
            .is_some_and(|decided| decided.action == AnswerAction::Wait)
    })
}

/// Opens the "original is back" card in place of the merge card and waits for your answer.
async fn ask(
    host: &Host,
    record: &CardRecord,
    back: Returning,
    debt: &QualityDebt,
    at: &StepAt,
) -> EngineResult<Option<(Choice, Returning, Party)>> {
    let merge_card = host.read(|state| {
        state
            .checkpoints
            .open_for(CheckpointKind::Merge, None)
            .map(|record| record.opened.id.clone())
    });
    if let Some(id) = merge_card {
        let reason = format!("{} is back", vendor_label(&back.original.vendor()));
        checkpoints::withdraw(host, &id, &reason)?;
    }
    let request = on_return::request(host, record, &back, debt, Some(at.clone())).await?;
    let Ok(decided) = checkpoints::open(host, request)?
        .decided_or_withdrawn(host)
        .await?
    else {
        return Ok(None);
    };
    Ok(Choice::from_answer(&decided.action).map(|choice| (choice, back, decided.by)))
}

/// Applies the choice to the integrated card. Returns how it runs again, if it does.
async fn act(
    host: &Host,
    record: &CardRecord,
    back: &Returning,
    debt: &QualityDebt,
    choice: Choice,
    by: Party,
) -> EngineResult<Option<Rerun>> {
    let card = &record.spec.id;
    match choice {
        Choice::Keep => Ok(None),
        Choice::Review => {
            let (verdict, _) = upgrade::review(host, card, &back.original).await?;
            Ok(matches!(verdict, Verdict::ChangesRequested { .. }).then_some(Rerun::Changes))
        }
        Choice::HandBack => {
            let why = on_return::hand_back_why(back, debt.since);
            on_return::reassign(host, record, back, &why, by).await?;
            Ok(Some(Rerun::NewOwner { why }))
        }
        Choice::Redo => {
            let kept_on = match revert::reset_to(host, record, debt.since).await {
                Ok(kept_on) => kept_on,
                // Nothing changed, so the debt stays for the merge card.
                Err(EngineError::Refused(why)) => {
                    let text = format!(
                        "{card} couldn't be redone from handoff-{}: {why}",
                        debt.since
                    );
                    host.record(
                        Some(card),
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
                Some(card),
                None,
                RunEventKind::Card(CardEvent::Reverted {
                    to: debt.since,
                    kept_on,
                }),
            )?;
            let why = on_return::redo_why(back, debt.since);
            let record = rules::record(host, card)?;
            on_return::reassign(host, &record, back, &why, by).await?;
            Ok(Some(Rerun::NewOwner { why }))
        }
        Choice::TakeOver => Ok(Some(Rerun::TakeOver)),
    }
}
