use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::api::{ApiError, ConductorApi, RunState, StartRun};
use argonauts_core::control::{ClientKind, ControlRequest};
use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::{
    ParkReason, Party, PhaseChange, ResumeCause, RunLifecycle, RunMode, RunOutcome, RunPhase,
    RunResumed,
};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::journal::RunLock;
use argonauts_core::layout::run_files as names;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::{PhaseStatus, RunId};
use time::OffsetDateTime;

use super::approvals::approvals_refusal;
use super::test_support::{
    FakeSupervisor, Script, TestHome, event, event_at, queued, run_started, serve, write_journal,
};
use super::*;
use crate::run_files;

fn request(text: &str) -> StartRun {
    StartRun {
        request: text.to_owned(),
        mode: RunMode::Solo,
        forced: false,
        estimate: None,
        triage: None,
        chat_turn: None,
        continues: None,
        overrides: Vec::new(),
        include_uncommitted: false,
        default_agent: None,
        approval_mode: None,
        front_end: Some("hermes".to_owned()),
        instructions: Vec::new(),
        request_settings: Vec::new(),
    }
}

fn app_client(home: &TestHome, launcher: Arc<FakeSupervisor>) -> Client {
    Client::new(home.env(true), launcher, true)
}

fn parked(seq: u64, at: OffsetDateTime) -> argonauts_core::events::RunEvent {
    event_at(
        seq,
        at,
        RunEventKind::Run(RunLifecycle::Parked {
            reason: ParkReason::Paused,
            waiting_on: Vec::new(),
        }),
    )
}

fn finished(seq: u64, outcome: RunOutcome) -> argonauts_core::events::RunEvent {
    event(
        seq,
        RunEventKind::Run(RunLifecycle::Finished {
            outcome,
            detail: None,
        }),
    )
}

fn resumed(seq: u64) -> argonauts_core::events::RunEvent {
    event(
        seq,
        RunEventKind::Run(RunLifecycle::Resumed(RunResumed {
            cause: ResumeCause::User,
            settings_changed: false,
            harness_versions_changed: Vec::new(),
            main_moved_by: None,
            sessions: Vec::new(),
        })),
    )
}

fn asked(script: &Script, request: &ControlRequest) -> bool {
    script.requests.lock().unwrap().contains(request)
}

fn other_run(suffix: &str) -> RunId {
    RunId::new(format!("run-20260930-101500-{suffix}")).unwrap()
}

