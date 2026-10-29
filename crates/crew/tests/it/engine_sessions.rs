//! Sessions through the `Host`: asks that replay or resume from the log, lifetimes, the
//! goal-first rule, pausing, and steering.

use crate::support;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use argonauts_core::LimitDimension;
use argonauts_core::event::AgentEvent;
use argonauts_core::events::run::{Party, ResumeCause, RunLifecycle, RunMode, RunResumed};
use argonauts_core::events::session::{Billing, EndReason, LimitEvent, SessionEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::meter::{MeterAction, MeterLimit, Spend, SpendStatus};
use argonauts_core::names::{ModelRef, RoleName, SeatId};
use argonauts_core::registry::ModelEntry;
use argonauts_core::session::AuthMode;
use argonauts_core::session_runner::{Access, RunnerEvent, SessionControl};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::roles::RoleSettings;
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::steps::StepError;
use argonauts_core::steps::understand::{Read, ReadInput};
use argonauts_crew::EngineError;
use argonauts_crew::engine::commands;
use argonauts_crew::engine::host::{Ask, Host, SessionPlan};
use argonauts_crew::state::RunState;
use argonauts_testkit::runner::{FakeRunner, FakeSession};

use support::{Crew, names, script};

fn reading(summary: &str) -> serde_json::Value {
    json!({
        "summary": summary,
        "success_criteria": [],
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

fn plan(worker: &ModelRef, access: Access, host: &Host) -> SessionPlan {
    SessionPlan {
        without_status: false,
        at: None,
        role: RoleName::Consultant,
        worker: worker.clone(),
        task: None,
        access,
        step: None,
        workdir: host.repo().root.clone(),
        parts: Vec::new(),
        json_schema: None,
        resume: None,
        slot: 0,
        label: None,
        reviews: false,
        fresh: false,
        independent_of: Vec::new(),
        in_place_of: None,
        fork_of: None,
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
    let worker = crew.worker("codex/gpt-x", "openai");
    let reference: ModelRef = "codex/gpt-x".parse()?;
    script(&worker, vec![reading("first")]);
    let (host, sink) = crew.host(RunMode::Captain)?;
    let first = host
        .ask::<Read>(read_ask(RoleName::Reader, &reference, 0))
        .await?;
    assert_eq!(first.output.summary, "first");

    let (again, _) = crew.host_over(sink.events())?;
    let replayed = again
        .ask::<Read>(read_ask(RoleName::Reader, &reference, 0))
        .await?;
    assert_eq!(replayed.output, first.output);
    assert_eq!(replayed.session, first.session);
    assert_eq!(worker.specs().len(), 1, "the answer came from the log");
    Ok(())
}

#[tokio::test]
async fn an_unfinished_ask_resumes_its_cli_session_under_the_same_key() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let worker = crew.worker("codex/gpt-x", "openai");
    let reference: ModelRef = "codex/gpt-x".parse()?;
    worker.push(FakeSession::until_stopped(vec![RunnerEvent::Session(
        SessionEvent::CliSession {
            id: "cli-7".to_owned(),
            transcript: None,
        },
    )]));
    let (host, sink) = crew.host(RunMode::Captain)?;
    let asking = {
        let host = host.clone();
        let reference = reference.clone();
        tokio::spawn(async move {
            host.ask::<Read>(read_ask(RoleName::Reader, &reference, 0))
                .await
        })
    };
    live(&host, 1).await;
    // The run host died here: the log has the start and the agent id, but no end.
    let history = sink.events();
    asking.abort();
    let _ = asking.await;

    script(&worker, vec![reading("resumed")]);
    let (again, _) = crew.host_over(history)?;
    let answered = again
        .ask::<Read>(read_ask(RoleName::Reader, &reference, 0))
        .await?;
    assert_eq!(answered.output.summary, "resumed");
    let specs = worker.specs();
    assert_eq!(specs.len(), 2);
    assert_eq!(
        specs[1].key, specs[0].key,
        "the same session, not a new one"
    );
    assert_eq!(specs[1].resume.as_deref(), Some("cli-7"));
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
    let worker = crew.worker("codex/gpt-x", "openai");
    let reference: ModelRef = "codex/gpt-x".parse()?;
    crew.settings.roles.insert(
        RoleName::Reader,
        RoleSettings {
            lifetime: Some(SessionLifetime::Run),
            ..RoleSettings::default()
        },
    );
    script(&worker, vec![reading("one"), reading("two")]);
    let (host, _) = crew.host(RunMode::Captain)?;
    host.ask::<Read>(read_ask(RoleName::Reader, &reference, 0))
        .await?;
    host.ask::<Read>(read_ask(RoleName::Reader, &reference, 1))
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

    let mut blind = read_ask(RoleName::Reader, &reference, 2);
    blind.blind = true;
    script(&worker, vec![reading("three")]);
    host.ask::<Read>(blind).await?;
    assert_eq!(
        worker.specs()[2].resume,
        None,
        "blind roles are always fresh"
    );
    Ok(())
}

/// Two seats of one model in one folder are two sessions: each carries on its own, and the log
/// says which seat each speaks for.
#[tokio::test]
async fn each_seat_carries_on_its_own_session_and_never_another_seats() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let worker = crew.worker("claude-code/opus", "anthropic");
    let reference: ModelRef = "claude-code/opus".parse()?;
    script(
        &worker,
        vec![reading("one"), reading("two"), reading("three")],
    );
    let (host, _) = crew.host(RunMode::Captain)?;
    let seat_ask = |seat: u32, slot: u32| {
        let mut ask = read_ask(RoleName::Reader, &reference, slot);
        ask.seat = Some(SeatId::numbered(seat));
        ask
    };
    host.ask::<Read>(seat_ask(1, 0)).await?;
    host.ask::<Read>(seat_ask(2, 1)).await?;
    host.ask::<Read>(seat_ask(1, 2)).await?;
    let specs = worker.specs();
    assert_eq!(specs[0].resume, None);
    assert_eq!(
        specs[1].resume, None,
        "the second seat of the same model starts a session of its own"
    );
    assert_eq!(
        specs[2].resume.as_deref(),
        Some("fake-session"),
        "the first seat carries on its own"
    );
    assert!(
        specs
            .iter()
            .all(|spec| spec.lifetime == SessionLifetime::Seat)
    );
    let seats: Vec<Option<SeatId>> = host.read(|state| {
        state
            .sessions
            .in_order()
            .iter()
            .map(|session| session.started.seat.clone())
            .collect()
    });
    let one = Some(SeatId::numbered(1));
    assert_eq!(seats, [one.clone(), Some(SeatId::numbered(2)), one]);
    Ok(())
}

#[tokio::test]
async fn building_sessions_wait_for_the_approved_goal() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let worker = crew.worker("codex/gpt-x", "openai");
    let reference: ModelRef = "codex/gpt-x".parse()?;
    let (host, _) = crew.host(RunMode::Crew)?;
    for access in [Access::Build, Access::Experiment] {
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
    let worker = crew.worker("codex/gpt-x", "openai");
    let reference: ModelRef = "codex/gpt-x".parse()?;
    worker.push(FakeSession::completed("all done"));
    let (host, sink) = crew.host(RunMode::Captain)?;
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
            .all(|event| event.source == "codex" || event.source == "argonauts-code")
    );

    // Started again with the same identity, it returns the recorded end.
    let replayed = host
        .start(plan(&reference, Access::ReadOnly, &host))
        .await?;
    assert_eq!(replayed.key, key);
    assert_eq!(replayed.ended().await?, end);
    assert_eq!(worker.specs().len(), 1);
    Ok(())
}

/// Each session's start keeps who pays for its tokens, as the settings had it then: the plan
/// a CLI signed in to, or the key it was given.
#[tokio::test]
async fn a_session_start_records_who_pays_for_it() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let on_plan = crew.worker("codex/gpt-x", "openai");
    let on_key = crew.worker("claude-code/sonnet", "anthropic");
    crew.settings
        .agents
        .entry("claude-code".to_owned())
        .or_default()
        .auth = AuthMode::ApiKey;
    on_plan.push(FakeSession::completed("all done"));
    on_key.push(FakeSession::completed("all done"));
    let (host, sink) = crew.host(RunMode::Captain)?;
    for worker in ["codex/gpt-x", "claude-code/sonnet"] {
        let running = host
            .start(plan(&worker.parse()?, Access::ReadOnly, &host))
            .await?;
        running.ended().await?;
    }

    let billed: Vec<(String, Option<Billing>)> = sink
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started)) => {
                Some((started.worker.agent.clone(), started.billing))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        billed,
        [
            ("codex".to_owned(), Some(Billing::Plan)),
            ("claude-code".to_owned(), Some(Billing::Paid)),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn pause_stops_running_step_sessions() -> anyhow::Result<()> {
    for now in [false, true] {
        let mut crew = Crew::new()?;
        let worker = crew.worker("codex/gpt-x", "openai");
        let reference: ModelRef = "codex/gpt-x".parse()?;
        worker.push(FakeSession::until_stopped(vec![RunnerEvent::Agent(
            AgentEvent::TextDelta {
                text: "thinking".to_owned(),
                parent: None,
            },
        )]));
        let (host, sink) = crew.host(RunMode::Captain)?;
        let asking = {
            let host = host.clone();
            let reference = reference.clone();
            tokio::spawn(async move {
                host.ask::<Read>(read_ask(RoleName::Reader, &reference, 0))
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
    let worker = crew.worker("codex/gpt-x", "openai");
    let reference: ModelRef = "codex/gpt-x".parse()?;
    worker.push(FakeSession::until_stopped(Vec::new()));
    let (host, _) = crew.host(RunMode::Captain)?;
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

/// A reading of what the session has spent so far.
fn spent(usd: f64) -> RunnerEvent {
    RunnerEvent::Session(SessionEvent::Limit(LimitEvent::Spend(SpendStatus {
        level: MeterAction::Continue,
        limit: MeterLimit::Usd(0.60),
        spend: Spend {
            cost_usd: usd,
            ..Spend::default()
        },
        fraction_used: None,
        work: Default::default(),
        work_limits: Default::default(),
        binding: None,
    })))
}

/// The council run under its $0.60 limit after its second session asked to start.
struct UnderTheLimit {
    crew: Crew,
    worker: Arc<FakeRunner>,
    reference: ModelRef,
    /// What the second session's ask came to.
    second: Result<(), EngineError>,
    /// How many sessions started.
    started: usize,
    stopped: bool,
    paused: bool,
    notes: Vec<String>,
    /// The run's log once it settled.
    history: Vec<RunEvent>,
}

/// The council run under its $0.60 limit: its first Claude Code session opened at $0.3619 and
/// ended at $0.3745, which leaves $0.2255, and a second one like it asks to start.
async fn a_second_session_under_the_limit(autopilot: bool) -> anyhow::Result<UnderTheLimit> {
    let mut crew = Crew::new()?;
    crew.settings.limits.spend.spend_per_run = Some(0.60);
    let worker = crew.worker("claude-code/haiku", "anthropic");
    let reference: ModelRef = "claude-code/haiku".parse()?;
    // Claude Code lists the model by the name every line of the run gives it.
    let mut listed: ModelEntry = serde_json::from_value(json!({
        "id": "claude-code/haiku",
        "provider": "anthropic",
        "kind": "cli",
        "access": "subscription",
        "cost": { "unit": "usd" },
    }))?;
    listed.discovered = true;
    listed.name = Some("Haiku 5.5".to_owned());
    crew.services.registry.add(listed);
    let mut first = FakeSession::structured(reading("first"));
    first.events.insert(0, spent(0.361891));
    first.end.spend.cost_usd = 0.3745;
    worker.push(first);
    script(&worker, vec![reading("second")]);
    let (host, sink) = crew.host(RunMode::Captain)?;
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
    host.ask::<Read>(read_ask(RoleName::Reader, &reference, 0))
        .await?;
    let second = host
        .ask::<Read>(read_ask(RoleName::Reader, &reference, 1))
        .await
        .map(|_| ());
    for _ in 0..200 {
        let settled = sink.events().iter().any(|event| {
            matches!(
                &event.kind,
                RunEventKind::Run(RunLifecycle::StopRequested { .. } | RunLifecycle::Paused { .. })
            )
        });
        if settled {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let history = sink.events();
    let stopped = history.iter().any(|event| {
        matches!(
            &event.kind,
            RunEventKind::Run(RunLifecycle::StopRequested { by: Party::Rules })
        )
    });
    let paused = history.iter().any(|event| {
        matches!(
            &event.kind,
            RunEventKind::Run(RunLifecycle::PauseRequested { .. })
        )
    });
    let notes = history
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Note(note) => Some(note.text.clone()),
            _ => None,
        })
        .collect();
    let started = worker.specs().len();
    Ok(UnderTheLimit {
        crew,
        worker,
        reference,
        second,
        started,
        stopped,
        paused,
        notes,
        history,
    })
}

/// A session that would cross the run's limit never starts. On autopilot its step hears that
/// it reached its limit and the run ends; otherwise the run pauses. Either way the run says
/// what a session costs to open.
#[tokio::test]
async fn a_session_the_run_has_no_room_for_never_starts() -> anyhow::Result<()> {
    let ended = a_second_session_under_the_limit(true).await?;
    assert!(
        matches!(
            &ended.second,
            Err(EngineError::Step(StepError::Session(end)))
                if end.reason == EndReason::LimitHit { dimension: LimitDimension::Spend }
        ),
        "{:?}",
        ended.second
    );
    assert_eq!(ended.started, 1, "the second session started");
    assert!(ended.stopped && !ended.paused, "{:?}", ended.notes);
    let said = "The run can't start another session under its $0.60 spend limit: a session of \
                Haiku 5.5 on Claude Code costs about $0.36 to start here and $0.23 is left, so it \
                ends here with its work kept.";
    assert!(
        ended.notes.iter().any(|note| note.starts_with(said)),
        "{:?}",
        ended.notes
    );

    let paused = a_second_session_under_the_limit(false).await?;
    assert_eq!(paused.started, 1);
    assert!(paused.paused && !paused.stopped, "{:?}", paused.notes);
    assert!(
        paused
            .notes
            .iter()
            .any(|note| note.contains("so it pauses here with its work kept")),
        "{:?}",
        paused.notes
    );
    Ok(())
}

/// The run that paused for a session it had no room for carries that session on as its note
/// says: once you raise the limit and resume, the session's step starts it, afresh and with
/// its whole prompt, and takes its answer. It cost nothing and never reached a CLI session.
#[tokio::test]
async fn a_session_the_run_had_no_room_for_starts_once_the_limit_is_raised() -> anyhow::Result<()> {
    let paused = a_second_session_under_the_limit(false).await?;
    assert!(
        matches!(paused.second, Err(EngineError::Interrupted)),
        "the step unwinds as a pause has it: {:?}",
        paused.second
    );
    let state = RunState::fold(&paused.history);
    assert!(state.run.paused, "{:?}", names(&paused.history));
    let refused = &state.sessions.in_order()[1];
    assert_eq!(
        refused.end.as_ref().map(|end| end.reason.clone()),
        Some(EndReason::Paused)
    );
    assert!(refused.resumable() && refused.cli_session.is_none());

    let UnderTheLimit {
        mut crew,
        worker,
        reference,
        history,
        ..
    } = paused;
    crew.settings.limits.spend.spend_per_run = Some(1.00);
    let (host, _) = crew.host_over(history)?;
    host.record(
        None,
        None,
        RunEventKind::Run(RunLifecycle::Resumed(RunResumed {
            cause: ResumeCause::User,
            settings_changed: true,
            cli_versions_changed: Vec::new(),
            main_moved_by: None,
            sessions: Vec::new(),
        })),
    )?;
    let second = host
        .ask::<Read>(read_ask(RoleName::Reader, &reference, 1))
        .await?;
    assert_eq!(worker.specs().len(), 2, "the second session started");
    assert_eq!(second.output.summary, "second");
    let spec = &worker.specs()[1];
    assert!(spec.resume.is_none(), "{:?}", spec.resume);
    assert_eq!(
        host.read(|state| state.sessions.in_order().len()),
        2,
        "it carried on in its own record"
    );
    Ok(())
}

/// A no-tools session works in a folder of its own, where nothing tells the sandbox which
/// checkout is yours, so the engine closes your checkout to it. A session in one of the run's
/// worktrees keeps the checkout its worktree's git needs.
#[tokio::test]
async fn a_session_in_a_folder_of_its_own_is_kept_out_of_your_checkout() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let worker = crew.worker("claude-code/haiku", "anthropic");
    let reference: ModelRef = "claude-code/haiku".parse()?;
    script(&worker, vec![reading("on its own"), reading("in the base")]);
    let (host, _) = crew.host(RunMode::Captain)?;
    let mut on_its_own = read_ask(RoleName::Reader, &reference, 0);
    on_its_own.access = Access::NoTools;
    host.ask::<Read>(on_its_own).await?;
    host.ask::<Read>(read_ask(RoleName::Reader, &reference, 1))
        .await?;

    let checkout = host.repo().root.clone();
    let specs = worker.specs();
    let [no_tools, in_base] = specs.as_slice() else {
        panic!("{} sessions started", specs.len());
    };
    assert!(
        !no_tools.workdir.starts_with(&checkout),
        "{}",
        no_tools.workdir.display()
    );
    assert!(
        no_tools.sandbox.deny_read.contains(&checkout),
        "{:?}",
        no_tools.sandbox.deny_read
    );
    assert_eq!(in_base.workdir, host.repo().base_worktree);
    assert!(
        !in_base.sandbox.deny_read.contains(&checkout),
        "{:?}",
        in_base.sandbox.deny_read
    );
    Ok(())
}

/// A worker's read of a path on the never-list that failed leaves the note saying the list kept
/// it out only where the run's mode lets its sessions out of the project: supervised does, once
/// asked, and with `[outside] ask` emptied it keeps them in, where the list holds nothing.
#[tokio::test]
async fn a_refusal_is_put_down_to_the_never_list_only_where_it_holds() -> anyhow::Result<()> {
    use argonauts_core::settings::approvals::SandboxChoice;
    // A container shows the session nothing outside its own paths, so the list holds nothing
    // there either.
    for (ask, sandbox, said) in [
        (vec![ApprovalMode::Supervised], SandboxChoice::Os, true),
        (Vec::new(), SandboxChoice::Os, false),
        (
            vec![ApprovalMode::Supervised],
            SandboxChoice::Container,
            false,
        ),
    ] {
        let mut crew = Crew::new()?;
        crew.settings.approvals.understand.mode = ApprovalMode::Supervised;
        crew.settings.outside.ask = ask;
        crew.settings.sandbox.mode = sandbox;
        let worker = crew.worker("opencode/glm", "zai");
        let reference: ModelRef = "opencode/glm".parse()?;
        let env = crew.repo.root().join(".env");
        worker.push(
            FakeSession::completed("Couldn't read it.")
                .with_event(RunnerEvent::Agent(AgentEvent::ToolStarted {
                    id: "r1".into(),
                    name: "read".into(),
                    input: json!({ "filePath": env }),
                    parent: None,
                }))
                .with_event(RunnerEvent::Agent(AgentEvent::ToolFinished {
                    id: "r1".into(),
                    ok: false,
                    output: Some("Unknown: FileSystem.stat".into()),
                    parent: None,
                })),
        );
        let (host, sink) = crew.host(RunMode::Captain)?;
        host.start(plan(&reference, Access::ReadOnly, &host))
            .await?
            .ended()
            .await?;

        let spec = worker.specs().remove(0);
        let reach = argonauts_conductor::sandbox::profile::outside_reach(&spec.sandbox);
        assert_eq!(reach.is_open(), said);
        let notes: Vec<String> = sink
            .events()
            .iter()
            .filter_map(|event| match &event.kind {
                RunEventKind::Note(note) => Some(note.text.clone()),
                _ => None,
            })
            .collect();
        let kept_out = notes.iter().any(|note| {
            note.starts_with("kept out: ") && note.ends_with("/.env is on the never-list")
        });
        assert_eq!(kept_out, said, "{notes:?}");
    }
    Ok(())
}

/// A run's sessions can't ask, so where the run's mode opens outside only once asked, as
/// supervised and auto preview do, they read there and write nothing: no question would come
/// before such a write. A run set to supervised, as your words or a checked run set it, is held
/// the same way whatever the settings' own mode.
#[tokio::test]
async fn a_run_that_waits_for_you_reads_outside_and_writes_nothing_there() -> anyhow::Result<()> {
    for (settings_mode, run_mode) in [
        (ApprovalMode::Supervised, None),
        (ApprovalMode::AutoPreview, None),
        (ApprovalMode::Autopilot, Some(ApprovalMode::Supervised)),
    ] {
        let mut crew = Crew::new()?;
        crew.settings.approvals.understand.mode = settings_mode;
        let worker = crew.worker("opencode/glm", "zai");
        let reference: ModelRef = "opencode/glm".parse()?;
        worker.push(FakeSession::completed("Read it."));
        let (host, _sink) = crew.host(RunMode::Captain)?;
        if let Some(mode) = run_mode {
            commands::approval_mode(&host, mode)?;
        }
        host.start(plan(&reference, Access::ReadOnly, &host))
            .await?
            .ended()
            .await?;
        let outside = worker.specs().remove(0).sandbox.outside;
        let case = format!("{settings_mode:?} {run_mode:?}: {outside:?}");
        assert!(outside.read, "{case}");
        assert!(!outside.write && !outside.after_asking, "{case}");
        assert!(!outside.never.is_empty(), "{case}");
    }
    let mut crew = Crew::new()?;
    crew.settings.approvals.understand.mode = ApprovalMode::Autopilot;
    let worker = crew.worker("opencode/glm", "zai");
    let reference: ModelRef = "opencode/glm".parse()?;
    worker.push(FakeSession::completed("Read it."));
    let (host, _sink) = crew.host(RunMode::Captain)?;
    host.start(plan(&reference, Access::ReadOnly, &host))
        .await?
        .ended()
        .await?;
    let outside = worker.specs().remove(0).sandbox.outside;
    assert!(outside.read && outside.write, "{outside:?}");
    Ok(())
}
