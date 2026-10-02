//! An MCP client starts a run through `argonauts mcp serve` and reads its events up to the
//! merge checkpoint, which waits for you. The run records the client as its front end.

mod policy_support;

use policy_support::{
    FrontEnd, McpServer, RUN_WAIT, SUPERVISED_MERGE, TASK, merge_opened, merge_waits, run_started,
    runtime, solo_config, trusted_scenario, wait_for_events,
};
use serde_json::json;

#[test]
fn policy_mcp_start_and_watch_run() -> anyhow::Result<()> {
    let scenario = trusted_scenario("owner-writes-hello.toml", &solo_config(SUPERVISED_MERGE))?;
    let run = runtime()?.block_on(async {
        let server = McpServer::start(&scenario, FrontEnd::OpenClaw).await?;
        let started = server
            .call_ok(
                "argonauts_start_run",
                json!({ "request": TASK, "mode": "solo" }),
            )
            .await?;
        let run = started["run"].as_str().unwrap_or_default().to_owned();
        assert!(run.starts_with("run-"), "{started}");

        let events = server.events_until(&run, RUN_WAIT, merge_waits).await?;
        assert_eq!(events[0]["seq"], json!(0));
        assert_eq!(events[0]["type"], json!("run"));
        assert_eq!(events[0]["event"], json!("started"));
        let seqs: Vec<u64> = events
            .iter()
            .filter_map(|event| event["seq"].as_u64())
            .collect();
        assert!(
            seqs.windows(2).all(|pair| pair[1] == pair[0] + 1),
            "{seqs:?}"
        );

        let listed = server.call_ok("argonauts_list_runs", json!({})).await?;
        let row = listed["runs"]
            .as_array()
            .and_then(|runs| runs.iter().find(|row| row["run"] == json!(run)))
            .cloned()
            .unwrap_or_default();
        assert_eq!(row["needs_you"], json!(true), "{listed}");
        server.close().await?;
        anyhow::Ok(run)
    })?;

    let events = wait_for_events(&scenario, "the merge checkpoint", RUN_WAIT, merge_opened)?;
    let started = run_started(&events).expect("RunStarted");
    assert_eq!(events[0].run_id.as_str(), run);
    assert_eq!(started.front_end.as_deref(), Some("openclaw"));
    assert_eq!(started.request, TASK);
    Ok(())
}
