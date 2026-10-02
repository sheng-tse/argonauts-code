//! The phase lead, by a fixed rule: the default agent for goal and planning (process work
//! only), the owner of the critical-path card for build (longest dependency chain, ties by card
//! id), and the integration chore's holder for integration and the final check. A lead that is
//! a party to a tie never breaks it; the tie goes to you.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::CardId;
use argonauts_core::events::card::{CardKind, CardSpec, CardState};
use argonauts_core::events::run::RunPhase;
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::Settings;

use crate::state::RunState;

/// What the rule needs to know about one card.
#[derive(Debug, Clone, Copy)]
pub struct CardView<'a> {
    pub spec: &'a CardSpec,
    pub state: CardState,
    pub owner: Option<&'a WorkerRef>,
}

/// `None` before the run has a phase.
pub fn phase_lead(state: &RunState, settings: &Settings) -> Option<WorkerRef> {
    let phase = state.run.phase?;
    let cards: Vec<CardView<'_>> = state
        .cards
        .by_id
        .values()
        .map(|record| CardView {
            spec: &record.spec,
            state: record.state,
            owner: record.owner.as_ref().or(record.spec.owner.as_ref()),
        })
        .collect();
    let critical = state
        .divide
        .graph
        .as_ref()
        .map(|graph| graph.critical_path.as_slice())
        .filter(|path| !path.is_empty());
    Some(lead_for(phase, &cards, critical, &settings.default_agent))
}