#[test]
fn run_ids_look_like_a_timestamp_and_a_suffix() {
    let id = RunId::generate();
    let text = id.as_str();
    let parts: Vec<&str> = text.split('-').collect();
    assert_eq!(parts.len(), 4, "{text}");
    assert_eq!(parts[0], "run");
    assert!(parts[1].len() == 8 && parts[1].chars().all(|c| c.is_ascii_digit()));
    assert!(parts[2].len() == 6 && parts[2].chars().all(|c| c.is_ascii_digit()));
    assert!(parts[3].len() == 4 && parts[3].chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn start_refuses_auto() {
    let home = TestHome::new();
    let launcher = Arc::new(FakeSupervisor::default());
    let client = app_client(&home, Arc::clone(&launcher));
    for mode in [ApprovalMode::Auto, ApprovalMode::Shadow] {
        let refused = client
            .start(StartRun {
                approval_mode: Some(mode),
                ..request("ship it")
            })
            .await;
        assert!(matches!(refused, Err(ApiError::Refused(_))), "{refused:?}");
    }
    assert!(launcher.launches().is_empty());
    assert!(!client.env.runs_dir().exists());
}

#[tokio::test]
async fn start_writes_its_files_and_waits_for_welcome() {
    let home = TestHome::new();
    let launcher = Arc::new(FakeSupervisor::default());
    let client = app_client(&home, Arc::clone(&launcher));
    let handle = client
        .start(StartRun {
            overrides: vec!["runs.remind_parked_days=3".to_owned()],
            approval_mode: Some(ApprovalMode::Supervised),
            ..request("Add --json to export")
        })
        .await
        .unwrap();
    let run_dir = client.env.run_dir(&handle.run);
    assert_eq!(handle.events, run_dir.join(names::EVENTS));
    assert_eq!(launcher.launches(), vec![handle.run.clone()]);

    let start = run_files::read_start(&run_dir).unwrap();
    assert_eq!(start.run, handle.run);
    assert_eq!(start.overrides, ["runs.remind_parked_days=3"]);
    assert!(start.interactive);
    assert_eq!(start.request.front_end.as_deref(), Some("hermes"));
    assert_eq!(start.repo_root, client.env.repo_root);

    let settings = run_files::read_settings(&run_dir).unwrap();
    assert_eq!(settings.settings.runs.remind_parked_days, 3);
    assert_eq!(settings.sha256.len(), 64);

    let token = run_files::read_token(&run_dir).unwrap();
    assert_eq!(token.len(), 64);
    let mode = std::fs::metadata(run_dir.join(names::APPROVE_TOKEN))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test]
async fn start_gives_up_when_no_supervisor_answers() {
    struct Silent;
    impl Launcher for Silent {
        fn launch(&self, _: &Path, _: &RunId, _: &Path) -> anyhow::Result<()> {
            Ok(())
        }
    }
    let home = TestHome::new();
    let client = Client::new(home.env(true), Arc::new(Silent), true);
    let run = other_run("0001");
    let run_dir = home.run_with(&client.env, &run, &[]);
    let waited = start::await_welcome(&client, &run_dir, Duration::from_millis(200)).await;
    let Err(ApiError::Supervisor(message)) = waited else {
        panic!("expected a timeout, got {waited:?}");
    };
    assert!(message.contains("supervisor.log"), "{message}");
}

#[tokio::test]
async fn list_tells_crashed_starting_and_parked_runs_apart() {
    let home = TestHome::new();
    let client = app_client(&home, Arc::new(FakeSupervisor::default()));
    let env = Arc::clone(&client.env);
    let now = OffsetDateTime::now_utc();

    let crashed = other_run("00c1");
    home.run_with(
        &env,
        &crashed,
        &[
            run_started(0, "crashed run", None),
            event(
                1,
                RunEventKind::Phase(PhaseChange {
                    phase: RunPhase::Build,
                    status: PhaseStatus::Started,
                    summary: None,
                }),
            ),
        ],
    );

    let starting = other_run("00d1");
    let starting_dir = env.run_dir(&starting);
    std::fs::create_dir_all(&starting_dir).unwrap();
    run_files::write_start(
        &starting_dir,
        &run_files::StartRecord {
            run: starting.clone(),
            request: request("Starting now\nwith details"),
            overrides: Vec::new(),
            interactive: true,
            replay: false,
            repo_root: env.repo_root.clone(),
            created: now,
        },
    )
    .unwrap();

    let old = other_run("00e1");
    home.run_with(
        &env,
        &old,
        &[
            run_started(0, "parked long ago", None),
            parked(1, now - time::Duration::days(8)),
        ],
    );
    let recent = other_run("00f1");
    home.run_with(
        &env,
        &recent,
        &[
            run_started(0, "parked today", None),
            parked(1, now - time::Duration::hours(2)),
        ],
    );
    let merged = other_run("0101");
    home.run_with(
        &env,
        &merged,
        &[
            run_started(0, "merged", None),
            finished(1, RunOutcome::Merged),
        ],
    );
    let live = other_run("0111");
    let live_dir = home.run_with(&env, &live, &[run_started(0, "running", None)]);
    let _lock = RunLock::try_acquire(&live_dir).unwrap().unwrap();

    let runs = client.list().await.unwrap();
    let by_id = |run: &RunId| runs.iter().find(|summary| &summary.run == run).unwrap();

    let summary = by_id(&crashed);
    assert_eq!(summary.state, RunState::Crashed);
    assert_eq!(summary.title, "crashed run");
    assert!(!summary.live);

    let summary = by_id(&starting);
    assert_eq!(summary.state, RunState::Running);
    assert_eq!(summary.stopped_at, "starting");
    assert_eq!(summary.title, "Starting now");

    let summary = by_id(&old);
    assert_eq!((summary.state, summary.remind), (RunState::Parked, true));
    assert_eq!(summary.stopped_at, "paused");
    let summary = by_id(&recent);
    assert_eq!((summary.state, summary.remind), (RunState::Parked, false));

    assert_eq!(by_id(&merged).state, RunState::Merged);
    let summary = by_id(&live);
    assert_eq!((summary.state, summary.live), (RunState::Running, true));

    assert!(
        runs.windows(2)
            .all(|pair| pair[0].updated >= pair[1].updated),
        "newest first"
    );
}

#[tokio::test]
async fn parked_guidance_lands_in_pending_answers_and_launches() {
    let home = TestHome::new();
    let launcher = Arc::new(FakeSupervisor::default());
    let client = app_client(&home, Arc::clone(&launcher));
    let run = other_run("0201");
    let run_dir = home.run_with(
        &client.env,
        &run,
        &[
            run_started(0, "parked", None),
            parked(1, OffsetDateTime::now_utc()),
        ],
    );
    let sent = client.guidance(&run, "use the v2 API".to_owned()).await;
    let Err(ApiError::NotNow(message)) = sent else {
        panic!("expected the message to wait for the resume, got {sent:?}");
    };
    assert!(message.contains("resumes"), "{message}");
    assert_eq!(
        run_files::take_pending(&run_dir).unwrap(),
        vec![ControlRequest::Guidance {
            text: "use the v2 API".to_owned(),
            front_end: None,
        }]
    );
    assert_eq!(launcher.launches(), vec![run.clone()]);

    // Now a supervisor answers, so the next call goes over the socket.
    let id = client
        .guidance(&run, "and add tests".to_owned())
        .await
        .unwrap();
    assert_eq!(id.as_str(), "m-001");
    assert_eq!(launcher.launches().len(), 1);
    assert_eq!(
        launcher.script.requests.lock().unwrap().clone(),
        vec![ControlRequest::Guidance {
            text: "and add tests".to_owned(),
            front_end: None,
        }]
    );
}

#[tokio::test]
async fn resume_refuses_a_merged_run_and_resumes_a_stopped_one() {
    let home = TestHome::new();
    let launcher = Arc::new(FakeSupervisor::default());
    let client = app_client(&home, Arc::clone(&launcher));
    let merged = other_run("0301");
    home.run_with(
        &client.env,
        &merged,
        &[
            run_started(0, "merged", None),
            finished(1, RunOutcome::Merged),
        ],
    );
    let stopped = other_run("0302");
    let stopped_dir = home.run_with(
        &client.env,
        &stopped,
        &[
            run_started(0, "stopped", None),
            finished(1, RunOutcome::Stopped),
        ],
    );
    assert_eq!(
        client.resume(&merged).await,
        Err(ApiError::Merged(merged.clone()))
    );
    assert_eq!(
        client.stop(&merged).await,
        Err(ApiError::Merged(merged.clone()))
    );
    // The new supervisor's engine reopens the stopped run and records `Resumed`.
    let engine = {
        let launcher = Arc::clone(&launcher);
        let events = stopped_dir.join(names::EVENTS);
        tokio::spawn(async move {
            while launcher.launches().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            write_journal(&events, &[resumed(2)]);
        })
    };
    client.resume(&stopped).await.unwrap();
    engine.await.unwrap();
    assert_eq!(launcher.launches(), vec![stopped.clone()]);
    assert_eq!(
        run_files::take_pending(&stopped_dir).unwrap(),
        [ControlRequest::CancelPause],
        "the next supervisor is asked to reopen it"
    );
    let missing = other_run("0303");
    assert_eq!(client.resume(&missing).await, Err(ApiError::NoRun(missing)));
}

#[tokio::test]
async fn resume_wakes_a_supervisor_waiting_out_its_park_grace() {
    let home = TestHome::new();
    let launcher = Arc::new(FakeSupervisor::default());
    let client = app_client(&home, Arc::clone(&launcher));
    let run = other_run("0304");
    let run_dir = home.run_with(
        &client.env,
        &run,
        &[
            run_started(0, "paused", None),
            parked(1, OffsetDateTime::now_utc()),
        ],
    );
    let _lock = RunLock::try_acquire(&run_dir).unwrap().unwrap();
    let script = Arc::new(Script::default());
    let _server = serve(&run_dir, run.clone(), Arc::clone(&script));
    // The woken engine rebuilds the run from the log and records `Resumed`.
    let engine = {
        let script = Arc::clone(&script);
        let events = run_dir.join(names::EVENTS);
        tokio::spawn(async move {
            while !asked(&script, &ControlRequest::CancelPause) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            write_journal(&events, &[resumed(2)]);
        })
    };
    client.resume(&run).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), engine)
        .await
        .expect("resume never woke the engine")
        .unwrap();
    assert!(
        launcher.launches().is_empty(),
        "the live supervisor took it"
    );

    // Now it runs, so resuming again asks for nothing.
    client.resume(&run).await.unwrap();
    assert_eq!(
        script.requests.lock().unwrap().clone(),
        vec![ControlRequest::CancelPause]
    );
}

