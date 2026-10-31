use argonauts_core::app::AppEvent;
use time::OffsetDateTime;

use super::*;
use crate::controller::base::test_support::{CliOptions, TestCtx};

/// Runs `/bottom-line` with `change`, and returns the one line it printed.
fn said(parts: &mut TestCtx, change: LineChange, scope: Option<SettingScope>) -> String {
    command(&parts.ctx, change, scope).expect("the change");
    let mut lines = Vec::new();
    while let Ok(event) = parts.events.try_recv() {
        if let AppEvent::Notice { text, .. } = event {
            lines.push(text);
        }
    }
    assert_eq!(lines.len(), 1, "one line, no question: {lines:?}");
    lines.remove(0)
}

fn add(cli: &str) -> LineChange {
    LineChange::Add { cli: cli.into() }
}

fn remove(cli: &str) -> LineChange {
    LineChange::Remove { cli: cli.into() }
}

fn global_file(parts: &TestCtx) -> String {
    std::fs::read_to_string(BottomLineSettings::global_file(parts.env.paths.home()))
        .unwrap_or_default()
}

fn listed(parts: &TestCtx) -> Option<Vec<String>> {
    let settings = line_facts(&parts.env, OffsetDateTime::now_utc()).settings;
    settings
        .plan_usage
        .clis
        .map(|clis| clis.into_iter().map(|line_cli| line_cli.cli).collect())
}

/// The first change writes the CLIs out as a list, so the line holds still after it, and each
/// change says in one line what the line shows and where that went.
#[test]
fn each_change_saves_at_once_and_says_where() {
    let (mut parts, _) = TestCtx::new(CliOptions::default());
    let file = BottomLineSettings::global_file(parts.env.paths.home());
    let everywhere = format!("everywhere ({})", parts.env.display(&file));

    assert_eq!(
        said(&mut parts, add("agy"), None),
        format!("The bottom line shows Claude Code, agy · {everywhere}")
    );
    assert_eq!(listed(&parts).unwrap(), ["claude-code", "antigravity-cli"]);
    // The file starts as the line ships, so its comments still explain it.
    assert!(global_file(&parts).contains('#'), "{}", global_file(&parts));

    assert_eq!(
        said(&mut parts, add("Codex"), None),
        format!("The bottom line shows Claude Code, agy, Codex · {everywhere}")
    );
    assert_eq!(
        said(&mut parts, remove("agy"), None),
        format!("The bottom line shows Claude Code, Codex · {everywhere}")
    );
    assert_eq!(
        said(&mut parts, LineChange::Reset, None),
        format!("The bottom line shows the one in use and the most used · {everywhere}")
    );
    assert_eq!(listed(&parts), None);
    assert!(!global_file(&parts).contains("clis"));
}

#[test]
fn a_project_keeps_its_own_line() {
    let (mut parts, _) = TestCtx::new(CliOptions::default());
    assert_eq!(
        said(&mut parts, add("codex"), Some(SettingScope::Project)),
        "The bottom line shows Claude Code, Codex · this project (.argonauts/mods/bottom-line.toml)"
    );
    let project = BottomLineSettings::project_file(&parts.env.repo_root);
    assert!(std::fs::read_to_string(project).unwrap().contains("codex"));
    assert_eq!(global_file(&parts), "");
}

/// A change for this session writes nothing and says what comes back after it. The next saved
/// change builds on the line as it is now, and replaces the session's.
#[test]
fn a_change_for_this_session_says_what_it_was() {
    let (mut parts, _) = TestCtx::new(CliOptions::default());
    said(&mut parts, add("codex"), None);
    said(&mut parts, add("agy"), None);
    let saved = global_file(&parts);

    assert_eq!(
        said(&mut parts, remove("codex"), Some(SettingScope::Session)),
        "The bottom line shows Claude Code, agy · this session · was Claude Code, Codex, agy"
    );
    assert_eq!(global_file(&parts), saved);
    assert_eq!(listed(&parts).unwrap(), ["claude-code", "antigravity-cli"]);

    said(&mut parts, add("opencode"), None);
    assert_eq!(
        listed(&parts).unwrap(),
        ["claude-code", "antigravity-cli", "opencode"]
    );
    assert!(!global_file(&parts).contains("\"codex\""));
}

/// The picker's save keeps each CLI's windows and the holding-up choice.
#[test]
fn the_pickers_list_saves_as_picked() {
    let (mut parts, _) = TestCtx::new(CliOptions::default());
    let mut codex = LineCli::new("codex");
    codex.windows = Some(argonauts_core::settings::bottom_line::PlanUsageWindows::Both);
    let change = LineChange::Set {
        clis: vec![LineCli::new("claude-code"), codex],
        holding_up: false,
    };
    assert!(
        said(&mut parts, change, None).starts_with("The bottom line shows Claude Code, Codex · ")
    );
    let settings = line_facts(&parts.env, OffsetDateTime::now_utc()).settings;
    assert!(!settings.plan_usage.holding_up);
    let text = global_file(&parts);
    assert!(
        text.contains(r#"{ cli = "codex", windows = "both" }"#),
        "{text}"
    );
}

#[test]
fn nothing_changes_where_the_change_cant_be_made() {
    let (mut parts, _) = TestCtx::new(CliOptions::default());
    assert_eq!(
        said(&mut parts, remove("claude"), None),
        "Claude Code is the one in use, which the bottom line always shows"
    );
    assert_eq!(
        said(&mut parts, add("claude code"), None),
        "The bottom line already shows Claude Code"
    );
    assert_eq!(
        said(&mut parts, remove("codex"), None),
        "The bottom line doesn't show Codex"
    );
    let unknown = said(&mut parts, add("pi"), None);
    assert!(
        unknown.starts_with("There's no CLI named pi; the bottom line can show Claude Code"),
        "{unknown}"
    );
    assert_eq!(global_file(&parts), "");
    assert_eq!(listed(&parts), None);
}

/// A bare `/bottom-line` is the screen's: it opens the picker, and the controller says nothing.
#[test]
fn the_picker_opens_on_the_screen() {
    let (mut parts, _) = TestCtx::new(CliOptions::default());
    let effects = command(&parts.ctx, LineChange::Open, None).unwrap();
    assert!(effects.is_empty());
    assert!(parts.events.try_recv().is_err());
}
