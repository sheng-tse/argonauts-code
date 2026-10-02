//! Which handoff rules apply to a card, whether a vendor is usable again, and the step a
//! handoff happens in.

use time::OffsetDateTime;

use argonauts_core::CardId;
use argonauts_core::events::card::{CardSpec, CardState};
use argonauts_core::limits::{HealthState, QuotaReading, QuotaStatus};
use argonauts_core::names::{RoleName, VendorId, WorkerRef};
use argonauts_core::settings::handoff::{AppliedHandoff, HandoffSettings};

use crate::card::machine::Trigger;
use crate::card::pipeline;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::state::RunState;
use crate::state::cards::CardRecord;
use crate::workflow::StepAt;

/// The handoff rules for `role` when `vendor` stops, walked from the most specific table.
pub fn applied(host: &Host, role: RoleName, vendor: &VendorId) -> AppliedHandoff {
    let settings = host.settings();
    let role_table = settings.roles.get(&role).map(|role| &role.handoff);
    HandoffSettings::applied(&settings.handoff, role_table, vendor)
}

/// The rules for this card's owner role, with the vendor of `worker`. A solo card's owner fills
/// the owner role too, as its sessions and the recovery read it.
pub fn applied_for(host: &Host, _card: &CardSpec, worker: &WorkerRef) -> AppliedHandoff {
    applied(host, RoleName::Owner, &worker.vendor())
}

pub fn record(host: &Host, card: &CardId) -> EngineResult<CardRecord> {
    host.read(|state| state.cards.get(card).cloned())
        .ok_or_else(|| EngineError::Refused(format!("{card} is not a card of this run")))
}

/// Who owns the card now: the latest reassignment, else the owner it was planned with.
pub fn owner(record: &CardRecord) -> Option<WorkerRef> {
    record.owner.clone().or_else(|| record.spec.owner.clone())
}

/// Healthy or degraded, and its quota not out.
pub fn vendor_usable(host: &Host, vendor: &VendorId) -> bool {
    let services = host.services();
    let health = services.health.state(vendor).state;
    let quota_ok = services
        .quota
        .get(vendor)
        .is_none_or(|reading| !quota_out(&reading, host.now()));
    matches!(health, HealthState::Healthy | HealthState::Degraded) && quota_ok
}

/// A rejected reading counts until its reset has passed, as the handoff planner reads it.
pub fn quota_out(reading: &QuotaReading, now: OffsetDateTime) -> bool {
    reading.status == QuotaStatus::Rejected && reading.resets_at.is_none_or(|at| at > now)
}

/// The newest step still running, which a handoff's sessions and checkpoints belong to.
pub fn current_step(state: &RunState) -> Option<StepAt> {
    state
        .workflows
        .by_id
        .values()
        .flat_map(|workflow| &workflow.steps)
        .filter(|progress| progress.end.is_none())
        .max_by_key(|progress| progress.since)
        .map(|progress| StepAt {
            step: progress.step.clone(),
            round: progress.round,
        })
}

pub fn step_now(host: &Host) -> EngineResult<StepAt> {
    host.read(current_step)
        .ok_or_else(|| EngineError::Refused("no step is running for this handoff".to_owned()))
}

/// Whether a session of this card is running now.
pub fn session_running(state: &RunState, card: &CardId) -> bool {
    state
        .sessions
        .running()
        .any(|session| session.card.as_ref() == Some(card))
}

/// States where no pipeline is working on the card, so a change can apply at once.
pub fn at_rest(state: CardState) -> bool {
    matches!(
        state,
        CardState::NeedsDecision
            | CardState::Escalated
            | CardState::Deferred
            | CardState::Failed
            | CardState::Waiting
            | CardState::Blocked
    )
}

/// The move that puts a card with a new owner back in line, from where it waited.
pub fn ready_trigger(state: CardState) -> Option<Trigger> {
    match state {
        CardState::NeedsDecision => Some(Trigger::Decided),
        CardState::Failed => Some(Trigger::Retry),
        CardState::Escalated => Some(Trigger::NewOwner),
        _ => None,
    }
}

/// The move that puts a card back to work after its owner or branch changed: toward `Ready`
/// when it waited, back to `Running` when its gates or review were next, so they never run on
/// work the new owner hasn't done.
pub fn back_to_work_trigger(state: CardState) -> Option<Trigger> {
    match state {
        CardState::Gating | CardState::InReview => Some(Trigger::NewOwner),
        other => ready_trigger(other),
    }
}

/// Moves the card back to work after a new owner or a reset branch.
pub fn back_in_line(host: &Host, card: &CardId, reason: &str) -> EngineResult<()> {
    let state = record(host, card)?.state;
    if let Some(trigger) = back_to_work_trigger(state) {
        pipeline::transition(host, card, trigger, reason)?;
    }
    Ok(())
}

/// Why the card's branch can't be reset now, if it can't: only a card at rest with nothing
/// running on it, and not waiting for an answer its session resumes with.
pub fn refuse_reset(state: &RunState, card: &CardId) -> Option<String> {
    let record = state.cards.get(card)?;
    if session_running(state, card) {
        return Some(format!("{card} is running. Pause it, then revert."));
    }
    match record.state {
        CardState::Blocked => Some(format!(
            "{card} waits for an answer its session resumes with. Answer it, then revert."
        )),
        CardState::TakenOver => Some(format!(
            "You took {card} over. Hand it back or finish it, then revert."
        )),
        CardState::Integrating => Some(format!("{card} is being merged, so it can't be reverted.")),
        state if !at_rest(state) => Some(format!(
            "{card} is {state:?}. Revert it once it stops and waits for you."
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_owner_puts_waiting_cards_back_in_line() {
        assert_eq!(
            ready_trigger(CardState::NeedsDecision),
            Some(Trigger::Decided)
        );
        assert_eq!(ready_trigger(CardState::Failed), Some(Trigger::Retry));
        assert_eq!(ready_trigger(CardState::Escalated), Some(Trigger::NewOwner));
        assert_eq!(ready_trigger(CardState::Running), None);
        assert!(at_rest(CardState::NeedsDecision));
        assert!(!at_rest(CardState::Running));
        assert!(!at_rest(CardState::Integrated));
    }

    #[test]
    fn a_change_between_the_session_and_integration_goes_back_to_running() {
        for state in [CardState::Gating, CardState::InReview] {
            let trigger = back_to_work_trigger(state);
            assert_eq!(trigger, Some(Trigger::NewOwner), "{state:?}");
            let to = trigger.and_then(|trigger| crate::card::machine::next(state, trigger));
            assert_eq!(to, Some(CardState::Running), "{state:?}");
        }
        assert_eq!(
            back_to_work_trigger(CardState::Escalated),
            Some(Trigger::NewOwner)
        );
        assert_eq!(back_to_work_trigger(CardState::Integrating), None);
        assert_eq!(back_to_work_trigger(CardState::Running), None);
    }
}
