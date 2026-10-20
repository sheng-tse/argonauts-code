//! The same reviewed task through the built-in `ollama` endpoint, its `base_url` pointed at the
//! fake OpenAI-compatible server. A local endpoint needs no key.

use crate::agents_support;
use agents_support::{
    assert_sessions_completed, finish_screen, launch_ready, reviews, scenario, start_captain,
    toml_string, wait_for_finished_task, write_global_config, write_global_models,
};
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_testkit::{FakeOpenAi, FakeReply};
use serde_json::json;

#[test]
fn workers_ollama_endpoint_reviews_task() -> anyhow::Result<()> {
    let server = FakeOpenAi::start()?;
    let approve = json!({
        "verdict": "approve",
        "summary": "hello.txt holds the word hi, as asked.",
        "comments": [],
        "tamper_suspected": false,
    });
    server.push(FakeReply::json(&approve));
    server.push(FakeReply::json(&approve));

    let scenario = scenario("claude-worker.toml")?;
    write_global_config(
        &scenario,
        &format!(
            "workflow = \"maker-checker\"\n\
             crew.main = \"claude-code\"\n\n\
             [captain]\nreview = true\n\n\
             [roles.reviewer]\nmodels = [\"ollama/fake-model\"]\n\n\
             [endpoints.ollama]\n\
             base_url = {}\n",
            toml_string(&server.base_url())
        ),
    )?;
    // Review needs another provider than the worker's, so the model's provider is declared.
    write_global_models(
        &scenario,
        "[[model]]\n\
         id = \"ollama/fake-model\"\n\
         provider = \"meta\"\n\
         kind = \"endpoint\"\n\
         access = \"local\"\n\
         tools = false\n\
         cost = { unit = \"none\" }\n",
    )?;

    let mut pty = launch_ready(&scenario, &[], &[])?;
    start_captain(&mut pty)?;
    let events = wait_for_finished_task(&scenario)?;

    assert_sessions_completed(&events, "claude-code");
    let reviews = reviews(&events);
    assert_eq!(reviews.len(), 1, "{reviews:?}");
    let (reviewer, review) = reviews[0];
    assert_eq!(reviewer.to_string(), "ollama/fake-model");
    assert_eq!(review.verdict, ReviewVerdict::Approve);

    let requests: Vec<_> = server
        .requests()
        .into_iter()
        .filter(|request| request.path.ends_with("/chat/completions"))
        .collect();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].body["model"], "fake-model");

    finish_screen(&scenario, &pty, "workers_ollama_endpoint_reviews_task")
}
