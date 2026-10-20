//! An auto-merged run's merge is reverted on main. The next run host start notices, drops
//! merges back to auto preview with a notice, and the next run's merge waits for you again.

use crate::policy_support;
use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_scenarios::WAIT;
use policy_support::{
    AUTO_MERGE_RULES, RUN_WAIT, captain_config, launch_ready, merge_opened, opened_of_kind,
    run_finished, start_captain, trusted_scenario, wait_for_events,
};

#[test]
fn policy_revert_of_auto_merge_demotes() -> anyhow::Result<()> {
    let scenario = trusted_scenario(
        "worker-writes-hello.toml",
        &captain_config(AUTO_MERGE_RULES),
    )?;
    let mut pty = launch_ready(&scenario, &[])?;
    start_captain(&mut pty)?;
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
    // The next run's run host is the next start, and it checks before the run goes on.
    start_captain(&mut pty)?;
    pty.wait_for("(?i)revert", WAIT)?;
    pty.wait_for("(?i)auto preview", WAIT)?;
    let events = wait_for_events(&scenario, "the next run's merge", RUN_WAIT, |events| {
        events.first().map(|event| &event.run_id) != first_run.as_ref() && merge_opened(events)
    })?;
    let merge = opened_of_kind(&events, ApprovalKind::Merge);
    assert_eq!(merge[0].mode, ApprovalMode::AutoPreview);
    assert!(merge[0].needs_you, "an auto preview merge waits for you");
    scenario.shot(&pty, "policy_revert_of_auto_merge_demotes")
}
