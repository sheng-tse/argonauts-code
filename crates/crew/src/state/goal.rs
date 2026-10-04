//! Goal alignment: restatements, differences, what was settled, the drafted and approved goal
//! file, and goal changes.
//!
//! A round is restatements, then listings, then one `Settled`, then drafts. A restatement or a
//! listing that arrives after a round settled starts the next round (a send back), and so does a
//! second `Settled`, which is how a round with no restatements left still shows up.

use serde::{Deserialize, Serialize};

use argonauts_core::events::RunEvent;
use argonauts_core::events::goal::{
    Assumption, Attempt, Decider, GoalChangeResolution, GoalChecked, GoalEvent, GoalFile,
    GoalQuestion, GoalRun, GoalStatus,
};
use argonauts_core::events::run::Party;
use argonauts_core::names::{MessageId, WorkerRef};
use argonauts_core::steps::goal::{Difference, Restatement};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GoalState {
    /// 1 for the first round of restatements, +1 for each send back.
    pub round: u32,
    /// `(label, worker, restatement)` in the order they were recorded, labels A, B, C.
    pub restated: Vec<(String, WorkerRef, Restatement)>,
    pub compared: Vec<(WorkerRef, Vec<Difference>)>,
    pub settled: Option<SettledGoal>,
    /// The seq of this round's `Settled`. Goal checkpoints opened after it belong to this round.
    pub settled_at: Option<u64>,
    /// The newest draft of this round, answers and edits included.
    pub drafted: Option<GoalFile>,
    /// Frozen once set: every build and spike session carries its hash. Only an approved goal
    /// change replaces it, with a higher version.
    pub approved: Option<GoalFile>,
    pub approved_by: Option<Party>,
    pub changes: Vec<GoalChange>,
    /// A goal-style run's goal, with its attempts and status as they stand.
    #[serde(default)]
    pub pursuit: Option<GoalRun>,
    /// Every check, oldest first.
    #[serde(default)]
    pub checks: Vec<GoalChecked>,
    /// `(question, answer, who)` for each question answered, oldest first.
    #[serde(default)]
    pub answers: Vec<(String, String, Decider)>,
    /// The attempts after whose check the goal was stuck, so the next one goes to another
    /// company's model.
    #[serde(default)]
    pub stuck_at: Vec<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SettledGoal {
    pub agreements: Vec<String>,
    pub assumptions: Vec<Assumption>,
    pub questions: Vec<GoalQuestion>,
    pub dropped: u32,
}

/// A goal change asked for mid-run, and every resolution recorded for it, the final one last.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalChange {
    pub id: MessageId,
    pub from: Party,
    pub text: String,
    pub clarification: bool,
    pub resolutions: Vec<GoalChangeResolution>,
    pub new_sha256: Option<String>,
}

impl GoalChange {
    /// Recorded, refused, or changed. `AskedYou` alone is still open.
    pub fn resolved(&self) -> Option<GoalChangeResolution> {
        self.resolutions
            .last()
            .copied()
            .filter(|resolution| *resolution != GoalChangeResolution::AskedYou)
    }
}

