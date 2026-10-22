//! The status summary: folded as events are recorded, written to each session's status file at
//! most once a second, and injected at session start.

use crate::support;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use time::{Duration, OffsetDateTime};

use argonauts_core::event::AgentEvent;
use argonauts_core::events::agent_status::{RunSpend, SpendEvent};
use argonauts_core::events::goal::{Criterion, GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::run::Party;
use argonauts_core::events::session::InputPartKind;
use argonauts_core::events::task::{
    CheckKind, CheckOutcome, TaskEvent, TaskKind, TaskSpec, TaskState,
};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::steps::understand::{Read, ReadInput};
use argonauts_core::{RunId, Spend, TaskId};
use argonauts_crew::engine::host::{Ask, Host};
use argonauts_crew::state::RunState;
use argonauts_crew::status::files::StatusFiles;
use argonauts_crew::status::folder::Folder;
use argonauts_crew::status::{self, Viewer};
use argonauts_testkit::runner::FakeSession;

fn at(seq: u64, task: Option<&str>, session: Option<&str>, kind: RunEventKind) -> RunEvent {
    RunEvent {
        seq,
        ts: OffsetDateTime::UNIX_EPOCH + Duration::seconds(seq as i64),
        run_id: RunId::new("run-01").unwrap(),
        task_id: task.map(|task| TaskId::new(task).unwrap()),
        session: session.map(|key| SessionKey::parse(key).unwrap()),
        source: "argonauts-code".to_owned(),
        kind,
    }
}

fn spec(id: &str, depends_on: &[&str]) -> TaskSpec {
    TaskSpec {
        id: TaskId::new(id).unwrap(),
        title: format!("Build {id}"),
        task: String::new(),
        kind: TaskKind::Build,
        owned_paths: vec![format!("src/{id}/**")],
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: depends_on
            .iter()
            .map(|task| TaskId::new(*task).unwrap())
            .collect(),
        criteria: Vec::new(),
        worker: Some(ModelRef::new("codex", None)),
        reviewers: Vec::new(),
        limit: Default::default(),
        predictions: None,
        item: None,
    }
}

fn state(from: TaskState, to: TaskState) -> RunEventKind {
    RunEventKind::Task(TaskEvent::State {
        from: Some(from),
        to,
        reason: String::new(),
    })
}

/// A short run: the goal, two tasks, output deltas, a check, a limit reading, a merge.
fn log() -> Vec<RunEvent> {
    let kinds: Vec<(Option<&str>, Option<&str>, RunEventKind)> = vec![
        (
            None,
            None,
            RunEventKind::Goal(GoalEvent::Approved {
                file: GoalFile {
                    run: RunId::new("run-01").unwrap(),
                    version: 1,
                    summary: "Per-key rate limiting on the public API".to_owned(),
                    criteria: vec![Criterion::new("G1", "A key past its limit gets 429")],
                    in_scope: Vec::new(),
                    out_of_scope: Vec::new(),
                    assumptions: Vec::new(),
                    answers: Vec::new(),
                    source: GoalSource::Readings,
                    path: PathBuf::from(".argonauts/goal/run-01.md"),
                    sha256: "abc".to_owned(),
                },
                by: Party::User,
            }),
        ),
        (
            Some("task-001"),
            None,
            RunEventKind::Task(TaskEvent::Created(Box::new(spec("task-001", &[])))),
        ),
        (
            Some("task-002"),
            None,
            RunEventKind::Task(TaskEvent::Created(Box::new(spec(
                "task-002",
                &["task-001"],
            )))),
        ),
        (
            Some("task-001"),
            None,
            state(TaskState::Ready, TaskState::Running),
        ),
        (
            Some("task-001"),
            Some("s-001"),
            RunEventKind::Agent(AgentEvent::TextDelta {
                text: "Reading the router".to_owned(),
                parent: None,
            }),
        ),
        (
            Some("task-001"),
            Some("s-001"),
            RunEventKind::Agent(AgentEvent::TextDelta {
                text: " and the middleware.".to_owned(),
                parent: None,
            }),
        ),
        (
            Some("task-001"),
            None,
            RunEventKind::Check(CheckOutcome {
                name: "cargo test".to_owned(),
                kind: CheckKind::Command,
                passed: true,
                command: None,
                exit_code: Some(0),
                duration_ms: 900,
                summary: "41 passed".to_owned(),
                output_tail: String::new(),
                violations: Vec::new(),
                sandboxed: true,
                skipped: false,
            }),
        ),
        (
            None,
            None,
            RunEventKind::Spend(SpendEvent::Run(RunSpend {
                spent: Spend {
                    cost_usd: 1.25,
                    ..Spend::default()
                },
                limit: None,
                lines: Vec::new(),
                reserve_left: None,
            })),
        ),
        (
            Some("task-001"),
            None,
            state(TaskState::Merging, TaskState::Merged),
        ),
    ];
    kinds
        .into_iter()
        .enumerate()
        .map(|(seq, (task, session, kind))| at(seq as u64, task, session, kind))
        .collect()
}

#[test]
fn status_refresh_is_incremental() {
    let events = log();
    let worker = Viewer::Session {
        role: RoleName::Worker,
        task: Some(TaskId::new("task-002").unwrap()),
        label: None,
    };
    let mut folder = Folder::default();
    for (at, event) in events.iter().enumerate() {
        folder.push(event);
        // One event at a time gives what a fold of the whole prefix gives.
        for viewer in [Viewer::User, worker.clone(), Viewer::Narrator] {
            assert_eq!(
                folder.summary(&viewer),
                status::summary(&events[..=at], &viewer),
                "after event {at}, for {viewer:?}"
            );
        }
        // Output deltas change nothing anyone sees, so no status file is rewritten for them.
        let delta = matches!(event.kind, RunEventKind::Agent(_));
        assert_eq!(folder.changed_for(&worker), !delta, "event {at}");
    }

    let summary = folder.summary(&worker);
    assert_eq!(summary.goal[0], "Per-key rate limiting on the public API");
    let waiting = summary
        .tasks
        .iter()
        .find(|task| task.task.as_str() == "task-002")
        .unwrap();
    assert_eq!(waiting.note, "", "task-001 is merged into the run now");
    assert!(summary.recent.iter().any(|line| line.contains("41 passed")));
    assert_eq!(summary.spend.as_ref().unwrap().spent.cost_usd, 1.25);

    // Status files: at most one write a second, and a held change is written once due.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions/s-002/status.json");
    let mut files = StatusFiles::default();
    let start = OffsetDateTime::UNIX_EPOCH;
    let before = status::summary(&events[..3], &worker);
    assert!(files.write(&path, &before, start).unwrap());
    assert!(
        !files
            .write(&path, &summary, start + Duration::milliseconds(400))
            .unwrap()
    );
    assert_eq!(files.flush(start + Duration::milliseconds(900)).unwrap(), 0);
    let on_disk: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(on_disk, serde_json::to_value(&before).unwrap());
    assert_eq!(files.flush(start + Duration::seconds(1)).unwrap(), 1);
    let on_disk: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(on_disk, serde_json::to_value(&summary).unwrap());
    assert!(
        files
            .write(&path, &summary, start + Duration::seconds(3))
            .unwrap()
    );
}

#[tokio::test]
async fn status_summary_injected_at_session_start() -> anyhow::Result<()> {
    let mut crew = support::Crew::new()?;
    let reader = crew.worker("claude-code", "anthropic");
    reader.push(FakeSession::structured(json!({
        "summary": "Rate limit the API per key.",
        "success_criteria": ["429 past the limit"],
        "in_scope": [],
        "out_of_scope": [],
        "assumptions": [],
        "open_questions": []
    })));
    let (context, sink, _commands) = crew.context(Vec::new())?;
    let host = Host::new(Arc::new(crew.shared(&context, RunState::default())?));
    let task = TaskId::new("task-001")?;
    host.record(
        Some(&task),
        None,
        RunEventKind::Task(TaskEvent::Created(Box::new(spec("task-001", &[])))),
    )?;
    let viewer = Viewer::Session {
        role: RoleName::Reader,
        task: None,
        label: None,
    };
    let expected = status::render(&status::summary(&sink.events(), &viewer));

    host.ask::<Read>(Ask::new(
        None,
        RoleName::Reader,
        ModelRef::new("claude-code", None),
        "Read the request back.",
        ReadInput {
            request: "add per-key rate limiting to the public API".to_owned(),
            notes: Vec::new(),
            decisions: Vec::new(),
        },
    ))
    .await?;

    let specs = reader.specs();
    let status = specs[0]
        .prompt
        .iter()
        .find(|part| part.kind == InputPartKind::Status)
        .expect("the session starts with a status part");
    assert_eq!(status.text, expected);
    assert!(status.text.contains("task-001"));
    Ok(())
}

/// A session's start or its end with what it spent, as Argonauts Code records them.
fn session_spend(step: &str, usd: f64) -> anyhow::Result<RunEventKind> {
    let data = match step {
        "started" => json!({
            "event": "started", "worker": "claude-code", "agent_id": "claude-code",
            "provider": "anthropic", "kind": "cli", "role": "worker", "step": "build",
            "access": "build", "lifetime": "task", "resumed_from": null, "follow_key": null,
            "workflow_step": null,
        }),
        _ => json!({
            "event": "ended", "cli_session": null, "exit_code": 0, "signal": null,
            "duration_ms": 1000, "reason": {"why": "completed"},
            "spend": {"input_tokens": 1000, "output_tokens": 100, "cached_tokens": 0,
                      "cost_usd": usd, "cost_partial": false, "cost_estimated": false},
            "tool_calls": 1, "result_text": null, "structured": null,
        }),
    };
    Ok(serde_json::from_value(
        json!({"type": "session", "data": data}),
    )?)
}

/// What the run did once its own limit was spent: stopped by your rules, or paused.
async fn at_the_run_limit(autopilot: bool) -> anyhow::Result<(bool, bool, Vec<String>)> {
    use argonauts_core::events::run::RunLifecycle;
    use argonauts_core::settings::approvals::ApprovalMode;
    let mut crew = support::Crew::new()?;
    crew.settings.limits.spend.spend_per_run = Some(0.02);
    let (context, sink, _commands) = crew.context(Vec::new())?;
    let host = Host::new(Arc::new(crew.shared(&context, RunState::default())?));
    if autopilot {
        host.record(
            None,
            None,
            RunEventKind::Run(RunLifecycle::ApprovalModeChanged {
                mode: ApprovalMode::Autopilot,
                by: Party::User,
            }),
        )?;
    }
    let session = SessionKey::parse("s-001").expect("a session key");
    host.record(None, Some(&session), session_spend("started", 0.0)?)?;
    host.record(None, Some(&session), session_spend("ended", 0.05)?)?;
    for _ in 0..200 {
        let settled = sink.events().iter().any(|event| {
            matches!(
                &event.kind,
                RunEventKind::Run(
                    RunLifecycle::StopRequested { .. } | RunLifecycle::PauseRequested { .. }
                )
            )
        });
        if settled {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let events = sink.events();
    let stopped = events.iter().any(|event| {
        matches!(
            &event.kind,
            RunEventKind::Run(RunLifecycle::StopRequested { by: Party::Rules })
        )
    });
    let paused = events.iter().any(|event| {
        matches!(
            &event.kind,
            RunEventKind::Run(RunLifecycle::PauseRequested { .. })
        )
    });
    let notes = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Note(note) => Some(note.text.clone()),
            _ => None,
        })
        .collect();
    Ok((stopped, paused, notes))
}

/// On autopilot a run that spent its own limit never waits for you: it ends with its work
/// kept and says which limit. Supervised, it pauses until you raise it.
#[tokio::test]
async fn on_autopilot_a_run_at_its_limit_ends_and_supervised_it_pauses() -> anyhow::Result<()> {
    let (stopped, paused, notes) = at_the_run_limit(true).await?;
    assert!(stopped && !paused, "{notes:?}");
    assert!(
        notes.iter().any(|note| note.starts_with(
            "The run reached its $0.02 spend limit, so it ends here with its work kept."
        )),
        "{notes:?}"
    );
    let (stopped, paused, notes) = at_the_run_limit(false).await?;
    assert!(paused && !stopped, "{notes:?}");
    assert!(
        notes
            .iter()
            .any(|note| note.contains("so it pauses here with its work kept")),
        "{notes:?}"
    );
    Ok(())
}
