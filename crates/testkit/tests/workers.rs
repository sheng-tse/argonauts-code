use std::path::Path;
use std::time::Duration;

use argonauts_core::events::run::Party;
use argonauts_core::events::session::{
    Delivery, EndReason, InputPart, InputPartKind, QueuedMessage, SessionEvent,
};
use argonauts_core::input::MentionTarget;
use argonauts_core::names::MessageId;
use argonauts_core::steps::intake::{ReplyKind, TriageEstimate};
use argonauts_core::steps::{StepRequest, StepRunner, decode, output_schema};
use argonauts_core::worker::{
    Access, SessionControl, SessionHandle, Worker, WorkerDirectory, WorkerError, WorkerEvent,
};
use argonauts_testkit::worker::{drain, session_spec};
use argonauts_testkit::{FakeDirectory, FakeServices, FakeSession, FakeWorker};
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
    let worker = FakeWorker::harness("codex/gpt-x", "gpt").then(FakeSession::completed("done"));
    let mut spec = session_spec("codex/gpt-x", Access::ReadOnly, Path::new("/repo"));
    spec.prompt = prompt("read the code");
    let handle = worker.start(spec.clone()).await.unwrap();
    let (events, end) = drain(handle).await;
    match &events[0] {
        WorkerEvent::Session(SessionEvent::Input(input)) => assert_eq!(input.parts, spec.prompt),
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
    let harness = FakeWorker::harness("claude-code/opus", "claude")
        .then(FakeSession::completed("ok"))
        .then(FakeSession::completed("ok"));
    let build = session_spec("claude-code/opus", Access::Build, Path::new("/wt"));
    assert!(matches!(
        harness.start(build.clone()).await,
        Err(WorkerError::Refused(_))
    ));
    let mut with_goal = build;
    with_goal.goal_sha256 = Some("abc".into());
    assert!(harness.start(with_goal).await.is_ok());

    let endpoint = FakeWorker::endpoint("openrouter/m-a", "glm").then(FakeSession::completed("ok"));
    let read_only = session_spec("openrouter/m-a", Access::ReadOnly, Path::new("/"));
    assert!(matches!(
        endpoint.start(read_only).await,
        Err(WorkerError::Refused(_))
    ));
}

#[tokio::test]
async fn stop_ends_the_session_as_stopped_and_is_recorded() {
    let worker = FakeWorker::harness("agy", "gemini").then(
        FakeSession::completed("slow")
            .with_event(WorkerEvent::Agent(argonauts_core::AgentEvent::Reasoning {
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
            FakeWorker::harness("claude-code", "claude").then(FakeSession::structured(
                json!({"kind": "answer", "answer_markdown": "Hi!", "estimate": null}),
            )),
        );
    let worker = services
        .workers
        .get(&"claude-code".parse().unwrap())
        .unwrap();
    let mut spec = session_spec("claude-code", Access::ReadOnly, Path::new("/repo"));
    spec.json_schema = Some(output_schema::<TriageEstimate>());
    spec.prompt = prompt("hi");
    let outcome = services
        .steps
        .run(StepRequest::new(worker, spec), Box::new(|_| {}))
        .await
        .unwrap();
    assert_eq!(
        decode::<TriageEstimate>(&outcome).unwrap().kind,
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
        FakeWorker::harness("claude-code/opus", "claude").then(FakeSession::until_stopped(vec![]));
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
        WorkerEvent::Session(SessionEvent::Input(_))
    ));
    assert!(matches!(
        &events[1],
        WorkerEvent::Session(SessionEvent::Delivered {
            via: Delivery::Steer,
            ..
        })
    ));
    match &events[2] {
        WorkerEvent::Session(SessionEvent::Input(input)) => {
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
    let worker = FakeWorker::harness("codex", "gpt").then(FakeSession::until_stopped(vec![]));
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
