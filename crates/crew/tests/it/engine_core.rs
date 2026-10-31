//! The engine as a whole: what sessions get from the settings, and a waiting run that waits
//! however long you take.

use crate::support;
use serde_json::json;
use std::time::Duration;
use tokio::sync::oneshot;

use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::runs::engine::{EngineCommand, EngineExit};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::ApprovalEvent;
use argonauts_core::runs::events::run::{RunMode, WaitReason};
use argonauts_core::settings::agents::AgentSettings;
use argonauts_core::settings::roles::RoleSettings;
use argonauts_core::workflows::steps::understand::{Read, ReadInput};
use argonauts_crew::engine::host::Ask;
use argonauts_crew::roles;

use argonauts_testkit::engine::runner::FakeSession;

use support::{Crew, names, script};

fn reading() -> serde_json::Value {
    json!({
        "summary": "Add a --verbose flag",
        "success_criteria": ["--verbose prints each step"],
        "in_scope": [],
        "out_of_scope": [],
        "assumptions": [],
        "open_questions": []
    })
}

fn read_ask(role: RoleName, worker: &ModelRef, slot: u32) -> Ask<ReadInput> {
    let input = ReadInput {
        request: "add a --verbose flag".to_owned(),
        notes: Vec::new(),
        decisions: Vec::new(),
    };
    let mut ask = Ask::new(None, role, worker.clone(), "Read the request back.", input);
    ask.slot = slot;
    ask
}

#[tokio::test]
async fn role_workers_and_effort_reach_sessions() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let claude = crew.worker("claude-code/opus", "anthropic");
    let codex = crew.worker("codex/gpt-x", "openai");
    let claude_ref: ModelRef = "claude-code/opus".parse()?;
    let codex_ref: ModelRef = "codex/gpt-x".parse()?;
    let everyone = crew.settings.crew.members();
    assert_eq!(
        roles::candidates(&crew.settings, RoleName::Reviewer),
        everyone,
        "with roles.reviewer.models unset, the crew is the candidates"
    );

    crew.settings.roles.insert(
        RoleName::Reviewer,
        RoleSettings {
            models: vec![claude_ref.clone()],
            ..RoleSettings::default()
        },
    );
    crew.settings.roles.insert(
        RoleName::Worker,
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
    assert_eq!(
        roles::candidates(&crew.settings, RoleName::Worker),
        everyone
    );

    script(&codex, vec![reading(), reading()]);
    script(&claude, vec![reading()]);
    let (host, _) = crew.host(RunMode::Crew)?;
    host.ask::<Read>(read_ask(RoleName::Worker, &codex_ref, 0))
        .await?;
    host.ask::<Read>(read_ask(RoleName::Reviewer, &codex_ref, 0))
        .await?;
    host.ask::<Read>(read_ask(RoleName::Reviewer, &claude_ref, 0))
        .await?;

    let codex_efforts: Vec<_> = codex.specs().into_iter().map(|spec| spec.effort).collect();
    assert_eq!(
        codex_efforts,
        vec![Some("high".to_owned()), Some("low".to_owned())],
        "the role's effort first, else the CLI's"
    );
    let claude_specs = claude.specs();
    assert_eq!(claude_specs.len(), 1);
    assert_eq!(
        claude_specs[0].effort, None,
        "neither is set, so the CLI decides"
    );
    assert_eq!(claude_specs[0].model, claude_ref, "its own model stays");
    Ok(())
}

#[tokio::test]
async fn waiting_run_never_expires() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.answering_worker("claude-code/opus", "anthropic");
    crew.answering_worker("codex/gpt-x", "openai");
    let first = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    assert_eq!(
        first.exit,
        EngineExit::Waiting(WaitReason::Approval),
        "{:?}, ending {:?}",
        names(&first.events),
        first.events.last().map(|event| &event.kind)
    );

    for days in [1, 30, 400] {
        crew.services
            .clock
            .advance(Duration::from_secs(days * 86_400));
        let again = crew.resume(first.events.clone(), Vec::new()).await?;
        assert_eq!(again.exit, EngineExit::Waiting(WaitReason::Approval));
        let later = &again.events[first.events.len()..];
        let decided = later.iter().any(|event| {
            matches!(
                event.kind,
                RunEventKind::Approval(ApprovalEvent::Decided(_))
            )
        });
        assert!(!decided, "nothing is approved because time passed");
        let later = names(later);
        assert!(later.contains(&"run/resumed".to_owned()), "{later:?}");
        assert_eq!(later.last().map(String::as_str), Some("run/waiting"));
    }
    Ok(())
}

