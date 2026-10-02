//! Planning: sealed drafts, maps, critiques, spikes, scores, the judged result, decision
//! records, and a coordinator's plan.
//!
//! A planning round ends once it's settled: judged, merged on the fast path, or the
//! coordinator's plan accepted. A draft or a coordinator answer that arrives after that can only
//! come from a plan you sent back, so it starts the next planning round. The earlier rounds stay
//! as history, and every query here reads only the current one.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use argonauts_core::CardId;
use argonauts_core::events::RunEvent;
use argonauts_core::events::checkpoint::{AnswerAction, CheckpointKind};
use argonauts_core::events::plan::{DesignDecision, JudgingResult, MappedTopic, PlanEvent};
use argonauts_core::names::{DraftId, PlanLabel, SessionKey, WorkerRef};
use argonauts_core::steps::plan::{CritiquePoint, SpikeProposal, SpikeReport};

use super::RunState;

/// The label of the plan merged on the fast path. Judges and the mapper never see it, since
/// they only see labels 1 to N.
pub const MERGED_LABEL: PlanLabel = 0;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanState {
    pub drafts: Vec<SealedDraft>,
    pub maps: Vec<MapRecord>,
    pub critiques: Vec<CritiqueRecord>,
    pub spikes: Vec<SpikeRecord>,
    pub spike_reports: Vec<SpikeResult>,
    pub scores: Vec<ScoreRecord>,
    pub fast_path: Option<WorkerRef>,
    pub judged: Option<JudgingResult>,
    pub decisions: Vec<DesignDecision>,
    pub coordinated: Vec<CoordinatedRecord>,
    /// The first draft round of the current planning round.
    #[serde(default)]
    pub first_round: u8,
    /// Where the current planning round's records start in `coordinated`.
    #[serde(default)]
    pub coordinated_from: usize,
    /// The seq of the event that settled the current planning round.
    #[serde(default)]
    pub settled_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SealedDraft {
    pub draft: DraftId,
    pub label: PlanLabel,
    pub worker: WorkerRef,
    pub round: u8,
    pub summary: String,
    pub path: PathBuf,
    pub sha256: String,
    /// The drafter's session, which it resumes to revise.
    pub session: Option<SessionKey>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapRecord {
    pub round: u8,
    pub worker: WorkerRef,
    pub topics: Vec<MappedTopic>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CritiqueRecord {
    pub critic: WorkerRef,
    pub designated: bool,
    pub points: Vec<CritiquePoint>,
    /// The draft round it critiqued.
    pub round: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpikeRecord {
    pub proposal: SpikeProposal,
    pub fits_budget: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpikeResult {
    pub card: CardId,
    pub report: SpikeReport,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreRecord {
    pub judge: WorkerRef,
    pub judge_family: String,
    pub plan: PlanLabel,
    pub order: u8,
    pub scores: Vec<f64>,
    pub rationale: String,
    pub flagged: bool,
    /// The draft round it scored.
    pub round: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoordinatedRecord {
    pub coordinator: WorkerRef,
    pub rejection: Option<String>,
    /// The coordinator's run-lifetime session.
    pub session: Option<SessionKey>,
    /// The seq of its event, which tells answers given before a send-back from the ones after.
    #[serde(default)]
    pub seq: u64,
}

impl PlanState {
    pub fn apply(&mut self, event: &PlanEvent, envelope: &RunEvent) {
        let replans = match event {
            PlanEvent::Sealed { label, .. } => *label != MERGED_LABEL,
            PlanEvent::Coordinated { .. } => true,
            _ => false,
        };
        if replans && self.settled_at.is_some() {
            self.next_round();
        }
        let round = self.current_round();
        match event {
            PlanEvent::Sealed {
                draft,
                label,
                worker,
                round,
                summary,
                path,
                sha256,
            } => {
                if self.drafts.iter().any(|sealed| &sealed.draft == draft) {
                    return;
                }
                self.drafts.push(SealedDraft {
                    draft: draft.clone(),
                    label: *label,
                    worker: worker.clone(),
                    round: *round,
                    summary: summary.clone(),
                    path: path.clone(),
                    sha256: sha256.clone(),
                    session: envelope.session.clone(),
                });
            }
            PlanEvent::Mapped {
                round,
                worker,
                topics,
            } => {
                self.maps.retain(|map| map.round != *round);
                self.maps.push(MapRecord {
                    round: *round,
                    worker: worker.clone(),
                    topics: topics.clone(),
                });
            }
            PlanEvent::FastPath { merged_by } => {
                self.fast_path = Some(merged_by.clone());
                self.settled_at = Some(envelope.seq);
            }
            PlanEvent::Critiqued {
                critic,
                designated,
                points,
            } => self.critiques.push(CritiqueRecord {
                critic: critic.clone(),
                designated: *designated,
                points: points.clone(),
                round,
            }),
            PlanEvent::SpikeProposed {
                proposal,
                fits_budget,
            } => self.spikes.push(SpikeRecord {
                proposal: proposal.clone(),
                fits_budget: *fits_budget,
            }),
            PlanEvent::SpikeFinished { card, report } => self.spike_reports.push(SpikeResult {
                card: card.clone(),
                report: report.clone(),
            }),
            PlanEvent::Scored {
                judge,
                judge_family,
                plan,
                order,
                scores,
                rationale,
                flagged,
            } => self.scores.push(ScoreRecord {
                judge: judge.clone(),
                judge_family: judge_family.clone(),
                plan: *plan,
                order: *order,
                scores: scores.clone(),
                rationale: rationale.clone(),
                flagged: *flagged,
                round,
            }),
            PlanEvent::Judged(result) => {
                self.judged = Some(result.clone());
                self.settled_at = Some(envelope.seq);
            }
            PlanEvent::Decided(decision) => {
                self.decisions.retain(|known| known.id != decision.id);
                self.decisions.push(decision.clone());
            }
            PlanEvent::Coordinated {
                coordinator,
                rejection,
            } => {
                if rejection.is_none() {
                    self.settled_at = Some(envelope.seq);
                }
                self.coordinated.push(CoordinatedRecord {
                    coordinator: coordinator.clone(),
                    rejection: rejection.clone(),
                    session: envelope.session.clone(),
                    seq: envelope.seq,
                });
            }
        }
    }

    /// The plan was sent back: what the settled round chose no longer counts. Its drafts get
    /// round numbers above every earlier one, so the rounds never mix.
    fn next_round(&mut self) {
        self.first_round = self.current_round().saturating_add(1);
        self.judged = None;
        self.fast_path = None;
        self.coordinated_from = self.coordinated.len();
        self.settled_at = None;
    }

    /// The round a blind draft goes in: a new one when `sent_back`, else the first round of the
    /// current planning round.
    pub fn blind_round(&self, sent_back: bool) -> u8 {
        if sent_back {
            self.current_round().saturating_add(1)
        } else {
            self.first_round.max(1)
        }
    }

    fn in_this_round(&self, round: u8) -> bool {
        round >= self.first_round
    }

    /// The merged plan of the fast path in this planning round.
    pub fn merged(&self) -> Option<&SealedDraft> {
        self.drafts
            .iter()
            .rev()
            .find(|draft| draft.label == MERGED_LABEL && self.in_this_round(draft.round))
    }

    /// This planning round's coordinator records.
    pub fn coordinated_now(&self) -> &[CoordinatedRecord] {
        self.coordinated
            .get(self.coordinated_from..)
            .unwrap_or_default()
    }

    /// The draft the run goes on with: the judged winner, or the only or merged draft.
    pub fn chosen(&self) -> Option<&SealedDraft> {
        if let Some(winner) = self.judged.as_ref().and_then(|judged| judged.winner) {
            return self
                .latest()
                .into_iter()
                .find(|draft| draft.label == winner);
        }
        if self.judged.is_some() {
            return None;
        }
        if self.fast_path.is_some() {
            return self.merged().or_else(|| self.only());
        }
        if self.coordinated_plan_chosen() {
            return self
                .drafts
                .iter()
                .rev()
                .find(|draft| self.in_this_round(draft.round));
        }
        None
    }

    /// Whether the chosen draft is the coordinator's, which comes with its cards and assignments.
    pub fn coordinated_plan_chosen(&self) -> bool {
        self.judged.is_none() && self.fast_path.is_none() && self.coordinator_accepted()
    }

    /// The newest draft of each label in this planning round, merged plans left out, by label.
    pub fn latest(&self) -> Vec<&SealedDraft> {
        let mut latest: Vec<&SealedDraft> = Vec::new();
        let drafts = self
            .drafts
            .iter()
            .filter(|d| d.label != MERGED_LABEL && self.in_this_round(d.round));
        for draft in drafts {
            match latest.iter_mut().find(|known| known.label == draft.label) {
                Some(known) if known.round <= draft.round => *known = draft,
                Some(_) => {}
                None => latest.push(draft),
            }
        }
        latest.sort_by_key(|draft| draft.label);
        latest
    }

    fn only(&self) -> Option<&SealedDraft> {
        match self.latest().as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    /// The newest draft round, 0 before any draft.
    pub fn current_round(&self) -> u8 {
        self.drafts
            .iter()
            .filter(|draft| draft.label != MERGED_LABEL)
            .map(|draft| draft.round)
            .max()
            .unwrap_or(0)
    }

    pub fn latest_map(&self) -> Option<&MapRecord> {
        self.maps
            .iter()
            .filter(|map| self.in_this_round(map.round))
            .max_by_key(|map| map.round)
    }

    /// Topics the newest map found disputed.
    pub fn disputed(&self) -> Vec<&MappedTopic> {
        self.latest_map()
            .map(|map| map.topics.iter().filter(|topic| topic.disputed).collect())
            .unwrap_or_default()
    }

    /// Topics of the newest map with a quote code couldn't find, for the plan checkpoint.
    pub fn unverified_topics(&self) -> Vec<String> {
        self.latest_map()
            .map(|map| {
                map.topics
                    .iter()
                    .filter(|topic| topic.positions.iter().any(|p| !p.quote_verified))
                    .map(|topic| topic.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Plans holding text addressed to judges. They block auto-approval.
    pub fn flagged_plans(&self) -> Vec<PlanLabel> {
        let mut labels: Vec<PlanLabel> = self
            .scores
            .iter()
            .filter(|score| score.flagged && self.in_this_round(score.round))
            .map(|score| score.plan)
            .collect();
        labels.sort_unstable();
        labels.dedup();
        labels
    }

    /// Every spike that fit its budget has a report on its topic.
    pub fn disputes_settled(&self) -> bool {
        self.spikes
            .iter()
            .filter(|spike| spike.fits_budget)
            .all(|spike| {
                self.spike_reports.iter().any(|result| {
                    result
                        .report
                        .topic
                        .eq_ignore_ascii_case(&spike.proposal.topic)
                })
            })
    }

    pub fn critiques_of_round(&self, round: u8) -> impl Iterator<Item = &CritiqueRecord> {
        self.critiques
            .iter()
            .filter(move |critique| critique.round == round)
    }

    /// Distinct draft rounds of this planning round that were critiqued.
    pub fn critique_rounds(&self) -> u32 {
        let mut rounds: Vec<u8> = self
            .critiques
            .iter()
            .map(|c| c.round)
            .filter(|round| self.in_this_round(*round))
            .collect();
        rounds.sort_unstable();
        rounds.dedup();
        rounds.len() as u32
    }

    pub fn coordinator_accepted(&self) -> bool {
        self.coordinated_now()
            .iter()
            .any(|record| record.rejection.is_none())
    }

    pub fn coordinator_rejections(&self) -> Vec<&str> {
        self.coordinated_now()
            .iter()
            .filter_map(|record| record.rejection.as_deref())
            .collect()
    }
}

/// Your note, when you sent the settled plan back at the plan checkpoint and no new planning
/// round has started yet. Empty when you gave none.
pub fn sent_back(state: &RunState) -> Option<String> {
    let settled_at = state.plan.settled_at?;
    let record = state.checkpoints.last_decided(CheckpointKind::Plan, None)?;
    let decided = record.decided.as_ref()?;
    (record.since > settled_at && decided.action == AnswerAction::SendBack)
        .then(|| decided.text.clone().unwrap_or_default())
}

/// Your note from the newest plan you sent back, for every step that re-plans after it.
pub fn replan_note(state: &RunState) -> Option<String> {
    state
        .checkpoints
        .by_id
        .values()
        .filter(|record| record.opened.kind == CheckpointKind::Plan && record.withdrawn.is_none())
        .filter_map(|record| {
            let decided = record.decided.as_ref()?;
            (decided.action == AnswerAction::SendBack).then_some((record.since, decided))
        })
        .max_by_key(|(since, _)| *since)
        .map(|(_, decided)| decided.text.clone().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::RunEventKind;
    use argonauts_core::events::plan::MappedPosition;
    use argonauts_core::settings::protocol::JudgingMethod;
    use argonauts_core::{CardId, RunId};
    use time::OffsetDateTime;

    use super::*;

    fn envelope(seq: u64, session: Option<&str>, event: PlanEvent) -> RunEvent {
        RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            card_id: None::<CardId>,
            session: session.map(|s| SessionKey::parse(s).unwrap()),
            source: "conductor".to_owned(),
            kind: RunEventKind::Plan(event),
        }
    }

    fn sealed(n: u32, label: PlanLabel, worker: &str, round: u8) -> PlanEvent {
        PlanEvent::Sealed {
            draft: DraftId::numbered(n),
            label,
            worker: worker.parse().unwrap(),
            round,
            summary: format!("plan {label}"),
            path: PathBuf::from(format!("drafts/dr-{n:03}.md")),
            sha256: "abc".to_owned(),
        }
    }

    fn fold(events: Vec<PlanEvent>) -> PlanState {
        let mut state = PlanState::default();
        for (seq, event) in events.into_iter().enumerate() {
            let session = format!("s-{:03}", seq + 1);
            let envelope = envelope(seq as u64, Some(&session), event);
            if let RunEventKind::Plan(event) = &envelope.kind {
                state.apply(event, &envelope);
            }
        }
        state
    }

    fn judged(winner: Option<PlanLabel>) -> PlanEvent {
        PlanEvent::Judged(JudgingResult {
            method: JudgingMethod::Cross,
            plans: Vec::new(),
            winner,
            margin: 1.5,
            near_tie: winner.is_none(),
        })
    }

    #[test]
    fn the_winner_is_its_newest_revision() {
        let state = fold(vec![
            sealed(1, 1, "claude-code", 1),
            sealed(2, 2, "codex", 1),
            sealed(3, 2, "codex", 2),
            sealed(4, 1, "claude-code", 2),
            judged(Some(2)),
        ]);
        assert_eq!(state.current_round(), 2);
        let chosen = state.chosen().unwrap();
        assert_eq!(chosen.draft, DraftId::numbered(3));
        assert_eq!(chosen.session, Some(SessionKey::parse("s-003").unwrap()));
    }

    #[test]
    fn a_near_tie_chooses_nothing() {
        let state = fold(vec![
            sealed(1, 1, "claude-code", 1),
            sealed(2, 2, "codex", 1),
            judged(None),
        ]);
        assert_eq!(state.chosen(), None);
    }

    #[test]
    fn the_fast_path_chooses_the_merged_or_only_draft() {
        let merged = fold(vec![
            sealed(1, 1, "claude-code", 1),
            sealed(2, 2, "codex", 1),
            sealed(3, MERGED_LABEL, "claude-code", 1),
            PlanEvent::FastPath {
                merged_by: "claude-code".parse().unwrap(),
            },
        ]);
        assert_eq!(merged.chosen().unwrap().draft, DraftId::numbered(3));
        assert_eq!(merged.latest().len(), 2);

        let only = fold(vec![
            sealed(1, 1, "codex", 1),
            PlanEvent::FastPath {
                merged_by: "codex".parse().unwrap(),
            },
        ]);
        assert_eq!(only.chosen().unwrap().draft, DraftId::numbered(1));
    }

    #[test]
    fn critiques_and_scores_remember_their_round() {
        let state = fold(vec![
            sealed(1, 1, "claude-code", 1),
            sealed(2, 2, "codex", 1),
            PlanEvent::Critiqued {
                critic: "codex".parse().unwrap(),
                designated: true,
                points: Vec::new(),
            },
            sealed(3, 1, "claude-code", 2),
            PlanEvent::Scored {
                judge: "codex".parse().unwrap(),
                judge_family: "gpt".to_owned(),
                plan: 1,
                order: 0,
                scores: vec![7.0; 5],
                rationale: String::new(),
                flagged: true,
            },
        ]);
        assert_eq!(state.critiques[0].round, 1);
        assert_eq!(state.critique_rounds(), 1);
        assert_eq!(state.scores[0].round, 2);
        assert_eq!(state.flagged_plans(), vec![1]);
    }

    #[test]
    fn unverified_quotes_flag_their_topic() {
        let position = |label, verified| MappedPosition {
            label,
            summary: "after auth".to_owned(),
            quote: "after auth".to_owned(),
            quote_verified: verified,
        };
        let state = fold(vec![PlanEvent::Mapped {
            round: 1,
            worker: "codex".parse().unwrap(),
            topics: vec![
                MappedTopic {
                    name: "middleware".to_owned(),
                    disputed: true,
                    positions: vec![position(1, true), position(2, false)],
                },
                MappedTopic {
                    name: "storage".to_owned(),
                    disputed: false,
                    positions: vec![position(1, true)],
                },
            ],
        }]);
        assert_eq!(state.unverified_topics(), vec!["middleware".to_owned()]);
        assert_eq!(state.disputed().len(), 1);
    }

    #[test]
    fn an_accepted_coordinator_plan_is_chosen() {
        let state = fold(vec![
            PlanEvent::Coordinated {
                coordinator: "claude-code".parse().unwrap(),
                rejection: Some("card api owns a protected path".to_owned()),
            },
            sealed(1, 1, "claude-code", 1),
            PlanEvent::Coordinated {
                coordinator: "claude-code".parse().unwrap(),
                rejection: None,
            },
        ]);
        assert_eq!(state.coordinator_rejections().len(), 1);
        assert_eq!(state.chosen().unwrap().draft, DraftId::numbered(1));
        assert!(state.coordinated_plan_chosen());
        assert_eq!(
            state.coordinated[0].session,
            Some(SessionKey::parse("s-001").unwrap())
        );
        assert_eq!(state.coordinated[1].seq, 2);
    }

    #[test]
    fn a_draft_after_the_round_settled_starts_a_new_planning_round() {
        let mapped = |round| PlanEvent::Mapped {
            round,
            worker: "codex".parse().unwrap(),
            topics: Vec::new(),
        };
        let mut events = vec![
            sealed(1, 1, "claude-code", 1),
            sealed(2, 2, "codex", 1),
            mapped(1),
            judged(None),
        ];
        let first = fold(events.clone());
        assert_eq!(first.blind_round(true), 2);
        assert!(first.settled_at.is_some());

        // Your send back: every drafter drafts again in round 2.
        events.push(sealed(3, 2, "codex", 2));
        let replanning = fold(events.clone());
        assert_eq!(replanning.judged, None);
        assert_eq!(replanning.settled_at, None);
        assert_eq!(replanning.first_round, 2);
        assert_eq!(replanning.blind_round(false), 2);
        assert_eq!(replanning.latest().len(), 1);
        assert!(replanning.latest_map().is_none());
        assert!(replanning.chosen().is_none());

        events.extend([sealed(4, 1, "claude-code", 2), mapped(2), judged(Some(1))]);
        let second = fold(events);
        assert_eq!(second.chosen().unwrap().draft, DraftId::numbered(4));
        assert_eq!(second.latest_map().unwrap().round, 2);
    }

    #[test]
    fn a_coordinator_answer_after_acceptance_starts_a_new_planning_round() {
        let coordinated = |rejection: Option<&str>| PlanEvent::Coordinated {
            coordinator: "claude-code".parse().unwrap(),
            rejection: rejection.map(str::to_owned),
        };
        let state = fold(vec![
            sealed(1, 1, "claude-code", 1),
            coordinated(None),
            coordinated(Some("card api owns a protected path")),
        ]);
        assert!(!state.coordinator_accepted());
        assert_eq!(state.coordinated_now().len(), 1);
        assert_eq!(state.coordinated.len(), 2);
        assert_eq!(state.chosen(), None);
        assert_eq!(state.blind_round(false), 2);
    }
}
