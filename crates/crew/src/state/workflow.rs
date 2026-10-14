//! Every workflow the run started (its own, and nested ones), their current definition after
//! approved changes, and each step's rounds.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use argonauts_core::events::RunEvent;
use argonauts_core::events::workflow::{StepEnd, WorkflowEvent};
use argonauts_core::names::{ApprovalId, WorkflowId};
use argonauts_core::workflow::{
    Phase, StandardPhase, Step, StepKind, StepRef, Workflow, WorkflowRef,
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Workflows {
    pub by_id: BTreeMap<WorkflowId, WorkflowRecord>,
    /// The run's own workflow: the newest one started at the top level.
    pub root: Option<WorkflowId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowRecord {
    pub id: WorkflowId,
    pub from: Option<WorkflowRef>,
    pub version: String,
    /// The `workflow` step that started it, for a nested one.
    pub parent: Option<StepRef>,
    /// As it runs now, after any approved change.
    pub workflow: Workflow,
    /// Every round of every step that started, in order.
    pub steps: Vec<StepProgress>,
    pub finished: bool,
    /// The phase an approved change takes effect from. Earlier phases ran the old definition.
    #[serde(default)]
    pub changed_from_phase: Option<u32>,
    /// The seq of its `Workflow::Started`.
    #[serde(default)]
    pub since: u64,
    /// The `change_request` approvals whose changes were recorded, oldest first.
    #[serde(default)]
    pub changes: Vec<ApprovalId>,
    /// The version its last `Declared` described, so a change declares it again.
    #[serde(default)]
    pub declared: Option<String>,
    /// Phases by index whose `PhaseStarted`, `PhaseEnded`, or `PhaseSkipped` is recorded.
    #[serde(default)]
    pub phases_started: BTreeSet<u32>,
    #[serde(default)]
    pub phases_ended: BTreeSet<u32>,
    #[serde(default)]
    pub phases_skipped: BTreeSet<u32>,
    /// What its notes said the run leaves undone, for the closing answer.
    #[serde(default)]
    pub left: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepProgress {
    pub step: StepRef,
    pub kind: StepKind,
    pub standard: StandardPhase,
    pub round: u32,
    /// The seq of its `StepStarted`.
    pub since: u64,
    pub end: Option<StepEnd>,
    pub summary: Option<String>,
    /// The seq of its `StepFinished`.
    #[serde(default)]
    pub ended_at: Option<u64>,
}

impl WorkflowRecord {
    /// The phase as it ran. `None` for a phase before an approved change, since only the
    /// changed definition is kept.
    pub fn phase_definition(&self, phase: u32) -> Option<&Phase> {
        if phase < self.changed_from_phase.unwrap_or(0) {
            return None;
        }
        self.workflow.phases.get(phase as usize)
    }

    /// The step at `step`'s path, inside loops and `for_each` too.
    pub fn step_definition(&self, step: &StepRef) -> Option<&Step> {
        let (first, inner) = step.path.split_first()?;
        let phase = self.phase_definition(step.phase)?;
        let mut at = phase.steps.get(*first as usize)?;
        for index in inner {
            at = at.steps.get(*index as usize)?;
        }
        Some(at)
    }

    /// The newest round of `step` that started.
    pub fn progress(&self, step: &StepRef) -> Option<&StepProgress> {
        self.steps
            .iter()
            .rev()
            .find(|progress| progress.step == *step)
    }

    /// Every top-level step of the phase has finished. Phases before an approved change count
    /// as finished, since the change only takes effect after them, except the one that was
    /// running when it came: that one goes on until its end is recorded, so a run picked up
    /// in the middle of it finishes it.
    pub fn phase_finished(&self, phase: u32) -> bool {
        if phase < self.changed_from_phase.unwrap_or(0) {
            return !self.phases_started.contains(&phase)
                || self.phases_ended.contains(&phase)
                || self.phases_skipped.contains(&phase);
        }
        let Some(definition) = self.workflow.phases.get(phase as usize) else {
            return false;
        };
        (0..definition.steps.len() as u32).all(|index| {
            let step = StepRef {
                workflow: self.id.clone(),
                phase,
                path: vec![index],
            };
            self.progress(&step)
                .is_some_and(|progress| progress.end.is_some())
        })
    }

    /// Whether any step of the phase has started.
    pub fn phase_started(&self, phase: u32) -> bool {
        self.steps
            .iter()
            .any(|progress| progress.step.phase == phase)
    }

    /// The given round of `step`, if it started.
    pub fn round(&self, step: &StepRef, round: u32) -> Option<&StepProgress> {
        self.steps
            .iter()
            .rev()
            .find(|progress| progress.step == *step && progress.round == round)
    }

    /// The first phase that hasn't finished, or the phase count when all have.
    pub fn next_phase(&self) -> u32 {
        let phases = self.workflow.phases.len() as u32;
        (0..phases)
            .find(|phase| !self.phase_finished(*phase))
            .unwrap_or(phases)
    }

    /// The phase a change recorded now takes effect from. The phase running now finishes as it
    /// was, so the change starts after it.
    pub fn change_from(&self) -> u32 {
        let next = self.next_phase();
        if self.phase_started(next) {
            next + 1
        } else {
            next
        }
    }
}

impl Workflows {
    pub fn apply(&mut self, event: &WorkflowEvent, envelope: &RunEvent) {
        match event {
            WorkflowEvent::Started(started) => {
                // A run that switched workflows, such as to Captain at the plan approval, now
                // follows the newest one.
                if started.parent.is_none() {
                    self.root = Some(started.id.clone());
                }
                self.by_id.insert(
                    started.id.clone(),
                    WorkflowRecord {
                        id: started.id.clone(),
                        from: started.from.clone(),
                        version: started.version.clone(),
                        parent: started.parent.clone(),
                        workflow: started.workflow.clone(),
                        steps: Vec::new(),
                        finished: false,
                        changed_from_phase: None,
                        since: envelope.seq,
                        changes: Vec::new(),
                        declared: None,
                        phases_started: BTreeSet::new(),
                        phases_ended: BTreeSet::new(),
                        phases_skipped: BTreeSet::new(),
                        left: Vec::new(),
                    },
                );
            }
            WorkflowEvent::Declared(declared) => {
                if let Some(record) = self.by_id.get_mut(&declared.id) {
                    record.declared = Some(record.version.clone());
                }
            }
            WorkflowEvent::PhaseStarted(mark) => {
                if let Some(record) = self.by_id.get_mut(&mark.id) {
                    record.phases_started.insert(mark.index);
                }
            }
            WorkflowEvent::PhaseEnded { phase, .. } => {
                if let Some(record) = self.by_id.get_mut(&phase.id) {
                    record.phases_ended.insert(phase.index);
                }
            }
            WorkflowEvent::PhaseSkipped { phase, .. } => {
                if let Some(record) = self.by_id.get_mut(&phase.id) {
                    record.phases_skipped.insert(phase.index);
                }
            }
            WorkflowEvent::Note(note) if note.left => {
                let id = note.workflow.as_ref().or(self.root.as_ref());
                if let Some(record) = id.and_then(|id| self.by_id.get_mut(id)) {
                    record.left.push(note.text.clone());
                }
            }
            // The main agent adds a phase through a change, which redefines the workflow, so the
            // record already holds it.
            WorkflowEvent::PhaseAdded { .. }
            | WorkflowEvent::Note(_)
            | WorkflowEvent::Reported(_) => {}
            WorkflowEvent::StepStarted(started) => {
                if let Some(record) = self.by_id.get_mut(&started.step.workflow) {
                    record.steps.push(StepProgress {
                        step: started.step.clone(),
                        kind: started.kind,
                        standard: started.standard,
                        round: started.round,
                        since: envelope.seq,
                        end: None,
                        summary: None,
                        ended_at: None,
                    });
                }
            }
            WorkflowEvent::StepFinished(finished) => {
                let Some(record) = self.by_id.get_mut(&finished.step.workflow) else {
                    return;
                };
                match record.steps.iter_mut().rev().find(|progress| {
                    progress.step == finished.step
                        && progress.round == finished.round
                        && progress.end.is_none()
                }) {
                    Some(progress) => {
                        progress.end = Some(finished.end);
                        progress.summary = finished.summary.clone();
                        progress.ended_at = Some(envelope.seq);
                    }
                    // A step whose `when` didn't hold is recorded by its finish alone, so a
                    // skip can't be cut in half.
                    None => record.steps.push(StepProgress {
                        step: finished.step.clone(),
                        kind: finished.kind,
                        standard: finished.standard,
                        round: finished.round,
                        since: envelope.seq,
                        end: Some(finished.end),
                        summary: finished.summary.clone(),
                        ended_at: Some(envelope.seq),
                    }),
                }
                let top_level = finished.step.path.len() == 1;
                let last_phase = record.workflow.phases.len().saturating_sub(1) as u32;
                if top_level
                    && finished.step.phase == last_phase
                    && record.phase_finished(last_phase)
                {
                    record.finished = true;
                }
                if matches!(finished.end, StepEnd::Stopped) && top_level {
                    record.finished = true;
                }
            }
            WorkflowEvent::Changed(changed) => {
                if let Some(record) = self.by_id.get_mut(&changed.id) {
                    record.changed_from_phase = Some(record.change_from());
                    record.version = changed.version.clone();
                    record.workflow = changed.workflow.clone();
                    record.finished = false;
                    record.changes.extend(changed.approval.clone());
                }
            }
        }
    }

    /// The step's newest round, if it started.
    pub fn progress(&self, step: &StepRef) -> Option<&StepProgress> {
        self.by_id.get(&step.workflow)?.progress(step)
    }

    /// 0 for a top-level workflow, then one more per `workflow` step above it.
    pub fn depth(&self, id: &WorkflowId) -> u32 {
        let mut depth = 0;
        let mut at = self.by_id.get(id);
        while let Some(parent) = at.and_then(|record| record.parent.as_ref()) {
            depth += 1;
            at = self.by_id.get(&parent.workflow);
        }
        depth
    }

    /// The standard phase of what the run's own workflow is doing now, for the working line.
    /// A nested workflow's running step speaks for its parent's.
    pub fn current_standard(&self) -> Option<StandardPhase> {
        self.running().last().map(|(_, progress)| progress.standard)
    }

    /// The running step at each level, from the run's own workflow down through the nested
    /// ones it started. Empty between steps and once the workflow has finished.
    pub fn running(&self) -> Vec<(&WorkflowRecord, &StepProgress)> {
        let mut levels = Vec::new();
        let mut at = self.root.as_ref().and_then(|id| self.by_id.get(id));
        while let Some(record) = at {
            // The newest round still open: inside a loop that's the inner step, not the loop.
            let Some(running) = record
                .steps
                .iter()
                .rev()
                .find(|progress| progress.end.is_none())
            else {
                break;
            };
            levels.push((record, running));
            at = self
                .by_id
                .values()
                .find(|child| child.parent.as_ref() == Some(&running.step) && !child.finished);
        }
        levels
    }

    /// The innermost step that was running when event `seq` was recorded, such as the
    /// `build` step a task's try started in.
    pub fn step_at(&self, seq: u64) -> Option<&StepProgress> {
        self.by_id
            .values()
            .flat_map(|record| &record.steps)
            .filter(|progress| {
                progress.since <= seq && progress.ended_at.is_none_or(|ended| seq <= ended)
            })
            .max_by_key(|progress| progress.since)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::OffsetDateTime;

    use argonauts_core::RunId;
    use argonauts_core::events::RunEventKind;
    use argonauts_core::events::workflow::{
        PhaseMark, StepFinished, StepStarted, WorkflowChanged, WorkflowStarted,
    };
    use argonauts_core::names::ApprovalId;

    use super::*;

    fn envelope(seq: u64, event: &WorkflowEvent) -> RunEvent {
        RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            task_id: None,
            session: None,
            source: "argonauts-code".to_owned(),
            kind: RunEventKind::Workflow(event.clone()),
        }
    }

    fn step(id: &WorkflowId, phase: u32, path: &[u32]) -> StepRef {
        StepRef {
            workflow: id.clone(),
            phase,
            path: path.to_vec(),
        }
    }

    fn started(step: StepRef, kind: StepKind) -> WorkflowEvent {
        WorkflowEvent::StepStarted(StepStarted {
            step,
            kind,
            standard: StandardPhase::Do,
            round: 1,
        })
    }

    fn finished(step: StepRef, kind: StepKind) -> WorkflowEvent {
        WorkflowEvent::StepFinished(StepFinished {
            step,
            kind,
            standard: StandardPhase::Do,
            round: 1,
            end: StepEnd::Done,
            summary: None,
        })
    }

    fn workflow(first_phase: &str) -> Workflow {
        serde_json::from_value(json!({
            "name": "loop",
            "phases": [
                {"name": first_phase, "standard": "do", "steps": [
                    {"kind": "loop_until", "until": "checks_pass", "max": 3, "steps": [
                        {"kind": "build", "name": "Fix"},
                        {"kind": "checks"},
                    ]},
                ]},
                {"name": "Check", "standard": "check", "steps": [{"kind": "review"}]},
            ],
        }))
        .unwrap()
    }

    /// Each step knows the seqs it ran between, so an event can be traced to the innermost
    /// step running when it was recorded.
    #[test]
    fn steps_found_by_seq_and_by_path() {
        let id = WorkflowId::numbered(1);
        let events = [
            WorkflowEvent::Started(Box::new(WorkflowStarted {
                id: id.clone(),
                from: None,
                version: "v1".to_owned(),
                parent: None,
                workflow: workflow("Build"),
            })),
            started(step(&id, 0, &[0]), StepKind::LoopUntil),
            started(step(&id, 0, &[0, 0]), StepKind::Build),
            finished(step(&id, 0, &[0, 0]), StepKind::Build),
            started(step(&id, 0, &[0, 1]), StepKind::Checks),
        ];
        let mut workflows = Workflows::default();
        for (seq, event) in (1..).zip(&events) {
            workflows.apply(event, &envelope(seq, event));
        }
        let kind_at = |seq| workflows.step_at(seq).map(|progress| progress.kind);
        assert_eq!(kind_at(1), None);
        assert_eq!(kind_at(2), Some(StepKind::LoopUntil));
        assert_eq!(kind_at(3), Some(StepKind::Build));
        assert_eq!(kind_at(4), Some(StepKind::Build));
        assert_eq!(kind_at(9), Some(StepKind::Checks));

        let levels = workflows.running();
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].1.step.path, [0, 1]);
        let record = &workflows.by_id[&id];
        let fix = record.step_definition(&step(&id, 0, &[0, 0])).unwrap();
        assert_eq!(fix.name.as_deref(), Some("Fix"));
        assert!(record.step_definition(&step(&id, 0, &[0, 2])).is_none());

        // The running phase finishes on the old definition, which isn't kept.
        let change = WorkflowEvent::Changed(Box::new(WorkflowChanged {
            id: id.clone(),
            version: "v2".to_owned(),
            workflow: workflow("Rebuild"),
            approval: Some(ApprovalId::numbered(1)),
            reason: "one more check".to_owned(),
        }));
        workflows.apply(&change, &envelope(6, &change));
        let record = &workflows.by_id[&id];
        assert!(record.phase_definition(0).is_none());
        assert!(record.step_definition(&step(&id, 0, &[0, 0])).is_none());
        assert_eq!(record.phase_definition(1).unwrap().name, "Check");
    }

    /// A change recorded while a phase runs takes effect after it, and the phase still has to
    /// end: a run picked up before then goes on with it rather than skipping the rest.
    #[test]
    fn the_phase_running_when_a_change_came_is_finished_only_when_it_ends() {
        let id = WorkflowId::numbered(1);
        let mark = PhaseMark {
            id: id.clone(),
            index: 0,
            name: "Build".to_owned(),
        };
        let events = [
            WorkflowEvent::Started(Box::new(WorkflowStarted {
                id: id.clone(),
                from: None,
                version: "v1".to_owned(),
                parent: None,
                workflow: workflow("Build"),
            })),
            WorkflowEvent::PhaseStarted(mark.clone()),
            started(step(&id, 0, &[0]), StepKind::LoopUntil),
            WorkflowEvent::Changed(Box::new(WorkflowChanged {
                id: id.clone(),
                version: "v2".to_owned(),
                workflow: workflow("Build"),
                approval: Some(ApprovalId::numbered(1)),
                reason: "one more check".to_owned(),
            })),
        ];
        let mut workflows = Workflows::default();
        for (seq, event) in (1..).zip(&events) {
            workflows.apply(event, &envelope(seq, event));
        }
        let record = &workflows.by_id[&id];
        assert_eq!(record.changed_from_phase, Some(1));
        assert!(!record.phase_finished(0));
        assert_eq!(record.next_phase(), 0);
        assert_eq!(record.change_from(), 1, "a second change also waits for it");

        let ended = WorkflowEvent::PhaseEnded {
            phase: mark,
            end: StepEnd::Done,
        };
        workflows.apply(&ended, &envelope(5, &ended));
        let record = &workflows.by_id[&id];
        assert!(record.phase_finished(0));
        assert_eq!(record.next_phase(), 1);
    }
}
