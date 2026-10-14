//! The defaults that let agents work on their own, and the sandbox each role's agents get.

use argonauts_core::names::RoleName;
use argonauts_core::settings::approvals::{
    ApprovalMode, CheckpointSettings, DecideWith, Otherwise, RoleSandbox, SandboxChoice,
};
use argonauts_core::settings::handoff::HandoffMode;
use argonauts_core::settings::{SAFETY_KEYS, Settings};
use serde_json::json;

fn rule(checkpoint: &CheckpointSettings) -> (ApprovalMode, DecideWith, Otherwise) {
    (
        checkpoint.mode,
        checkpoint.decide_with,
        checkpoint.otherwise,
    )
}

#[test]
fn agents_work_on_their_own_by_default_and_ask_when_a_rule_fails() {
    let settings = Settings::default();

    let sandbox = &settings.sandbox;
    assert_eq!(sandbox.mode, SandboxChoice::Os);
    assert!(sandbox.network);
    assert!(sandbox.agents_commit);
    assert!(!sandbox.agents_see_other_worktrees);
    assert!(settings.roles.is_empty());

    assert_eq!(settings.fallback.mode, HandoffMode::Auto);
    assert!(settings.fallback.order.is_empty() && settings.fallback.replace.is_empty());

    let approvals = &settings.approvals;
    let reviewed = (
        ApprovalMode::Auto,
        DecideWith::RulesAndModel,
        Otherwise::Ask,
    );
    assert_eq!(rule(&approvals.goal), reviewed);
    assert_eq!(rule(&approvals.plan), reviewed);
    assert_eq!(rule(&approvals.escalation), reviewed);
    assert_eq!(rule(&approvals.budget), reviewed);
    assert_eq!(rule(&approvals.change_request), reviewed);
    assert_eq!(
        rule(&approvals.merge),
        (ApprovalMode::Auto, DecideWith::Rules, Otherwise::Ask)
    );
    // Nothing caps automatic merges until you set a cap.
    assert_eq!(settings.limits.auto_merges_per_day, None);
    // Promoting a recipe change is automatic too: its replays pass first, and a canary watches it.
    assert_eq!(
        rule(&approvals.workflow_promotion.as_checkpoint()),
        (ApprovalMode::Auto, DecideWith::Rules, Otherwise::Ask)
    );
    // Unset, so the policy builder picks the cheapest model from another company.
    assert_eq!(approvals.approver_model(), None);
    assert!(approvals.approver.different_provider);
}

#[test]
fn sandbox_modes_keep_their_words_and_off_reads_as_the_tools_own() {
    let cases = [
        ("os", SandboxChoice::Os),
        ("container", SandboxChoice::Container),
        ("tool", SandboxChoice::Tool),
        ("none", SandboxChoice::None),
        ("off", SandboxChoice::Tool),
    ];
    for (word, mode) in cases {
        let settings: Settings = toml::from_str(&format!("[sandbox]\nmode = {word:?}\n")).unwrap();
        assert_eq!(settings.sandbox.mode, mode, "{word}");
        assert_eq!(serde_json::to_value(mode).unwrap(), json!(mode.as_str()));
        assert_eq!(mode.ours_off(), matches!(word, "tool" | "none" | "off"));
    }
    let mut settings = Settings::default();
    settings.sandbox.mode = SandboxChoice::Tool;
    let table = toml::Table::try_from(&settings).unwrap();
    assert_eq!(table["sandbox"]["mode"].as_str(), Some("tool"));
    assert!(toml::from_str::<Settings>("[sandbox]\nmode = \"loose\"\n").is_err());
}

#[test]
fn a_roles_sandbox_keys_win_and_the_rest_come_from_the_top_level() {
    let settings: Settings = toml::from_str(
        r#"
[sandbox]
network = false
extra_writable = ["../shared"]

[roles.reviewer.sandbox]
mode = "tool"
network = true

[roles.worker.sandbox]
agents_commit = false
agents_see_other_worktrees = true

[roles.adjudicator]
effort = "low"
"#,
    )
    .unwrap();

    let reviewer = settings.sandbox_for(&RoleName::Reviewer);
    assert_eq!(reviewer.mode, SandboxChoice::Tool);
    assert!(reviewer.network);
    assert!(reviewer.agents_commit && !reviewer.agents_see_other_worktrees);
    assert_eq!(reviewer.extra_writable, ["../shared"]);

    let owner = settings.sandbox_for(&RoleName::Worker);
    assert_eq!(owner.mode, SandboxChoice::Os);
    assert!(!owner.network);
    assert!(!owner.agents_commit && owner.agents_see_other_worktrees);

    // A role table without sandbox keys, and a role with no table, get the top level as is.
    assert_eq!(
        settings.sandbox_for(&RoleName::Adjudicator),
        settings.sandbox
    );
    assert_eq!(
        settings.sandbox_for(&RoleName::Orchestrator),
        settings.sandbox
    );
    assert_eq!(settings.sandbox.applied(None), settings.sandbox);
    let unset = RoleSandbox::default();
    assert!(unset.is_empty());
    assert_eq!(settings.sandbox.applied(Some(&unset)), settings.sandbox);

    // Only the keys a role sets are written back.
    let table = toml::Table::try_from(&settings).unwrap();
    assert!(table["roles"]["adjudicator"].get("sandbox").is_none());
    let reviewer = table["roles"]["reviewer"]["sandbox"].as_table().unwrap();
    assert_eq!(reviewer.len(), 2);
    let back: Settings = toml::from_str(&toml::to_string(&settings).unwrap()).unwrap();
    assert_eq!(back, settings);
}

#[test]
fn a_roles_sandbox_passes_the_schema() {
    let mut settings = Settings::default();
    let reviewer = settings.roles.entry(RoleName::Reviewer).or_default();
    reviewer.sandbox = RoleSandbox {
        mode: Some(SandboxChoice::None),
        network: Some(false),
        agents_see_other_worktrees: Some(true),
        agents_commit: Some(false),
    };
    let schema = schemars::schema_for!(Settings).to_value();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let value = serde_json::to_value(&settings).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(serde_json::from_value::<Settings>(value).unwrap(), settings);
}

#[test]
fn every_sandbox_and_approval_key_stays_a_safety_key() {
    for key in ["sandbox.*", "roles.*.sandbox.*", "approvals.*", "merge.*"] {
        assert!(SAFETY_KEYS.contains(&key), "{key}");
    }
}
