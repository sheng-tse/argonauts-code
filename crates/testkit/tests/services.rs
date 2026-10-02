use argonauts_core::engine::EventSink;
use argonauts_core::events::RunEventKind;
use argonauts_core::limits::{HealthBoard, HealthSignal, HealthState, QuotaBoard};
use argonauts_core::names::{SessionKey, VendorId, WorkerRef};
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
        .record(None, None, "conductor", note.clone())
        .unwrap();
    let second = sink.record(None, None, "conductor", note).unwrap();
    assert_eq!((first.seq, second.seq), (0, 1));
    assert_eq!(sink.events().len(), 2);

    let mut changes = services.health.subscribe();
    let vendor = VendorId("codex".into());
    for _ in 0..3 {
        services.health.observe(HealthSignal::Error {
            vendor: vendor.clone(),
            session: SessionKey::numbered(1),
            category: ErrorCategory::Overloaded,
        });
    }
    assert_eq!(services.health.state(&vendor).state, HealthState::Down);
    assert!(changes.try_recv().is_ok());
    services.health.test_result(&vendor, true);
    assert_eq!(services.health.state(&vendor).state, HealthState::Healthy);
    assert!(services.quota.all().is_empty());
}

#[test]
fn the_static_registry_falls_back_to_the_harness_family() {
    let registry = StaticRegistry::default();
    let family = |worker: &str| registry.family(&worker.parse::<WorkerRef>().unwrap());
    assert_eq!(family("claude-code/opus"), "claude");
    assert_eq!(family("codex"), "gpt");
    assert_eq!(family("antigravity-cli"), "gemini");
    // The model id says more than the harness.
    assert_eq!(family("opencode/gpt-oss-120b"), "gpt");
    assert_eq!(family("codex/claude-x"), "claude");
    assert_eq!(family("opencode"), "unknown");
    let entry: ModelEntry = serde_json::from_value(serde_json::json!({
        "id": "codex/house-model", "family": "llama", "kind": "harness", "access": "subscription",
        "cost": {"unit": "quota"},
    }))
    .unwrap();
    registry.add(entry);
    assert_eq!(family("codex/house-model"), "llama");
}
