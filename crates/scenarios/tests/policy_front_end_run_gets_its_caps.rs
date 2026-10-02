//! A run a front end starts gets `mcp.run_budget` as its time budget and counts against
//! `mcp.runs_per_day`, across every repo, so the next start past the cap is refused.

mod policy_support;

use argonauts_testkit::journal;
use policy_support::{
    FrontEnd, McpServer, RUN_WAIT, TASK, failed, run_started, runtime, solo_config, text,
    trusted_scenario, wait_for_events,
};
use serde_json::json;

#[test]
fn policy_front_end_run_gets_its_caps() -> anyhow::Result<()> {
    let scenario = trusted_scenario(
        "owner-writes-hello.toml",
        &solo_config("[mcp]\nrun_budget = \"1m\"\nruns_per_day = 1\n"),
    )?;
    let run = runtime()?.block_on(async {
        let server = McpServer::start(&scenario, FrontEnd::Hermes).await?;
        let started = server
            .call_ok(
                "argonauts_start_run",
                json!({ "request": TASK, "mode": "solo" }),
            )
            .await?;
        assert_eq!(started["front_end_runs_today"], json!(1), "{started}");
        let run = started["run"].as_str().unwrap_or_default().to_owned();

        let second = server
            .call(
                "argonauts_start_run",
                json!({ "request": TASK, "mode": "solo" }),
            )
            .await?;
        assert!(
            failed(&second),
            "a second run passed the daily cap: {}",
            text(&second)
        );
        server.close().await?;
        anyhow::Ok(run)
    })?;

    let events = wait_for_events(&scenario, "RunStarted", RUN_WAIT, |events| {
        run_started(events).is_some()
    })?;
    let started = run_started(&events).expect("RunStarted");
    assert_eq!(events[0].run_id.as_str(), run);
    assert_eq!(started.front_end.as_deref(), Some("hermes"));
    // The run's supervisor pauses a front-end run once the `mcp.run_budget` it was started
    // with is spent, so the run carries that budget in its own settings.
    assert_eq!(
        started.settings["mcp"]["run_budget"].as_str(),
        Some("1m"),
        "the run's mcp settings: {}",
        started.settings["mcp"]
    );
    assert_eq!(journal::run_dirs(scenario.repo.home()).len(), 1);
    assert!(scenario.repo.home().join("daily.json").is_file());
    Ok(())
}
