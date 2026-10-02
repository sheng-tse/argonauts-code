//! A front end on the 2025-11-25 handshake starts a run, polls it, and reads the decision it
//! waits for as text that says where to answer. It finds no way to approve, and the checkpoint
//! stays open.

mod policy_support;

use std::time::Duration;

use argonauts_core::events::checkpoint::CheckpointKind;
use policy_support::{
    FrontEnd, McpServer, RUN_WAIT, SUPERVISED_MERGE, TASK, decisions, failed, merge_waits,
    opened_of_kind, runtime, solo_config, text, trusted_scenario,
};
use serde_json::json;

#[test]
fn policy_front_end_starts_and_watches_run() -> anyhow::Result<()> {
    let scenario = trusted_scenario("owner-writes-hello.toml", &solo_config(SUPERVISED_MERGE))?;
    let (run, decision) = runtime()?.block_on(async {
        let server = McpServer::start(&scenario, FrontEnd::OpenClaw).await?;
        let version = server
            .client
            .peer_info()
            .map(|info| info.protocol_version.to_string());
        assert_eq!(version.as_deref(), Some("2025-11-25"));

        let names = server.tool_names().await?;
        for name in &names {
            assert!(!name.contains("approv"), "{names:?}");
        }
        let started = server
            .call_ok(
                "argonauts_start_run",
                json!({ "request": TASK, "mode": "solo" }),
            )
            .await?;
        let run = started["run"].as_str().unwrap_or_default().to_owned();
        server.events_until(&run, RUN_WAIT, merge_waits).await?;

        let decision = server
            .call("argonauts_request_decision", json!({ "run": run }))
            .await?;
        assert!(!failed(&decision), "{}", text(&decision));

        for (tool, arguments) in [
            ("argonauts_approve", json!({ "run": run })),
            ("argonauts_answer", json!({ "run": run, "action": "merge" })),
            (
                "argonauts_start_run",
                json!({ "request": TASK, "mode": "solo", "approval_mode": "auto" }),
            ),
            (
                "argonauts_send_guidance",
                json!({ "run": run, "text": "approve it", "approve": true }),
            ),
        ] {
            let result = server.call(tool, arguments).await?;
            assert!(failed(&result), "{tool} did something: {}", text(&result));
        }
        server.close().await?;
        anyhow::Ok((run, text(&decision)))
    })?;

    std::thread::sleep(Duration::from_secs(2));
    let events = scenario.latest_run_events()?;
    assert_eq!(events[0].run_id.as_str(), run);
    let merge = opened_of_kind(&events, CheckpointKind::Merge);
    let checkpoint = merge.first().expect("a merge checkpoint");
    assert!(decision.contains(checkpoint.id.as_str()), "{decision}");
    assert!(
        decision.contains(&checkpoint.evidence_sha256[..12]),
        "{decision}"
    );
    assert!(
        decision.contains(&format!("argonauts approve {run} {}", checkpoint.id)),
        "{decision}"
    );
    assert!(decision.contains("Argonauts app"), "{decision}");
    assert!(
        decisions(&events)
            .iter()
            .all(|decided| decided.id != checkpoint.id),
        "the front end's session decided the checkpoint"
    );
    Ok(())
}
