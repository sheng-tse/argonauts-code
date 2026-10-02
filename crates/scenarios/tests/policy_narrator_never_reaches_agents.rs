//! What the narrator says is for you only: it never reaches an agent's `status.json`, the
//! run's journal, or any prompt an agent gets.

mod policy_support;

use std::fs;
use std::time::Duration;

use argonauts_scenarios::WAIT;
use argonauts_testkit::journal;
use policy_support::{
    RUN_WAIT, files_named, invocations_of, launch_ready, merge_opened, solo_config, start_solo,
    trusted_scenario, wait_for_events,
};

const NARRATOR_MARK: &str = "NARRATOR-MARK-4412";

#[test]
fn policy_narrator_never_reaches_agents() -> anyhow::Result<()> {
    // Codex names no model it ran, so the narrator names one.
    let scenario = trusted_scenario(
        "narrator.toml",
        &solo_config(
            "[ui]\nnarrator = \"live\"\n\n\
             [narrator]\nmodel = \"codex/gpt-5.6-luna\"\ninterval = \"1s\"\nspend_cap = \"0.50 usd\"\n",
        ),
    )?;
    let mut pty = launch_ready(&scenario, &[])?;
    start_solo(&mut pty)?;
    pty.wait_for(NARRATOR_MARK, WAIT)?;
    // Let the owner's session start after at least one narration.
    std::thread::sleep(Duration::from_secs(2));
    wait_for_events(&scenario, "the merge checkpoint", RUN_WAIT, merge_opened)?;

    let run = journal::latest_run(scenario.repo.home()).expect("a run");
    let narrated = fs::read_to_string(run.join("narrator.jsonl"))?;
    assert!(narrated.contains(NARRATOR_MARK), "the narrator never spoke");

    let journal_text = fs::read_to_string(run.join("events.jsonl"))?;
    assert!(
        !journal_text.contains(NARRATOR_MARK),
        "narration reached events.jsonl"
    );
    let status_files = files_named(&run, "status.json");
    for status in &status_files {
        let text = fs::read_to_string(status)?;
        assert!(
            !text.contains(NARRATOR_MARK),
            "narration reached {}",
            status.display()
        );
    }
    let agent_calls = invocations_of(&scenario, "claude")?;
    assert!(!agent_calls.is_empty(), "no agent ran");
    for call in &agent_calls {
        let prompt = call.stdin.as_deref().unwrap_or_default();
        assert!(
            !prompt.contains(NARRATOR_MARK),
            "narration reached a prompt"
        );
        assert!(
            !call.args.iter().any(|arg| arg.contains(NARRATOR_MARK)),
            "narration reached an agent's arguments"
        );
    }
    scenario.shot(&pty, "policy_narrator_never_reaches_agents")
}
