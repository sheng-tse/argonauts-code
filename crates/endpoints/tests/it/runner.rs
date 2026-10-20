//! The endpoint runner against the fake OpenAI-compatible server.

use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use argonauts_core::events::run::Party;
use argonauts_core::events::session::{
    Delivery, EndReason, InputPart, InputPartKind, PromptVia, QueuedMessage, SessionEnd,
    SessionEvent,
};
use argonauts_core::input::MentionTarget;
use argonauts_core::limits::{
    HealthBoard, HealthState, ModelState, PlanUsageBoard, PlanUsageStatus,
};
use argonauts_core::names::{AgentId, MessageId, ModelRef};
use argonauts_core::registry::ModelEntry;
use argonauts_core::session_runner::{
    Access, RunnerError, RunnerEvent, SessionControl, SessionRunner, SessionSpec, SteerSupport,
};
use argonauts_core::settings::Span;
use argonauts_core::settings::agents::{EndpointApi, EndpointSettings, StructuredMode};
use argonauts_core::{AgentEvent, ErrorCategory, RetryReason};
use argonauts_endpoints::EndpointRunner;
use argonauts_testkit::runner::{drain, session_spec};
use argonauts_testkit::services::{MemoryHealthBoard, MemoryPlanUsageBoard, StaticRegistry};
use argonauts_testkit::{FakeOpenAi, FakeReply};
use serde_json::{Value, json};

const ENDPOINT: &str = "fake";

struct Rig {
    worker: EndpointRunner,
    plan_usage: Arc<MemoryPlanUsageBoard>,
    health: Arc<MemoryHealthBoard>,
}

fn settings(server: &FakeOpenAi) -> EndpointSettings {
    EndpointSettings {
        base_url: Some(server.base_url()),
        ..EndpointSettings::default()
    }
}

fn rig(settings: &EndpointSettings) -> Rig {
    let registry = Arc::new(StaticRegistry::default());
    let entry: ModelEntry = serde_json::from_value(json!({
        "id": "fake/fake-model", "provider": "meta", "kind": "endpoint", "access": "api-key",
        "cost": {"unit": "usd", "input_per_mtok": 1.0, "output_per_mtok": 2.0},
    }))
    .unwrap();
    registry.add(entry);
    let plan_usage = Arc::new(MemoryPlanUsageBoard::default());
    let health = Arc::new(MemoryHealthBoard::default());
    let worker = EndpointRunner::new(
        ENDPOINT,
        settings,
        model_ref(),
        "llama".to_owned(),
        registry,
        plan_usage.clone(),
        health.clone(),
    );
    Rig {
        worker,
        plan_usage,
        health,
    }
}

fn model_ref() -> ModelRef {
    ModelRef::new(ENDPOINT, Some("fake-model".to_owned()))
}

fn part(kind: InputPartKind, text: &str) -> InputPart {
    InputPart {
        kind,
        label: format!("{kind:?}").to_lowercase(),
        text: text.to_owned(),
        approx_tokens: 1,
        source: None,
    }
}

fn spec(dir: &Path) -> SessionSpec {
    let mut spec = session_spec("fake/fake-model", Access::NoTools, dir);
    spec.prompt = vec![
        part(InputPartKind::Prompt, "You are a careful reviewer.\n"),
        part(InputPartKind::StepInput, "Review task 3."),
    ];
    spec
}

async fn run(worker: &EndpointRunner, spec: SessionSpec) -> (Vec<RunnerEvent>, SessionEnd) {
    let handle = worker.start(spec).await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), drain(handle))
        .await
        .expect("the session ended")
}

fn agent(events: &[RunnerEvent]) -> Vec<&AgentEvent> {
    events
        .iter()
        .filter_map(|event| match event {
            RunnerEvent::Agent(agent) => Some(agent),
            _ => None,
        })
        .collect()
}

