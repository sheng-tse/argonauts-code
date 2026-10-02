//! The fake OpenAI server behind the real endpoint worker, reached the way the approver and
//! the narrator reach a model: through a worker directory, with no tools and a JSON schema.

use std::sync::Arc;
use std::time::Duration;

use argonauts_core::events::session::{EndReason, InputPart, InputPartKind};
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::agents::{EndpointApi, EndpointSettings};
use argonauts_core::worker::{Access, WorkerDirectory};
use argonauts_endpoints::EndpointWorker;
use argonauts_testkit::services::{MemoryHealthBoard, MemoryQuotaBoard, StaticRegistry};
use argonauts_testkit::worker::{drain, session_spec};
use argonauts_testkit::{FakeDirectory, FakeOpenAi, FakeReply};
use serde_json::json;

fn approver() -> WorkerRef {
    WorkerRef::new("fake", Some("fake-model".to_owned()))
}

fn directory(settings: &EndpointSettings) -> FakeDirectory {
    let directory = FakeDirectory::new();
    directory.add_worker(Arc::new(EndpointWorker::new(
        "fake",
        settings,
        approver(),
        "llama".to_owned(),
        Arc::new(StaticRegistry::default()),
        Arc::new(MemoryQuotaBoard::default()),
        Arc::new(MemoryHealthBoard::default()),
    )));
    directory
}

#[tokio::test]
async fn a_structured_verdict_comes_back_through_the_endpoint_worker() {
    let schema = json!({
        "type": "object", "additionalProperties": false, "required": ["decision", "reason"],
        "properties": {"decision": {"type": "string", "enum": ["approve", "ask"]},
                       "reason": {"type": "string"}},
    });
    let verdict = json!({"decision": "approve", "reason": "the gates passed"});
    for api in [EndpointApi::ChatCompletions, EndpointApi::Responses] {
        let server = FakeOpenAi::start().unwrap();
        server.push(FakeReply::json(&verdict));
        let settings = EndpointSettings {
            api,
            ..server.endpoint_settings()
        };
        let worker = directory(&settings).get(&approver()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut spec = session_spec("fake/fake-model", Access::NoTools, dir.path());
        spec.prompt = vec![InputPart {
            kind: InputPartKind::StepInput,
            label: "evidence".to_owned(),
            text: "Approve the merge of card 3?".to_owned(),
            approx_tokens: 8,
            source: None,
        }];
        spec.json_schema = Some(schema.clone());
        let handle = worker.start(spec).await.unwrap();
        let (_, end) = tokio::time::timeout(Duration::from_secs(20), drain(handle))
            .await
            .expect("the session ended");

        assert_eq!(end.reason, EndReason::Completed, "{api:?}");
        assert_eq!(end.structured.as_ref(), Some(&verdict), "{api:?}");
        let requests = server.requests();
        assert_eq!(requests.len(), 1, "{api:?}");
        let path = match api {
            EndpointApi::ChatCompletions => "/v1/chat/completions",
            EndpointApi::Responses => "/v1/responses",
        };
        assert_eq!(requests[0].path, path);
        assert_eq!(requests[0].body["model"], json!("fake-model"), "{api:?}");
    }
}
