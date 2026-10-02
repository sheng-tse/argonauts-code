//! Dividing the work: the task graph, owners, reviewers, predictions, and budget lines.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use argonauts_core::CardId;
use argonauts_core::events::RunEvent;
use argonauts_core::events::plan::{BudgetPlan, DivideEvent, TaskGraph};
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::protocol::RoutingAlgorithm;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DivideState {
    pub graph: Option<TaskGraph>,
    /// The seq of the newest `Divide::Graph`, which a later send-back makes stale.
    pub graph_seq: Option<u64>,
    pub budget: Option<BudgetPlan>,
    /// Why each card got its owner, for the card view.
    pub owners: BTreeMap<CardId, OwnerPick>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OwnerPick {
    pub worker: WorkerRef,
    pub algorithm: RoutingAlgorithm,
    pub explored: bool,
    pub reason: String,
}

impl DivideState {
    /// Owners, reviewers, and predictions recorded after the graph (your edits) update its cards.
    pub fn apply(&mut self, event: &DivideEvent, envelope: &RunEvent) {
        match event {
            DivideEvent::Graph(graph) => {
                self.owners
                    .retain(|card, _| graph.cards.iter().any(|spec| &spec.id == card));
                self.graph = Some(graph.clone());
                self.graph_seq = Some(envelope.seq);
            }
            DivideEvent::Owner {
                card,
                worker,
                algorithm,
                explored,
                reason,
            } => {
                self.owners.insert(
                    card.clone(),
                    OwnerPick {
                        worker: worker.clone(),
                        algorithm: *algorithm,
                        explored: *explored,
                        reason: reason.clone(),
                    },
                );
                if let Some(spec) = self.card_mut(card) {
                    spec.owner = Some(worker.clone());
                }
            }
            DivideEvent::Reviewers {
                card, reviewers, ..
            } => {
                if let Some(spec) = self.card_mut(card) {
                    spec.reviewers.clone_from(reviewers);
                }
            }
            DivideEvent::Predicted { card, predictions } => {
                if let Some(spec) = self.card_mut(card) {
                    spec.predictions = Some(*predictions);
                }
            }
            DivideEvent::Budget(plan) => self.budget = Some(plan.clone()),
        }
    }

    fn card_mut(&mut self, card: &CardId) -> Option<&mut argonauts_core::events::card::CardSpec> {
        self.graph
            .as_mut()?
            .cards
            .iter_mut()
            .find(|spec| &spec.id == card)
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::RunId;
    use argonauts_core::events::RunEventKind;
    use argonauts_core::events::card::{CardBudget, CardKind, CardSpec};
    use argonauts_core::steps::card::Predictions;

    use super::*;

    fn event(seq: u64, kind: DivideEvent) -> RunEvent {
        RunEvent {
            seq,
            ts: time::OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            card_id: None,
            session: None,
            source: "conductor".to_owned(),
            kind: RunEventKind::Divide(kind),
        }
    }

    fn spec(n: u32) -> CardSpec {
        CardSpec {
            id: CardId::numbered(n),
            title: format!("card {n}"),
            task: String::new(),
            kind: CardKind::Build,
            owned_paths: vec![format!("src/{n}/**")],
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            owner: None,
            reviewers: Vec::new(),
            budget: CardBudget::default(),
            predictions: None,
        }
    }

    #[test]
    fn the_fold_keeps_the_newest_graph_and_its_edits() {
        let codex: WorkerRef = "codex".parse().unwrap();
        let agy: WorkerRef = "agy".parse().unwrap();
        let events = [
            event(
                3,
                DivideEvent::Owner {
                    card: CardId::numbered(1),
                    worker: codex.clone(),
                    algorithm: RoutingAlgorithm::Bandit,
                    explored: false,
                    reason: "9 of 10 similar cards passed".into(),
                },
            ),
            event(
                4,
                DivideEvent::Graph(TaskGraph {
                    cards: vec![spec(1), spec(2)],
                    critical_path: vec![CardId::numbered(1)],
                }),
            ),
            event(
                7,
                DivideEvent::Reviewers {
                    card: CardId::numbered(2),
                    reviewers: vec![agy.clone()],
                    blind: false,
                },
            ),
            event(
                8,
                DivideEvent::Predicted {
                    card: CardId::numbered(2),
                    predictions: Predictions {
                        p_pass_gates_first_try: 0.7,
                        p_within_time_cap: 0.8,
                        p_at_most_two_attempts: 0.9,
                    },
                },
            ),
        ];
        let mut state = DivideState::default();
        for event in &events {
            let RunEventKind::Divide(kind) = &event.kind else {
                unreachable!()
            };
            state.apply(kind, event);
        }
        assert_eq!(state.graph_seq, Some(4));
        assert_eq!(state.owners[&CardId::numbered(1)].worker, codex);
        let graph = state.graph.as_ref().unwrap();
        assert_eq!(graph.cards[1].reviewers, [agy]);
        assert_eq!(graph.cards[1].predictions.unwrap().p_within_time_cap, 0.8);
    }
}
