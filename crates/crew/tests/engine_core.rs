//! The engine as a whole: what sessions get from the settings, and a parked run that waits
//! however long you take.

mod support;

use serde_json::json;
use std::time::Duration;

use argonauts_core::engine::EngineExit;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::checkpoint::CheckpointEvent;
use argonauts_core::events::run::{ParkReason, RunMode};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::agents::AgentSettings;
use argonauts_core::settings::roles::RoleSettings;
use argonauts_core::steps::goal::{Restate, RestateInput};
use argonauts_crew::engine::host::Ask;
use argonauts_crew::roles;

use argonauts_testkit::worker::FakeSession;

use support::{Crew, names, script};

fn restatement() -> serde_json::Value {
    json!({
        "summary": "Add a --verbose flag",
        "success_criteria": ["--verbose prints each step"],
        "in_scope": [],
        "out_of_scope": [],
        "assumptions": [],
        "open_questions": []
    })
}

fn restate(role: RoleName, worker: &WorkerRef, slot: u32) -> Ask<RestateInput> {
    let input = RestateInput {
        request: "add a --verbose flag".to_owned(),
        notes: Vec::new(),
        decisions: Vec::new(),
    };
    let mut ask = Ask::new(None, role, worker.clone(), "Restate the request.", input);
    ask.slot = slot;
    ask
}

#[tokio::test]
async fn role_workers_and_effort_reach_sessions() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let claude = crew.worker("claude-code/opus", "claude");
    let codex = crew.worker("codex/gpt-x", "gpt");
    let claude_ref: WorkerRef = "claude-code/opus".parse()?;
    let codex_ref: WorkerRef = "codex/gpt-x".parse()?;
    let everyone = crew.settings.crew.clone();
    assert_eq!(
        roles::candidates(&crew.settings, RoleName::Reviewer),
        everyone,
        "with roles.reviewer.workers unset, the crew is the candidates"
    );

    crew.settings.roles.insert(
        RoleName::Reviewer,
        RoleSettings {
            workers: vec![claude_ref.clone()],
            ..RoleSettings::default()
        },
    );
    crew.settings.roles.insert(
        RoleName::Owner,
        RoleSettings {
            effort: Some("high".to_owned()),
            ..RoleSettings::default()
        },
    );
    crew.settings.agents.insert(
        "codex".to_owned(),
        AgentSettings {
            effort: Some("low".to_owned()),
            ..AgentSettings::default()
        },
    );
    assert_eq!(
        roles::candidates(&crew.settings, RoleName::Reviewer),
        vec![claude_ref.clone()]
    );
    assert_eq!(roles::candidates(&crew.settings, RoleName::Owner), everyone);

    script(&codex, vec![restatement(), restatement()]);
    script(&claude, vec![restatement()]);
    let (host, _) = crew.host(RunMode::Crew)?;
    host.ask::<Restate>(restate(RoleName::Owner, &codex_ref, 0))
        .await?;
    host.ask::<Restate>(restate(RoleName::Reviewer, &codex_ref, 0))
        .await?;
    host.ask::<Restate>(restate(RoleName::Reviewer, &claude_ref, 0))
        .await?;

    let codex_efforts: Vec<_> = codex.specs().into_iter().map(|spec| spec.effort).collect();
    assert_eq!(
        codex_efforts,
        vec![Some("high".to_owned()), Some("low".to_owned())],
        "the role's effort first, else the harness's"
    );
    let claude_specs = claude.specs();
    assert_eq!(claude_specs.len(), 1);
    assert_eq!(
        claude_specs[0].effort, None,
        "neither is set, so the CLI decides"
    );
    assert_eq!(claude_specs[0].worker, claude_ref, "its own model stays");
    Ok(())
}

#[tokio::test]
async fn parked_run_never_expires() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.answering_worker("claude-code/opus", "claude");
    crew.answering_worker("codex/gpt-x", "gpt");
    let first = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    assert_eq!(
        first.exit,
        EngineExit::Parked(ParkReason::Checkpoint),
        "{:?}, ending {:?}",
        names(&first.events),
        first.events.last().map(|event| &event.kind)
    );

    for days in [1, 30, 400] {
        crew.services
            .clock
            .advance(Duration::from_secs(days * 86_400));
        let again = crew.resume(first.events.clone(), Vec::new()).await?;
        assert_eq!(again.exit, EngineExit::Parked(ParkReason::Checkpoint));
        let later = &again.events[first.events.len()..];
        let decided = later.iter().any(|event| {
            matches!(
                event.kind,
                RunEventKind::Checkpoint(CheckpointEvent::Decided(_))
            )
        });
        assert!(!decided, "nothing is approved because time passed");
        let later = names(later);
        assert!(later.contains(&"run/resumed".to_owned()), "{later:?}");
        assert_eq!(later.last().map(String::as_str), Some("run/parked"));
    }
    Ok(())
}

#[tokio::test]
async fn a_finished_run_is_left_alone() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    let started = crew.run_started(RunMode::Solo, "done already")?;
    let mut finished = started.clone();
    finished.seq = 1;
    finished.kind = RunEventKind::Run(argonauts_core::events::run::RunLifecycle::Finished {
        outcome: argonauts_core::events::run::RunOutcome::Merged,
        detail: None,
    });
    let again = crew.resume(vec![started, finished], Vec::new()).await?;
    assert_eq!(
        again.exit,
        EngineExit::Finished(argonauts_core::events::run::RunOutcome::Merged)
    );
    assert_eq!(again.events.len(), 2, "nothing more is recorded");
    Ok(())
}

#[tokio::test]
async fn shutdown_pauses_then_parks() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    for (name, family) in [("claude-code/opus", "claude"), ("codex/gpt-x", "gpt")] {
        let worker = crew.worker(name, family);
        worker.push(FakeSession::until_stopped(Vec::new()));
        worker.push(FakeSession::until_stopped(Vec::new()));
    }
    let _env = crew.repo.env_guard();
    let run = crew.spawn(RunMode::Crew, "add a --verbose flag")?;
    let mut started = false;
    for _ in 0..500 {
        started = names(&run.sink.events()).contains(&"session/started".to_owned());
        if started {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(started, "{:?}", names(&run.sink.events()));
    run.shutdown.cancel();
    let exit = run.exit.await?;
    assert_eq!(exit, EngineExit::Parked(ParkReason::Paused));
    let kinds = names(&run.sink.events());
    let tail: Vec<&str> = kinds
        .iter()
        .rev()
        .take(2)
        .rev()
        .map(String::as_str)
        .collect();
    assert_eq!(tail, ["run/paused", "run/parked"], "{kinds:?}");
    assert!(kinds.contains(&"run/pause_requested".to_owned()));
    Ok(())
}
