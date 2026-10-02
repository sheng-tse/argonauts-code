//! The retro: prediction scores, outcome records, memory proposals, and the process change.

use serde::{Deserialize, Serialize};

use argonauts_core::CardId;
use argonauts_core::events::RunEvent;
use argonauts_core::events::shared::RetroEvent;
use argonauts_core::names::WorkerRef;
use argonauts_core::steps::card::MemoryProposal;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RetroState {
    pub events: Vec<RetroEvent>,
}

impl RetroState {
    pub fn apply(&mut self, event: &RetroEvent, _envelope: &RunEvent) {
        self.events.push(event.clone());
    }

    pub fn scored(&self, card: &CardId, metric: &str) -> bool {
        self.events.iter().any(|event| {
            matches!(event, RetroEvent::Scored { card: c, metric: m, .. } if c == card && m == metric)
        })
    }

    pub fn has_outcome(&self, card: &CardId) -> bool {
        self.events
            .iter()
            .any(|event| matches!(event, RetroEvent::Outcome { card: c, .. } if c == card))
    }

    pub fn scanned(&self, from: &WorkerRef, proposal: &MemoryProposal) -> bool {
        self.events.iter().any(|event| {
            matches!(event, RetroEvent::Memory { from: f, proposal: p, .. } if f == from && p == proposal)
        })
    }

    /// Proposals that passed the scan, in the order they were scanned.
    pub fn accepted(&self) -> Vec<(WorkerRef, MemoryProposal)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                RetroEvent::Memory {
                    from,
                    proposal,
                    accepted: true,
                    ..
                } => Some((from.clone(), proposal.clone())),
                _ => None,
            })
            .collect()
    }

    pub fn has_candidate(&self) -> bool {
        self.events
            .iter()
            .any(|event| matches!(event, RetroEvent::Candidate(_)))
    }

    /// The mean Brier score over every prediction scored, when there is one.
    pub fn mean_brier(&self) -> Option<f64> {
        let scores: Vec<f64> = self
            .events
            .iter()
            .filter_map(|event| match event {
                RetroEvent::Scored { brier, .. } => Some(*brier),
                _ => None,
            })
            .collect();
        (!scores.is_empty()).then(|| scores.iter().sum::<f64>() / scores.len() as f64)
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::RunId;
    use argonauts_core::events::RunEventKind;
    use argonauts_core::steps::card::ProcessChange;

    use super::*;

    #[test]
    fn the_fold_answers_what_the_retro_already_did() {
        let codex: WorkerRef = "codex".parse().unwrap();
        let note = MemoryProposal {
            topic: "redis".into(),
            text: "Counters expire with the window.".into(),
        };
        let events = [
            RetroEvent::Scored {
                card: CardId::numbered(1),
                metric: "p_within_time_cap".into(),
                predicted: 0.5,
                outcome: true,
                brier: 0.25,
            },
            RetroEvent::Scored {
                card: CardId::numbered(1),
                metric: "p_at_most_two_attempts".into(),
                predicted: 0.9,
                outcome: true,
                brier: 0.01,
            },
            RetroEvent::Outcome {
                card: CardId::numbered(1),
                worker: codex.clone(),
                task_type: "build".into(),
                first_try: true,
                attempts: 1,
                integrated: true,
            },
            RetroEvent::Memory {
                from: codex.clone(),
                proposal: note.clone(),
                accepted: true,
                reason: None,
            },
            RetroEvent::Candidate(ProcessChange {
                setting: "critique.rounds".into(),
                value: "2".into(),
                why: "Plans missed risks.".into(),
            }),
        ];
        let mut state = RetroState::default();
        for (seq, event) in events.iter().enumerate() {
            let envelope = RunEvent {
                seq: seq as u64,
                ts: time::OffsetDateTime::UNIX_EPOCH,
                run_id: RunId::new("run-01").unwrap(),
                card_id: None,
                session: None,
                source: "conductor".into(),
                kind: RunEventKind::Retro(event.clone()),
            };
            state.apply(event, &envelope);
        }
        assert!(state.scored(&CardId::numbered(1), "p_within_time_cap"));
        assert!(!state.scored(&CardId::numbered(2), "p_within_time_cap"));
        assert!(state.has_outcome(&CardId::numbered(1)));
        assert!(state.scanned(&codex, &note));
        assert_eq!(state.accepted(), [(codex, note)]);
        assert!(state.has_candidate());
        assert!((state.mean_brier().unwrap() - 0.13).abs() < 1e-12);
    }
}
