//! Settings in the app: `/config` with each value's layer, a picker then the scope question,
//! a picker's Reset row, and a changed `[ui]` setting drawn at once.

use crate::pty_support;
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

fn open_theme_picker(driver: &mut TestDriver) -> Screen {
    driver.send("/theme");
    driver.press(Key::Enter);
    let request = driver.wait_command("request");
    assert_eq!(
        request["view"],
        json!({"view": "settings", "section": "ui"})
    );
    let picker = driver.wait_visible("light");
    assert!(
        row_of(&picker, "(?i)bell|keymap|screen.reader|verbs").is_none(),
        "the theme picker shows the themes, not the whole ui section:\n{}",
        picker.visible_text()
    );
    picker
}

/// Esc until the main screen is back, since Esc on the scope question may step back to the
/// picker first.
fn close_everything(driver: &mut TestDriver) {
    for _ in 0..3 {
        if driver
            .poll_visible(MODE_IDLE, Duration::from_millis(500))
            .is_some()
        {
            return;
        }
        driver.press(Key::Esc);
    }
    driver.wait_visible(MODE_IDLE);
}

#[test]
fn all_settings_view_layers() {
    let rows = vec![
        setting_row("ui.keys", "\"vim\"", "global", &[]),
        setting_row("ui.theme", "\"auto\"", "built-in", &[]),
        setting_row("limits.spend_per_run", "30.0", "project", &[]),
        setting_row("roles.reviewer.effort", "\"high\"", "project-local", &[]),
        setting_row("approvals.merge.mode", "\"auto_preview\"", "session", &[]),
    ];
    let mut driver = TestDriver::start(pickers(vec![settings_view("config", rows)]));
    driver.wait_visible(MODE_IDLE);
    driver.send("/config all");
    driver.press(Key::Enter);
    let slash = driver.wait_command("slash");
    assert_eq!(slash["command"]["command"], "config");
    assert_eq!(slash["command"]["key"], "all");
    let view = driver.wait_visible("approvals\\.merge\\.mode");
    for (key, layer) in [
        ("ui.keys", "global"),
        ("ui.theme", "built-in"),
        ("limits.spend_per_run", "project"),
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

/// A value's number saves it at once, with nothing asked of where; ctrl+s on a value keeps it
/// for this session only.
#[test]
fn settings_picker_saves_a_pick_at_once() {
    let script = theme_answers(vec![vec![theme_row("project")], vec![theme_row("project")]]);
    let mut driver = TestDriver::start(script);
    driver.wait_visible(MODE_IDLE);
    let picker = open_theme_picker(&mut driver);
    let current = expect_row(&picker, "dark");
    assert!(
        picker.visible[current].contains('✓'),
        "{}",
        picker.visible_text()
    );
    assert!(
        row_of(&picker, "ctrl\\+s for this session only").is_some(),
        "{}",
        picker.visible_text()
    );
    driver.press(Key::Char('3'));
    let set = driver.wait_command("set_setting");
    assert_eq!(
        set,
        json!({"type": "set_setting", "key": "ui.theme", "value": "\"light\""})
    );

    let picker = open_theme_picker(&mut driver);
    assert!(
        row_of(&picker, "Where to save it").is_none(),
        "{}",
        picker.visible_text()
    );
    driver.press(Key::Down);
    driver.press(Key::Ctrl('s'));
    let set = driver.wait_command("set_setting");
    assert_eq!(
        set,
        json!({"type": "set_setting", "key": "ui.theme", "value": "\"light\"",
               "scope": "session"})
    );
}

#[test]
fn settings_picker_reset_row() {
    let script = theme_answers(vec![vec![theme_row("built-in")], vec![theme_row("global")]]);
    let mut driver = TestDriver::start(script);
    driver.wait_visible(MODE_IDLE);
    let built_in = open_theme_picker(&mut driver);
    assert!(
        row_of(&built_in, "(?i)reset").is_none(),
        "a built-in value has nothing to reset:\n{}",
        built_in.visible_text()
    );
    close_everything(&mut driver);

    let from_file = open_theme_picker(&mut driver);
    let reset = expect_row(&from_file, "(?i)reset");
    let number = from_file.visible[reset]
        .chars()
        .find(char::is_ascii_digit)
        .expect("the Reset row is numbered");
    driver.press(Key::Char(number));
    let sent = driver.wait_command("reset_setting");
    assert_eq!(
        sent,
        json!({"type": "reset_setting", "key": "ui.theme", "scope": "global"})
    );
    driver.assert_no_command("set_setting");
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
                conversation_tool_started("t-001", "c1", "Bash", json!({"command": "cargo build"})),
            ],
        )
        .await_signal("change")
        .at(0, changed)
        .at(
            0,
            ui_settings(json!({"accent": "#ff8800", "theme": "light"})),
        );
    let mut driver = TestDriver::launch(script)
        .env("COLORTERM", "truecolor")
        .start();
    driver.wait_visible(MODE_IDLE);
    driver.submit("check the build");
    driver.wait_visible(WORKING_LINE);
    let before = driver.raw().len();
    assert!(!contains(&driver.raw(), b"255;136;0"), "not the accent yet");
    driver.signal("change");
    driver.wait_visible("ui\\.accent");
    driver.wait_until("the new accent drawn", |_| {
        contains(&driver.raw()[before..], b"38;2;255;136;0")
    });
    driver.wait_visible(WORKING_LINE);
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// An open picker takes the input's place, as a dialog does in Claude Code 2.1.303: one rule,
/// the picker's own, no input row, and the cursor hidden. Esc brings the input back.
#[test]
fn an_open_picker_takes_the_inputs_place() {
    let mut driver = TestDriver::start(theme_answers(vec![vec![theme_row("global")]]));
    driver.wait_visible(MODE_IDLE);
    let picker = open_theme_picker(&mut driver);
    let rules = picker.visible.iter().filter(|row| is_rule(row)).count();
    assert_eq!(rules, 1, "one rule:\n{}", picker.visible_text());
    // Above the rule, `❯ /theme` is the echo in the conversation.
    let rule = picker.visible.iter().position(|row| is_rule(row)).unwrap();
    assert!(
        !picker.visible[rule..]
            .iter()
            .any(|row| row.starts_with('❯')),
        "no input row while the picker is open:\n{}",
        picker.visible_text()
    );
    let raw = driver.raw();
    let hidden = raw.windows(6).rposition(|w| w == b"\x1b[?25l");
    let shown = raw.windows(6).rposition(|w| w == b"\x1b[?25h");
    assert!(
        hidden > shown,
        "the cursor is hidden while the picker is open"
    );

    driver.press(Key::Esc);
    let back = driver.wait_visible(MODE_IDLE);
    assert_eq!(
        back.visible.iter().filter(|row| is_rule(row)).count(),
        2,
        "the input's two rules are back:\n{}",
        back.visible_text()
    );
}
