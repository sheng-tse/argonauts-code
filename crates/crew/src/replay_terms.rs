//! The terms of one of the evolution loop's replays, which the app writes into the run's folder
//! before the run starts: the paths no task checks out or names to an agent, and the answers the
//! run its task came from recorded before the phase the learned workflow change touches. Asks in
//! those phases take the recorded answers, so only the rest of the run spends again, while
//! Argonauts Code still does every step's own work in the replay's repository.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use argonauts_core::events::plan::DivideEvent;
use argonauts_core::events::session::EndReason;
use argonauts_core::events::task::TaskEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::RoleName;
use argonauts_core::steps::StepName;
use argonauts_core::workflow::{Phase, StandardPhase, Workflow};
use argonauts_core::{RunId, TaskId, canonical_json, sha256_hex, write_private_file};

use crate::error::{EngineError, EngineResult};
use crate::state::RunState;

/// `runs/<run>/replay.json`.
pub const FILE: &str = "replay.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplayTerms {
    /// Added to every task's `hidden_paths`: the hidden tests at their repo paths, and the
    /// replay folder that holds them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reuse: Option<Recorded>,
}

/// What the run a task came from answered before `from`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recorded {
    pub run: RunId,
    /// The first phase whose asks go to the replay's workers again.
    pub from: StandardPhase,
    /// The digest of the recorded workflow's phases before `from`. The answers fit only a
    /// run whose own workflow starts with the same phases.
    pub phases: String,
    /// In the order their asks started.
    pub answers: Vec<Answer>,
}

/// One ask's recorded answer, and the place in the workflow that asked it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub phase: u32,
    pub path: Vec<u32>,
    pub round: u32,
    pub role: RoleName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<StepName>,
    pub structured: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_text: Option<String>,
}

pub fn write(run_dir: &Path, terms: &ReplayTerms) -> std::io::Result<()> {
    std::fs::create_dir_all(run_dir)?;
    let text = serde_json::to_vec_pretty(terms).map_err(std::io::Error::other)?;
    write_private_file(&run_dir.join(FILE), &text)
}