impl GoalState {
    pub fn apply(&mut self, event: &GoalEvent, envelope: &RunEvent) {
        match event {
            GoalEvent::Restated {
                label,
                worker,
                restatement,
            } => {
                if self.approved.is_some() {
                    return;
                }
                if self.round_settled() || !self.compared.is_empty() {
                    self.next_round();
                }
                self.round = self.round.max(1);
                self.restated
                    .push((label.clone(), worker.clone(), restatement.clone()));
            }
            GoalEvent::Compared {
                worker,
                differences,
            } => {
                if self.approved.is_some() {
                    return;
                }
                if self.round_settled() {
                    self.next_round();
                }
                self.round = self.round.max(1);
                self.compared.push((worker.clone(), differences.clone()));
            }
            GoalEvent::Settled {
                agreements,
                assumptions,
                questions,
                dropped,
            } => {
                if self.approved.is_some() {
                    return;
                }
                if self.round_settled() {
                    self.next_round();
                }
                self.round = self.round.max(1);
                self.settled = Some(SettledGoal {
                    agreements: agreements.clone(),
                    assumptions: assumptions.clone(),
                    questions: questions.clone(),
                    dropped: *dropped,
                });
                self.settled_at = Some(envelope.seq);
            }
            GoalEvent::Drafted(file) => {
                if self.approved.is_none() {
                    self.drafted = Some(file.clone());
                }
            }
            GoalEvent::Approved { file, by } => {
                let newer = self
                    .approved
                    .as_ref()
                    .is_none_or(|approved| file.version > approved.version);
                if newer {
                    self.approved = Some(file.clone());
                    self.approved_by = Some(by.clone());
                }
            }
            GoalEvent::ChangeRequested {
                id,
                from,
                text,
                clarification,
            } => {
                if self.change(id).is_none() {
                    self.changes.push(GoalChange {
                        id: id.clone(),
                        from: from.clone(),
                        text: text.clone(),
                        clarification: *clarification,
                        resolutions: Vec::new(),
                        new_sha256: None,
                    });
                }
            }
            GoalEvent::ChangeResolved {
                id,
                resolution,
                new_sha256,
            } => {
                if let Some(change) = self.changes.iter_mut().find(|change| change.id == *id) {
                    change.resolutions.push(*resolution);
                    if new_sha256.is_some() {
                        change.new_sha256.clone_from(new_sha256);
                    }
                }
            }
            GoalEvent::Set(goal) => self.pursuit = Some((**goal).clone()),
            GoalEvent::AttemptStarted { n, title, phase } => {
                if let Some(goal) = &mut self.pursuit {
                    goal.attempts.push(Attempt {
                        n: *n,
                        title: title.clone(),
                        phase: *phase,
                        verdict: None,
                    });
                }
            }
            GoalEvent::Checked(checked) => {
                if let Some(attempt) = self.pursuit.as_mut().and_then(|goal| {
                    goal.attempts
                        .iter_mut()
                        .find(|attempt| attempt.n == checked.attempt)
                }) {
                    attempt.verdict = Some(checked.overall);
                }
                self.checks.push((**checked).clone());
            }
            GoalEvent::StatusChanged { status, .. } => {
                if let Some(goal) = &mut self.pursuit {
                    goal.status = *status;
                    if *status == GoalStatus::Stuck
                        && let Some(last) = goal.attempts.last()
                        && !self.stuck_at.contains(&last.n)
                    {
                        self.stuck_at.push(last.n);
                    }
                }
            }
            GoalEvent::Answered {
                question,
                answer,
                by,
            } => self.answers.push((question.clone(), answer.clone(), *by)),
            // The changed assumption reaches the goal file as its next version.
            GoalEvent::AssumptionChanged { .. } => {}
        }
    }

    pub fn approved_sha256(&self) -> Option<&str> {
        self.approved.as_ref().map(|file| file.sha256.as_str())
    }

    pub fn change(&self, id: &MessageId) -> Option<&GoalChange> {
        self.changes.iter().find(|change| change.id == *id)
    }

    fn round_settled(&self) -> bool {
        self.settled.is_some() || self.drafted.is_some()
    }

    fn next_round(&mut self) {
        self.round += 1;
        self.restated.clear();
        self.compared.clear();
        self.settled = None;
        self.settled_at = None;
        self.drafted = None;
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use argonauts_core::RunId;
    use argonauts_core::events::RunEventKind;
    use argonauts_core::events::goal::GoalSource;
    use argonauts_core::steps::goal::DiffTag;
    use time::OffsetDateTime;

    use super::*;

    fn envelope(seq: u64, event: GoalEvent) -> RunEvent {
        RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            card_id: None,
            session: None,
            source: "conductor".to_owned(),
            kind: RunEventKind::Goal(event),
        }
    }

    fn fold(events: Vec<GoalEvent>) -> GoalState {
        let mut state = GoalState::default();
        for (seq, event) in events.into_iter().enumerate() {
            let envelope = envelope(seq as u64, event.clone());
            state.apply(&event, &envelope);
        }
        state
    }

    fn restated(label: &str) -> GoalEvent {
        GoalEvent::Restated {
            label: label.to_owned(),
            worker: WorkerRef::new(format!("w{label}"), None),
            restatement: Restatement {
                summary: format!("reading {label}"),
                success_criteria: vec!["it works".to_owned()],
                in_scope: Vec::new(),
                out_of_scope: Vec::new(),
                assumptions: Vec::new(),
                open_questions: Vec::new(),
            },
        }
    }

