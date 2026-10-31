//! Understand: readings, differences, what was settled, the drafted and approved goal
//! file, and goal changes. And the goals being kept: the run's own, which its workflow goes
//! round until it holds, and each task's, when its role works a task as a goal.
//!
//! A round is readings, then listings, then one `Settled`, then drafts. A reading or a
//! listing that arrives after a round settled starts the next round (a send back), and so does a
//! second `Settled`, which is how a round with no readings left still shows up.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use argonauts_core::TaskId;
use argonauts_core::base::names::{MessageId, ModelRef};
use argonauts_core::runs::events::RunEvent;
use argonauts_core::runs::events::goal::{
    Assumption, CliGoalSaid, Decider, GoalChangeResolution, GoalChecked, GoalEvent, GoalFile,
    GoalQuestion, GoalRun, GoalStatus, Try,
};
use argonauts_core::runs::events::run::Party;
use argonauts_core::workflows::steps::understand::{Difference, Reading};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GoalState {
    /// 1 for the first round of readings, +1 for each send back.
    pub round: u32,
    /// `(label, worker, reading)` in the order they were recorded, labels A, B, C.
    pub readings: Vec<(String, ModelRef, Reading)>,
    pub compared: Vec<(ModelRef, Vec<Difference>)>,
    pub settled: Option<SettledGoal>,
    /// The seq of this round's `Settled`. Understand approvals opened after it belong to this
    /// round.
    pub settled_at: Option<u64>,
    /// The newest draft of this round, answers and edits included.
    pub drafted: Option<GoalFile>,
    /// Frozen once set: every build and experiment session carries its hash. Only an approved goal
    /// change replaces it, with a higher version.
    pub approved: Option<GoalFile>,
    pub approved_by: Option<Party>,
    pub changes: Vec<GoalChange>,
    /// The run's own goal, with its rounds and status as they stand.
    #[serde(default)]
    pub pursuit: Option<GoalRun>,
    /// Every check, oldest first: the run's goal's and each task's.
    #[serde(default)]
    pub checks: Vec<GoalChecked>,
    /// `(question, answer, who)` for each question answered, oldest first.
    #[serde(default)]
    pub answers: Vec<(String, String, Decider)>,
    /// The rounds after whose check the run's goal changed approach, so the next one goes to
    /// another provider's model.
    #[serde(default)]
    pub stuck_at: Vec<u32>,
    /// Why the run's goal ended short of holding.
    #[serde(default)]
    pub ended_because: Option<String>,
    /// The goals of the tasks worked as goals.
    #[serde(default)]
    pub tasks: BTreeMap<TaskId, TaskGoal>,
}