#[tokio::test]
async fn a_finished_run_is_left_alone() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    let started = crew.run_started(RunMode::Captain, "done already")?;
    let mut finished = started.clone();
    finished.seq = 1;
    finished.kind = RunEventKind::Run(argonauts_core::runs::events::run::RunLifecycle::Finished {
        outcome: argonauts_core::runs::events::run::RunOutcome::Merged,
        detail: None,
    });
    let again = crew.resume(vec![started, finished], Vec::new()).await?;
    assert_eq!(
        again.exit,
        EngineExit::Finished(argonauts_core::runs::events::run::RunOutcome::Merged)
    );
    assert_eq!(again.events.len(), 2, "nothing more is recorded");
    Ok(())
}

#[tokio::test]
async fn shutdown_pauses_then_waits() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    for (name, provider) in [("claude-code/opus", "anthropic"), ("codex/gpt-x", "openai")] {
        let worker = crew.worker(name, provider);
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
    assert_eq!(exit, EngineExit::Waiting(WaitReason::Paused));
    let kinds = names(&run.sink.events());
    let tail: Vec<&str> = kinds
        .iter()
        .rev()
        .take(2)
        .rev()
        .map(String::as_str)
        .collect();
    assert_eq!(tail, ["run/paused", "run/waiting"], "{kinds:?}");
    assert!(kinds.contains(&"run/pause_requested".to_owned()));
    Ok(())
}

/// Guidance that wakes a paused run is on the board before the run's sessions resume, so a
/// resumed session's first prompt carries it, however soon the session starts.
#[tokio::test]
async fn guidance_that_wakes_a_paused_run_is_in_the_resumed_prompt() -> anyhow::Result<()> {
    const GUIDANCE: &str = "Keep the short form of the flag.";
    let mut crew = Crew::new()?;
    let mut workers = Vec::new();
    for (name, provider) in [("claude-code/opus", "anthropic"), ("codex/gpt-x", "openai")] {
        let worker = crew.worker(name, provider);
        worker.push(FakeSession::until_stopped(Vec::new()));
        worker.push(FakeSession::until_stopped(Vec::new()));
        workers.push(worker);
    }
    let _env = crew.repo.env_guard();
    let run = crew.spawn(RunMode::Crew, "add a --verbose flag")?;
    eventually("a session to start", || {
        names(&run.sink.events()).contains(&"session/started".to_owned())
    })
    .await?;
    run.shutdown.cancel();
    assert_eq!(run.exit.await?, EngineExit::Waiting(WaitReason::Paused));

    // The run host puts what woke the run in the new engine's inbox before it starts.
    let (context, _sink, inbox) = crew.context(run.sink.events())?;
    let shutdown = context.shutdown.clone();
    let (reply, _posted) = oneshot::channel();
    inbox
        .send(EngineCommand::Guidance {
            text: GUIDANCE.to_owned(),
            reply,
        })
        .await?;
    let engine = argonauts_crew::engine_for(&crew.settings, RunMode::Crew);
    let exit = tokio::spawn(engine.run(context));
    let resumed = || {
        workers
            .iter()
            .flat_map(|worker| worker.specs())
            .find(|spec| spec.resume.is_some())
    };
    eventually("a session to resume", || resumed().is_some()).await?;
    shutdown.cancel();
    exit.await?;
    let prompt = resumed().map(|spec| spec.prompt).unwrap_or_default();
    assert!(
        prompt.iter().any(|part| part.text.contains(GUIDANCE)),
        "{prompt:#?}"
    );
    Ok(())
}

/// Waits up to 2.5 seconds for `done`.
async fn eventually(what: &str, done: impl Fn() -> bool) -> anyhow::Result<()> {
    for _ in 0..500 {
        if done() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    anyhow::bail!("timed out waiting for {what}")
}

/// A repository that ignores `.argonauts/` never stops a run: the run's own files live under
/// Argonauts Code's home, so nothing of theirs is written into the repository, committed, or said.
#[tokio::test]
async fn an_ignored_argonauts_folder_never_stops_a_run() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.repo.write(".gitignore", ".argonauts/\n")?;
    crew.repo.commit_all("ignore .argonauts/")?;
    crew.answering_worker("claude-code/opus", "anthropic");
    crew.answering_worker("codex/gpt-x", "openai");
    let outcome = crew.start(RunMode::Captain, "add a --verbose flag").await?;
    let failed = matches!(
        outcome.exit,
        EngineExit::Finished(argonauts_core::runs::events::run::RunOutcome::Failed)
    );
    assert!(!failed, "{:?}", names(&outcome.events));
    let said = outcome.events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Note(note)
            if note.text == argonauts_crew::merge::conductor::IGNORED_LINE)
    });
    assert!(!said, "{:?}", names(&outcome.events));
    let touched = crew.repo.git(&[
        "log",
        "--all",
        "--name-only",
        "--format=",
        "--",
        ".argonauts",
    ])?;
    assert!(touched.trim().is_empty(), "{touched}");
    assert!(!crew.repo.root().join(".argonauts").exists());
    Ok(())
}