#[tokio::test]
async fn take_back_asks_a_started_supervisor_and_never_defers() {
    let home = TestHome::new();
    let launcher = Arc::new(FakeSupervisor::default());
    let client = app_client(&home, Arc::clone(&launcher));
    let run = other_run("0204");
    let waiting = queued(1, "use the v2 API", Party::User);
    let RunEventKind::Session(SessionEvent::Queued(message)) = waiting.kind.clone() else {
        panic!("{:?}", waiting.kind);
    };
    let run_dir = home.run_with(
        &client.env,
        &run,
        &[
            run_started(0, "parked", None),
            waiting,
            parked(2, OffsetDateTime::now_utc()),
        ],
    );
    *launcher.script.queued.lock().unwrap() = vec![message.clone()];
    let back = client
        .take_back(&run, vec![message.id.clone()])
        .await
        .unwrap();
    assert_eq!(back, vec![message.clone()]);
    assert_eq!(launcher.launches(), vec![run.clone()]);
    assert!(!run_dir.join(names::PENDING_ANSWERS).exists());

    let stopped = other_run("0205");
    home.run_with(
        &client.env,
        &stopped,
        &[
            run_started(0, "stopped", None),
            finished(1, RunOutcome::Stopped),
        ],
    );
    let refused = client.take_back(&stopped, vec![message.id]).await;
    assert!(matches!(refused, Err(ApiError::NotNow(_))), "{refused:?}");
    assert_eq!(
        launcher.launches(),
        vec![run],
        "a stopped run stays stopped"
    );
    assert!(
        super::control::Reply::Deferred.taken_back().is_err(),
        "a deferred take-back would read as every message delivered"
    );
}