/// The rule itself: `critical` is the task graph's critical path when it has one.
pub fn lead_for(
    phase: RunPhase,
    cards: &[CardView<'_>],
    critical: Option<&[CardId]>,
    default: &WorkerRef,
) -> WorkerRef {
    let lead = match phase {
        RunPhase::Goal | RunPhase::Planning | RunPhase::Dividing | RunPhase::Retro => None,
        RunPhase::Contracts | RunPhase::Build => critical_card_owner(cards, critical),
        RunPhase::FinalCheck | RunPhase::Merge => integration_chore_holder(cards),
    };
    lead.unwrap_or_else(|| default.clone())
}

/// The owner of the first card on the critical path that isn't in yet: the card the build's
/// end waits on now.
fn critical_card_owner(cards: &[CardView<'_>], critical: Option<&[CardId]>) -> Option<WorkerRef> {
    let path = match critical {
        Some(path) => path.to_vec(),
        None => {
            let specs: Vec<CardSpec> = cards.iter().map(|card| card.spec.clone()).collect();
            critical_path(&specs)
        }
    };
    let on_path: Vec<&CardView<'_>> = path
        .iter()
        .filter_map(|id| cards.iter().find(|card| &card.spec.id == id))
        .collect();
    on_path
        .iter()
        .find(|card| !finished(card.state))
        .and_then(|card| card.owner.cloned())
        .or_else(|| on_path.last().and_then(|card| card.owner.cloned()))
}

fn finished(card: CardState) -> bool {
    matches!(card, CardState::Integrated | CardState::Cancelled)
}

/// The holder of the integration chore: the first chore card by id with an owner.
fn integration_chore_holder(cards: &[CardView<'_>]) -> Option<WorkerRef> {
    let mut chores: Vec<&CardView<'_>> = cards
        .iter()
        .filter(|card| card.spec.kind == CardKind::Chore && card.owner.is_some())
        .collect();
    chores.sort_by(|a, b| a.spec.id.cmp(&b.spec.id));
    chores.first().and_then(|card| card.owner.cloned())
}

/// The longest dependency chain, ties broken by card id.
pub fn critical_path(cards: &[CardSpec]) -> Vec<CardId> {
    let deps: BTreeMap<&CardId, Vec<&CardId>> = cards
        .iter()
        .map(|card| (&card.id, card.depends_on.iter().collect()))
        .collect();
    let mut memo: BTreeMap<&CardId, Vec<CardId>> = BTreeMap::new();
    let mut best: Vec<CardId> = Vec::new();
    for id in deps.keys() {
        let chain = longest_to(id, &deps, &mut memo, &mut BTreeSet::new());
        if better(&chain, &best) {
            best = chain;
        }
    }
    best
}

/// The longest chain ending at `id`, from its first dependency to `id`. A cycle, which a
/// checked graph never has, is cut where it closes.
fn longest_to<'a>(
    id: &'a CardId,
    deps: &BTreeMap<&'a CardId, Vec<&'a CardId>>,
    memo: &mut BTreeMap<&'a CardId, Vec<CardId>>,
    visiting: &mut BTreeSet<&'a CardId>,
) -> Vec<CardId> {
    if let Some(chain) = memo.get(id) {
        return chain.clone();
    }
    visiting.insert(id);
    let mut best: Vec<CardId> = Vec::new();
    for dep in deps.get(id).into_iter().flatten() {
        if visiting.contains(dep) || !deps.contains_key(dep) {
            continue;
        }
        let chain = longest_to(dep, deps, memo, visiting);
        if better(&chain, &best) {
            best = chain;
        }
    }
    visiting.remove(id);
    best.push(id.clone());
    memo.insert(id, best.clone());
    best
}

/// Longer wins; between equals, the one whose ids sort first.
fn better(chain: &[CardId], than: &[CardId]) -> bool {
    chain.len() > than.len() || (chain.len() == than.len() && chain < than)
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::card::CardBudget;

    use super::*;

    fn id(s: &str) -> CardId {
        CardId::new(s).unwrap()
    }

    fn spec(card: &str, kind: CardKind, deps: &[&str], owner: Option<&str>) -> CardSpec {
        CardSpec {
            id: id(card),
            title: card.to_owned(),
            task: String::new(),
            kind,
            owned_paths: Vec::new(),
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: deps.iter().map(|d| id(d)).collect(),
            criteria: Vec::new(),
            owner: owner.map(|o| o.parse().unwrap()),
            reviewers: Vec::new(),
            budget: CardBudget::default(),
            predictions: None,
        }
    }

    fn ids(list: &[&str]) -> Vec<CardId> {
        list.iter().map(|s| id(s)).collect()
    }

    #[test]
    fn the_longest_chain_wins() {
        let cards = vec![
            spec("card-001", CardKind::Interface, &[], None),
            spec("card-002", CardKind::Build, &["card-001"], None),
            spec("card-003", CardKind::Build, &["card-002"], None),
            spec("card-004", CardKind::Build, &["card-001"], None),
        ];
        assert_eq!(
            critical_path(&cards),
            ids(&["card-001", "card-002", "card-003"])
        );
    }

    #[test]
    fn equal_chains_go_by_card_id() {
        let cards = vec![
            spec("card-004", CardKind::Build, &["card-003"], None),
            spec("card-003", CardKind::Build, &[], None),
            spec("card-002", CardKind::Build, &["card-001"], None),
            spec("card-001", CardKind::Build, &[], None),
        ];
        assert_eq!(critical_path(&cards), ids(&["card-001", "card-002"]));
    }

    #[test]
    fn cycles_and_unknown_dependencies_do_not_hang() {
        let cards = vec![
            spec("card-001", CardKind::Build, &["card-002", "card-009"], None),
            spec("card-002", CardKind::Build, &["card-001"], None),
        ];
        assert_eq!(critical_path(&cards).len(), 2);
        assert!(critical_path(&[]).is_empty());
    }

    #[test]
    fn the_lead_follows_the_phase() {
        let default: WorkerRef = "claude-code".parse().unwrap();
        let specs = [
            spec("card-001", CardKind::Interface, &[], None),
            spec("card-002", CardKind::Build, &["card-001"], None),
            spec("card-003", CardKind::Build, &["card-001"], None),
            spec("card-004", CardKind::Chore, &[], None),
        ];
        let owners: Vec<WorkerRef> = ["claude-code", "codex", "antigravity-cli", "antigravity-cli"]
            .iter()
            .map(|o| o.parse().unwrap())
            .collect();
        let states = [
            CardState::Integrated,
            CardState::Running,
            CardState::Running,
            CardState::Waiting,
        ];
        let cards: Vec<CardView<'_>> = specs
            .iter()
            .zip(&owners)
            .zip(states)
            .map(|((spec, owner), state)| CardView {
                spec,
                state,
                owner: Some(owner),
            })
            .collect();
        let lead = |phase, critical| lead_for(phase, &cards, critical, &default).to_string();
        assert_eq!(lead(RunPhase::Planning, None), "claude-code");
        assert_eq!(lead(RunPhase::Build, None), "codex");
        assert_eq!(lead(RunPhase::FinalCheck, None), "antigravity-cli");
        let path = ids(&["card-001", "card-003"]);
        assert_eq!(
            lead(RunPhase::Build, Some(path.as_slice())),
            "antigravity-cli"
        );
        assert_eq!(lead_for(RunPhase::Build, &[], None, &default), default);
        assert_eq!(phase_lead(&RunState::default(), &Settings::default()), None);
    }
}
