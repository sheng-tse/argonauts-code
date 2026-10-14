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
    // Wide enough for the mode's hints beside the two CLIs' chips.
    let mut harness = Harness::launch(Script::idle())
        .ui("keys = \"vim\"")
        .size(ROWS, 140)
        .start();
    // The status with the chips comes a moment after the first frame.
    let screen = harness.wait_visible("-- INSERT -- .*● Claude");
    let mode = expect_row(&screen, "-- INSERT --");
    // Claude Code puts the vim mode right before the approval mode, with no dot between. Each
    // CLI's plan usage sits against the right edge.
    let row = &screen.visible[mode];
    assert!(
        row.starts_with("  -- INSERT -- ǁ supervised mode on (shift+tab to cycle)  "),
        "{row:?}"
    );
    assert_eq!(expect_row(&screen, STATUS_IDLE), mode, "{row:?}");
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
    assert_eq!(expect_row(&screen, STATUS_IDLE), mode, "{row:?}");
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
    harness.wait_visible(WORKING);
    harness.send("@cod");
    wait_input(&harness, "❯ @cod");
    // A worker is listed once by its harness, with its model and place beside it, as
    // `MentionTarget::Worker` reads `@codex`.
    let list = harness.wait_visible("@codex +gpt-5\\.6 · worker on task-003 +agent");
    let rule = expect_row(&list, TOP_RULE);
    assert!(
        list.visible[rule..]
            .iter()
            .all(|row| !row.contains("@antigravity-cli") && !row.contains("task-004")),
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

fn repo_files(paths: &[&str]) -> argonauts_core::app::AppEvent {
    app(json!({"type": "repo_files", "paths": paths}))
}

#[test]
fn file_mentions_list_the_repo_and_complete() {
    let script = Script::idle()
        .at(0, repo_files(&["README.md", "src/main.rs", "src/menu.rs"]))
        .await_command("request")
        .at(
            0,
            repo_files(&["README.md", "notes.txt", "src/main.rs", "src/menu.rs"]),
        )
        .await_command("submit")
        .timed(chat_turn(
            "t-001",
            "@src/main.rs explain it",
            "It starts the app.",
            5,
        ));
    let mut harness = Harness::start(script);
    harness.wait_visible(MODE_IDLE);
    // A fresh `@` reads the files again, so one made since the start is listed.
    harness.send("@");
    harness.wait_command_where("request", |c| c["view"]["view"] == "repo_files");
    let list = harness.wait_visible("^  \\+ notes\\.txt *$");
    let rule = expect_row(&list, TOP_RULE);
    assert!(
        expect_row(&list, "^  \\+ README\\.md *$") > rule,
        "under the input:\n{}",
        list.visible_text()
    );
    // Files first, ignoring case, then folders.
    assert_order(&list, &["\\+ notes\\.txt", "\\+ README\\.md", "\\+ src/"]);
    assert!(
        row_of(&list, MODE_IDLE).is_none(),
        "in place of the mode line"
    );

    harness.send("men");
    let filtered = harness.wait_gone("^  \\+ notes\\.txt");
    assert!(
        row_of(&filtered, "^  \\+ src/menu\\.rs *$").is_some(),
        "{}",
        filtered.visible_text()
    );
    // Esc closes it, and the next key opens it again.
    harness.press(Key::Esc);
    harness.wait_visible(MODE_IDLE);
    harness.press(Key::Backspace);
    harness.wait_visible("^  \\+ src/menu\\.rs *$");
    harness.press_all(&[Key::Backspace, Key::Backspace]);
    harness.send("main");
    harness.wait_gone("^  \\+ src/menu\\.rs");
    harness.wait_visible("^  \\+ src/main\\.rs *$");
    harness.press(Key::Tab);
    wait_input(&harness, "❯ @src/main.rs");
    harness.send("explain it");
    harness.press(Key::Enter);
    harness.wait_command_where("submit", |c| c["text"] == "@src/main.rs explain it");
    harness.wait_visible("It starts the app\\.");
    harness.assert_no_command("mention");
}

#[test]
fn mention_ambiguous_opens_picker() {
    let (_, _, events) = crew_run(4);
    let mut harness = Harness::start(Script::idle().all(0, events));
    harness.wait_visible(WORKING);
    harness.send("@claude-code/opus keep the CSV path");
    harness.press(Key::Enter);
    // The cards show in the transcript too, so wait for the picker to take the screen.
    harness.wait_gone(TOP_RULE);
    let picker = harness.wait_visible("task-005");
    assert_order(&picker, &["task-002", "task-005"]);
    harness.assert_no_command("mention");
    // The first row sends to every session the name matches, so card-005 is two rows down.
    harness.press(Key::Down);
    harness.press(Key::Down);
    harness.press(Key::Enter);
    let mention = harness.wait_command("mention");
    assert_eq!(mention["text"], "keep the CSV path");
    let target = &mention["target"];
    assert!(
        *target == json!({"target": "card", "value": "task-005"})
            || *target == json!({"target": "number", "value": 4}),
        "the picked agent: {mention}"
    );
}

/// Another repo's prompt history, three days old, for the everywhere scope.
fn other_repo_history() -> (&'static str, String) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let line = json!({ "prompt": "deploy the web app", "at": now - 3 * 86_400 });
    (
        ".argonauts/state/web-111/history.jsonl",
        format!("{line}\n"),
    )
}

fn submits(harness: &Harness) -> usize {
    harness
        .command_types()
        .iter()
        .filter(|kind| *kind == "submit")
        .count()
}

#[test]
fn vim_slash_searches_prompt_history_and_enter_only_fills_the_input() {
    let (path, history) = other_repo_history();
    let script = Script::idle()
        .await_command("submit")
        .timed(chat_turn("t-001", "send me", "Sent.", 5));
    let mut harness = Harness::launch(script)
        .ui("keys = \"vim\"")
        .home_file(path, &history)
        .start();
    harness.wait_visible("-- INSERT --");
    harness.submit("send me");
    harness.wait_visible("Sent\\.");

    harness.send("draft");
    wait_input(&harness, "draft");
    harness.press(Key::Esc);
    harness.wait_visible("-- NORMAL --");
    harness.press(Key::Char('/'));
    let screen = harness.wait_visible("Search prompts · everywhere");
    expect_row(&screen, "^ {4}3d ago {3}deploy the web app");
    expect_row(&screen, "^  ❯ \\S+( ago)? +send me");
    expect_row(&screen, "│ ⌕ Filter history…");
    expect_row(
        &screen,
        "^  ↑/↓ to navigate · Enter to use · Esc to cancel · ctrl\\+s to scope · Esc i / for slash commands$",
    );
    assert!(
        row_of(&screen, "-- NORMAL --").is_none(),
        "the search stands in for the input and the mode line"
    );

    // Typing filters; the input keeps its draft until a prompt is picked.
    harness.send("dwa");
    let screen = harness.wait_visible("│ ⌕ dwa");
    expect_row(&screen, "^  ❯ 3d ago   deploy the web app");
    assert!(
        row_of(&screen, "ago +send me").is_none(),
        "{}",
        screen.visible.join("\n")
    );

    // Enter puts it in the input and sends nothing.
    harness.press(Key::Enter);
    let screen = harness.wait_visible("-- NORMAL --");
    assert_eq!(input_rows(&screen), ["❯ deploy the web app"]);
    assert_eq!(submits(&harness), 1, "only the first prompt went out");

    // Ctrl+S narrows to this project, and Esc leaves the input as it was.
    harness.press(Key::Char('/'));
    harness.wait_visible("Search prompts · everywhere");
    harness.press(Key::Ctrl('s'));
    let screen = harness.wait_visible("Search prompts · this project");
    assert!(row_of(&screen, "deploy the web app").is_none());
    expect_row(&screen, "^  ❯ \\S+( ago)? +send me");
    harness.press(Key::Esc);
    let screen = harness.wait_visible("-- NORMAL --");
    assert_eq!(input_rows(&screen), ["❯ deploy the web app"]);
    assert_eq!(submits(&harness), 1);
}

#[test]
fn ctrl_r_searches_prompt_history_with_default_keys() {
    let (path, history) = other_repo_history();
    let mut harness = Harness::launch(Script::idle())
        .home_file(path, &history)
        .start();
    harness.wait_for(TOP_RULE);
    harness.wait_visible(MODE_IDLE);
    harness.send("half typed");
    wait_input(&harness, "half typed");

    harness.press(Key::Ctrl('r'));
    let screen = harness.wait_visible("Search prompts · everywhere");
    expect_row(&screen, "^  ❯ 3d ago   deploy the web app");
    let hints = expect_row(&screen, "^  ↑/↓ to navigate");
    assert!(
        !screen.visible[hints].contains("Esc i /"),
        "the way to `/` commands is for vim keys"
    );
    harness.press(Key::Esc);
    let screen = harness.wait_visible(MODE_IDLE);
    assert_eq!(input_rows(&screen), ["❯ half typed"]);

    harness.press(Key::Ctrl('r'));
    harness.wait_visible("Search prompts");
    harness.send("web");
    harness.wait_visible("│ ⌕ web");
    harness.press(Key::Enter);
    let screen = wait_input(&harness, "deploy the web app");
    assert_eq!(input_rows(&screen), ["❯ deploy the web app"]);
    harness.assert_no_command("submit");
}

/// This repo's prompt history from an earlier session, an hour old.
fn this_repo_history(prompts: &[&str]) -> (String, String) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let lines: String = prompts
        .iter()
        .map(|prompt| format!("{}\n", json!({ "prompt": prompt, "at": now - 3_600 })))
        .collect();
    (".argonauts/state/web-111/history.jsonl".to_owned(), lines)
}