/// One task's goal as it's kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskGoal {
    pub goal: GoalRun,
    /// The try after whose check it changed approach, which happens once.
    pub approach_changed_after: Option<u32>,
    /// Why it ended short of holding.
    pub ended_because: Option<String>,
    /// The CLI's own account of the goal, when the CLI keeps it, with the turns it has reported
    /// on so far.
    pub cli_says: Option<CliGoalSaid>,
    /// Turns in a row, in this try, that the CLI ended with its goal still active.
    #[serde(default)]
    pub continues: u32,
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
            GoalEvent::Read {
                label,
                worker,
                reading,
            } => {
                if self.approved.is_some() {
                    return;
                }
                if self.round_settled() || !self.compared.is_empty() {
                    self.next_round();
                }
                self.round = self.round.max(1);
                self.readings
                    .push((label.clone(), worker.clone(), reading.clone()));
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
            GoalEvent::Set(goal) => match &goal.task {
                Some(task) => {
                    let kept = self.tasks.remove(task);
                    self.tasks.insert(
                        task.clone(),
                        TaskGoal {
                            goal: (**goal).clone(),
                            approach_changed_after: kept
                                .as_ref()
                                .and_then(|kept| kept.approach_changed_after),
                            ended_because: None,
                            continues: kept.as_ref().map_or(0, |kept| kept.continues),
                            cli_says: kept.and_then(|kept| kept.cli_says),
                        },
                    );
                }
                None => self.pursuit = Some((**goal).clone()),
            },
            GoalEvent::TryStarted {
                n,
                title,
                phase,
                task,
            } => {
                if let Some(goal) = self.goal_mut(task.as_ref()) {
                    goal.tries.push(Try {
                        n: *n,
                        title: title.clone(),
                        phase: *phase,
                        verdict: None,
                    });
                }
                if let Some(kept) = task.as_ref().and_then(|task| self.tasks.get_mut(task)) {
                    kept.continues = 0;
                }
            }
            GoalEvent::Checked(checked) => {
                if let Some(a_try) = self.goal_mut(checked.task.as_ref()).and_then(|goal| {
                    goal.tries
                        .iter_mut()
                        .find(|a_try| a_try.n == checked.try_number)
                }) {
                    a_try.verdict = Some(checked.overall);
                }
                self.checks.push((**checked).clone());
            }
            GoalEvent::StatusChanged {
                status, task, why, ..
            } => {
                if let Some(goal) = self.goal_mut(task.as_ref()) {
                    goal.status = *status;
                }
                let because = match status {
                    GoalStatus::Pursuing | GoalStatus::Met => None,
                    _ => why.clone(),
                };
                match task {
                    Some(task) => {
                        if let Some(kept) = self.tasks.get_mut(task) {
                            kept.ended_because = because;
                        }
                    }
                    None if self.pursuit.is_some() => self.ended_because = because,
                    None => {}
                }
            }
            GoalEvent::ApproachChanged { after, task, .. } => match task {
                Some(task) => {
                    if let Some(kept) = self.tasks.get_mut(task) {
                        kept.approach_changed_after = Some(*after);
                    }
                }
                None => {
                    if !self.stuck_at.contains(after) {
                        self.stuck_at.push(*after);
                    }
                }
            },
            GoalEvent::CliSaid { task, said } => {
                if let Some(kept) = task.as_ref().and_then(|task| self.tasks.get_mut(task)) {
                    let turns = kept.cli_says.as_ref().and_then(|said| said.turns);
                    kept.continues = if crate::goal::cli::still_going(said) {
                        kept.continues + 1
                    } else {
                        0
                    };
                    kept.cli_says = Some(CliGoalSaid {
                        turns: Some(turns.unwrap_or(0) + 1),
                        ..said.clone()
                    });
                }
            }
            GoalEvent::Answered {
                question,
                answer,
                by,
            } => self.answers.push((question.clone(), answer.clone(), *by)),
            // The changed assumption reaches the goal file as its next version.
            GoalEvent::AssumptionChanged { .. } => {}
            // A check you asked for decides nothing, so it stays out of `checks`, which says
            // what follows each try.
            GoalEvent::CheckingNow { .. }
            | GoalEvent::CheckedNow(_)
            | GoalEvent::CheckNowDropped { .. } => {}
        }
    }

    pub fn approved_sha256(&self) -> Option<&str> {
        self.approved.as_ref().map(|file| file.sha256.as_str())
    }

    /// The goal `task` is worked as, or the run's own for `None`.
    pub fn goal_of(&self, task: Option<&TaskId>) -> Option<&GoalRun> {
        match task {
            Some(task) => self.tasks.get(task).map(|kept| &kept.goal),
            None => self.pursuit.as_ref(),
        }
    }

    fn goal_mut(&mut self, task: Option<&TaskId>) -> Option<&mut GoalRun> {
        match task {
            Some(task) => self.tasks.get_mut(task).map(|kept| &mut kept.goal),
            None => self.pursuit.as_mut(),
        }
    }

    /// The newest check of each try of that goal, oldest try first.
    pub fn checks_of(&self, task: Option<&TaskId>) -> Vec<&GoalChecked> {
        let Some(goal) = self.goal_of(task) else {
            return Vec::new();
        };
        goal.tries
            .iter()
            .filter_map(|a_try| {
                self.checks
                    .iter()
                    .rev()
                    .find(|checked| checked.try_number == a_try.n && checked.task.as_ref() == task)
            })
            .collect()
    }

    /// Whether that goal already changed approach, which it does once.
    pub fn approach_changed(&self, task: Option<&TaskId>) -> bool {
        match task {
            Some(task) => self
                .tasks
                .get(task)
                .is_some_and(|kept| kept.approach_changed_after.is_some()),
            None => !self.stuck_at.is_empty(),
        }
    }

    pub fn change(&self, id: &MessageId) -> Option<&GoalChange> {
        self.changes.iter().find(|change| change.id == *id)
    }

    fn round_settled(&self) -> bool {
        self.settled.is_some() || self.drafted.is_some()
    }

    fn next_round(&mut self) {
        self.round += 1;
        self.readings.clear();
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
    use argonauts_core::runs::events::RunEventKind;
    use argonauts_core::runs::events::goal::GoalSource;
    use argonauts_core::workflows::steps::understand::DiffTag;
    use time::OffsetDateTime;

    use super::*;

    fn envelope(seq: u64, event: GoalEvent) -> RunEvent {
        RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            task_id: None,
            session: None,
            source: "argonauts-code".to_owned(),
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

    fn read_event(label: &str) -> GoalEvent {
        GoalEvent::Read {
            label: label.to_owned(),
            worker: ModelRef::new(format!("w{label}"), None),
            reading: Reading {
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
            source: GoalSource::Readings,
            path: PathBuf::from(".argonauts/goal/run-01.md"),
            sha256: sha.to_owned(),
        }
    }

    #[test]
    fn a_round_folds_in_order() {
        let state = fold(vec![
            read_event("A"),
            read_event("B"),
            GoalEvent::Compared {
                worker: ModelRef::new("d", None),
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
        assert_eq!(state.readings.len(), 2);
        assert_eq!(state.compared.len(), 1);
        assert_eq!(state.settled_at, Some(3));
        assert_eq!(state.drafted.as_ref().unwrap().sha256, "aa");
        assert_eq!(state.approved_sha256(), None);
    }

    #[test]
    fn a_send_back_starts_the_next_round() {
        let state = fold(vec![
            read_event("A"),
            read_event("B"),
            settled(),
            GoalEvent::Drafted(file(1, "aa")),
            read_event("A"),
        ]);
        assert_eq!(state.round, 2);
        assert_eq!(state.readings.len(), 1);
        assert!(state.settled.is_none() && state.drafted.is_none());

        let empty_round = fold(vec![
            read_event("A"),
            settled(),
            GoalEvent::Drafted(file(1, "aa")),
            settled(),
        ]);
        assert_eq!(empty_round.round, 2);
        assert!(empty_round.readings.is_empty());
        assert_eq!(empty_round.settled_at, Some(3));
        assert!(empty_round.drafted.is_none());
    }

    #[test]
    fn the_approved_goal_is_frozen() {
        let state = fold(vec![
            read_event("A"),
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
            read_event("B"),
        ]);
        assert_eq!(state.approved_sha256(), Some("aa"));
        assert_eq!(state.approved_by, Some(Party::User));
        assert_eq!(state.drafted.as_ref().unwrap().sha256, "aa");
        assert_eq!(state.readings.len(), 1);

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
