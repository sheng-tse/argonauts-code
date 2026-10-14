use std::fs;

use argonauts_core::names::RoleName;
use argonauts_core::settings::approvals::ApprovalMode;

use super::*;
use crate::{SettingsSources, resolve};

/// A settings file as Argonauts wrote it before its words changed, with its comments.
const BEFORE: &str = r#"# The harnesses and endpoints that work on crew runs
crew = ["claude-code", "codex", "antigravity-cli"]
# Who you chat with, and who works solo: a harness, or harness/model
default_agent = "claude-code"
# How the crew works: peer-crew plans and judges as peers, coordinated-crew has one coordinator
preset = "solo"

[approvals]
# Full auto for every checkpoint a run opens that its own mode leaves to auto
full_auto = false

[approvals.goal]
# The goal checkpoint: supervised waits for you, shadow also records what auto would do
mode = "auto"

[approvals.escalation]
# A stuck card: supervised waits for you, shadow also records what auto would do
mode = "shadow"

[approvals.budget]
# More budget from the run's reserve
mode = "auto"

[ui]
# Key bindings: the usual keys, or vim keys for lists and the prompt
keys = "vim"

[roles.coordinator]
# Who fills this role, in order. Empty lets the router pick from the crew
workers = ["claude-code"]
"#;

fn table(text: &str) -> toml::Table {
    text.parse().unwrap()
}

fn at<'a>(table: &'a toml::Table, key: &str) -> Option<&'a toml::Value> {
    let mut parts = key.split('.');
    let first = table.get(parts.next()?)?;
    parts.try_fold(first, |value, part| value.as_table()?.get(part))
}

#[test]
fn an_old_file_reads_in_the_new_names() {
    let mut layer = table(BEFORE);
    let found = read_as_new(&mut layer);

    assert_eq!(at(&layer, "workflow").unwrap().as_str(), Some("solo"));
    assert_eq!(
        at(&layer, "main_agent").unwrap().as_str(),
        Some("claude-code")
    );
    assert_eq!(
        at(&layer, "approvals.autopilot").unwrap().as_bool(),
        Some(false)
    );
    assert_eq!(
        at(&layer, "approvals.stuck.mode").unwrap().as_str(),
        Some("auto_preview")
    );
    assert_eq!(
        at(&layer, "approvals.limit.mode").unwrap().as_str(),
        Some("auto")
    );
    assert!(at(&layer, "roles.orchestrator.workers").is_some());
    for gone in [
        "preset",
        "default_agent",
        "approvals.full_auto",
        "approvals.escalation",
        "approvals.budget",
        "roles.coordinator",
    ] {
        assert!(at(&layer, gone).is_none(), "{gone} is still there");
    }
    let old: Vec<&str> = found.iter().map(|name| name.old.as_str()).collect();
    assert_eq!(
        old,
        [
            "preset",
            "default_agent",
            "roles.coordinator",
            "approvals.full_auto",
            "approvals.escalation",
            "approvals.budget",
            "approvals.stuck.mode = \"shadow\"",
        ]
    );
}

#[test]
fn the_new_name_wins_when_a_file_sets_both() {
    let mut layer = table("default_agent = \"codex\"\nmain_agent = \"claude-code\"\n");
    read_as_new(&mut layer);
    assert_eq!(
        at(&layer, "main_agent").unwrap().as_str(),
        Some("claude-code")
    );
    assert!(at(&layer, "default_agent").is_none());
}

#[test]
fn a_preset_makes_the_old_workflow_key_the_phases() {
    let mut layer = table("preset = \"peer-crew\"\nworkflow = \"coordinated-crew\"\n");
    read_as_new(&mut layer);
    assert_eq!(at(&layer, "workflow").unwrap().as_str(), Some("council"));
    assert_eq!(at(&layer, "phases").unwrap().as_str(), Some("orchestrator"));

    // Without a preset, `workflow` is already today's key.
    let mut layer = table("workflow = \"council\"\n");
    assert!(read_as_new(&mut layer).is_empty());
}

