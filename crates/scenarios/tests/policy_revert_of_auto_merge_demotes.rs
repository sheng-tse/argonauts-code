//! An auto-merged run's merge is reverted on main. The next supervisor start notices, drops
//! merges back to shadow with a notice, and the next run's merge waits for you again.

mod policy_support;

use argonauts_core::events::checkpoint::CheckpointKind;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_scenarios::WAIT;
use policy_support::{
    AUTO_MERGE_RULES, RUN_WAIT, launch_ready, merge_opened, opened_of_kind, run_finished,
    solo_config, start_solo, trusted_scenario, wait_for_events,
};

#[test]
fn policy_revert_of_auto_merge_demotes() -> anyhow::Result<()> {
    let scenario = trusted_scenario("owner-writes-hello.toml", &solo_config(AUTO_MERGE_RULES))?;
    let mut pty = launch_ready(&scenario, &[])?;
    start_solo(&mut pty)?;
    wait_for_events(&scenario, "the auto-merged run", RUN_WAIT, run_finished)?;
    assert_eq!(scenario.repo.git(&["show", "main:hello.txt"])?, "hi");
    pty.kill();

    let merged = scenario.repo.git(&["rev-parse", "main"])?;
    let parents = scenario
        .repo
        .git(&["rev-list", "--parents", "-n", "1", &merged])?;
    scenario.repo.git(&["checkout", "--quiet", "main"])?;
    if parents.split_whitespace().count() > 2 {
        scenario
            .repo
            .git(&["revert", "--no-edit", "-m", "1", &merged])?;
    } else {
        scenario.repo.git(&["revert", "--no-edit", &merged])?;
    }

    let mut pty = launch_ready(&scenario, &[])?;
    let first_run = scenario
        .latest_run_events()?
        .first()
        .map(|event| event.run_id.clone());
    // The next run's supervisor is the next start, and it checks before the run goes on.
    start_solo(&mut pty)?;
    pty.wait_for("(?i)revert", WAIT)?;
    pty.wait_for("(?i)shadow", WAIT)?;
    let events = wait_for_events(&scenario, "the next run's merge", RUN_WAIT, |events| {
        events.first().map(|event| &event.run_id) != first_run.as_ref() && merge_opened(events)
    })?;
    let merge = opened_of_kind(&events, CheckpointKind::Merge);
    assert_eq!(merge[0].mode, ApprovalMode::Shadow);
    assert!(merge[0].needs_you, "a shadow merge waits for you");
    scenario.shot(&pty, "policy_revert_of_auto_merge_demotes")
}