#[tokio::test]
async fn revert_asks_the_supervisor_and_starts_one_when_none_runs() {
    let home = TestHome::new();
    let launcher = Arc::new(FakeSupervisor::default());
    let client = app_client(&home, Arc::clone(&launcher));
    let card = argonauts_core::CardId::numbered(2);
    let revert = ControlRequest::Revert {
        card: card.clone(),
        checkpoint: 1,
    };
    let run = other_run("0601");
    let run_dir = home.run_with(
        &client.env,
        &run,
        &[
            run_started(0, "parked", None),
            parked(1, OffsetDateTime::now_utc()),
        ],
    );
    client.revert(&run, &card, 1).await.unwrap();
    assert_eq!(launcher.launches(), vec![run.clone()]);
    assert!(asked(&launcher.script, &revert));
    assert!(
        !run_dir.join(names::PENDING_ANSWERS).exists(),
        "a revert needs the engine's answer, so it never waits on disk"
    );

    let merged = other_run("0602");
    home.run_with(
        &client.env,
        &merged,
        &[
            run_started(0, "merged", None),
            finished(1, RunOutcome::Merged),
        ],
    );
    let refused = client.revert(&merged, &card, 1).await;
    assert!(matches!(refused, Err(ApiError::Merged(_))), "{refused:?}");
    assert_eq!(launcher.launches(), vec![run]);
}