/// The run's replay terms, if it is a replay. Terms that can't be read refuse the run, since
/// they keep the hidden tests hidden.
pub fn read(run_dir: &Path) -> EngineResult<Option<ReplayTerms>> {
    let path = run_dir.join(FILE);
    let text = match std::fs::read(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    serde_json::from_slice(&text)
        .map(Some)
        .map_err(|err| EngineError::Refused(format!("{}: {err}", path.display())))
}

impl ReplayTerms {
    /// `kind` with the hidden paths added to every task it creates or plans.
    pub fn hide(&self, kind: RunEventKind) -> RunEventKind {
        if self.hidden_paths.is_empty() {
            return kind;
        }
        let add = |hidden: &mut Vec<String>| {
            for path in &self.hidden_paths {
                if !hidden.contains(path) {
                    hidden.push(path.clone());
                }
            }
        };
        match kind {
            RunEventKind::Task(TaskEvent::Created(mut spec)) => {
                add(&mut spec.hidden_paths);
                RunEventKind::Task(TaskEvent::Created(spec))
            }
            RunEventKind::Divide(DivideEvent::Graph(mut graph)) => {
                for task in &mut graph.tasks {
                    add(&mut task.hidden_paths);
                }
                RunEventKind::Divide(DivideEvent::Graph(graph))
            }
            other => other,
        }
    }
}

impl Recorded {
    /// Whether the answers fit `workflow`: it starts with the same phases before `from`.
    pub fn fits(&self, workflow: &Workflow) -> bool {
        let before = phases_before(workflow, self.from);
        !before.is_empty() && digest(before) == self.phases
    }
}

/// The phases of `workflow` before the first one at `from` or later.
pub fn phases_before(workflow: &Workflow, from: StandardPhase) -> &[Phase] {
    let at = workflow
        .phases
        .iter()
        .position(|phase| phase.standard >= from)
        .unwrap_or(workflow.phases.len());
    &workflow.phases[..at]
}

/// SHA-256 of the phases' canonical JSON.
pub fn digest(phases: &[Phase]) -> String {
    let value = serde_json::to_value(phases).unwrap_or(Value::Null);
    sha256_hex(canonical_json(&value).as_bytes())
}

/// What a replay starting at `from` may reuse from a finished run's log: every ask its own
/// workflow answered in the phases before, in the order the asks started. `None` when the
/// workflow has no phase to start from, an approved change rewrote it, or nothing before
/// `from` was answered.
pub fn recorded(events: &[RunEvent], from: StandardPhase) -> Option<Recorded> {
    let run = events.first()?.run_id.clone();
    let state = RunState::fold(events);
    let root = state
        .workflows
        .by_id
        .values()
        .filter(|workflow| workflow.parent.is_none())
        .min_by_key(|workflow| workflow.since)?;
    let before = phases_before(&root.workflow, from);
    if before.is_empty()
        || before.len() == root.workflow.phases.len()
        || root.changed_from_phase.is_some()
    {
        return None;
    }
    let answers: Vec<Answer> = state
        .sessions
        .in_order()
        .into_iter()
        .filter_map(|session| {
            let step = session.started.workflow_step.as_ref()?;
            if step.workflow != root.id || step.phase as usize >= before.len() {
                return None;
            }
            let end = session.end.as_ref()?;
            let structured = match (&end.reason, &end.structured) {
                (EndReason::Completed, Some(value)) => value.clone(),
                _ => return None,
            };
            // The round whose start came last before the session's. An ask at a phase's end
            // belongs to no step, and has only the one round.
            let round = root
                .steps
                .iter()
                .filter(|progress| progress.step == *step && progress.since <= session.since)
                .map(|progress| progress.round)
                .max()
                .unwrap_or(1);
            Some(Answer {
                phase: step.phase,
                path: step.path.clone(),
                round,
                role: session.started.role,
                task: session.task.clone(),
                step: session.started.step,
                structured,
                result_text: end.result_text.clone(),
            })
        })
        .collect();
    (!answers.is_empty()).then(|| Recorded {
        run,
        from,
        phases: digest(before),
        answers,
    })
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::plan::TaskGraph;
    use argonauts_core::events::task::TaskSpec;

    use super::*;

    fn task(id: &str, hidden: &[&str]) -> TaskSpec {
        let mut spec = crate::testutil::task_spec(id, &[], None);
        spec.hidden_paths = hidden.iter().map(|path| (*path).to_owned()).collect();
        spec
    }

    /// Every task a replay creates or plans keeps its hidden tests and the replay folder out
    /// of reach, beside whatever the plan hid already, and nothing else changes.
    #[test]
    fn replay_tasks_keep_the_hidden_paths() {
        let terms = ReplayTerms {
            hidden_paths: vec!["tests/readme.sh".to_owned(), "/state/replay".to_owned()],
            reuse: None,
        };
        let created = terms.hide(RunEventKind::Task(TaskEvent::Created(Box::new(task(
            "task-001",
            &["tests/hidden/**"],
        )))));
        let RunEventKind::Task(TaskEvent::Created(spec)) = created else {
            panic!("{created:?}");
        };
        assert_eq!(
            spec.hidden_paths,
            ["tests/hidden/**", "tests/readme.sh", "/state/replay"]
        );

        let graph = TaskGraph {
            tasks: vec![
                task("task-001", &[]),
                task("task-002", &["tests/readme.sh"]),
            ],
            critical_path: Vec::new(),
        };
        let planned = terms.hide(RunEventKind::Divide(DivideEvent::Graph(graph)));
        let RunEventKind::Divide(DivideEvent::Graph(graph)) = planned else {
            panic!("{planned:?}");
        };
        for spec in &graph.tasks {
            assert_eq!(spec.hidden_paths, ["tests/readme.sh", "/state/replay"]);
        }

        let note = RunEventKind::Note(argonauts_core::log::Note {
            level: argonauts_core::log::NoteLevel::Info,
            text: "tests/readme.sh".to_owned(),
        });
        assert_eq!(terms.hide(note.clone()), note);
        let none = ReplayTerms::default();
        let created = RunEventKind::Task(TaskEvent::Created(Box::new(task("task-003", &[]))));
        assert_eq!(none.hide(created.clone()), created);
    }

    /// Recorded answers fit only a workflow that starts with the same phases before `from`.
    #[test]
    fn answers_fit_only_the_same_phases_before() {
        let council = crate::workflow::builtin::load("council").unwrap();
        let recorded = Recorded {
            run: RunId::new("run-001").unwrap(),
            from: StandardPhase::Do,
            phases: digest(phases_before(&council, StandardPhase::Do)),
            answers: Vec::new(),
        };
        assert_eq!(phases_before(&council, StandardPhase::Do).len(), 2);
        assert!(phases_before(&council, StandardPhase::Understand).is_empty());
        assert!(recorded.fits(&council));
        assert!(!recorded.fits(&crate::workflow::builtin::load("captain").unwrap()));
        let mut later = council.clone();
        if let Some(last) = later.phases.last_mut() {
            last.name = "Land".to_owned();
        }
        assert!(recorded.fits(&later), "a later phase is the replay's own");
        let mut earlier = council.clone();
        earlier.phases[0].name = "Aim".to_owned();
        assert!(!recorded.fits(&earlier));
    }

    /// Terms that don't parse refuse the run rather than run it without its hidden paths, and
    /// a run without terms isn't a replay.
    #[test]
    fn unreadable_terms_refuse_the_run() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()).unwrap(), None);
        let terms = ReplayTerms {
            hidden_paths: vec!["tests/readme.sh".to_owned()],
            reuse: None,
        };
        write(dir.path(), &terms).unwrap();
        assert_eq!(read(dir.path()).unwrap(), Some(terms));
        std::fs::write(dir.path().join(FILE), "{ not json").unwrap();
        assert!(matches!(read(dir.path()), Err(EngineError::Refused(_))));
    }
}
