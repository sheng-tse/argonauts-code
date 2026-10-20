//! A live check against a local Ollama, at most two model calls. It never pulls a model and
//! never starts the server: it skips unless both are already there. With
//! `ARGONAUTS_RECORD_FIXTURES=1` it replaces the replay fixtures with what it recorded.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::events::session::{EndReason, InputPart, InputPartKind, SessionEnd};
use argonauts_core::names::ModelRef;
use argonauts_core::registry::ModelEntry;
use argonauts_core::session_runner::{Access, SessionRunner, SessionSpec};
use argonauts_core::settings::agents::{EndpointApi, EndpointSettings, StructuredMode};
use argonauts_endpoints::EndpointRunner;
use argonauts_testkit::runner::{drain, session_spec};
use argonauts_testkit::services::{MemoryHealthBoard, MemoryPlanUsageBoard, StaticRegistry};
use serde_json::{Value, json};

const OLLAMA: &str = "http://localhost:11434";
const TAG: &str = "llama3.2:latest";
const TEXT_PROMPT: &str = "Say hello in one short sentence.";
const SCHEMA_PROMPT: &str = "Does add(a, b) return a + b? Answer as JSON.";

/// The server's version, or why the test can't run.
async fn ollama_ready() -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|err| err.to_string())?;
    let version: Value = client
        .get(format!("{OLLAMA}/api/version"))
        .send()
        .await
        .map_err(|err| format!("Ollama isn't running at {OLLAMA} ({err})"))?
        .json()
        .await
        .map_err(|err| format!("{OLLAMA}/api/version didn't answer JSON ({err})"))?;
    let models: Value = client
        .get(format!("{OLLAMA}/v1/models"))
        .send()
        .await
        .map_err(|err| err.to_string())?
        .json()
        .await
        .map_err(|err| err.to_string())?;
    let listed = models["data"]
        .as_array()
        .is_some_and(|data| data.iter().any(|model| model["id"] == json!(TAG)));
    if !listed {
        return Err(format!(
            "Ollama doesn't list {TAG}; pull it yourself to run this test"
        ));
    }
    Ok(version["version"].as_str().unwrap_or("unknown").to_owned())
}

fn worker() -> EndpointRunner {
    let settings = EndpointSettings {
        base_url: Some("http://localhost:11434/v1".to_owned()),
        api: EndpointApi::ChatCompletions,
        structured_output: StructuredMode::JsonSchema,
        local: true,
        ..EndpointSettings::default()
    };
    let registry = Arc::new(StaticRegistry::default());
    let entry: ModelEntry = serde_json::from_value(json!({
        "id": "ollama/llama3.2", "provider": "meta", "kind": "endpoint", "access": "local",
        "cost": {"unit": "none"},
    }))
    .unwrap();
    registry.add(entry);
    EndpointRunner::new(
        "ollama",
        &settings,
        ModelRef::new("ollama", Some("llama3.2".to_owned())),
        "llama".to_owned(),
        registry,
        Arc::new(MemoryPlanUsageBoard::default()),
        Arc::new(MemoryHealthBoard::default()),
    )
}

fn spec(dir: &Path, prompt: &str, schema: Option<Value>) -> SessionSpec {
    let mut spec = session_spec("ollama/llama3.2", Access::NoTools, dir);
    spec.prompt = vec![InputPart {
        kind: InputPartKind::StepInput,
        label: "input".to_owned(),
        text: prompt.to_owned(),
        approx_tokens: 10,
        source: None,
    }];
    spec.json_schema = schema;
    spec
}

async fn call(worker: &EndpointRunner, spec: SessionSpec) -> SessionEnd {
    let handle = worker.start(spec).await.unwrap();
    let (_, end) = tokio::time::timeout(Duration::from_secs(300), drain(handle))
        .await
        .expect("Ollama answered within five minutes");
    end
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ollama")
}

#[tokio::test]
#[ignore = "calls a local Ollama; run it by hand"]
async fn live_ollama_llama32_reply() {
    let version = match ollama_ready().await {
        Ok(version) => version,
        Err(why) => {
            eprintln!("skipped live_ollama_llama32_reply: {why}");
            return;
        }
    };
    let worker = worker();
    let dir = tempfile::tempdir().unwrap();

    let text_spec = spec(&dir.path().join("text"), TEXT_PROMPT, None);
    let text_log = text_spec.session_dir.join("raw.stdout.jsonl");
    let text = call(&worker, text_spec).await;
    assert_eq!(text.reason, EndReason::Completed, "{:?}", text.reason);
    let reply = text.result_text.clone().unwrap_or_default();
    assert!(!reply.trim().is_empty());

    let schema = json!({
        "type": "object", "additionalProperties": false, "required": ["verdict", "reason"],
        "properties": {"verdict": {"type": "string", "enum": ["pass", "fail"]},
                       "reason": {"type": "string"}},
    });
    let schema_spec = spec(
        &dir.path().join("schema"),
        SCHEMA_PROMPT,
        Some(schema.clone()),
    );
    let schema_log = schema_spec.session_dir.join("raw.stdout.jsonl");
    let structured = call(&worker, schema_spec).await;
    assert_eq!(
        structured.reason,
        EndReason::Completed,
        "{:?}",
        structured.reason
    );
    let answer = structured
        .structured
        .clone()
        .expect("a reply that matches the schema");

    if std::env::var_os("ARGONAUTS_RECORD_FIXTURES").is_none() {
        return;
    }
    let out = fixtures();
    std::fs::copy(&text_log, out.join("text.stdout.jsonl")).unwrap();
    std::fs::copy(&schema_log, out.join("schema.stdout.jsonl")).unwrap();
    let meta = json!({
        "synthetic": false,
        "note": "Recorded by live_ollama_llama32_reply with ARGONAUTS_RECORD_FIXTURES=1.",
        "server": "ollama",
        "server_version": version,
        "base_url": format!("{OLLAMA}/v1"),
        "api": "chat_completions",
        "worker": "ollama/llama3.2",
        "recordings": {
            "text": {"file": "text.stdout.jsonl", "prompt": TEXT_PROMPT, "text": reply,
                     "input_tokens": text.spend.input_tokens,
                     "output_tokens": text.spend.output_tokens},
            "schema": {"file": "schema.stdout.jsonl", "structured_output": "json_schema",
                       "prompt": SCHEMA_PROMPT, "schema": schema, "structured": answer,
                       "input_tokens": structured.spend.input_tokens,
                       "output_tokens": structured.spend.output_tokens},
        },
    });
    let meta = serde_json::to_string_pretty(&meta).unwrap() + "\n";
    std::fs::write(out.join("meta.json"), meta).unwrap();
}
