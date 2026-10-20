use argonauts_core::engine::EventSink;
use argonauts_core::events::RunEventKind;
use argonauts_core::limits::{HealthBoard, HealthSignal, HealthState, PlanUsageBoard};
use argonauts_core::names::{AgentId, ModelRef, SessionKey};
use argonauts_core::registry::{ModelEntry, ModelRegistry};
use argonauts_core::settings::Settings;
use argonauts_core::{ErrorCategory, Note, NoteLevel};
use argonauts_testkit::FakeServices;
use argonauts_testkit::services::{StaticRegistry, engine_context};

#[tokio::test]
async fn the_memory_sink_numbers_events_and_the_breaker_trips() {
    let services = FakeServices::new();
    let (context, sink, _commands) = engine_context(&services, Settings::default());
    let note = RunEventKind::Note(Note {
        level: NoteLevel::Info,
        text: "x".into(),
    });
    let first = context
        .sink
        .record(None, None, "argonauts-code", note.clone())
        .unwrap();
    let second = sink.record(None, None, "argonauts-code", note).unwrap();
    assert_eq!((first.seq, second.seq), (0, 1));
    assert_eq!(sink.events().len(), 2);

    let mut changes = services.health.subscribe();
    let agent_id = AgentId("codex".into());
    for _ in 0..3 {
        services.health.observe(HealthSignal::Error {
            agent_id: agent_id.clone(),
            model: None,
            session: SessionKey::numbered(1),
            category: ErrorCategory::Network,
        });
    }
    assert_eq!(services.health.state(&agent_id).state, HealthState::Down);
    assert!(changes.try_recv().is_ok());
    services.health.test_result(&agent_id, true);
    assert_eq!(services.health.state(&agent_id).state, HealthState::Healthy);
    assert!(services.plan_usage.all().is_empty());
}

#[test]
fn the_static_registry_falls_back_to_the_cli_provider() {
    let registry = StaticRegistry::default();
    let provider = |worker: &str| registry.provider(&worker.parse::<ModelRef>().unwrap());
    assert_eq!(provider("claude-code/opus"), "anthropic");
    assert_eq!(provider("codex"), "openai");
    assert_eq!(provider("antigravity-cli"), "google");
    // The model id says more than the CLI.
    assert_eq!(provider("opencode/gpt-oss-120b"), "openai");
    assert_eq!(provider("codex/claude-x"), "anthropic");
    assert_eq!(provider("opencode"), "unknown");
    let entry: ModelEntry = serde_json::from_value(serde_json::json!({
        "id": "codex/house-model", "provider": "meta", "kind": "cli", "access": "subscription",
        "cost": {"unit": "plan_usage"},
    }))
    .unwrap();
    registry.add(entry);
    assert_eq!(provider("codex/house-model"), "meta");
}
