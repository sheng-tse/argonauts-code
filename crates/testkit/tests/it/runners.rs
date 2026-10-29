use std::path::Path;
use std::time::Duration;

use argonauts_core::events::run::Party;
use argonauts_core::events::session::{
    Delivery, EndReason, InputPart, InputPartKind, QueuedMessage, SessionEvent,
};
use argonauts_core::input::MentionTarget;
use argonauts_core::names::MessageId;
use argonauts_core::session_runner::{
    Access, RunnerDirectory, RunnerError, RunnerEvent, SessionControl, SessionHandle, SessionRunner,
};
use argonauts_core::steps::intake::{Estimate, ReplyKind};
use argonauts_core::steps::{StepRequest, StepRunner, decode, output_schema};
use argonauts_testkit::runner::{drain, session_spec};
use argonauts_testkit::{FakeDirectory, FakeRunner, FakeServices, FakeSession, Hold, deadline};
use serde_json::json;

fn prompt(text: &str) -> Vec<InputPart> {
    vec![InputPart {
        kind: InputPartKind::Prompt,
        label: "prompt".into(),
        text: text.into(),
        approx_tokens: 1,
        source: None,
    }]
}

#[tokio::test]
async fn sessions_record_their_input_first_and_play_the_script() {
    let worker = FakeRunner::cli("codex/gpt-x", "openai").then(FakeSession::completed("done"));
    let mut spec = session_spec("codex/gpt-x", Access::ReadOnly, Path::new("/repo"));
    spec.prompt = prompt("read the code");
    let handle = worker.start(spec.clone()).await.unwrap();
    let (events, end) = drain(handle).await;
    match &events[0] {
        RunnerEvent::Session(SessionEvent::Input(input)) => assert_eq!(input.parts, spec.prompt),
        other => panic!("{other:?}"),
    }
    assert_eq!(events.len(), 3);
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(worker.specs(), [spec]);
    assert!(
        worker
            .start(session_spec("codex", Access::ReadOnly, Path::new("/")))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn build_sessions_need_the_goal_hash_and_endpoints_need_no_tools() {
    let cli = FakeRunner::cli("claude-code/opus", "anthropic")
        .then(FakeSession::completed("ok"))
        .then(FakeSession::completed("ok"));
    let build = session_spec("claude-code/opus", Access::Build, Path::new("/wt"));
    assert!(matches!(
        cli.start(build.clone()).await,
        Err(RunnerError::Refused(_))
    ));
    let mut with_goal = build;
    with_goal.goal_sha256 = Some("abc".into());
    assert!(cli.start(with_goal).await.is_ok());

    let endpoint =
        FakeRunner::endpoint("openrouter/m-a", "zhipu").then(FakeSession::completed("ok"));
    let read_only = session_spec("openrouter/m-a", Access::ReadOnly, Path::new("/"));
    assert!(matches!(
        endpoint.start(read_only).await,
        Err(RunnerError::Refused(_))
    ));
}

#[tokio::test]
async fn stop_ends_the_session_as_stopped_and_is_recorded() {
    let worker = FakeRunner::cli("agy", "google").then(
        FakeSession::completed("slow")
            .with_event(RunnerEvent::Agent(argonauts_core::AgentEvent::Reasoning {
                text: "thinking".into(),
            }))
            .with_delay(Duration::from_millis(50)),
    );
    let handle = worker
        .start(session_spec("agy", Access::ReadOnly, Path::new("/")))
        .await
        .unwrap();
    handle.control.send(SessionControl::Stop).await.unwrap();
    let (_, end) = drain(handle).await;
    assert_eq!(end.reason, EndReason::Stopped);
    assert_eq!(worker.controls().len(), 1);
}

#[tokio::test]
async fn the_step_runner_works_over_a_fake_directory() {
    let services = FakeServices::new();
    let fake = services
        .workers
        .add(
            FakeRunner::cli("claude-code", "anthropic").then(FakeSession::structured(
                json!({"kind": "answer", "answer_markdown": "Hi!", "estimate": null}),
            )),
        );
    let worker = services
        .workers
        .get(&"claude-code".parse().unwrap())
        .unwrap();
    let mut spec = session_spec("claude-code", Access::ReadOnly, Path::new("/repo"));
    spec.json_schema = Some(output_schema::<Estimate>());
    spec.prompt = prompt("hi");
    let outcome = services
        .steps
        .run(StepRequest::new(worker, spec), Box::new(|_| {}))
        .await
        .unwrap();
    assert_eq!(
        decode::<Estimate>(&outcome).unwrap().kind,
        ReplyKind::Answer
    );
    assert_eq!(fake.specs().len(), 1);
    assert!(services.workers.get(&"codex".parse().unwrap()).is_err());
    assert_eq!(FakeDirectory::new().available().len(), 0);
}

fn message(text: &str) -> QueuedMessage {
    QueuedMessage {
        id: MessageId::numbered(4),
        from: Party::User,
        to: MentionTarget::All,
        text: text.into(),
        queued_at: time::OffsetDateTime::UNIX_EPOCH,
        delivery: Delivery::Steer,
    }
}

#[tokio::test]
async fn delivered_messages_are_recorded_and_until_stopped_waits_for_stop() {
    let worker =
        FakeRunner::cli("claude-code/opus", "anthropic").then(FakeSession::until_stopped(vec![]));
    let handle = worker
        .start(session_spec(
            "claude-code/opus",
            Access::ReadOnly,
            Path::new("/"),
        ))
        .await
        .unwrap();
    handle
        .control
        .send(SessionControl::Deliver(message("use EVAL")))
        .await
        .unwrap();
    handle
        .control
        .send(SessionControl::StopAfterTool)
        .await
        .unwrap();
    let (events, end) = drain(handle).await;
    assert_eq!(end.reason, EndReason::Stopped);
    assert!(matches!(
        &events[0],
        RunnerEvent::Session(SessionEvent::Input(_))
    ));
    assert!(matches!(
        &events[1],
        RunnerEvent::Session(SessionEvent::Delivered {
            via: Delivery::Steer,
            ..
        })
    ));
    match &events[2] {
        RunnerEvent::Session(SessionEvent::Input(input)) => {
            assert_eq!(input.parts.len(), 1, "only the new part");
            assert_eq!(input.parts[0].text, "use EVAL");
            assert!(
                input
                    .launch
                    .env_set
                    .contains(&"ARGONAUTS_SESSION".to_owned())
            );
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn dropping_the_control_sender_stops_the_session() {
    let worker = FakeRunner::cli("codex", "openai").then(FakeSession::until_stopped(vec![]));
    let SessionHandle {
        mut events,
        control,
        ended,
        ..
    } = worker
        .start(session_spec("codex", Access::ReadOnly, Path::new("/")))
        .await
        .unwrap();
    drop(control);
    while events.recv().await.is_some() {}
    assert_eq!(ended.await.unwrap().reason, EndReason::Stopped);
}

/// Polls `done` until it holds, failing the test with `what` after the deadline.
async fn until(what: &str, done: impl Fn() -> bool) {
    let give_up = deadline::after(Duration::from_secs(5));
    while !done() {
        assert!(std::time::Instant::now() < give_up, "{what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Nothing more comes out of the session for a moment.
async fn quiet(handle: &mut SessionHandle) {
    let next = tokio::time::timeout(Duration::from_millis(50), handle.events.recv()).await;
    assert!(next.is_err(), "the held session went on: {next:?}");
}

#[tokio::test]
async fn a_held_session_waits_for_its_release_and_records_messages_meanwhile() {
    let hold = Hold::new();
    let worker =
        FakeRunner::cli("agy", "google").then(FakeSession::completed("done").held_after(1, &hold));
    let mut handle = worker
        .start(session_spec("agy", Access::ReadOnly, Path::new("/")))
        .await
        .unwrap();
    assert!(matches!(
        handle.events.recv().await,
        Some(RunnerEvent::Session(SessionEvent::Input(_)))
    ));
    assert!(matches!(
        handle.events.recv().await,
        Some(RunnerEvent::Agent(
            argonauts_core::AgentEvent::TextDelta { .. }
        ))
    ));
    handle
        .control
        .send(SessionControl::Deliver(message("use EVAL")))
        .await
        .unwrap();
    until("the held session takes the message in", || {
        worker.controls().len() == 1
    })
    .await;
    quiet(&mut handle).await;
    hold.release();
    let (events, end) = drain(handle).await;
    assert!(matches!(
        &events[0],
        RunnerEvent::Session(SessionEvent::Delivered { .. })
    ));
    assert!(matches!(
        &events[1],
        RunnerEvent::Session(SessionEvent::Input(_))
    ));
    assert!(matches!(
        &events[2],
        RunnerEvent::Agent(argonauts_core::AgentEvent::Result { .. })
    ));
    assert_eq!(events.len(), 3, "{events:#?}");
    assert_eq!(end.reason, EndReason::Completed);
}

#[tokio::test]
async fn a_stop_taken_in_while_held_ends_the_session_once_released() {
    let hold = Hold::new();
    let worker =
        FakeRunner::cli("agy", "google").then(FakeSession::completed("done").held_after(0, &hold));
    let mut handle = worker
        .start(session_spec("agy", Access::ReadOnly, Path::new("/")))
        .await
        .unwrap();
    handle.control.send(SessionControl::Stop).await.unwrap();
    until("the held session takes the stop in", || {
        worker.controls().len() == 1
    })
    .await;
    assert!(
        handle.ended.try_recv().is_err(),
        "a held session doesn't end"
    );
    hold.release();
    let (events, end) = drain(handle).await;
    assert_eq!(events.len(), 1, "only the input record: {events:#?}");
    assert_eq!(end.reason, EndReason::Stopped);
}

#[tokio::test]
async fn a_hold_past_the_last_event_keeps_the_session_from_ending() {
    let hold = Hold::new();
    let worker =
        FakeRunner::cli("agy", "google").then(FakeSession::completed("done").held_after(2, &hold));
    let mut handle = worker
        .start(session_spec("agy", Access::ReadOnly, Path::new("/")))
        .await
        .unwrap();
    for _ in 0..3 {
        assert!(handle.events.recv().await.is_some());
    }
    quiet(&mut handle).await;
    hold.release();
    let (events, end) = drain(handle).await;
    assert!(events.is_empty(), "{events:#?}");
    assert_eq!(end.reason, EndReason::Completed);
}

#[tokio::test]
async fn dropping_the_hold_lets_its_sessions_go_on() {
    let hold = Hold::new();
    let worker = FakeRunner::cli("agy", "google")
        .then(FakeSession::completed("one").held_after(0, &hold))
        .then(FakeSession::completed("two").held_after(1, &hold));
    drop(hold);
    for _ in 0..2 {
        let handle = worker
            .start(session_spec("agy", Access::ReadOnly, Path::new("/")))
            .await
            .unwrap();
        let (events, end) = drain(handle).await;
        assert_eq!(events.len(), 3, "{events:#?}");
        assert_eq!(end.reason, EndReason::Completed);
    }
}
