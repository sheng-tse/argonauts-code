//! A maker-checker task whose reviewer is an endpoint model, `myapi/fake-model`, served by the
//! fake OpenAI-compatible server. The fake Claude Code owns the task.

use crate::agents_support;
use agents_support::{
    assert_sessions_completed, finish_screen, invocations_of, launch_ready, reviews, scenario,
    start_captain, toml_string, try_workers, wait_for_finished_task, write_global_config,
    write_global_models,
};
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_testkit::{FakeOpenAi, FakeReply};
use serde_json::json;

const KEY_ENV: &str = "ARGONAUTS_TEST_MYAPI_KEY";

#[test]
fn workers_endpoint_reviewer_task() -> anyhow::Result<()> {
    let server = FakeOpenAi::start()?;
    let approve = json!({
        "verdict": "approve",
        "summary": "hello.txt holds the word hi, as asked.",
        "comments": [],
        "tamper_suspected": false,
    });
    // One review is expected. The second covers a retry of a malformed answer.
    server.push(FakeReply::json(&approve));
    server.push(FakeReply::json(&approve));

    let scenario = scenario("claude-worker.toml")?;
    write_global_config(
        &scenario,
        &format!(
            "workflow = \"maker-checker\"\n\
             crew.main = \"claude-code\"\n\n\
             [captain]\nreview = true\n\n\
             [roles.reviewer]\nmodels = [\"myapi/fake-model\"]\n\n\
             [endpoints.myapi]\n\
             base_url = {}\n\
             api_key_env = \"{KEY_ENV}\"\n\
             allow_api_billing = true\n",
            toml_string(&server.base_url())
        ),
    )?;
    write_global_models(
        &scenario,
        "[[model]]\n\
         id = \"myapi/fake-model\"\n\
         provider = \"alibaba\"\n\
         kind = \"endpoint\"\n\
         access = \"api-key\"\n\
         tools = false\n\
         cost = { unit = \"usd\", input_per_mtok = 0.0, output_per_mtok = 0.0 }\n",
    )?;

    let mut pty = launch_ready(&scenario, &[], &[(KEY_ENV, "test-key")])?;
    start_captain(&mut pty)?;
    let events = wait_for_finished_task(&scenario)?;

    let workers: Vec<String> = try_workers(&events)
        .iter()
        .map(|model| model.agent.clone())
        .collect();
    assert_eq!(workers, ["claude-code"]);
    assert_sessions_completed(&events, "claude-code");
    assert!(!invocations_of(&scenario, "claude")?.is_empty());

    let reviews = reviews(&events);
    assert_eq!(reviews.len(), 1, "{reviews:?}");
    let (reviewer, review) = reviews[0];
    assert_eq!(reviewer.to_string(), "myapi/fake-model");
    assert_eq!(review.verdict, ReviewVerdict::Approve);

    let requests: Vec<_> = server
        .requests()
        .into_iter()
        .filter(|request| request.path.ends_with("/chat/completions"))
        .collect();
    assert_eq!(requests.len(), 1, "{requests:?}");
    let request = &requests[0];
    assert_eq!(request.body["model"], "fake-model");
    assert_eq!(request.header("authorization"), Some("Bearer test-key"));
    // The review request carries the task's diff.
    assert!(
        request.body.to_string().contains("hello.txt"),
        "{}",
        request.body
    );

    finish_screen(&scenario, &pty, "workers_endpoint_reviewer_task")
}