#[tokio::test]
async fn a_spot_check_goes_to_the_live_engine_or_waits_for_the_next() {
    let home = TestHome::new();
    let launcher = Arc::new(FakeSupervisor::default());
    let client = app_client(&home, Arc::clone(&launcher));
    let id = argonauts_core::names::CheckpointId::numbered(4);
    let check = ControlRequest::SpotCheck {
        id: id.clone(),
        correct: false,
    };

    let live = other_run("0603");
    let live_dir = home.run_with(&client.env, &live, &[run_started(0, "live", None)]);
    let script = Arc::new(Script::default());
    let _server = serve(&live_dir, live.clone(), Arc::clone(&script));
    client.spot_check(&live, &id, false).await.unwrap();
    assert!(asked(&script, &check));

    let run = other_run("0604");
    let run_dir = home.run_with(
        &client.env,
        &run,
        &[
            run_started(0, "parked", None),
            parked(1, OffsetDateTime::now_utc()),
        ],
    );
    client.spot_check(&run, &id, false).await.unwrap();
    assert_eq!(run_files::take_pending(&run_dir).unwrap(), vec![check]);
    assert_eq!(launcher.launches(), vec![run]);
}

#[tokio::test]
async fn a_finished_runs_spot_check_goes_to_the_repos_policy() {
    use argonauts_core::events::checkpoint::CheckpointEvent;

    let home = TestHome::new();
    let launcher = Arc::new(FakeSupervisor::default());
    let client = app_client(&home, Arc::clone(&launcher));
    let id = argonauts_core::names::CheckpointId::numbered(4);
    let run = other_run("0605");
    home.run_with(
        &client.env,
        &run,
        &[
            run_started(0, "merged", None),
            super::test_support::plan_checkpoint(1, &id, 1),
            event(
                2,
                RunEventKind::Checkpoint(CheckpointEvent::SpotCheckQueued { id: id.clone() }),
            ),
            finished(3, RunOutcome::Merged),
        ],
    );
    client.spot_check(&run, &id, false).await.unwrap();
    assert!(
        launcher.launches().is_empty(),
        "a finished run starts nothing"
    );
    let records = client
        .env
        .state_dir
        .join(argonauts_core::layout::repo_state::APPROVALS);
    let records = std::fs::read_to_string(&records).unwrap();
    assert!(
        records.contains(run.as_str()) && records.contains(id.as_str()),
        "{records}"
    );

    let unqueued = argonauts_core::names::CheckpointId::numbered(1);
    let refused = client.spot_check(&run, &unqueued, true).await;
    assert!(matches!(refused, Err(ApiError::Refused(_))), "{refused:?}");

    let unattended = Client::new(home.env(false), launcher, true);
    let refused = unattended.spot_check(&run, &id, true).await;
    assert!(matches!(refused, Err(ApiError::Refused(_))), "{refused:?}");
}

#[tokio::test]
async fn continuing_needs_a_merged_run() {
    let home = TestHome::new();
    let launcher = Arc::new(FakeSupervisor::default());
    let client = app_client(&home, Arc::clone(&launcher));
    let merged = other_run("0401");
    home.run_with(
        &client.env,
        &merged,
        &[
            run_started(0, "merged", None),
            finished(1, RunOutcome::Merged),
        ],
    );
    let stopped = other_run("0402");
    home.run_with(
        &client.env,
        &stopped,
        &[
            run_started(0, "stopped", None),
            finished(1, RunOutcome::Stopped),
        ],
    );
    let refused = client.continue_run(&stopped, request("more")).await;
    assert!(matches!(refused, Err(ApiError::Refused(_))), "{refused:?}");
    let handle = client.continue_run(&merged, request("more")).await.unwrap();
    let start = run_files::read_start(&client.env.run_dir(&handle.run)).unwrap();
    assert_eq!(start.request.continues, Some(merged));
}

#[tokio::test]
async fn the_conductor_holds_no_approvals_and_resolves_without_a_person() {
    let home = TestHome::new();
    let launcher = Arc::new(FakeSupervisor::default());
    let client = conductor_client(home.paths.clone(), &home.repo, launcher).unwrap();
    assert!(!client.interactive);
    assert_eq!(client.kind, ClientKind::Cli);
    assert!(approvals_refusal(&client.env).is_some());

    let refused = client
        .start(StartRun {
            overrides: vec!["approvals.mode=\"auto\"".to_owned()],
            ..request("loosen it")
        })
        .await;
    let Err(ApiError::Refused(message)) = refused else {
        panic!("expected a refusal, got {refused:?}");
    };
    assert!(message.contains("safety"), "{message}");

    let handle = client.start(request("fine")).await.unwrap();
    let start = run_files::read_start(&client.env.run_dir(&handle.run)).unwrap();
    assert!(!start.interactive);

    let (_, approvals) = for_app(home.env(false)).unwrap();
    assert!(approvals.is_none(), "plain mode holds no approvals");
}

