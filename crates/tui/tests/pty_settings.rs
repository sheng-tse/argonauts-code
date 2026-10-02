//! Settings in the app: `/config` with each value's layer, a picker then the scope question,
//! a picker's Reset row, and a changed `[ui]` setting drawn at once.

mod pty_support;

use std::time::Duration;

use argonauts_core::app::AppEvent;
use pty_support::*;
use serde_json::json;

const THEMES: [&str; 3] = ["\"auto\"", "\"dark\"", "\"light\""];

fn theme_row(layer: &str) -> serde_json::Value {
    setting_row("ui.theme", "\"dark\"", layer, &THEMES)
}

/// Answers each settings command with the next picker.
fn pickers(pickers: Vec<AppEvent>) -> Script {
    pickers.into_iter().fold(Script::idle(), |script, picker| {
        script.await_command("slash").at(0, picker)
    })
}

/// Answers each `/theme` with the next `ui.theme` row, among the rest of the `ui` section as
/// the app sends it. `/theme` asks for them with `Request { Settings { section: "ui" } }`.
fn theme_answers(answers: Vec<Vec<serde_json::Value>>) -> Script {
    answers.into_iter().fold(Script::idle(), |script, rows| {
        script
            .await_command("request")
            .at(0, section_view("ui", whole_ui_section(rows)))
    })
}

fn whole_ui_section(rows: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let on_off = ["true", "false"];
    let mut section = vec![
        setting_row("ui.accent", "\"\"", "built-in", &[]),
        setting_row("ui.bell", "true", "built-in", &on_off),
        setting_row(
            "ui.glyphs",
            "\"auto\"",
            "built-in",
            &["\"auto\"", "\"ascii\""],
        ),
        setting_row("ui.keymap", "\"standard\"", "built-in", &[]),
    ];
    section.extend(rows);
    section.extend([
        setting_row("ui.panel.height", "8", "built-in", &[]),
        setting_row("ui.screen_reader", "false", "built-in", &on_off),
        setting_row(
            "ui.verbs",
            "\"voyage\"",
            "built-in",
            &["\"voyage\"", "\"plain\""],
        ),
    ]);
    section
}

fn open_theme_picker(harness: &mut Harness) -> Screen {
    harness.send("/theme");
    harness.press(Key::Enter);
    let request = harness.wait_command("request");
    assert_eq!(
        request["view"],
        json!({"view": "settings", "section": "ui"})
    );
    let picker = harness.wait_visible("light");
    assert!(
        row_of(&picker, "(?i)bell|keymap|screen.reader|verbs").is_none(),
        "the theme picker shows the themes, not the whole ui section:\n{}",
        picker.visible_text()
    );
    picker
}

/// Esc until the main screen is back, since Esc on the scope question may step back to the
/// picker first.
fn close_everything(harness: &mut Harness) {
    for _ in 0..3 {
        if harness
            .poll_visible(MODE_IDLE, Duration::from_millis(500))
            .is_some()
        {
            return;
        }
        harness.press(Key::Esc);
    }
    harness.wait_visible(MODE_IDLE);
}

#[test]
fn config_view_layers() {
    let rows = vec![
        setting_row("ui.keys", "\"vim\"", "global", &[]),
        setting_row("ui.theme", "\"auto\"", "built-in", &[]),
        setting_row("budget.run_usd", "30.0", "project", &[]),
        setting_row("roles.reviewer.effort", "\"high\"", "project-local", &[]),
        setting_row("approvals.merge.mode", "\"shadow\"", "session", &[]),
    ];
    let mut harness = Harness::start(pickers(vec![settings_view("config", rows)]));
    harness.wait_visible(MODE_IDLE);
    harness.send("/config");
    harness.press(Key::Enter);
    let slash = harness.wait_command("slash");
    assert_eq!(slash["command"]["command"], "config");
    let view = harness.wait_visible("approvals\\.merge\\.mode");
    for (key, layer) in [
        ("ui.keys", "global"),
        ("ui.theme", "built-in"),
        ("budget.run_usd", "project"),
        ("roles.reviewer.effort", "project-local"),
        ("approvals.merge.mode", "session"),
    ] {
        let row = expect_row(&view, &regex::escape(key));
        assert!(
            view.visible[row].contains(layer),
            "{key} shows its layer {layer}:\n{}",
            view.visible_text()
        );
    }
    assert_frames(&view, COLS);
}

