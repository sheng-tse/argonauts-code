//! In auto mode with rules deciding, a small solo change that passes the rules merges without
//! asking, the decision is recorded as the rules', and the auto-merge counts toward the day.

mod policy_support;

use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::events::run::Party;
use policy_support::{
    AUTO_MERGE_RULES, RUN_WAIT, launch_ready, merge_decided, run_finished, solo_config, start_solo,
    trusted_scenario, wait_for_events,
};

#[test]
fn policy_auto_mode_merges_under_rules() -> anyhow::Result<()> {
    let scenario = trusted_scenario("owner-writes-hello.toml", &solo_config(AUTO_MERGE_RULES))?;
    let mut pty = launch_ready(&scenario, &[])?;
    start_solo(&mut pty)?;

    let events = wait_for_events(&scenario, "the run to finish", RUN_WAIT, run_finished)?;
    let decided = merge_decided(&events).expect("the merge was decided");
    assert_eq!(decided.by, Party::Rules);
    assert!(
        matches!(decided.action, AnswerAction::Merge | AnswerAction::Approve),
        "{:?}",
        decided.action
    );
    assert_eq!(scenario.repo.git(&["show", "main:hello.txt"])?, "hi");
    assert!(
        scenario.repo.home().join("daily.json").is_file(),
        "the auto-merge wasn't counted for the day"
    );
    scenario.shot(&pty, "policy_auto_mode_merges_under_rules")
}