#[test]
fn presence_required_refused_until_passkey_exists() {
    let home = TestHome::new();
    home.write_global_config("[approvals]\npresence = \"required\"\n");
    let refusal = approvals_refusal(&home.env(true)).unwrap();
    assert!(refusal.contains("passkey"), "{refusal}");
    let mut in_agent = (*home.env(true)).clone();
    in_agent.agent_session = true;
    assert!(
        approvals_refusal(&in_agent)
            .unwrap()
            .contains("agent session")
    );
}

#[tokio::test]
async fn status_comes_from_the_supervisor_or_else_the_journal() {
    let home = TestHome::new();
    let launcher = Arc::new(FakeSupervisor::default());
    let client = app_client(&home, Arc::clone(&launcher));
    let run = other_run("0501");
    let run_dir = home.run_with(
        &client.env,
        &run,
        &[
            run_started(0, "parked", None),
            parked(1, OffsetDateTime::now_utc()),
        ],
    );
    let offline = client.status(&run).await.unwrap();
    assert!(offline.run.is_some());
    assert_ne!(offline.recent, ["from the supervisor"]);
    assert!(
        launcher.launches().is_empty(),
        "reading status starts nothing"
    );

    let _server = serve(&run_dir, run.clone(), Arc::new(Script::default()));
    let live = client.status(&run).await.unwrap();
    assert_eq!(live.recent, ["from the supervisor"]);
    let steer = client
        .steer(
            &other_run("0502"),
            &argonauts_core::names::SessionKey::numbered(1),
            "x".to_owned(),
        )
        .await;
    assert!(matches!(steer, Err(ApiError::NoRun(_))));
}

async fn collect(stream: &mut argonauts_core::api::EventStream) -> Vec<u64> {
    let mut seqs = Vec::new();
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_secs(5), stream.events.recv()).await
    {
        seqs.push(event.seq);
    }
    seqs
}

fn numbered(range: std::ops::RangeInclusive<u64>) -> Vec<argonauts_core::events::RunEvent> {
    range
        .map(|seq| event(seq, RunEventKind::Run(RunLifecycle::PauseCancelled)))
        .collect()
}

#[tokio::test]
async fn a_stream_dedupes_across_the_socket_to_tail_handover() {
    let home = TestHome::new();
    let client = app_client(&home, Arc::new(FakeSupervisor::default()));
    let run = other_run("0601");
    let mut journal = numbered(0..=4);
    journal.push(finished(5, RunOutcome::Merged));
    let run_dir = home.run_with(&client.env, &run, &journal);

    let script = Arc::new(Script::default());
    *script.events.lock().unwrap() = numbered(0..=3);
    let server = serve(&run_dir, run.clone(), Arc::clone(&script));

    let mut stream = client.subscribe(&run, 0).await.unwrap();
    assert_eq!(stream.replay_until, Some(5));
    assert!(*stream.live.borrow());
    let first: Vec<u64> = {
        let mut seqs = Vec::new();
        for _ in 0..4 {
            seqs.push(stream.events.recv().await.unwrap().seq);
        }
        seqs
    };
    assert_eq!(first, [0, 1, 2, 3]);
    server.stop();
    let rest = collect(&mut stream).await;
    assert_eq!(rest, [4, 5], "the journal takes over without repeating");
    assert!(!*stream.live.borrow());
}

#[tokio::test]
async fn a_stream_dedupes_when_a_supervisor_comes_back() {
    let home = TestHome::new();
    let client = app_client(&home, Arc::new(FakeSupervisor::default()));
    let run = other_run("0701");
    let run_dir = home.run_with(&client.env, &run, &numbered(0..=2));

    let mut stream = client.subscribe(&run, 1).await.unwrap();
    assert_eq!(stream.replay_until, Some(2));
    assert!(!*stream.live.borrow());
    assert_eq!(stream.events.recv().await.unwrap().seq, 1);
    assert_eq!(stream.events.recv().await.unwrap().seq, 2);

    // A supervisor comes back and resends everything from the start.
    let _lock = RunLock::try_acquire(&run_dir).unwrap().unwrap();
    let script = Arc::new(Script::default());
    let mut resent = numbered(0..=3);
    resent.push(finished(4, RunOutcome::Stopped));
    *script.events.lock().unwrap() = resent;
    let _server = serve(&run_dir, run.clone(), script);
    // The journal ends at 2, so these can only come over the socket.
    assert_eq!(collect(&mut stream).await, [3, 4]);
    assert!(!*stream.live.borrow());
}

