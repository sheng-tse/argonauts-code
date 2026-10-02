//! Who takes a stopped card. Your pick for the card (on its handoff card) comes first; the
//! role and `[handoff]` lists come from the planner's shortlist, and with no list set, the other
//! installed CLIs in [`defaults`](crate::handoff::defaults)' order. With `spread`, the stopped
//! vendor's cards go to whichever valid candidate holds the fewest cards.

use argonauts_core::CardId;
use argonauts_core::events::card::CardState;
use argonauts_core::events::checkpoint::{AnswerAction, CheckpointKind};
use argonauts_core::events::run::Party;
use argonauts_core::limits::{Candidate, CandidateQuery};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::handoff::HandoffStrategy;

use crate::engine::host::Host;
use crate::handoff::{defaults, rules};
use crate::state::RunState;
use crate::state::cards::CardRecord;

/// The candidate you chose on this card's latest handoff card.
pub fn your_pick(state: &RunState, card: &CardId) -> Option<WorkerRef> {
    let checkpoint = state
        .checkpoints
        .last_decided(CheckpointKind::Handoff, Some(card))?;
    let decided = checkpoint.decided.as_ref()?;
    match (&decided.action, &decided.by) {
        (AnswerAction::Candidate { worker }, Party::User) => Some(worker.clone()),
        _ => None,
    }
}

/// The query the planner answers for this card, with the stopped worker.
pub fn query(
    host: &Host,
    record: &CardRecord,
    stopped: &WorkerRef,
    all_valid: bool,
) -> CandidateQuery {
    let rules = rules::applied_for(host, &record.spec, stopped);
    let fallback = if rules.candidates.is_empty() {
        defaults::candidates(host, &record.spec, stopped)
    } else {
        rules.candidates
    };
    let exclude_families = record
        .spec
        .reviewers
        .iter()
        .map(|reviewer| host.services().registry.family(reviewer))
        .collect();
    CandidateQuery {
        card: record.spec.clone(),
        role: RoleName::Owner,
        stopped: stopped.clone(),
        exclude_families,
        fallback,
        all_valid,
    }
}

/// The new owner: your pick when it's usable, else the one proposed from the lists, balanced
/// across candidates when the strategy is `spread`.
pub fn choose(
    host: &Host,
    record: &CardRecord,
    stopped: Option<&WorkerRef>,
    proposed: WorkerRef,
) -> WorkerRef {
    let card = &record.spec.id;
    if let Some(pick) = host.read(|state| your_pick(state, card))
        && Some(&pick) != stopped
        && rules::vendor_usable(host, &pick.vendor())
    {
        return pick;
    }
    let Some(stopped) = stopped else {
        return proposed;
    };
    let shortlist = host
        .services()
        .handoff
        .shortlist(&query(host, record, stopped, false));
    if shortlist.strategy != HandoffStrategy::Spread {
        // The proposal came from the lists the settings name. With none, the defaults' first
        // valid CLI that needs no OK of yours stands.
        if shortlist.strategy == HandoffStrategy::First
            && defaults::apply(host, &record.spec, stopped)
            && let Some(first) = shortlist
                .candidates
                .iter()
                .find(|candidate| !candidate.needs_confirm && candidate.worker != *stopped)
        {
            return first.worker.clone();
        }
        return proposed;
    }
    let loads: Vec<usize> = host.read(|state| {
        shortlist
            .candidates
            .iter()
            .map(|candidate| cards_held(state, &candidate.worker))
            .collect()
    });
    spread(&shortlist.candidates, &loads, &proposed).unwrap_or(proposed)
}

/// Cards a worker owns that aren't finished.
pub fn cards_held(state: &RunState, worker: &WorkerRef) -> usize {
    state
        .cards
        .by_id
        .values()
        .filter(|card| !matches!(card.state, CardState::Integrated | CardState::Cancelled))
        .filter(|card| rules::owner(card).as_ref() == Some(worker))
        .count()
}

/// The candidate holding the fewest cards. Ties keep the proposed one, then the planner's
/// order, which already weighs quota and track record.
pub fn spread(
    candidates: &[Candidate],
    loads: &[usize],
    proposed: &WorkerRef,
) -> Option<WorkerRef> {
    candidates
        .iter()
        .zip(loads)
        .enumerate()
        .min_by_key(|(index, (candidate, load))| (**load, &candidate.worker != proposed, *index))
        .map(|(_, (candidate, _))| candidate.worker.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::limits::CostEstimate;

    fn candidate(worker: &str) -> Candidate {
        Candidate {
            worker: worker.parse().unwrap(),
            quota_left: None,
            quota_text: String::new(),
            track_record: None,
            cost: CostEstimate::Unknown,
            needs_confirm: false,
            may_run_out: false,
        }
    }

    #[test]
    fn spread_goes_to_the_least_loaded_candidate() {
        let candidates = [candidate("codex/gpt-5.6"), candidate("agy/gemini-3.6")];
        let proposed: WorkerRef = "codex/gpt-5.6".parse().unwrap();
        assert_eq!(
            spread(&candidates, &[0, 0], &proposed),
            Some(proposed.clone())
        );
        assert_eq!(
            spread(&candidates, &[1, 0], &proposed),
            Some("agy/gemini-3.6".parse().unwrap())
        );
        let other: WorkerRef = "agy/gemini-3.6".parse().unwrap();
        assert_eq!(spread(&candidates, &[2, 2], &other), Some(other));
        assert_eq!(spread(&[], &[], &proposed), None);
    }
}
