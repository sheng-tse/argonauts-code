//! The merge rule (`merge_rule = "upgrade_review"`): a card with debt can't merge until the
//! original, or another model at least as strong, has reviewed it. With the original still out,
//! the merge card offers wait, review with a strong-enough candidate, or merge anyway, which
//! always needs you unless `approvals.merge.rules.allow_debt`. With it back, `merge_return`
//! decides what comes first.

use argonauts_core::CardId;
use argonauts_core::events::card::CardState;
use argonauts_core::events::run::Party;
use argonauts_core::handoff::QualityDebt;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::handoff::MergeRule;

use crate::engine::host::Host;
use crate::handoff::{debt, rules};
use crate::state::RunState;

/// Integrated cards that still carry debt the rule enforces. A card the merge leaves out
/// brings no debt into it. Empty with `merge_rule = "none"`.
pub fn blocking(state: &RunState, enforce: bool) -> Vec<CardId> {
    if !enforce {
        return Vec::new();
    }
    state
        .cards
        .by_id
        .values()
        .filter(|record| record.state == CardState::Integrated && record.debt.is_some())
        .map(|record| record.spec.id.clone())
        .collect()
}

/// Whether the rule holds for this card's owner role.
pub fn enforced(host: &Host, card: &CardId) -> bool {
    let Ok(record) = rules::record(host, card) else {
        return false;
    };
    let Some(owner) = rules::owner(&record) else {
        return false;
    };
    rules::applied_for(host, &record.spec, &owner).merge_rule == MergeRule::UpgradeReview
}

/// The debt that blocks merging now, card by card.
pub fn blocking_debt(host: &Host) -> Vec<(CardId, QualityDebt)> {
    let cards = host.read(|state| blocking(state, true));
    cards
        .into_iter()
        .filter(|card| enforced(host, card))
        .filter_map(|card| {
            let debt = host.read(|state| state.cards.get(&card)?.debt.clone())?;
            Some((card, debt))
        })
        .collect()
}

/// A model that could clear every listed debt instead of its original: usable now, not the
/// card's current owner, and at least as strong as each original.
pub fn stronger_candidate(host: &Host, blocked: &[(CardId, QualityDebt)]) -> Option<WorkerRef> {
    let settings = host.settings();
    let mut pool = crate::roles::candidates(settings, RoleName::Reviewer);
    if pool.is_empty() {
        pool = settings.crew.clone();
    }
    pool.into_iter().find(|candidate| {
        rules::vendor_usable(host, &candidate.vendor())
            && blocked.iter().all(|(card, owed)| {
                let Ok(record) = rules::record(host, card) else {
                    return false;
                };
                let unknown = rules::applied_for(host, &record.spec, &owed.needs).unknown_tier;
                rules::owner(&record).as_ref() != Some(candidate)
                    && !debt::weaker(host, &record.spec, candidate, &owed.needs, unknown)
            })
    })
}

/// Merge anyway, with debt still open: yours to decide, unless your rules may
/// (`approvals.merge.rules.allow_debt`).
pub fn may_merge_with_debt(host: &Host, by: &Party) -> bool {
    matches!(by, Party::User) || host.settings().approvals.merge.rules.allow_debt
}