    fn settled() -> GoalEvent {
        GoalEvent::Settled {
            agreements: vec!["shared".to_owned()],
            assumptions: Vec::new(),
            questions: Vec::new(),
            dropped: 1,
        }
    }

    fn file(version: u32, sha: &str) -> GoalFile {
        GoalFile {
            run: RunId::new("run-01").unwrap(),
            version,
            summary: "s".to_owned(),
            criteria: Vec::new(),
            in_scope: Vec::new(),
            out_of_scope: Vec::new(),
            assumptions: Vec::new(),
            answers: Vec::new(),
            source: GoalSource::Restatements,
            path: PathBuf::from(".argonauts/goal/run-01.md"),
            sha256: sha.to_owned(),
        }
    }

    #[test]
    fn a_round_folds_in_order() {
        let state = fold(vec![
            restated("A"),
            restated("B"),
            GoalEvent::Compared {
                worker: WorkerRef::new("d", None),
                differences: vec![Difference {
                    topic: "t".to_owned(),
                    readings: Vec::new(),
                    tag: DiffTag::Wording,
                    question: None,
                }],
            },
            settled(),
            GoalEvent::Drafted(file(1, "aa")),
        ]);
        assert_eq!(state.round, 1);
        assert_eq!(state.restated.len(), 2);
        assert_eq!(state.compared.len(), 1);
        assert_eq!(state.settled_at, Some(3));
        assert_eq!(state.drafted.as_ref().unwrap().sha256, "aa");
        assert_eq!(state.approved_sha256(), None);
    }

    #[test]
    fn a_send_back_starts_the_next_round() {
        let state = fold(vec![
            restated("A"),
            restated("B"),
            settled(),
            GoalEvent::Drafted(file(1, "aa")),
            restated("A"),
        ]);
        assert_eq!(state.round, 2);
        assert_eq!(state.restated.len(), 1);
        assert!(state.settled.is_none() && state.drafted.is_none());

        let empty_round = fold(vec![
            restated("A"),
            settled(),
            GoalEvent::Drafted(file(1, "aa")),
            settled(),
        ]);
        assert_eq!(empty_round.round, 2);
        assert!(empty_round.restated.is_empty());
        assert_eq!(empty_round.settled_at, Some(3));
        assert!(empty_round.drafted.is_none());
    }

    #[test]
    fn the_approved_goal_is_frozen() {
        let state = fold(vec![
            restated("A"),
            settled(),
            GoalEvent::Drafted(file(1, "aa")),
            GoalEvent::Approved {
                file: file(1, "aa"),
                by: Party::User,
            },
            GoalEvent::Drafted(file(1, "bb")),
            GoalEvent::Approved {
                file: file(1, "cc"),
                by: Party::Rules,
            },
            restated("B"),
        ]);
        assert_eq!(state.approved_sha256(), Some("aa"));
        assert_eq!(state.approved_by, Some(Party::User));
        assert_eq!(state.drafted.as_ref().unwrap().sha256, "aa");
        assert_eq!(state.restated.len(), 1);

        let changed = fold(vec![
            GoalEvent::Approved {
                file: file(1, "aa"),
                by: Party::User,
            },
            GoalEvent::Approved {
                file: file(2, "dd"),
                by: Party::User,
            },
        ]);
        assert_eq!(changed.approved_sha256(), Some("dd"));
    }

    #[test]
    fn goal_changes_keep_every_resolution() {
        let id = MessageId::numbered(4);
        let state = fold(vec![
            GoalEvent::ChangeRequested {
                id: id.clone(),
                from: Party::User,
                text: "also cover the CLI".to_owned(),
                clarification: false,
            },
            GoalEvent::ChangeResolved {
                id: id.clone(),
                resolution: GoalChangeResolution::AskedYou,
                new_sha256: None,
            },
        ]);
        assert_eq!(state.change(&id).unwrap().resolved(), None);

        let mut state = state;
        let event = GoalEvent::ChangeResolved {
            id: id.clone(),
            resolution: GoalChangeResolution::Changed,
            new_sha256: Some("ee".to_owned()),
        };
        state.apply(&event, &envelope(9, event.clone()));
        let change = state.change(&id).unwrap();
        assert_eq!(change.resolved(), Some(GoalChangeResolution::Changed));
        assert_eq!(change.new_sha256.as_deref(), Some("ee"));
    }
}