#[test]
fn names_inside_moved_tables_and_lists_follow() {
    let mut layer = table(
        r#"
[budget]
run_usd = 5.0
soft_stop = 0.8
on_hit = "handoff"
[budget.lines]
cards = 0.5
[approvals.merge.rules]
max_combined_diff_lines = 400
[ui.keymap]
widget_quota = "ctrl+q"
[ui.above_input]
widgets = ["cards", "quota"]
"#,
    );
    read_as_new(&mut layer);
    assert_eq!(
        at(&layer, "limits.spend_per_run").unwrap().as_float(),
        Some(5.0)
    );
    assert_eq!(
        at(&layer, "limits.soft_stop").unwrap().as_float(),
        Some(0.8)
    );
    assert_eq!(
        at(&layer, "limits.when_reached").unwrap().as_str(),
        Some("fallback")
    );
    assert_eq!(
        at(&layer, "limits.lines.tasks").unwrap().as_float(),
        Some(0.5)
    );
    assert_eq!(
        at(&layer, "approvals.rules.merge.max_changed_lines")
            .unwrap()
            .as_integer(),
        Some(400)
    );
    assert_eq!(
        at(&layer, "ui.keymap.mod_quota").unwrap().as_str(),
        Some("ctrl+q")
    );
    let mods = at(&layer, "ui.above_input.mods")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(mods[0].as_str(), Some("tasks"));
    assert!(at(&layer, "budget").is_none());
    assert!(at(&layer, "approvals.merge").is_none());
}

/// A home folder with only a global settings file, as the app lays it out.
fn home_with(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, text).unwrap();
    (dir, file)
}

#[test]
fn an_old_file_resolves_with_a_warning_that_names_its_old_names() {
    let (_home, file) = home_with(BEFORE);
    let resolved = resolve(&SettingsSources {
        global: Some(file),
        ..SettingsSources::default()
    })
    .unwrap();

    let settings = &resolved.settings;
    assert_eq!(settings.workflow, "solo");
    assert_eq!(settings.main_agent.to_string(), "claude-code");
    assert!(!settings.approvals.autopilot);
    assert_eq!(
        settings.approvals.escalation.mode,
        ApprovalMode::AutoPreview
    );
    let orchestrator = &settings.roles[&RoleName::Orchestrator];
    assert_eq!(orchestrator.workers.len(), 1);
    assert!(
        resolved
            .warnings
            .iter()
            .any(|warning| warning.contains("preset → workflow")),
        "{:?}",
        resolved.warnings
    );
}

#[test]
fn the_rewrite_keeps_the_file_as_it_was_and_happens_once() {
    let (_home, file) = home_with(BEFORE);

    let renamed = rename_old_names(&file).unwrap().unwrap();
    assert_eq!(fs::read_to_string(&renamed.backup).unwrap(), BEFORE);
    assert_eq!(renamed.backup.file_name().unwrap(), "config.toml.bak");
    assert!(renamed.listed().starts_with("preset → workflow, "));

    let now = fs::read_to_string(&file).unwrap();
    let mut layer = table(&now);
    assert!(read_as_new(&mut layer).is_empty(), "{now}");
    assert!(now.contains("workflow = \"solo\""), "{now}");
    assert!(now.contains("[roles.orchestrator]"), "{now}");
    assert!(now.contains("[approvals.stuck]"), "{now}");
    // Comments you didn't write in older words stay as they are.
    assert!(now.contains("# Key bindings: the usual keys"), "{now}");
    assert!(!now.contains("Full auto for every"), "{now}");
    // The tables keep their places in the file.
    assert!(now.find("[approvals.stuck]") < now.find("[ui]"), "{now}");

    assert_eq!(rename_old_names(&file).unwrap(), None);
    assert!(!file.with_file_name("config.toml.bak2").exists());
}

#[test]
fn a_second_rewrite_keeps_the_first_backup() {
    let (_home, file) = home_with("default_agent = \"codex\"\n");
    fs::write(file.with_file_name("config.toml.bak"), "kept").unwrap();
    let renamed = rename_old_names(&file).unwrap().unwrap();
    assert_eq!(renamed.backup.file_name().unwrap(), "config.toml.bak2");
    assert_eq!(
        fs::read_to_string(file.with_file_name("config.toml.bak")).unwrap(),
        "kept"
    );
}

#[test]
fn nothing_is_written_for_a_missing_or_broken_file() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("config.toml");
    assert_eq!(rename_old_names(&missing).unwrap(), None);
    fs::write(&missing, "default_agent = \n").unwrap();
    assert_eq!(rename_old_names(&missing).unwrap(), None);
    assert!(!dir.path().join("config.toml.bak").exists());
}
