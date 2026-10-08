//! Checks of the scenario helpers themselves, which every other `app_*` test leans on.

mod app_support;

use app_support::*;
use argonauts_scenarios::scenario_file;
use argonauts_testkit::FakeScenario;
use serde_json::Value;

#[test]
fn app_helpers_read_settings_files() {
    let text = "\
# The main agent.
main_agent = \"claude-code\"
crew = [
  \"claude-code\",
  \"codex\", # the second
]
workflow = \"solo\"

# Approvals
[approvals.merge]
mode = \"auto\"

[ui]
# Vim or default keys.
keys = 'vim'
theme = \"dark\" # trailing
";
    let entries = toml_entries(text);
    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "main_agent",
            "crew",
            "workflow",
            "approvals.merge.mode",
            "ui.keys",
            "ui.theme"
        ]
    );
    let commented: Vec<bool> = entries.iter().map(|e| e.commented).collect();
    assert_eq!(commented, [true, false, false, true, true, false]);
    assert!(same_toml(
        &toml_value(text, "crew").unwrap(),
        "[\"claude-code\", \"codex\"]"
    ));
    assert!(same_toml(&toml_value(text, "ui.keys").unwrap(), "\"vim\""));
    assert_eq!(toml_value(text, "ui.theme").unwrap(), "\"dark\"");
    let json: Value = serde_json::json!({"roles": {"reviewer": {"effort": "high"}}});
    assert_eq!(json_at(&json, "roles.reviewer.effort").unwrap(), "high");
    assert!(json_at(&json, "roles.coder").is_none());
}

#[test]
fn app_helpers_load_the_vendor_scenario() {
    let scenario = FakeScenario::load(&scenario_file(VENDORS)).unwrap();
    assert_eq!(scenario.version.len(), 3);
    for program in ["claude", "codex", "agy"] {
        assert!(
            scenario
                .rules
                .iter()
                .any(|rule| rule.argv0.as_deref() == Some(program)),
            "no rule for {program}"
        );
    }
}

#[test]
fn app_helpers_match_words_and_flags() {
    let args: Vec<String> = ["exec", "--json", "resume", "t-1", "-"]
        .map(str::to_owned)
        .to_vec();
    assert!(has_words(&args, &["resume", "t-1"]));
    assert!(!has_words(&args, &["resume", "t-2"]));
    assert!(has_flag(&args, "--json", "resume"));
    assert!(same_toml("[ \"a\", \"b\", ]", "[\"a\",\"b\"]"));
}
