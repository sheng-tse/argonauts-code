//! The input: several lines, pastes, history, the placeholder, vim keys, the `/` menu, and
//! `@` mentions.

mod pty_support;

use pty_support::*;
use serde_json::json;

/// The rows between the input's two rules.
fn input_rows(screen: &Screen) -> Vec<String> {
    let top = expect_row(screen, TOP_RULE);
    screen.visible[top + 1..]
        .iter()
        .take_while(|row| !is_rule(row))
        .cloned()
        .collect()
}

fn wait_input(harness: &Harness, text: &str) -> Screen {
    harness.wait_until(&format!("the input to read {text:?}"), |screen| {
        row_of(screen, TOP_RULE).is_some() && input_rows(screen).join("\n").contains(text)
    })
}

#[test]
fn composer_multiline_paste_history_placeholder() {
    let script = Script::new()
        .at(0, welcome_with_example("fix the flaky export test"))
        .at(0, status_line())
        .at(0, mode_idle())
        .await_command("submit")
        .timed(chat_turn("t-001", "three lines", "Got all three.", 5));
    let mut harness = Harness::start(script);
    // The first frame has the mode line before the status arrives, so wait for the label.
    harness.wait_for(TOP_RULE);
    let screen = harness.wait_visible(MODE_IDLE);
    let empty = input_rows(&screen);
    assert_eq!(empty.len(), 1, "{empty:?}");
    // The example comes from the app's welcome, made from recent commit subjects.
    assert!(
        empty[0].starts_with("❯ Try \"fix the flaky export test\""),
        "an example waits in the empty input: {empty:?}"
    );

    harness.paste("line one\nline two\nline three");
    let screen = wait_input(&harness, "line three");
    let rows = input_rows(&screen);
    assert_eq!(rows.len(), 3, "the input grows a row per line: {rows:?}");
    assert!(rows[0].starts_with("❯ line one"), "{rows:?}");
    assert!(
        !rows.join("\n").contains("Try "),
        "no example once you type"
    );
    harness.press(Key::Enter);
    harness.wait_command_where("submit", |c| c["text"] == "line one\nline two\nline three");
    let screen = harness.wait_visible("Got all three\\.");
    let rows = input_rows(&screen);
    assert!(
        !rows.join("\n").contains("Try "),
        "the example goes with the first prompt: {rows:?}"
    );

    harness.press(Key::Up);
    let screen = wait_input(&harness, "line three");
    assert_eq!(input_rows(&screen).len(), 3);
    let history = walk(&harness.home())
        .into_iter()
        .find(|path| path.ends_with("history.jsonl"))
        .expect("the prompt history is saved under ARGONAUTS_HOME");
    let saved = std::fs::read_to_string(history).unwrap();
    assert!(saved.contains("line one"), "{saved}");
}

#[test]
fn composer_offers_a_fixed_example_without_one_from_the_app() {
    let harness = Harness::start(Script::idle());
    harness.wait_for(TOP_RULE);
    let screen = harness.wait_visible(MODE_IDLE);
    let empty = input_rows(&screen);
    assert!(
        empty[0].starts_with("❯ Try \"review the last commit\""),
        "{empty:?}"
    );
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .flat_map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                walk(&path)
            } else {
                vec![path]
            }
        })
        .collect()
}

#[test]
fn mode_line_shows_vim_mode_and_hints() {
    let mut harness = Harness::launch(Script::idle()).ui("keys = \"vim\"").start();
    let screen = harness.wait_visible("-- INSERT --");
    let mode = expect_row(&screen, "-- INSERT --");
    // Claude Code puts the vim mode right before the approval mode, with no dot between. What
    // fits of the quotas and spend sits at the right end.
    let row = &screen.visible[mode];
    assert!(
        row.starts_with("  -- INSERT -- ǁ supervised mode on (shift+tab to cycle)  "),
        "{row:?}"
    );
    assert!(row.ends_with("  Claude 5h 71% used · $0.00"), "{row:?}");
    harness.press(Key::Esc);
    let screen = harness.wait_visible("-- NORMAL --");
    let mode = expect_row(&screen, "-- NORMAL --");
    let row = &screen.visible[mode];
    assert!(
        row.starts_with(
            "  -- NORMAL -- ǁ supervised mode on (shift+tab to cycle) · ? for shortcuts  "
        ),
        "{row:?}"
    );
    assert!(row.ends_with("  $0.00"), "{row:?}");
    harness.press(Key::Char('i'));
    harness.wait_visible("-- INSERT --");
    harness.wait_gone("\\? for shortcuts");
}

