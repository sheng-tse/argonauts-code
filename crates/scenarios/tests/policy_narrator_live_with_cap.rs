//! A live narrator writes `※ narrator:` lines while the run works, and at its spend cap it
//! stops with a notice instead of spending more.

mod policy_support;

use std::fs;
use std::time::Duration;

use argonauts_scenarios::WAIT;
use argonauts_testkit::journal;
use policy_support::{
    RUN_WAIT, invocations_of, launch_ready, merge_opened, solo_config, start_solo,
    trusted_scenario, wait_for_events,
};

const NARRATOR_MARK: &str = "NARRATOR-MARK-4412";

#[test]
fn policy_narrator_live_with_cap() -> anyhow::Result<()> {
    // One narration reports 13464 input tokens, well past the cap.
    // Codex names no model it ran, so the narrator names one.
    let scenario = trusted_scenario(
        "narrator.toml",
        &solo_config(
            "[ui]\nnarrator = \"live\"\n\n\
             [narrator]\nmodel = \"codex/gpt-5.6-luna\"\ninterval = \"1s\"\nspend_cap = \"1k tokens\"\n",
        ),
    )?;
    let mut pty = launch_ready(&scenario, &[])?;
    start_solo(&mut pty)?;

    pty.wait_for("※ narrator:", WAIT)?;
    pty.wait_for("(?i)narrator.*(cap|limit)", WAIT)?;
    wait_for_events(&scenario, "the merge checkpoint", RUN_WAIT, merge_opened)?;
    // The owner worked for 8 seconds at a 1 second interval; the cap held it to one call.
    std::thread::sleep(Duration::from_secs(3));
    let narrations = invocations_of(&scenario, "codex")?;
    assert_eq!(
        narrations.len(),
        1,
        "the narrator kept spending: {narrations:?}"
    );

    let run = journal::latest_run(scenario.repo.home()).expect("a run");
    let said = fs::read_to_string(run.join("narrator.jsonl"))?;
    assert!(said.contains(NARRATOR_MARK), "narrator.jsonl: {said}");
    scenario.shot(&pty, "policy_narrator_live_with_cap")
}
