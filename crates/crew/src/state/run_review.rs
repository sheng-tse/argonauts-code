//! The run review: prediction scores, outcome records, memory proposals, and the process change.

use serde::{Deserialize, Serialize};

use argonauts_core::TaskId;
use argonauts_core::events::RunEvent;
use argonauts_core::events::shared::RunReviewEvent;
use argonauts_core::names::ModelRef;
use argonauts_core::steps::task::MemoryProposal;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunReviewState {
    pub events: Vec<RunReviewEvent>,
}

impl RunReviewState {
    pub fn apply(&mut self, event: &RunReviewEvent, _envelope: &RunEvent) {
        self.events.push(event.clone());
    }

    pub fn scored(&self, task: &TaskId, metric: &str) -> bool {
        self.events.iter().any(|event| {
            matches!(event, RunReviewEvent::Scored { task: c, metric: m, .. } if c == task && m == metric)
        })
    }

    pub fn has_outcome(&self, task: &TaskId) -> bool {
        self.events
            .iter()
            .any(|event| matches!(event, RunReviewEvent::Outcome { task: c, .. } if c == task))
    }

    pub fn scanned(&self, from: &ModelRef, proposal: &MemoryProposal) -> bool {
        self.events.iter().any(|event| {
            matches!(event, RunReviewEvent::Memory { from: f, proposal: p, .. } if f == from && p == proposal)
        })
    }

    /// Proposals that passed the scan, in the order they were scanned.
    pub fn accepted(&self) -> Vec<(ModelRef, MemoryProposal)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                RunReviewEvent::Memory {
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
            .any(|event| matches!(event, RunReviewEvent::Candidate(_)))
    }

    /// The mean Brier score over every prediction scored, when there is one.
    pub fn mean_brier(&self) -> Option<f64> {
        let scores: Vec<f64> = self
            .events
            .iter()
            .filter_map(|event| match event {
                RunReviewEvent::Scored { brier, .. } => Some(*brier),
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
    use argonauts_core::steps::task::ProcessChange;

    use super::*;

    #[test]
    fn the_fold_answers_what_the_run_review_already_did() {
        let codex: ModelRef = "codex".parse().unwrap();
        let note = MemoryProposal {
            topic: "redis".into(),
            text: "Counters expire with the window.".into(),
        };
        let events = [
            RunReviewEvent::Scored {
                task: TaskId::numbered(1),
                metric: "p_within_time_cap".into(),
                predicted: 0.5,
                outcome: true,
                brier: 0.25,
            },
            RunReviewEvent::Scored {
                task: TaskId::numbered(1),
                metric: "p_at_most_two_tries".into(),
                predicted: 0.9,
                outcome: true,
                brier: 0.01,
            },
            RunReviewEvent::Outcome {
                task: TaskId::numbered(1),
                worker: codex.clone(),
                task_type: "build".into(),
                first_try: true,
                tries: 1,
                merged: true,
            },
            RunReviewEvent::Memory {
                from: codex.clone(),
                proposal: note.clone(),
                accepted: true,
                reason: None,
            },
            RunReviewEvent::Candidate(ProcessChange {
                setting: "critique.rounds".into(),
                value: "2".into(),
                why: "Plans missed risks.".into(),
            }),
        ];
        let mut state = RunReviewState::default();
        for (seq, event) in events.iter().enumerate() {
            let envelope = RunEvent {
                seq: seq as u64,
                ts: time::OffsetDateTime::UNIX_EPOCH,
                run_id: RunId::new("run-01").unwrap(),
                task_id: None,
                session: None,
                source: "argonauts-code".into(),
                kind: RunEventKind::RunReview(event.clone()),
            };
            state.apply(event, &envelope);
        }
        assert!(state.scored(&TaskId::numbered(1), "p_within_time_cap"));
        assert!(!state.scored(&TaskId::numbered(2), "p_within_time_cap"));
        assert!(state.has_outcome(&TaskId::numbered(1)));
        assert!(state.scanned(&codex, &note));
        assert_eq!(state.accepted(), [(codex, note)]);
        assert!(state.has_candidate());
        assert!((state.mean_brier().unwrap() - 0.13).abs() < 1e-12);
    }
}
