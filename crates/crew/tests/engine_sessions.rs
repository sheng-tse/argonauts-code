//! Sessions through the `Host`: asks that replay or resume from the log, lifetimes, the
//! goal-first rule, pausing, and steering.

mod support;

use std::time::Duration;

use serde_json::json;

use argonauts_core::event::AgentEvent;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::RunMode;
use argonauts_core::events::session::{EndReason, SessionEvent};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::roles::RoleSettings;
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::steps::goal::{Restate, RestateInput};
use argonauts_core::worker::{Access, SessionControl, WorkerEvent};
use argonauts_crew::EngineError;
use argonauts_crew::engine::commands;
use argonauts_crew::engine::host::{Ask, Host, SessionPlan};
use argonauts_testkit::worker::FakeSession;

use support::{Crew, names, script};

fn restatement(summary: &str) -> serde_json::Value {
    json!({
        "summary": summary,
        "success_criteria": [],
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

fn plan(worker: &WorkerRef, access: Access, host: &Host) -> SessionPlan {
    SessionPlan {
        at: None,
        role: RoleName::Consultant,
        worker: worker.clone(),
        card: None,
        access,
        step: None,
        workdir: host.repo().root.clone(),
        parts: Vec::new(),
        json_schema: None,
        resume: None,
        slot: 0,
        label: None,
        reviews: false,
    }
}

/// Waits until `count` sessions run, so a test can act on them mid-flight.
async fn live(host: &Host, count: usize) {
    for _ in 0..500 {
        if host.shared().live.len() == count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("{count} sessions never went live");
}

#[tokio::test]
async fn a_finished_ask_replays_from_the_log_and_runs_nothing() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let worker = crew.worker("codex/gpt-x", "gpt");
    let reference: WorkerRef = "codex/gpt-x".parse()?;
    script(&worker, vec![restatement("first")]);
    let (host, sink) = crew.host(RunMode::Solo)?;
    let first = host
        .ask::<Restate>(restate(RoleName::Restater, &reference, 0))
        .await?;
    assert_eq!(first.output.summary, "first");

    let (again, _) = crew.host_over(sink.events())?;
    let replayed = again
        .ask::<Restate>(restate(RoleName::Restater, &reference, 0))
        .await?;
    assert_eq!(replayed.output, first.output);
    assert_eq!(replayed.session, first.session);
    assert_eq!(worker.specs().len(), 1, "the answer came from the log");
    Ok(())
}

#[tokio::test]
async fn an_unfinished_ask_resumes_its_vendor_session_under_the_same_key() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let worker = crew.worker("codex/gpt-x", "gpt");
    let reference: WorkerRef = "codex/gpt-x".parse()?;
    worker.push(FakeSession::until_stopped(vec![WorkerEvent::Session(
        SessionEvent::VendorSession {
            id: "vendor-7".to_owned(),
            transcript: None,
        },
    )]));
    let (host, sink) = crew.host(RunMode::Solo)?;
    let asking = {
        let host = host.clone();
        let reference = reference.clone();
        tokio::spawn(async move {
            host.ask::<Restate>(restate(RoleName::Restater, &reference, 0))
                .await
        })
    };
    live(&host, 1).await;
    // The supervisor died here: the log has the start and the vendor id, but no end.
    let history = sink.events();
    asking.abort();
    let _ = asking.await;

    script(&worker, vec![restatement("resumed")]);
    let (again, _) = crew.host_over(history)?;
    let answered = again
        .ask::<Restate>(restate(RoleName::Restater, &reference, 0))
        .await?;
    assert_eq!(answered.output.summary, "resumed");
    let specs = worker.specs();
    assert_eq!(specs.len(), 2);
    assert_eq!(
        specs[1].key, specs[0].key,
        "the same session, not a new one"
    );
    assert_eq!(specs[1].resume.as_deref(), Some("vendor-7"));
    assert!(
        specs[1].prompt[0].text.contains("resumed"),
        "{:?}",
        specs[1].prompt
    );
    Ok(())
}

#[tokio::test]
async fn lifetime_run_reuses_session() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let worker = crew.worker("codex/gpt-x", "gpt");
    let reference: WorkerRef = "codex/gpt-x".parse()?;
    crew.settings.roles.insert(
        RoleName::Restater,
        RoleSettings {
            lifetime: Some(SessionLifetime::Run),
            ..RoleSettings::default()
        },
    );
    script(&worker, vec![restatement("one"), restatement("two")]);
    let (host, _) = crew.host(RunMode::Solo)?;
    host.ask::<Restate>(restate(RoleName::Restater, &reference, 0))
        .await?;
    host.ask::<Restate>(restate(RoleName::Restater, &reference, 1))
        .await?;
    let specs = worker.specs();
    assert_eq!(specs[0].resume, None);
    assert_eq!(specs[0].lifetime, SessionLifetime::Run);
    assert_eq!(
        specs[1].resume.as_deref(),
        Some("fake-session"),
        "the role's one session for the run continues"
    );
    assert_ne!(
        specs[1].key, specs[0].key,
        "each task keeps its own key and logs"
    );

    let mut blind = restate(RoleName::Restater, &reference, 2);
    blind.blind = true;
    script(&worker, vec![restatement("three")]);
    host.ask::<Restate>(blind).await?;
    assert_eq!(
        worker.specs()[2].resume,
        None,
        "blind roles are always fresh"
    );
    Ok(())
}

#[tokio::test]
async fn building_sessions_wait_for_the_approved_goal() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let worker = crew.worker("codex/gpt-x", "gpt");
    let reference: WorkerRef = "codex/gpt-x".parse()?;
    let (host, _) = crew.host(RunMode::Crew)?;
    for access in [Access::Build, Access::Spike] {
        let refused = host.start(plan(&reference, access, &host)).await;
        assert!(
            matches!(refused, Err(EngineError::Refused(_))),
            "{access:?}: {refused:?}"
        );
    }
    assert!(worker.specs().is_empty(), "no worker was asked");
    Ok(())
}

