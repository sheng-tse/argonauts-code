//! The same reviewed card through the built-in `ollama` endpoint, its `base_url` pointed at the
//! fake OpenAI-compatible server. A local endpoint needs no key.

mod workers_support;

use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_testkit::{FakeOpenAi, FakeReply};
use serde_json::json;
use workers_support::{
    assert_sessions_completed, finish_screen, launch_ready, reviews, scenario, start_solo,
    toml_string, wait_for_finished_card, write_global_config, write_global_models,
};

#[test]
fn workers_ollama_endpoint_reviews_card() -> anyhow::Result<()> {
    let server = FakeOpenAi::start()?;
    let approve = json!({
        "verdict": "approve",
        "summary": "hello.txt holds the word hi, as asked.",
        "comments": [],
        "tamper_suspected": false,
    });
    server.push(FakeReply::json(&approve));
    server.push(FakeReply::json(&approve));

    let scenario = scenario("claude-owner.toml")?;
    write_global_config(
        &scenario,
        &format!(
            "workflow = \"maker-checker\"\n\
             main_agent = \"claude-code\"\n\n\
             [solo]\nreview = true\n\n\
             [roles.reviewer]\nworkers = [\"ollama/fake-model\"]\n\n\
             [endpoints.ollama]\n\
             base_url = {}\n",
            toml_string(&server.base_url())
        ),
    )?;
    // Review needs another family than the owner's, so the model's family is declared.
    write_global_models(
        &scenario,
        "[[model]]\n\
         id = \"ollama/fake-model\"\n\
         family = \"llama\"\n\
         kind = \"endpoint\"\n\
         access = \"local\"\n\
         tools = false\n\
         cost = { unit = \"none\" }\n",
    )?;

    let mut pty = launch_ready(&scenario, &[], &[])?;
    start_solo(&mut pty)?;
    let events = wait_for_finished_card(&scenario)?;

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

    finish_screen(&scenario, &pty, "workers_ollama_endpoint_reviews_card")
}