/// A terminal without bracketed paste types a paste out as keys in one write. Its line breaks
/// are Enter, and none of them may send.
#[test]
fn a_paste_typed_out_as_keys_never_sends() {
    let mut harness = Harness::launch(Script::idle()).start();
    harness.wait_for(TOP_RULE);
    harness.wait_visible(MODE_IDLE);
    harness.send_at_once("wor\r\rbar");
    let screen = wait_input(&harness, "bar");
    assert_eq!(input_rows(&screen), ["❯ wor", "", "  bar"]);
    std::thread::sleep(std::time::Duration::from_millis(300));
    harness.assert_no_command("submit");
}

/// Terminal.app types a dropped file's path out as keys in one write. The path starts with
/// `/`, so every key also runs the commands list, which a debug build takes a while over; the
/// keys still came together, so the path is one paste and becomes its picture's chip.
#[test]
fn a_dropped_picture_typed_out_as_keys_becomes_its_chip() {
    let mut harness = Harness::launch(Script::idle()).start();
    harness.wait_for(TOP_RULE);
    harness.wait_visible(MODE_IDLE);
    let picture = harness
        .scratch()
        .join("Screenshot 2026-10-08 at 10.07.05 PM.png");
    std::fs::write(&picture, b"\x89PNG\r\n\x1a\n").unwrap();
    let typed = picture.display().to_string().replace(' ', "\\ ");
    assert!(typed.starts_with('/'), "{typed}");

    harness.send_at_once(&typed);
    let screen = wait_input(&harness, "[Image #1]");
    assert_eq!(input_rows(&screen), ["❯ [Image #1]"]);
    harness.assert_no_command("submit");
}

