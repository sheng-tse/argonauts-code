//! Recorded Ollama streams, replayed through the decoder in uneven chunks.

use argonauts_core::AgentEvent;
use argonauts_core::settings::agents::EndpointApi;
use serde_json::Value;

use crate::session::structured;
use crate::stream::sse::SseDecoder;
use crate::stream::{Reply, ReplyStream};

const META: &str = include_str!("../../tests/fixtures/ollama/meta.json");
const TEXT: &str = include_str!("../../tests/fixtures/ollama/text.stdout.jsonl");
const SCHEMA: &str = include_str!("../../tests/fixtures/ollama/schema.stdout.jsonl");

/// A raw log back as the stream it came from: the log leaves out the blank line that ends each
/// event, and every event here is one line.
fn as_sent(raw: &str) -> String {
    raw.lines().map(|line| format!("{line}\n\n")).collect()
}

/// Feeds the logged stream in chunks of `size` bytes, so lines and characters split at every
/// boundary.
fn replay(raw: &str, size: usize) -> (Vec<AgentEvent>, Reply) {
    let sent = as_sent(raw);
    let mut decoder = SseDecoder::default();
    let mut stream = ReplyStream::new(EndpointApi::ChatCompletions);
    let mut events = Vec::new();
    let mut lines = Vec::new();
    for chunk in sent.as_bytes().chunks(size) {
        lines.extend(decoder.push(chunk));
    }
    lines.extend(decoder.finish());
    for data in lines.into_iter().filter_map(|line| line.event) {
        events.extend(stream.push(&data).unwrap());
    }
    (events, stream.finish())
}

fn deltas(events: &[AgentEvent]) -> String {
    events
        .iter()
        .map(|event| match event {
            AgentEvent::TextDelta { text, .. } => text.as_str(),
            other => panic!("unexpected event {other:?}"),
        })
        .collect()
}

#[test]
fn endpoint_replays_recorded_ollama_stream() {
    let meta: Value = serde_json::from_str(META).unwrap();
    let text = &meta["recordings"]["text"];
    for size in [1, 3, 7, 64, TEXT.len()] {
        let (events, reply) = replay(TEXT, size);
        assert_eq!(
            deltas(&events),
            text["text"].as_str().unwrap(),
            "chunks of {size}"
        );
        assert_eq!(reply.text, text["text"].as_str().unwrap());
        assert_eq!(Some(reply.input_tokens), text["input_tokens"].as_u64());
        assert_eq!(Some(reply.output_tokens), text["output_tokens"].as_u64());
        assert!(reply.finished && reply.usage_seen);
        assert_eq!(reply.model.as_deref(), Some("llama3.2"));
    }

    let recorded = &meta["recordings"]["schema"];
    let (events, reply) = replay(SCHEMA, 5);
    assert_eq!(deltas(&events), reply.text);
    let structured = structured::read(&recorded["schema"], &reply.text).unwrap();
    assert_eq!(structured, recorded["structured"]);
    assert_eq!(
        Some(reply.output_tokens),
        recorded["output_tokens"].as_u64()
    );
}

#[test]
fn recordings_are_logged_without_blank_lines() {
    for raw in [TEXT, SCHEMA] {
        assert!(raw.ends_with('\n'));
        assert!(raw.lines().all(|line| line.starts_with("data: ")), "{raw}");
    }
}

#[test]
fn recordings_hold_no_secrets() {
    for raw in [TEXT, SCHEMA, META] {
        assert!(argonauts_core::safety::redact::find_secrets(raw).is_empty());
        assert!(!raw.to_lowercase().contains("bearer"));
    }
}
