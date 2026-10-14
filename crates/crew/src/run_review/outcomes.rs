//! The outcome log, through `argonauts_learning::outcomes`: one file per run in
//! `state/<repo>/outcomes/<run>.jsonl`, appended and never edited. Predictions and the router's
//! draws are written at divide time, before tasks start; an outcome per task and one run line
//! at the run review. Limits from history and the track records read it.

use std::collections::BTreeMap;
use std::path::Path;

use argonauts_core::limits::TrackRecord;
use argonauts_core::{RunId, TaskId};
use argonauts_learning::{LearningError, Store};

use crate::error::{EngineError, EngineResult};
use crate::state::RunState;

pub use argonauts_learning::outcomes::{
    Difficulty, LogLine, Outcome as OutcomeRecord, Prediction, Predictor, StepKey,
};

pub const OUTCOMES_DIR: &str = argonauts_core::layout::repo_state::OUTCOMES;

/// The step a task's newest try started in, usually its `build` step.
pub trait TaskStep: Sized {
    fn of_task(state: &RunState, task: &TaskId) -> Option<Self>;
}

impl TaskStep for StepKey {
    fn of_task(state: &RunState, task: &TaskId) -> Option<Self> {
        let seq = state.tasks.progress.get(task)?.try_seq?;
        let progress = state.workflows.step_at(seq)?;
        Some(Self {
            kind: progress.kind,
            standard: progress.standard,
        })
    }
}

/// Learning's errors, in the engine's terms.
pub fn learned<T>(result: argonauts_learning::Result<T>) -> EngineResult<T> {
    result.map_err(|err| match err {
        LearningError::Io(err) => EngineError::Io(err),
        LearningError::Json(err) => EngineError::Json(err),
        LearningError::NotYet(what) => EngineError::NotBuilt(what),
        other => EngineError::Refused(other.to_string()),
    })
}

/// Only the state folder matters to the log; the repo root is for what learning commits.
fn store(state_dir: &Path) -> Store {
    Store::new(state_dir, state_dir)
}

pub fn append(state_dir: &Path, run: &RunId, records: &[OutcomeRecord]) -> EngineResult<()> {
    let lines: Vec<LogLine> = records.iter().cloned().map(LogLine::Outcome).collect();
    append_lines(state_dir, run, &lines)
}

pub fn append_lines(state_dir: &Path, run: &RunId, lines: &[LogLine]) -> EngineResult<()> {
    learned(argonauts_learning::outcomes::append(
        &store(state_dir),
        run,
        lines,
    ))
}

/// Every line of every run, oldest file first.
pub fn read_all(state_dir: &Path) -> EngineResult<Vec<LogLine>> {
    learned(argonauts_learning::outcomes::read_all(&store(state_dir)))
}

/// Every line of this run.
pub fn run_lines(state_dir: &Path, run: &RunId) -> EngineResult<Vec<LogLine>> {
    learned(argonauts_learning::outcomes::read_run(
        &store(state_dir),
        run,
    ))
}

/// Every record of this task type across past runs.
pub fn load(state_dir: &Path, task_type: &str) -> EngineResult<Vec<OutcomeRecord>> {
    load_matching(state_dir, |record| record.task_type == task_type)
}

/// Every record of tasks built by this kind of step in this standard phase, across past runs.
pub fn load_step(state_dir: &Path, key: StepKey) -> EngineResult<Vec<OutcomeRecord>> {
    load_matching(state_dir, |record| record.step == Some(key))
}

/// Tasks that passed their checks on the first try, per step kind and standard phase.
pub fn step_track_records(state_dir: &Path) -> EngineResult<BTreeMap<StepKey, TrackRecord>> {
    let mut tally = BTreeMap::new();
    for record in load_matching(state_dir, |record| record.step.is_some())? {
        let Some(key) = record.step else { continue };
        let track = tally.entry(key).or_insert(TrackRecord { passed: 0, of: 0 });
        track.of += 1;
        track.passed += u32::from(record.first_try);
    }
    Ok(tally)
}

/// This run's outcome records, so a re-entered run review appends only what's missing.
pub fn load_run(state_dir: &Path, run: &RunId) -> EngineResult<Vec<OutcomeRecord>> {
    Ok(outcomes(run_lines(state_dir, run)?))
}

/// How many runs have an outcome log.
pub fn runs_logged(state_dir: &Path) -> EngineResult<usize> {
    let dir = state_dir.join(OUTCOMES_DIR);
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut runs = 0;
    for entry in std::fs::read_dir(dir)? {
        if entry?
            .path()
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            runs += 1;
        }
    }
    Ok(runs)
}

fn load_matching(
    state_dir: &Path,
    keep: impl Fn(&OutcomeRecord) -> bool,
) -> EngineResult<Vec<OutcomeRecord>> {
    Ok(outcomes(read_all(state_dir)?)
        .into_iter()
        .filter(|record| keep(record))
        .collect())
}