#[test]
fn a_paste_typed_out_as_keys_never_picks_a_prompt_in_the_search() {
    let (path, history) = other_repo_history();
    let (_, mine) = this_repo_history(&["Should this be a workflow?"]);
    let mut harness = Harness::launch(Script::idle())
        .ui("keys = \"vim\"")
        .home_file(path, &format!("{history}{mine}"))
        .start();
    harness.wait_visible("-- INSERT --");
    harness.press(Key::Esc);
    harness.wait_visible("-- NORMAL --");
    harness.press(Key::Char('/'));
    harness.wait_visible("Search prompts · everywhere");

    harness.send_at_once("wor\r\rbar");
    let screen = harness.wait_visible("│ ⌕ wor bar");
    expect_row(&screen, "Search prompts · everywhere");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let screen = harness.screen();
    expect_row(&screen, "│ ⌕ wor bar");
    harness.assert_no_command("submit");

    // The search is still yours: Esc leaves it with the input as it was.
    harness.press(Key::Esc);
    let screen = harness.wait_visible("-- NORMAL --");
    let input = input_rows(&screen);
    assert!(input[0].starts_with("❯ Try "), "still empty: {input:?}");
    harness.assert_no_command("submit");
}

/// The footer's Esc i / reaches the `/` commands, so it shows only where it does, and works
/// when Esc and i arrive together, as Alt+i.
#[test]
fn esc_i_slash_from_the_search_opens_the_slash_menu() {
    let (path, history) = other_repo_history();
    let mut harness = Harness::launch(Script::idle())
        .ui("keys = \"vim\"")
        .home_file(path, &history)
        .start();
    harness.wait_visible("-- INSERT --");

    // Ctrl+R in INSERT goes back to INSERT, where Esc i / would type, so no hint.
    harness.press(Key::Ctrl('r'));
    let screen = harness.wait_visible("Search prompts · everywhere");
    let hints = expect_row(&screen, "^  ↑/↓ to navigate");
    assert!(
        !screen.visible[hints].contains("Esc i /"),
        "{}",
        screen.visible[hints]
    );
    harness.press(Key::Esc);
    harness.wait_visible("-- INSERT --");

    harness.press(Key::Esc);
    harness.wait_visible("-- NORMAL --");
    harness.press(Key::Char('/'));
    harness.wait_visible("Esc i / for slash commands");
    harness.send_at_once("\x1bi");
    let screen = harness.wait_visible("-- INSERT --");
    assert!(row_of(&screen, "Search prompts").is_none());
    let input = input_rows(&screen);
    assert!(input[0].starts_with("❯ Try "), "nothing typed: {input:?}");
    harness.send("/");
    harness.wait_visible("^  /clear ");
}
