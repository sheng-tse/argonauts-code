//! `handoff.broken_work`: whether a stopped card's new owner continues from the work it left
//! or restarts from the base. With `restart`, the gates run on that work first, and work that
//! fails them is kept on a side branch while the card starts again from its base.

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::CardEvent;
use argonauts_core::events::session::EndReason;
use argonauts_core::settings::handoff::BrokenWork;

use crate::card::gates;
use crate::card::support::card_events;
use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::handoff::{revert, rules};
use crate::state::cards::CardRecord;

/// Runs the gates on the work a stopped session left, when `restart` needs their verdict. A
/// rotation or your own stop leaves the work in the same hands, so it needs none.
pub async fn gate(host: &Host, card: &CardId, reason: &EndReason) -> EngineResult<()> {
    let same_hands = matches!(
        reason,
        EndReason::Completed | EndReason::Paused | EndReason::Stopped | EndReason::ContextFull
    );
    let record = rules::record(host, card)?;
    if same_hands || !restarts(host, &record) || record.branch.is_none() {
        return Ok(());
    }
    if verdict(host, card)?.is_none() {
        gates::run(host, card).await?;
    }
    Ok(())
}

/// Whether the gates passed on the card's newest work, once they ran on it.
pub fn verdict(host: &Host, card: &CardId) -> EngineResult<Option<bool>> {
    let events = card_events(host, card)?;
    let since = events
        .iter()
        .rposition(|event| {
            matches!(&event.kind, RunEventKind::Card(CardEvent::Committed(info)) if info.sha.is_some())
        })
        .map_or(0, |at| at + 1);
    let mut ran = false;
    let mut passed = true;
    for event in &events[since..] {
        if let RunEventKind::Gate(gate) = &event.kind {
            ran = true;
            passed &= gate.passed;
        }
    }
    Ok(ran.then_some(passed))
}

/// With `restart`, work that failed its gates moves to a side branch and the card's branch
/// goes back to its base before the new owner starts. Returns that side branch.
pub async fn set_aside(host: &Host, record: &CardRecord) -> EngineResult<Option<String>> {
    let card = &record.spec.id;
    if !restarts(host, record) || verdict(host, card)? != Some(false) {
        return Ok(None);
    }
    let kept_on = revert::reset_to(host, record, 0).await?;
    host.record(
        Some(card),
        None,
        RunEventKind::Card(CardEvent::Reverted {
            to: 0,
            kept_on: kept_on.clone(),
        }),
    )?;
    Ok(Some(kept_on))
}

fn restarts(host: &Host, record: &CardRecord) -> bool {
    rules::owner(record).is_some_and(|owner| {
        rules::applied_for(host, &record.spec, &owner).broken_work == BrokenWork::Restart
    })
}