#[test]
fn settings_picker_then_scope_question() {
    let script = theme_answers(vec![vec![theme_row("project")], vec![theme_row("project")]]);
    let mut harness = Harness::start(script);
    harness.wait_visible(MODE_IDLE);
    let picker = open_theme_picker(&mut harness);
    let current = expect_row(&picker, "dark");
    assert!(
        picker.visible[current].contains('✓'),
        "{}",
        picker.visible_text()
    );
    harness.press(Key::Char('3'));
    let question = harness.wait_visible("Everywhere");
    assert_order(
        &question,
        &[
            "Just this session",
            "This project, just me",
            "This project, shared",
            "Everywhere",
        ],
    );
    for (n, label) in [
        "Just this session",
        "This project, just me",
        "This project, shared",
        "Everywhere",
    ]
    .into_iter()
    .enumerate()
    {
        let row = &question.visible[expect_row(&question, label)];
        assert!(
            row.contains(&format!("{}. {label}", n + 1)),
            "{label} takes its number alone: {row:?}"
        );
    }
    assert!(
        row_of(&question, "❯ \\d\\. Everywhere").is_some(),
        "the command's default is picked:\n{}",
        question.visible_text()
    );
    assert!(
        row_of(&question, "(?i)project")
            .is_some_and(|row| row < expect_row(&question, "Just this session")),
        "a project file sets it, so saving everywhere changes nothing; the question says so:\n{}",
        question.visible_text()
    );
    close_everything(&mut harness);
    harness.submit("still here");
    harness.assert_no_command("set_setting");

    open_theme_picker(&mut harness);
    harness.press(Key::Char('3'));
    harness.wait_visible("Everywhere");
    harness.press(Key::Char('2'));
    let set = harness.wait_command("set_setting");
    assert_eq!(
        set,
        json!({"type": "set_setting", "key": "ui.theme", "value": "\"light\"",
               "scope": "project_local"})
    );
}

#[test]
fn settings_picker_reset_row() {
    let script = theme_answers(vec![vec![theme_row("built-in")], vec![theme_row("global")]]);
    let mut harness = Harness::start(script);
    harness.wait_visible(MODE_IDLE);
    let built_in = open_theme_picker(&mut harness);
    assert!(
        row_of(&built_in, "(?i)reset").is_none(),
        "a built-in value has nothing to reset:\n{}",
        built_in.visible_text()
    );
    close_everything(&mut harness);

    let from_file = open_theme_picker(&mut harness);
    let reset = expect_row(&from_file, "(?i)reset");
    let number = from_file.visible[reset]
        .chars()
        .find(char::is_ascii_digit)
        .expect("the Reset row is numbered");
    harness.press(Key::Char(number));
    let sent = harness.wait_command("reset_setting");
    assert_eq!(
        sent,
        json!({"type": "reset_setting", "key": "ui.theme", "scope": "global"})
    );
    harness.assert_no_command("set_setting");
}

#[test]
fn setting_changed_ui_key_applies_at_once() {
    let changed = app(json!({"type": "setting_changed", "key": "ui.accent",
        "value": "\"#ff8800\"", "scope": "global", "previous": null, "reset": false}));
    let script = Script::idle()
        .await_command("submit")
        .all(
            0,
            [
                mode_answering("t-001"),
                turn_started("t-001", "check the build"),
                chat_tool_started("t-001", "c1", "Bash", json!({"command": "cargo build"})),
            ],
        )
        .await_signal("change")
        .at(0, changed)
        .at(
            0,
            ui_settings(json!({"accent": "#ff8800", "theme": "light"})),
        );
    let mut harness = Harness::launch(script)
        .env("COLORTERM", "truecolor")
        .start();
    harness.wait_visible(MODE_IDLE);
    harness.submit("check the build");
    harness.wait_visible(WORKING_LINE);
    let before = harness.raw().len();
    assert!(
        !contains(&harness.raw(), b"255;136;0"),
        "not the accent yet"
    );
    harness.signal("change");
    harness.wait_visible("ui\\.accent");
    harness.wait_until("the new accent drawn", |_| {
        contains(&harness.raw()[before..], b"38;2;255;136;0")
    });
    harness.wait_visible(WORKING_LINE);
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