#[tokio::test]
async fn a_started_session_is_pumped_into_the_log() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let worker = crew.worker("codex/gpt-x", "gpt");
    let reference: WorkerRef = "codex/gpt-x".parse()?;
    worker.push(FakeSession::completed("all done"));
    let (host, sink) = crew.host(RunMode::Solo)?;
    let running = host
        .start(plan(&reference, Access::ReadOnly, &host))
        .await?;
    let key = running.key.clone();
    let end = running.ended().await?;
    assert_eq!(end.reason, EndReason::Completed);
    assert!(host.shared().live.is_empty());

    let events = sink.events();
    let session: Vec<_> = events
        .iter()
        .filter(|event| event.session.as_ref() == Some(&key))
        .collect();
    let kinds = names(
        &session
            .iter()
            .map(|event| (*event).clone())
            .collect::<Vec<_>>(),
    );
    assert_eq!(kinds.first().map(String::as_str), Some("session/started"));
    assert_eq!(kinds.last().map(String::as_str), Some("session/ended"));
    assert!(
        kinds.iter().any(|kind| kind == "agent/text_delta"),
        "{kinds:?}"
    );
    assert!(
        session
            .iter()
            .all(|event| event.source == "codex" || event.source == "conductor")
    );

    // Started again with the same identity, it hands back the recorded end.
    let replayed = host
        .start(plan(&reference, Access::ReadOnly, &host))
        .await?;
    assert_eq!(replayed.key, key);
    assert_eq!(replayed.ended().await?, end);
    assert_eq!(worker.specs().len(), 1);
    Ok(())
}

#[tokio::test]
async fn pause_stops_running_step_sessions() -> anyhow::Result<()> {
    for now in [false, true] {
        let mut crew = Crew::new()?;
        let worker = crew.worker("codex/gpt-x", "gpt");
        let reference: WorkerRef = "codex/gpt-x".parse()?;
        worker.push(FakeSession::until_stopped(vec![WorkerEvent::Agent(
            AgentEvent::TextDelta {
                text: "thinking".to_owned(),
                parent: None,
            },
        )]));
        let (host, sink) = crew.host(RunMode::Solo)?;
        let asking = {
            let host = host.clone();
            let reference = reference.clone();
            tokio::spawn(async move {
                host.ask::<Restate>(restate(RoleName::Restater, &reference, 0))
                    .await
            })
        };
        live(&host, 1).await;
        commands::pause(&host, now).await?;
        let asked = asking.await?;
        assert!(
            matches!(asked, Err(EngineError::Interrupted)),
            "the step unwinds to resume later: {asked:?}"
        );
        let stop = if now {
            SessionControl::Stop
        } else {
            SessionControl::StopAfterTool
        };
        assert!(
            worker
                .controls()
                .iter()
                .any(|(_, control)| *control == stop),
            "{:?}",
            worker.controls()
        );
        let events = sink.events();
        let ended = events.iter().find_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Ended(end)) => Some(end.reason.clone()),
            _ => None,
        });
        assert_eq!(ended, Some(EndReason::Paused));
        let kinds = names(&events);
        assert!(
            kinds.contains(&"run/pause_requested".to_owned()),
            "{kinds:?}"
        );
        assert_eq!(kinds.last().map(String::as_str), Some("run/paused"));
        assert!(host.read(|state| state.run.paused));
        assert!(
            host.read(|state| state.sessions.in_order()[0].resumable()),
            "a paused session resumes when its step runs again"
        );
    }
    Ok(())
}

#[tokio::test]
async fn steering_reaches_a_running_session() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let worker = crew.worker("codex/gpt-x", "gpt");
    let reference: WorkerRef = "codex/gpt-x".parse()?;
    worker.push(FakeSession::until_stopped(Vec::new()));
    let (host, _) = crew.host(RunMode::Solo)?;
    let running = host
        .start(plan(&reference, Access::ReadOnly, &host))
        .await?;
    host.control(&running.key, SessionControl::WrapUp)
        .map_err(anyhow::Error::msg)?;
    host.control(&running.key, SessionControl::Stop)
        .map_err(anyhow::Error::msg)?;
    let key = running.key.clone();
    let end = running.ended().await?;
    assert_eq!(end.reason, EndReason::Stopped);
    let controls: Vec<_> = worker.controls().into_iter().map(|(_, c)| c).collect();
    assert_eq!(controls, vec![SessionControl::WrapUp, SessionControl::Stop]);
    assert!(
        host.control(&key, SessionControl::Stop).is_err(),
        "it has ended"
    );
    Ok(())
}
