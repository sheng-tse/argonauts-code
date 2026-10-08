//! Who reviews: the card's reviewers from the ring, other families than the owner, never the
//! owner's notes.

use argonauts_core::CardId;
use argonauts_core::names::WorkerRef;

use crate::state::RunState;

/// The card's reviewers, skipping the owner and any reviewer now of the owner's family (after
/// a handoff, say). `families` gives a worker's model family from the registry.
pub fn for_card(
    state: &RunState,
    card: &CardId,
    families: &dyn Fn(&WorkerRef) -> String,
) -> Vec<WorkerRef> {
    let Some(record) = state.cards.get(card) else {
        return Vec::new();
    };
    let owner = record.owner.as_ref().or(record.spec.owner.as_ref());
    eligible(&record.spec.reviewers, owner, families)
}

/// `reviewers` without the owner, the owner's family, or repeats.
pub fn eligible(
    reviewers: &[WorkerRef],
    owner: Option<&WorkerRef>,
    families: &dyn Fn(&WorkerRef) -> String,
) -> Vec<WorkerRef> {
    let owner_family = owner.map(families);
    let mut kept: Vec<WorkerRef> = Vec::new();
    for reviewer in reviewers {
        let own = Some(reviewer) == owner;
        let same_family = owner_family.as_deref() == Some(families(reviewer).as_str());
        if !own && !same_family && !kept.contains(reviewer) {
            kept.push(reviewer.clone());
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family(worker: &WorkerRef) -> String {
        match worker.worker.as_str() {
            "claude-code" => "claude",
            "codex" | "opencode" => "gpt",
            _ => "gemini",
        }
        .to_owned()
    }

    fn workers(names: &[&str]) -> Vec<WorkerRef> {
        names.iter().map(|n| n.parse().unwrap()).collect()
    }

    #[test]
    fn reviewers_of_the_owners_family_are_skipped() {
        // After a handoff to codex, the gpt-family reviewer can't review.
        let reviewers = workers(&["opencode", "antigravity-cli", "codex", "antigravity-cli"]);
        let owner: WorkerRef = "codex".parse().unwrap();
        let kept = eligible(&reviewers, Some(&owner), &family);
        assert_eq!(kept, workers(&["antigravity-cli"]));
    }

    #[test]
    fn with_no_owner_every_distinct_reviewer_stays() {
        let reviewers = workers(&["codex", "codex", "claude-code"]);
        assert_eq!(
            eligible(&reviewers, None, &family),
            workers(&["codex", "claude-code"])
        );
    }

    #[test]
    fn an_unknown_card_has_no_reviewers() {
        let other = CardId::new("task-009").unwrap();
        assert!(for_card(&RunState::default(), &other, &family).is_empty());
    }
}