fn outcomes(lines: Vec<LogLine>) -> Vec<OutcomeRecord> {
    lines
        .into_iter()
        .filter_map(|line| match line {
            LogLine::Outcome(outcome) => Some(outcome),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use argonauts_core::workflow::{StandardPhase, StepKind};

    use super::*;

    fn record(run: &str, task: u32, task_type: &str, spent_usd: f64) -> OutcomeRecord {
        OutcomeRecord {
            run: RunId::new(run).unwrap(),
            task: TaskId::numbered(task),
            worker: "codex".parse().unwrap(),
            task_type: task_type.to_owned(),
            first_try: task.is_multiple_of(2),
            tries: task,
            merged: true,
            spent_usd,
            step: None,
            at: None,
            model_id: None,
            assigned: None,
            assign_p: None,
            reviewer: None,
            approved_first_review: None,
            within_time_cap: None,
            minutes: None,
            difficulty: None,
            learned_workflow: None,
        }
    }

    fn built_in(
        mut record: OutcomeRecord,
        kind: StepKind,
        standard: StandardPhase,
    ) -> OutcomeRecord {
        record.step = Some(StepKey { kind, standard });
        record
    }

    #[test]
    fn run_review_outcome_log_written() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        let one = RunId::new("run-01").unwrap();
        let two = RunId::new("run-02").unwrap();
        assert!(load(state, "build").unwrap().is_empty());
        let first = vec![
            record("run-01", 1, "build", 1.5),
            record("run-01", 2, "docs", 0.2),
        ];
        append(state, &one, &first).unwrap();
        append(state, &one, &[record("run-01", 3, "build", 2.5)]).unwrap();
        append(state, &two, &[record("run-02", 1, "build", 4.0)]).unwrap();

        let text = std::fs::read_to_string(state.join("outcomes/run-01.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 3);
        let line: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(line["task"], "task-001");
        assert_eq!(line["worker"], "codex");
        assert_eq!(line["first_try"], false);

        let builds: Vec<f64> = load(state, "build")
            .unwrap()
            .into_iter()
            .map(|record| record.spent_usd)
            .collect();
        assert_eq!(builds, [1.5, 2.5, 4.0]);
        assert_eq!(load_run(state, &one).unwrap().len(), 3);
        assert_eq!(runs_logged(state).unwrap(), 2);

        // A torn last line doesn't lose the rest.
        let mut file = OpenOptions::new()
            .append(true)
            .open(state.join("outcomes/run-02.jsonl"))
            .unwrap();
        file.write_all(b"{\"run\":\"run-02\",\"ca").unwrap();
        assert_eq!(load(state, "build").unwrap().len(), 3);
    }

    /// Records name the step that built each task, so the run review keeps track records per step
    /// kind and standard phase. Lines logged before that still load.
    #[test]
    fn outcome_records_keyed_by_step() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        let run = RunId::new("run-01").unwrap();
        let build = StepKey {
            kind: StepKind::Build,
            standard: StandardPhase::Do,
        };
        append(
            state,
            &run,
            &[
                built_in(
                    record("run-01", 1, "build", 1.0),
                    StepKind::Build,
                    StandardPhase::Do,
                ),
                built_in(
                    record("run-01", 2, "build", 1.0),
                    StepKind::Build,
                    StandardPhase::Do,
                ),
                built_in(
                    record("run-01", 3, "build", 1.0),
                    StepKind::FanOut,
                    StandardPhase::Check,
                ),
            ],
        )
        .unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(state.join("outcomes/run-01.jsonl"))
            .unwrap();
        file.write_all(
            b"{\"run\":\"run-01\",\"task\":\"task-004\",\"worker\":\"codex\",\"task_type\":\"build\",\"first_try\":true,\"tries\":1,\"merged\":true,\"spent_usd\":0.5}\n",
        )
        .unwrap();

        let text = std::fs::read_to_string(state.join("outcomes/run-01.jsonl")).unwrap();
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(
            first["step"],
            serde_json::json!({"kind": "build", "standard": "do"})
        );
        assert_eq!(load(state, "build").unwrap().len(), 4);

        let tasks: Vec<TaskId> = load_step(state, build)
            .unwrap()
            .into_iter()
            .map(|record| record.task)
            .collect();
        assert_eq!(tasks, [TaskId::numbered(1), TaskId::numbered(2)]);

        let tally = step_track_records(state).unwrap();
        assert_eq!(tally.len(), 2);
        assert_eq!(tally[&build], TrackRecord { passed: 1, of: 2 });
    }

    /// A task's step is the one its newest try started in.
    #[test]
    fn task_keyed_by_the_step_it_was_built_in() {
        use argonauts_core::events::workflow::{StepStarted, WorkflowEvent, WorkflowStarted};
        use argonauts_core::events::{RunEvent, RunEventKind};
        use argonauts_core::names::WorkflowId;
        use argonauts_core::workflow::StepRef;

        let id = WorkflowId::numbered(1);
        let events = [
            WorkflowEvent::Started(Box::new(WorkflowStarted {
                id: id.clone(),
                from: None,
                version: "v1".to_owned(),
                parent: None,
                workflow: serde_json::from_value(serde_json::json!({
                    "name": "one",
                    "phases": [{"name": "Build", "standard": "do", "steps": [{"kind": "build"}]}],
                }))
                .unwrap(),
            })),
            WorkflowEvent::StepStarted(StepStarted {
                step: StepRef {
                    workflow: id,
                    phase: 0,
                    path: vec![0],
                },
                kind: StepKind::Build,
                standard: StandardPhase::Do,
                round: 1,
            }),
        ];
        let mut state = RunState::default();
        for (seq, event) in (1..).zip(&events) {
            let envelope = RunEvent {
                seq,
                ts: time::OffsetDateTime::UNIX_EPOCH,
                run_id: RunId::new("run-01").unwrap(),
                task_id: None,
                session: None,
                source: "argonauts-code".to_owned(),
                kind: RunEventKind::Workflow(event.clone()),
            };
            state.workflows.apply(event, &envelope);
        }
        let task = TaskId::numbered(1);
        assert_eq!(StepKey::of_task(&state, &task), None);
        state
            .tasks
            .progress
            .entry(task.clone())
            .or_default()
            .try_seq = Some(3);
        assert_eq!(
            StepKey::of_task(&state, &task),
            Some(StepKey {
                kind: StepKind::Build,
                standard: StandardPhase::Do,
            })
        );
    }
}