fn streamed_text(events: &[RunnerEvent]) -> String {
    agent(events)
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn result(events: &[RunnerEvent]) -> (Option<String>, Option<Value>, bool) {
    agent(events)
        .into_iter()
        .find_map(|event| match event {
            AgentEvent::Result {
                text,
                structured,
                is_error,
            } => Some((text.clone(), structured.clone(), *is_error)),
            _ => None,
        })
        .expect("a result event")
}

fn sent_prompt(body: &Value, api: EndpointApi) -> String {
    match api {
        EndpointApi::ChatCompletions => body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["content"].as_str().unwrap())
            .collect(),
        EndpointApi::Responses => format!(
            "{}{}",
            body["instructions"].as_str().unwrap_or_default(),
            body["input"].as_str().unwrap()
        ),
    }
}

#[tokio::test]
async fn endpoint_refuses_tools_access() {
    let server = FakeOpenAi::start().unwrap();
    let rig = rig(&settings(&server));
    let dir = tempfile::tempdir().unwrap();
    for access in [Access::ReadOnly, Access::Build, Access::Experiment] {
        let mut spec = spec(dir.path());
        spec.access = access;
        spec.goal_sha256 = Some("abc".into());
        let refused = rig.worker.start(spec).await.err();
        assert!(
            matches!(&refused, Some(RunnerError::Refused(why)) if why.contains("no-tools")),
            "{access:?}: {refused:?}"
        );
    }
    let mut resumed = spec(dir.path());
    resumed.resume = Some("earlier".into());
    assert!(matches!(
        rig.worker.start(resumed).await.err(),
        Some(RunnerError::Refused(_))
    ));
    assert!(server.requests().is_empty());
    let capabilities = rig.worker.capabilities();
    assert!(capabilities.streams_text && capabilities.structured_output);
    assert!(!capabilities.resume && !capabilities.can_build && !capabilities.take_over);
    assert_eq!(capabilities.steer, SteerSupport::NextTurnOnly);
    assert!(rig.worker.take_over_command("x", dir.path()).is_none());
}

#[tokio::test]
async fn endpoint_streams_text_and_usage() {
    for api in [EndpointApi::ChatCompletions, EndpointApi::Responses] {
        let server = FakeOpenAi::start().unwrap();
        let rig = rig(&EndpointSettings {
            api,
            ..settings(&server)
        });
        let dir = tempfile::tempdir().unwrap();
        let spec = spec(dir.path());
        server.push(FakeReply::text("Looks right to me."));
        let (events, end) = run(&rig.worker, spec.clone()).await;

        let RunnerEvent::Session(SessionEvent::Started(started)) = &events[0] else {
            panic!("{api:?}: started first, got {:?}", events[0]);
        };
        assert_eq!(started.agent_id, AgentId(ENDPOINT.into()));
        assert_eq!(started.provider, "llama");
        let RunnerEvent::Session(SessionEvent::Input(input)) = &events[1] else {
            panic!("{api:?}: input second, got {:?}", events[1]);
        };
        assert_eq!(input.parts, spec.prompt);
        assert_eq!(input.launch.prompt_via, PromptVia::Request);
        assert_eq!(input.launch.program, ENDPOINT);
        assert!(input.launch.args.contains(&"model=fake-model".to_owned()));

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let body = &requests[0].body;
        assert_eq!(body["model"], json!("fake-model"));
        assert_eq!(body["stream"], json!(true));
        let sent: String = spec.prompt.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(sent_prompt(body, api), sent, "{api:?}: sent byte for byte");

        assert_eq!(streamed_text(&events), "Looks right to me.");
        let tail: Vec<&str> = agent(&events)
            .iter()
            .rev()
            .take(4)
            .map(|e| e.type_name())
            .collect();
        assert_eq!(
            tail,
            ["result", "context_fill", "usage", "message"],
            "{api:?}"
        );
        let filled = agent(&events).into_iter().find_map(|event| match event {
            AgentEvent::ContextFill { used, size, sent } => Some((*used, *size, sent.is_some())),
            _ => None,
        });
        // The registry knows no window for the fake model, and its server says none.
        assert_eq!(filled, Some((16, None, true)), "{api:?}");
        let usage = agent(&events).into_iter().find_map(|event| match event {
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                cost_usd,
                cumulative,
                ..
            } => Some((*input_tokens, *output_tokens, *cost_usd, *cumulative)),
            _ => None,
        });
        // 12 input tokens at $1 and 4 output at $2 per million.
        let cost = 12.0 / 1e6 + 4.0 * 2.0 / 1e6;
        assert_eq!(usage, Some((12, 4, Some(cost), true)), "{api:?}");
        assert_eq!(
            result(&events),
            (Some("Looks right to me.".into()), None, false)
        );

        assert_eq!(end.reason, EndReason::Completed);
        assert_eq!(end.result_text.as_deref(), Some("Looks right to me."));
        assert_eq!((end.spend.input_tokens, end.spend.output_tokens), (12, 4));
        assert!((end.spend.cost_usd - cost).abs() < 1e-12 && end.spend.cost_estimated);
        assert_eq!(end.raw_log, Some(spec.session_dir.join("raw.stdout.jsonl")));
        assert_eq!(end.tool_calls, 0);
        let health = rig.health.state(&AgentId(ENDPOINT.into()));
        assert_eq!(health.state, HealthState::Healthy);
    }
}

