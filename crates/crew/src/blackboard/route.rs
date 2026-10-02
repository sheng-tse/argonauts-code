//! Where a question goes: the owner of the piece it concerns, the consultant when addressed, or
//! you as a `Question` checkpoint.

use argonauts_core::events::card::CardState;
use argonauts_core::events::shared::Recipient;
use argonauts_core::names::RoleName;

use crate::blackboard::refs;
use crate::state::RunState;
use crate::state::cards::CardRecord;

/// The recipient for a question about these paths or cards, `Recipient::User` when no owner
/// fits. A card it names outright wins; otherwise the card owning most of the cited paths, ties
/// going to the lower card id.
pub fn question(state: &RunState, text: &str, refs: &[String]) -> Recipient {
    if addresses_consultant(text) {
        return Recipient::Role {
            role: RoleName::Consultant,
        };
    }
    let named = refs
        .iter()
        .filter_map(|reference| refs::card_in(reference))
        .find(|card| state.cards.get(card).is_some_and(still_working));
    if let Some(card) = named {
        return Recipient::Card { card };
    }
    let mut best = None;
    for (card, record) in &state.cards.by_id {
        if !still_working(record) {
            continue;
        }
        let owned = refs
            .iter()
            .filter(|path| {
                record
                    .spec
                    .owned_paths
                    .iter()
                    .any(|pattern| refs::under(pattern, path))
            })
            .count();
        if owned > 0 && best.as_ref().is_none_or(|(most, _)| owned > *most) {
            best = Some((owned, card.clone()));
        }
    }
    match best {
        Some((_, card)) => Recipient::Card { card },
        None => Recipient::User,
    }
}

/// `@consultant` anywhere, or a question that starts with `consultant:`.
pub fn addresses_consultant(text: &str) -> bool {
    let text = text.trim_start().to_ascii_lowercase();
    text.starts_with("consultant:") || text.contains("@consultant")
}

/// A card whose owner can still answer: it has a session to come.
pub fn still_working(record: &CardRecord) -> bool {
    !matches!(record.state, CardState::Integrated | CardState::Cancelled)
}
