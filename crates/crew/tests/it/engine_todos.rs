//! A task's to-do count: the crew records it from the list its own session keeps, once each
//! time the count changes, and a reviewer's list counts nothing for the task.

use crate::support;
use std::sync::Arc;

use argonauts_core::agents::session_runner::{Access, RunnerEvent, RunnerKind};
use argonauts_core::base::names::{AgentId, ModelRef, RoleName, SessionKey};
use argonauts_core::runs::event::AgentEvent;
use argonauts_core::runs::events::session::{SessionEvent, SessionStarted};
use argonauts_core::runs::events::task::{TaskEvent, TaskKind, TaskSpec};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::{TaskId, TodoItem, TodoStatus};
use argonauts_crew::engine::host::Host;
use argonauts_crew::state::RunState;
use argonauts_testkit::engine::services::MemorySink;

fn task_id() -> TaskId {
    TaskId::new("task-001").unwrap()
}

fn worker() -> ModelRef {
    "codex/gpt-5.6-luna".parse().unwrap()
}

fn host(crew: &support::Crew) -> anyhow::Result<(Host, Arc<MemorySink>)> {
    let (context, sink, _commands) = crew.context(Vec::new())?;
    let host = Host::new(Arc::new(crew.shared(&context, RunState::default())?));
    Ok((host, sink))
}

/// Records the task and a session of it in `role`.
fn session(host: &Host, n: u32, role: RoleName) -> anyhow::Result<SessionKey> {
    let key = SessionKey::numbered(n);
    host.record(
        Some(&task_id()),
        Some(&key),
        RunEventKind::Session(SessionEvent::Started(Box::new(SessionStarted {
            worker: worker(),
            agent_id: AgentId("codex".to_owned()),
            provider: "codex".to_owned(),
            kind: RunnerKind::Cli,
            role,
            step: None,
            access: Access::Build,
            lifetime: SessionLifetime::Task,
            resumed_from: None,
            follow_key: u8::try_from(n).ok(),
            workflow_step: None,
            label: None,
            asked: None,
            asked_by: None,
            asked_why: None,
            forked: false,
            seat: None,
            billing: None,
        }))),
    )?;
    Ok(key)
}

fn list(done: usize, all: usize) -> RunnerEvent {
    let items = (0..all)
        .map(|i| TodoItem {
            text: format!("Step {i}"),
            status: if i < done {
                TodoStatus::Completed
            } else {
                TodoStatus::Pending
            },
        })
        .collect();
    RunnerEvent::Agent(AgentEvent::Todo { items })
}

fn counted(events: &[RunEvent]) -> Vec<(u32, u32)> {
    events
        .iter()
        .filter_map(|event| match event.kind {
            RunEventKind::Task(TaskEvent::Todos { done, total }) => {
                assert_eq!(event.task_id, Some(task_id()));
                Some((done, total))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_workers_list_counts_for_its_task_once_per_change() -> anyhow::Result<()> {
    let crew = support::Crew::new()?;
    let (host, sink) = host(&crew)?;
    let spec = TaskSpec {
        id: task_id(),
        title: "Due date field".into(),
        task: String::new(),
        kind: TaskKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        worker: Some(worker()),
        reviewers: Vec::new(),
        limit: Default::default(),
        predictions: None,
        item: None,
        way: None,
    };
    host.record(
        Some(&task_id()),
        None,
        RunEventKind::Task(TaskEvent::Created(Box::new(spec))),
    )?;
    let builder = session(&host, 1, RoleName::Worker)?;
    let reviewer = session(&host, 2, RoleName::Reviewer)?;
    let task = task_id();
    for (key, event) in [
        (&builder, list(0, 3)),
        // The same count again, as when only a step's words change.
        (&builder, list(0, 3)),
        (&builder, list(1, 3)),
        (&reviewer, list(0, 2)),
        // An empty list counts nothing.
        (&builder, list(0, 0)),
        (&builder, list(3, 3)),
    ] {
        host.record_worker_event("codex", key, Some(&task), event)?;
    }
    assert_eq!(counted(&sink.events()), [(0, 3), (1, 3), (3, 3)]);
    Ok(())
}