#[tokio::test]
async fn endpoint_worker_json_schema_output() {
    let schema = json!({
        "type": "object", "additionalProperties": false, "required": ["verdict", "reason"],
        "properties": {"verdict": {"type": "string", "enum": ["pass", "fail"]},
                       "reason": {"type": "string"}},
    });
    let answer = json!({"verdict": "pass", "reason": "adds both numbers"});
    let modes = [
        StructuredMode::JsonSchema,
        StructuredMode::JsonObject,
        StructuredMode::Prompt,
    ];
    for api in [EndpointApi::ChatCompletions, EndpointApi::Responses] {
        for mode in modes {
            let server = FakeOpenAi::start().unwrap();
            let rig = rig(&EndpointSettings {
                api,
                structured_output: mode,
                ..settings(&server)
            });
            let dir = tempfile::tempdir().unwrap();
            let mut spec = spec(dir.path());
            spec.json_schema = Some(schema.clone());
            server.push(FakeReply::json(&answer));
            let (events, end) = run(&rig.worker, spec.clone()).await;

            let case = format!("{api:?} {mode:?}");
            assert_eq!(end.reason, EndReason::Completed, "{case}");
            assert_eq!(end.structured.as_ref(), Some(&answer), "{case}");
            assert_eq!(result(&events).1.as_ref(), Some(&answer), "{case}");
            let body = &server.requests()[0].body;
            let format = match api {
                EndpointApi::ChatCompletions => &body["response_format"],
                EndpointApi::Responses => &body["text"]["format"],
            };
            match mode {
                StructuredMode::JsonSchema => {
                    assert_eq!(format["type"], json!("json_schema"), "{case}");
                    let strict = match api {
                        EndpointApi::ChatCompletions => &format["json_schema"],
                        EndpointApi::Responses => format,
                    };
                    assert_eq!(strict["strict"], json!(true), "{case}");
                    assert_eq!(strict["schema"], schema, "{case}");
                }
                StructuredMode::JsonObject => {
                    assert_eq!(format, &json!({"type": "json_object"}), "{case}")
                }
                StructuredMode::Prompt => assert!(format.is_null(), "{case}"),
            }
            // Whatever the mode, the input record is exactly what was sent.
            let RunnerEvent::Session(SessionEvent::Input(input)) = &events[1] else {
                panic!("{case}: no input record");
            };
            let recorded: String = input.parts.iter().map(|p| p.text.as_str()).collect();
            assert_eq!(sent_prompt(body, api), recorded, "{case}");
            let schema_in_prompt = input.parts.iter().any(|p| p.kind == InputPartKind::Schema);
            assert_eq!(
                schema_in_prompt,
                mode != StructuredMode::JsonSchema,
                "{case}"
            );
        }
    }

    // A reply that doesn't match still completes; the step runner judges it.
    let server = FakeOpenAi::start().unwrap();
    let rig = rig(&settings(&server));
    let dir = tempfile::tempdir().unwrap();
    let mut spec = spec(dir.path());
    spec.json_schema = Some(schema);
    server.push(FakeReply::json(&json!({"verdict": "maybe"})));
    let (events, end) = run(&rig.worker, spec).await;
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(end.structured, None);
    assert_eq!(end.result_text.as_deref(), Some(r#"{"verdict":"maybe"}"#));
    assert!(result(&events).2, "the result is marked as an error");
}

/// One failing session per row, each against a fresh server.
async fn failing(
    reply: FakeReply,
    tweak: impl FnOnce(&mut EndpointSettings, &mut SessionSpec),
) -> (Rig, Vec<RunnerEvent>, SessionEnd) {
    let server = FakeOpenAi::start().unwrap();
    server.push(reply);
    let mut settings = settings(&server);
    let dir = tempfile::tempdir().unwrap();
    let mut spec = spec(dir.path());
    tweak(&mut settings, &mut spec);
    let rig = rig(&settings);
    let (events, end) = run(&rig.worker, spec).await;
    (rig, events, end)
}

fn error_category(events: &[RunnerEvent]) -> Option<ErrorCategory> {
    agent(events).into_iter().find_map(|event| match event {
        AgentEvent::Error { category, .. } => Some(*category),
        _ => None,
    })
}

fn cli_error(reason: &EndReason) -> Option<ErrorCategory> {
    match reason {
        EndReason::CliError { category, .. } => Some(*category),
        _ => None,
    }
}

#[tokio::test]
async fn endpoint_errors_to_end_reasons() {
    let none = |_: &mut EndpointSettings, _: &mut SessionSpec| {};
    let agent_id = AgentId(ENDPOINT.into());

    let (_, events, end) = failing(FakeReply::error(401, "Incorrect API key provided"), none).await;
    assert_eq!(
        end.reason,
        EndReason::Auth {
            message: "Incorrect API key provided".into()
        }
    );
    assert_eq!(error_category(&events), Some(ErrorCategory::Auth));
    let (_, _, end) = failing(FakeReply::error(403, "forbidden"), none).await;
    assert!(matches!(end.reason, EndReason::Auth { .. }));
    let (_, _, end) = failing(FakeReply::error(402, "Insufficient credits"), none).await;
    assert!(matches!(end.reason, EndReason::Billing { .. }));

    // A 429 whose wait is longer than the session's time left.
    let limited = FakeReply::Error {
        status: 429,
        message: "Rate limit reached for requests".into(),
        retry_after: Some(5),
    };
    let short =
        |_: &mut EndpointSettings, spec: &mut SessionSpec| spec.limits.max_minutes = Some(0.02);
    let started = Instant::now();
    let (rig, events, end) = failing(limited, short).await;
    assert_eq!(end.reason, EndReason::RateLimited);
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "it didn't wait past the limit"
    );
    assert_eq!(error_category(&events), Some(ErrorCategory::RateLimit));
    let reading = rig.plan_usage.get(&agent_id).expect("a plan usage reading");
    assert_eq!(reading.status, PlanUsageStatus::Rejected);
    assert!(reading.resets_at.is_some());
    let (_, _, end) = failing(FakeReply::error(429, "Rate limit reached"), none).await;
    assert_eq!(end.reason, EndReason::RateLimited);

    let used_up = "You exceeded your current quota, please check your plan and billing details.";
    let (rig, _, end) = failing(FakeReply::error(429, used_up), none).await;
    assert!(
        matches!(end.reason, EndReason::Billing { .. }),
        "{:?}",
        end.reason
    );
    assert!(rig.plan_usage.get(&agent_id).is_some());
    let per_day = "Rate limit exceeded: free-models-per-day";
    let (_, events, end) = failing(FakeReply::error(429, per_day), none).await;
    assert!(
        matches!(end.reason, EndReason::UsageLimit { .. }),
        "{:?}",
        end.reason
    );
    assert_eq!(error_category(&events), Some(ErrorCategory::UsageLimit));

    let (rig, events, end) = failing(FakeReply::error(500, "boom"), none).await;
    assert_eq!(cli_error(&end.reason), Some(ErrorCategory::Server));
    assert_eq!(error_category(&events), Some(ErrorCategory::Server));
    // A server error and an overload are about the model they came from: in a row they take
    // the fake board's model out, so each one sent a health signal that named its model.
    for _ in 0..2 {
        let server = FakeOpenAi::start().unwrap();
        server.push(FakeReply::error(503, "overloaded"));
        let worker = EndpointRunner::new(
            ENDPOINT,
            &settings(&server),
            model_ref(),
            "llama".into(),
            Arc::new(StaticRegistry::default()),
            rig.plan_usage.clone(),
            rig.health.clone(),
        );
        let dir = tempfile::tempdir().unwrap();
        let (_, end) = run(&worker, spec(dir.path())).await;
        assert_eq!(cli_error(&end.reason), Some(ErrorCategory::Overloaded));
    }
    let models = rig.health.models(&agent_id);
    let states: Vec<ModelState> = models.iter().map(|health| health.state).collect();
    assert_eq!(states, [ModelState::Out], "{models:?}");
    assert_eq!(models[0].model, model_ref());
    assert_ne!(rig.health.state(&agent_id).state, HealthState::Down);

    let slow = FakeReply::Delay(Duration::from_secs(3), Box::new(FakeReply::text("late")));
    let one_second =
        |settings: &mut EndpointSettings, _: &mut SessionSpec| settings.timeout = Span::new("1s");
    let (_, events, end) = failing(slow, one_second).await;
    assert_eq!(cli_error(&end.reason), Some(ErrorCategory::Timeout));
    assert_eq!(error_category(&events), Some(ErrorCategory::Timeout));
    let slow = FakeReply::Delay(Duration::from_secs(3), Box::new(FakeReply::text("late")));
    let one_minute_limit = |_: &mut EndpointSettings, spec: &mut SessionSpec| {
        spec.limits.max_minutes = Some(1.0 / 60.0)
    };
    let (_, _, end) = failing(slow, one_minute_limit).await;
    assert!(
        matches!(end.reason, EndReason::LimitHit { .. }),
        "{:?}",
        end.reason
    );

    let context = "This model's maximum context length is 8192 tokens.";
    let (_, _, end) = failing(FakeReply::error(400, context), none).await;
    assert_eq!(end.reason, EndReason::ContextFull);
    let missing = "model \"fake-model\" not found, try pulling it first";
    let (_, _, end) = failing(FakeReply::error(404, missing), none).await;
    assert_eq!(
        end.reason,
        EndReason::ModelUnavailable {
            model: "fake-model".into()
        }
    );

    // Nothing listening.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let closed = |settings: &mut EndpointSettings, _: &mut SessionSpec| {
        settings.base_url = Some(format!("http://127.0.0.1:{port}/v1"));
    };
    let (_, events, end) = failing(FakeReply::text("unused"), closed).await;
    assert_eq!(cli_error(&end.reason), Some(ErrorCategory::Network));
    assert_eq!(error_category(&events), Some(ErrorCategory::Network));

    // Settings that make a request impossible still end like any session.
    let no_key = |settings: &mut EndpointSettings, _: &mut SessionSpec| {
        settings.api_key_env = Some("ARGONAUTS_TEST_NEVER_SET_KEY".into());
    };
    let (_, events, end) = failing(FakeReply::text("unused"), no_key).await;
    assert!(
        matches!(&end.reason, EndReason::Auth { message } if message.contains("ARGONAUTS_TEST_NEVER_SET_KEY"))
    );
    assert!(matches!(
        &events[1],
        RunnerEvent::Session(SessionEvent::Input(_))
    ));
    let no_url = |settings: &mut EndpointSettings, _: &mut SessionSpec| settings.base_url = None;
    let (_, _, end) = failing(FakeReply::text("unused"), no_url).await;
    assert!(
        matches!(&end.reason, EndReason::StartFailed { message } if message.contains("base_url"))
    );
}

