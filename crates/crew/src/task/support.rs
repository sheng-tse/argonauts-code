//! Small reads and records the task modules share.

use std::time::Duration;

use argonauts_core::TaskId;
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::events::task::{TaskEvent, TaskState};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::layout::run_files;
use argonauts_core::names::{ModelRef, SessionKey};
use argonauts_core::steps::StepName;

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::state::RunState;
use crate::state::sessions::SessionRecord;
use crate::state::tasks::{TaskEntry, TaskProgress};

/// How often a wait on something outside the task (an answer, an agent, limit) looks again.
const POLL: Duration = Duration::from_millis(100);

pub(crate) fn record_of(host: &Host, task: &TaskId) -> EngineResult<TaskEntry> {
    host.read(|state| state.tasks.get(task).cloned())
        .ok_or_else(|| EngineError::Refused(format!("there is no task {task}")))
}

/// The task as a question's title names it: by its title, as people read it, and never by its
/// id. Titles reach the transcript's lines, the run's row and the recap.
pub(crate) fn called(record: &TaskEntry) -> &str {
    record.spec.title.trim().trim_end_matches('.')
}

/// The pipeline's bookkeeping for `task`.
pub(crate) fn progress_of(host: &Host, task: &TaskId) -> TaskProgress {
    host.read(|state| state.tasks.progress(task).clone())
}

pub(crate) fn state_of(host: &Host, task: &TaskId) -> EngineResult<TaskState> {
    record_of(host, task).map(|record| record.state)
}

pub(crate) fn record_task(host: &Host, task: &TaskId, event: TaskEvent) -> EngineResult<RunEvent> {
    host.record(Some(task), None, RunEventKind::Task(event))
}

pub(crate) fn record_task_in(
    host: &Host,
    task: &TaskId,
    session: &SessionKey,
    event: TaskEvent,
) -> EngineResult<RunEvent> {
    host.record(Some(task), Some(session), RunEventKind::Task(event))
}

/// The task's own events so far, for its fallback note. None when the run keeps no log on
/// disk, as with an in-memory sink; a log that can't be read is an error.
pub(crate) fn task_events(host: &Host, task: &TaskId) -> EngineResult<Vec<RunEvent>> {
    let path = host.repo().run_dir.join(run_files::EVENTS);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let back = argonauts_core::journal::read_all(&path)?;
    Ok(back
        .events
        .into_iter()
        .filter(|event| event.task_id.as_ref() == Some(task))
        .collect())
}

/// The task's worker: the one recorded, else the planned one, else the main agent.
pub(crate) fn worker_of(host: &Host, record: &TaskEntry) -> ModelRef {
    record
        .worker
        .clone()
        .or_else(|| record.spec.worker.clone())
        .unwrap_or_else(|| host.settings().crew.main.clone())
}

/// The task's newest worker session, finished or not.
pub(crate) fn newest_worker_session<'a>(
    state: &'a RunState,
    task: &TaskId,
) -> Option<&'a SessionRecord> {
    state
        .sessions
        .by_key
        .values()
        .filter(|s| s.task.as_ref() == Some(task) && s.started.step == Some(StepName::Build))
        .max_by_key(|s| s.since)
}

pub(crate) fn part(
    kind: InputPartKind,
    label: impl Into<String>,
    text: impl Into<String>,
    source: Option<String>,
) -> InputPart {
    let text = text.into();
    InputPart {
        kind,
        label: label.into(),
        approx_tokens: u32::try_from(text.chars().count().div_ceil(4)).unwrap_or(u32::MAX),
        text,
        source,
    }
}

/// Returns once anything is recorded after `seq`, or `Err(Interrupted)` when the run stops.
pub(crate) async fn changed_since(host: &Host, seq: Option<u64>) -> EngineResult<()> {
    loop {
        if host.read(|state| state.seq) != seq {
            return Ok(());
        }
        tokio::select! {
            () = host.interrupted().cancelled() => return Err(EngineError::Interrupted),
            () = tokio::time::sleep(POLL) => {}
        }
    }
}

/// Waits until `done` holds for the state, or the run stops.
pub(crate) async fn wait_until(host: &Host, done: impl Fn(&RunState) -> bool) -> EngineResult<()> {
    loop {
        let (holds, seq) = host.read(|state| (done(state), state.seq));
        if holds {
            return Ok(());
        }
        changed_since(host, seq).await?;
    }
}