#[test]
fn vim_keys_and_mode_indicator() {
    let script = Script::idle()
        .await_command("vim")
        .at(0, ui_settings(json!({"keys": "vim"})));
    let mut harness = Harness::start(script);
    harness.wait_visible(MODE_IDLE);
    harness.send("/vim");
    harness.press(Key::Enter);
    assert_eq!(harness.wait_command("vim")["on"], true);
    harness.wait_visible("-- INSERT --");

    harness.send("hello brave world");
    wait_input(&harness, "hello brave world");
    harness.press(Key::Esc);
    harness.wait_visible("-- NORMAL --");
    harness.send("0dw");
    wait_input(&harness, "❯ brave world");
    harness.send("A again");
    harness.wait_visible("-- INSERT --");
    wait_input(&harness, "brave world again");
    harness.press(Key::Esc);
    // An Esc and a letter in one read arrive as Alt+letter, as in any terminal.
    harness.wait_visible("-- NORMAL --");
    harness.send("cc");
    harness.wait_visible("-- INSERT --");
    harness.send("fresh");
    harness.press(Key::Enter);
    harness.wait_command_where("submit", |c| c["text"] == "fresh");
    let sent: Vec<_> = harness.command_types();
    assert_eq!(sent, ["vim", "submit"], "motions never send anything");
}

#[test]
fn slash_menu_filters_and_completes() {
    let mut harness = Harness::start(Script::idle());
    harness.wait_visible(MODE_IDLE);
    harness.send("/");
    let menu = harness.wait_visible("^  /clear ");
    let rule = expect_row(&menu, TOP_RULE);
    assert!(
        expect_row(&menu, "^  /clear ") > rule + 2,
        "under the input:\n{}",
        menu.visible_text()
    );
    assert!(
        row_of(&menu, MODE_IDLE).is_none(),
        "in place of the mode line"
    );
    // The list shows as many commands as the rows left allow, in the registry's order.
    for command in ["/resume", "/compact"] {
        assert!(
            row_of(&menu, &format!("^  {command} ")).is_some(),
            "{command}:\n{}",
            menu.visible_text()
        );
    }
    harness.send("verb");
    let filtered = harness.wait_gone("^  /clear ");
    assert!(
        row_of(&filtered, "^  /verbosity ").is_some(),
        "{}",
        filtered.visible_text()
    );
    let listed = filtered
        .visible
        .iter()
        .filter(|row| row.starts_with("  /"))
        .count();
    assert!(listed <= 2, "only matches:\n{}", filtered.visible_text());
    harness.press(Key::Tab);
    wait_input(&harness, "❯ /verbosity");
    harness.press(Key::Esc);
    harness.wait_visible(MODE_IDLE);
    harness.assert_no_command("slash");
}

#[test]
fn mention_autocomplete() {
    let (_, _, events) = crew_run(3);
    let mut harness = Harness::start(Script::idle().all(0, events));
    harness.wait_visible("esc to interrupt");
    harness.send("@cod");
    wait_input(&harness, "❯ @cod");
    // A worker is listed once by its harness, with its model and place beside it, as
    // `MentionTarget::Worker` reads `@codex`.
    let list = harness.wait_visible("@codex +gpt-5\\.6 · owner on card-003 +agent");
    let rule = expect_row(&list, TOP_RULE);
    assert!(
        list.visible[rule..]
            .iter()
            .all(|row| !row.contains("@antigravity-cli") && !row.contains("card-004")),
        "only matches:\n{}",
        list.visible_text()
    );
    harness.press(Key::Tab);
    wait_input(&harness, "❯ @codex");
    harness.send("use EVAL for the counter");
    harness.press(Key::Enter);
    let mention = harness.wait_command("mention");
    assert_eq!(
        mention["target"],
        json!({"target": "worker", "value": "codex"})
    );
    assert_eq!(mention["text"], "use EVAL for the counter");
}

#[test]
fn mention_ambiguous_opens_picker() {
    let (_, _, events) = crew_run(4);
    let mut harness = Harness::start(Script::idle().all(0, events));
    harness.wait_visible("esc to interrupt");
    harness.send("@claude-code/opus keep the CSV path");
    harness.press(Key::Enter);
    // The cards show in the transcript too, so wait for the picker to take the screen.
    harness.wait_gone(TOP_RULE);
    let picker = harness.wait_visible("card-005");
    assert_order(&picker, &["card-002", "card-005"]);
    harness.assert_no_command("mention");
    // The first row sends to every session the name matches, so card-005 is two rows down.
    harness.press(Key::Down);
    harness.press(Key::Down);
    harness.press(Key::Enter);
    let mention = harness.wait_command("mention");
    assert_eq!(mention["text"], "keep the CSV path");
    let target = &mention["target"];
    assert!(
        *target == json!({"target": "card", "value": "card-005"})
            || *target == json!({"target": "number", "value": 4}),
        "the picked agent: {mention}"
    );
}