#[tokio::test]
async fn endpoint_waits_on_retry_after() {
    let server = FakeOpenAi::start().unwrap();
    server.push(FakeReply::Error {
        status: 429,
        message: "Rate limit reached for requests".into(),
        retry_after: Some(1),
    });
    server.push(FakeReply::text("after the wait"));
    let rig = rig(&settings(&server));
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let (events, end) = run(&rig.worker, spec(dir.path())).await;
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(end.result_text.as_deref(), Some("after the wait"));
    assert_eq!(server.requests().len(), 2);
    let retry = agent(&events).into_iter().find_map(|event| match event {
        AgentEvent::Retry { reason, delay_ms } => Some((reason.clone(), *delay_ms)),
        _ => None,
    });
    assert_eq!(retry, Some((RetryReason::RateLimit, Some(1000))));
    assert!(error_category(&events).is_none());
}

#[tokio::test]
async fn endpoint_raw_log_keeps_every_event_line_without_the_key() {
    const KEY: &str = "plain-test-key-0042";
    let server = FakeOpenAi::start().unwrap();
    let reply = format!("the key was {KEY} ok");
    server.push(FakeReply::text(&reply));
    let rig = rig(&EndpointSettings {
        api_key_env: Some("ARGONAUTS_TEST_ENDPOINT_KEY".into()),
        header_env: [("x-title".to_owned(), "ARGONAUTS_TEST_TITLE".to_owned())].into(),
        ..settings(&server)
    });
    let dir = tempfile::tempdir().unwrap();
    let mut spec = spec(dir.path());
    spec.env = vec![
        ("ARGONAUTS_TEST_ENDPOINT_KEY".into(), KEY.into()),
        ("ARGONAUTS_TEST_TITLE".into(), "argonauts".into()),
    ];
    let (events, end) = run(&rig.worker, spec.clone()).await;
    assert_eq!(end.reason, EndReason::Completed);

    let request = &server.requests()[0];
    assert_eq!(
        request.header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    assert_eq!(request.header("x-title"), Some("argonauts"));

    let log = std::fs::read_to_string(spec.session_dir.join("raw.stdout.jsonl")).unwrap();
    assert!(!log.contains(KEY));
    let data: Vec<&str> = log
        .lines()
        .filter(|line| line.starts_with("data: "))
        .collect();
    // The role chunk, one per word, the finish chunk, usage, and [DONE].
    let words = reply.split(' ').count();
    assert_eq!(data.len(), 1 + words + 3);
    // The blank lines between events aren't logged.
    assert_eq!(log.lines().count(), data.len());
    assert_eq!(data.last(), Some(&"data: [DONE]"));
    assert!(data.iter().any(|line| line.contains("[redacted]")));

    let RunnerEvent::Session(SessionEvent::Input(input)) = &events[1] else {
        panic!("no input record");
    };
    let launch = serde_json::to_string(&input.launch).unwrap();
    assert!(!launch.contains(KEY));
    assert!(launch.contains("$ARGONAUTS_TEST_ENDPOINT_KEY"));
}

#[tokio::test]
async fn endpoint_stop_cancels_the_request_and_messages_wait() {
    for control in [
        SessionControl::Stop,
        SessionControl::WrapUp,
        SessionControl::StopAfterTool,
    ] {
        let server = FakeOpenAi::start().unwrap();
        server.push(FakeReply::Delay(
            Duration::from_secs(10),
            Box::new(FakeReply::text("never")),
        ));
        let rig = rig(&settings(&server));
        let dir = tempfile::tempdir().unwrap();
        let handle = rig.worker.start(spec(dir.path())).await.unwrap();
        let message = QueuedMessage {
            id: MessageId::numbered(1),
            from: Party::User,
            to: MentionTarget::All,
            text: "also check the tests".into(),
            queued_at: time::OffsetDateTime::UNIX_EPOCH,
            delivery: Delivery::NextTurn,
        };
        handle
            .control
            .send(SessionControl::Deliver(message))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let started = Instant::now();
        handle.control.send(control.clone()).await.unwrap();
        let (events, end) = tokio::time::timeout(Duration::from_secs(5), drain(handle))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        let want = match control {
            SessionControl::StopAfterTool => EndReason::Paused,
            _ => EndReason::Stopped,
        };
        assert_eq!(end.reason, want);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RunnerEvent::Session(SessionEvent::Queued(_))))
        );
        assert!(error_category(&events).is_none());
    }

    // Dropping every control sender stops it too.
    let server = FakeOpenAi::start().unwrap();
    server.push(FakeReply::Delay(
        Duration::from_secs(10),
        Box::new(FakeReply::text("never")),
    ));
    let rig = rig(&settings(&server));
    let dir = tempfile::tempdir().unwrap();
    let handle = rig.worker.start(spec(dir.path())).await.unwrap();
    let (control, ended) = (handle.control, handle.ended);
    drop(control);
    let end = tokio::time::timeout(Duration::from_secs(5), ended)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(end.reason, EndReason::Stopped);
}