#[tokio::test]
async fn a_stream_passes_the_last_events_on_before_it_turns_not_live() {
    let home = TestHome::new();
    let client = app_client(&home, Arc::new(FakeSupervisor::default()));
    let run = other_run("0901");
    let mut journal = numbered(0..=3);
    journal.push(parked(4, OffsetDateTime::now_utc()));
    let run_dir = home.run_with(&client.env, &run, &journal);

    // The socket closes before the supervisor's `Parked` goes out on it.
    let script = Arc::new(Script::default());
    *script.events.lock().unwrap() = numbered(0..=3);
    let server = serve(&run_dir, run.clone(), Arc::clone(&script));
    let mut stream = client.subscribe(&run, 0).await.unwrap();
    for seq in 0..=3 {
        assert_eq!(stream.events.recv().await.unwrap().seq, seq);
    }
    server.stop();
    tokio::time::timeout(Duration::from_secs(5), stream.live.wait_for(|live| !live))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stream.events.try_recv().unwrap().seq,
        4,
        "`Parked` came before the exit"
    );
}

#[tokio::test]
async fn a_stream_stays_live_while_the_supervisor_holds_the_run() {
    let home = TestHome::new();
    let client = app_client(&home, Arc::new(FakeSupervisor::default()));
    let run = other_run("0902");
    let run_dir = home.run_with(&client.env, &run, &numbered(0..=1));
    let lock = RunLock::try_acquire(&run_dir).unwrap().unwrap();
    let script = Arc::new(Script::default());
    *script.events.lock().unwrap() = numbered(0..=1);
    let server = serve(&run_dir, run.clone(), Arc::clone(&script));
    let mut stream = client.subscribe(&run, 0).await.unwrap();
    for seq in 0..=1 {
        assert_eq!(stream.events.recv().await.unwrap().seq, seq);
    }
    server.stop();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(*stream.live.borrow(), "a dropped socket isn't an exit");
    drop(lock);
    tokio::time::timeout(Duration::from_secs(5), stream.live.wait_for(|live| !live))
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn a_run_crashed_only_when_nothing_holds_it_and_its_log_is_unsettled() {
    let home = TestHome::new();
    let env = home.env(true);
    let run_dir = home.run_with(&env, &other_run("1001"), &numbered(0..=1));
    assert!(supervisor_crashed(&run_dir));
    let lock = RunLock::try_acquire(&run_dir).unwrap().unwrap();
    assert!(!supervisor_crashed(&run_dir), "a supervisor still holds it");
    drop(lock);
    write_journal(
        &run_dir.join(names::EVENTS),
        &[parked(2, OffsetDateTime::now_utc())],
    );
    assert!(!supervisor_crashed(&run_dir), "it parked before leaving");
}

#[tokio::test]
async fn a_reattach_without_a_supervisor_starts_at_the_latest_snapshot() {
    use argonauts_core::events::run::RunSnapshot;
    let home = TestHome::new();
    let client = app_client(&home, Arc::new(FakeSupervisor::default()));
    let run = other_run("0801");
    let mut journal = numbered(0..=2);
    journal.push(event(
        3,
        RunEventKind::Run(RunLifecycle::Snapshot(Box::new(RunSnapshot {
            upto: 2,
            engine: serde_json::json!({}),
            summary: Default::default(),
            open_checkpoints: Vec::new(),
            running: Vec::new(),
        }))),
    ));
    journal.push(finished(4, RunOutcome::Merged));
    home.run_with(&client.env, &run, &journal);
    let mut stream = client.reattach(&run).await.unwrap();
    assert_eq!(collect(&mut stream).await, [3, 4]);
}
