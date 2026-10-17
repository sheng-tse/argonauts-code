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

fn wait_input(driver: &TestDriver, text: &str) -> Screen {
    driver.wait_until(&format!("the input to read {text:?}"), |screen| {
        row_of(screen, TOP_RULE).is_some() && input_rows(screen).join("\n").contains(text)
    })
}

#[test]
fn composer_multiline_paste_history_placeholder() {
    let script = Script::new()
        .at(0, welcome_with_example("fix the flaky export test"))
        .at(0, bottom_line())
        .at(0, mode_idle())
        .await_command("submit")
        .timed(conversation_turn(
            "t-001",
            "three lines",
            "Got all three.",
            5,
        ));
    let mut driver = TestDriver::start(script);
    // The first frame has the bottom line before the status arrives, so wait for the label.
    driver.wait_for(TOP_RULE);
    let screen = driver.wait_visible(MODE_IDLE);
    let empty = input_rows(&screen);
    assert_eq!(empty.len(), 1, "{empty:?}");
    // The example comes from the app's welcome, made from recent commit subjects.
    assert!(
        empty[0].starts_with("❯ Try \"fix the flaky export test\""),
        "an example waits in the empty input: {empty:?}"
    );

    driver.paste("line one\nline two\nline three");
    let screen = wait_input(&driver, "line three");
    let rows = input_rows(&screen);
    assert_eq!(rows.len(), 3, "the input grows a row per line: {rows:?}");
    assert!(rows[0].starts_with("❯ line one"), "{rows:?}");
    assert!(
        !rows.join("\n").contains("Try "),
        "no example once you type"
    );
    driver.press(Key::Enter);
    driver.wait_command_where("submit", |c| c["text"] == "line one\nline two\nline three");
    let screen = driver.wait_visible("Got all three\\.");
    let rows = input_rows(&screen);
    assert!(
        !rows.join("\n").contains("Try "),
        "the example goes with the first prompt: {rows:?}"
    );

    driver.press(Key::Up);
    let screen = wait_input(&driver, "line three");
    assert_eq!(input_rows(&screen).len(), 3);
    let history = walk(&driver.home())
        .into_iter()
        .find(|path| path.ends_with("history.jsonl"))
        .expect("the prompt history is saved under ARGONAUTS_HOME");
    let saved = std::fs::read_to_string(history).unwrap();
    assert!(saved.contains("line one"), "{saved}");
}

#[test]
fn composer_offers_a_fixed_example_without_one_from_the_app() {
    let driver = TestDriver::start(Script::idle());
    driver.wait_for(TOP_RULE);
    let screen = driver.wait_visible(MODE_IDLE);
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
fn bottom_row_shows_vim_mode_and_hints() {
    // Wide enough for the mode's hints beside the two CLIs' chips.
    let mut driver = TestDriver::launch(Script::idle())
        .ui("keys = \"vim\"")
        .size(ROWS, 140)
        .start();
    // The status with the chips comes a moment after the first frame.
    let screen = driver.wait_visible("-- INSERT -- .*● Claude");
    let mode = expect_row(&screen, "-- INSERT --");
    // Claude Code puts the vim mode right before the approval mode, with no dot between. Each
    // CLI's plan usage sits against the right edge.
    let row = &screen.visible[mode];
    assert!(
        row.starts_with("  -- INSERT -- ǁ supervised mode on (shift+tab to cycle)  "),
        "{row:?}"
    );
    assert_eq!(expect_row(&screen, STATUS_IDLE), mode, "{row:?}");
    // NORMAL has no label, as in Claude Code, and its footer hint comes back.
    driver.press(Key::Esc);
    let screen = driver.wait_visible("^  ǁ supervised mode on .*\\? for shortcuts");
    let mode = expect_row(&screen, "^  ǁ supervised mode on");
    let row = &screen.visible[mode];
    assert!(
        row.starts_with("  ǁ supervised mode on (shift+tab to cycle) · ? for shortcuts  "),
        "{row:?}"
    );
    assert!(!row.contains("--"), "{row:?}");
    assert_eq!(expect_row(&screen, STATUS_IDLE), mode, "{row:?}");
    driver.press(Key::Char('i'));
    driver.wait_visible("-- INSERT --");
    driver.wait_gone("\\? for shortcuts");
}

#[test]
fn vim_keys_and_mode_indicator() {
    let script = Script::idle()
        .await_command("vim")
        .at(0, ui_settings(json!({"keys": "vim"})));
    let mut driver = TestDriver::start(script);
    driver.wait_visible(MODE_IDLE);
    driver.send("/vim");
    driver.press(Key::Enter);
    assert_eq!(driver.wait_command("vim")["on"], true);
    driver.wait_visible("-- INSERT --");

    driver.send("hello brave world");
    wait_input(&driver, "hello brave world");
    driver.press(Key::Esc);
    driver.wait_visible(MODE_IDLE);
    driver.send("0dw");
    wait_input(&driver, "❯ brave world");
    driver.send("A again");
    driver.wait_visible("-- INSERT --");
    wait_input(&driver, "brave world again");
    driver.press(Key::Esc);
    // An Esc and a letter in one read arrive as Alt+letter, as in any terminal.
    driver.wait_visible(MODE_IDLE);
    driver.send("cc");
    driver.wait_visible("-- INSERT --");
    driver.send("fresh");
    driver.press(Key::Enter);
    driver.wait_command_where("submit", |c| c["text"] == "fresh");
    let sent: Vec<_> = driver.command_types();
    // `/vim` as typed goes in the conversation's record before the switch.
    assert_eq!(
        sent,
        ["command_typed", "vim", "submit"],
        "motions never send anything"
    );
}

#[test]
fn slash_menu_filters_and_completes() {
    let mut driver = TestDriver::start(Script::idle());
    driver.wait_visible(MODE_IDLE);
    driver.send("/");
    let menu = driver.wait_visible("^  /clear ");
    let rule = expect_row(&menu, TOP_RULE);
    assert!(
        expect_row(&menu, "^  /clear ") > rule + 2,
        "under the input:\n{}",
        menu.visible_text()
    );
    assert!(
        row_of(&menu, MODE_IDLE).is_none(),
        "in place of the bottom line"
    );
    // The list shows as many commands as the rows left allow, in the registry's order.
    for command in ["/resume", "/compact"] {
        assert!(
            row_of(&menu, &format!("^  {command} ")).is_some(),
            "{command}:\n{}",
            menu.visible_text()
        );
    }
    driver.send("verb");
    let filtered = driver.wait_gone("^  /clear ");
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
    driver.press(Key::Tab);
    wait_input(&driver, "❯ /verbosity");
    driver.press(Key::Esc);
    // With the menu closed the draft is still a real command, so the bottom line says what
    // Enter does with it.
    driver.wait_visible("^  enter runs /verbosity( {2,}\\S.*)?$");
    driver.assert_no_command("slash");
}

#[test]
fn mention_autocomplete() {
    let (_, _, events) = crew_run(3);
    let mut driver = TestDriver::start(Script::idle().all(0, events));
    driver.wait_visible(WORKING);
    driver.send("@cod");
    wait_input(&driver, "❯ @cod");
    // A worker is listed once by its CLI, with its model and place beside it, as
    // `MentionTarget::Worker` reads `@codex`.
    let list = driver.wait_visible("@codex +gpt-5\\.6 · worker on task-003 +agent");
    let rule = expect_row(&list, TOP_RULE);
    assert!(
        list.visible[rule..]
            .iter()
            .all(|row| !row.contains("@antigravity-cli") && !row.contains("task-004")),
        "only matches:\n{}",
        list.visible_text()
    );
    driver.press(Key::Tab);
    wait_input(&driver, "❯ @codex");
    driver.send("use EVAL for the counter");
    driver.press(Key::Enter);
    let mention = driver.wait_command("mention");
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
        .timed(conversation_turn(
            "t-001",
            "@src/main.rs explain it",
            "It starts the app.",
            5,
        ));
    let mut driver = TestDriver::start(script);
    driver.wait_visible(MODE_IDLE);
    // A fresh `@` reads the files again, so one made since the start is listed.
    driver.send("@");
    driver.wait_command_where("request", |c| c["view"]["view"] == "repo_files");
    let list = driver.wait_visible("^  \\+ notes\\.txt *$");
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
        "in place of the bottom line"
    );

    driver.send("men");
    let filtered = driver.wait_gone("^  \\+ notes\\.txt");
    assert!(
        row_of(&filtered, "^  \\+ src/menu\\.rs *$").is_some(),
        "{}",
        filtered.visible_text()
    );
    // Esc closes it, and the next key opens it again.
    driver.press(Key::Esc);
    driver.wait_visible(MODE_IDLE);
    driver.press(Key::Backspace);
    driver.wait_visible("^  \\+ src/menu\\.rs *$");
    driver.press_all(&[Key::Backspace, Key::Backspace]);
    driver.send("main");
    driver.wait_gone("^  \\+ src/menu\\.rs");
    driver.wait_visible("^  \\+ src/main\\.rs *$");
    driver.press(Key::Tab);
    wait_input(&driver, "❯ @src/main.rs");
    driver.send("explain it");
    driver.press(Key::Enter);
    driver.wait_command_where("submit", |c| c["text"] == "@src/main.rs explain it");
    driver.wait_visible("It starts the app\\.");
    driver.assert_no_command("mention");
}

#[test]
fn mention_ambiguous_opens_picker() {
    let (_, _, events) = crew_run(4);
    let mut driver = TestDriver::start(Script::idle().all(0, events));
    driver.wait_visible(WORKING);
    driver.send("@claude-code/opus keep the CSV path");
    driver.press(Key::Enter);
    // The tasks show in the transcript too, so wait for the picker to take the screen.
    driver.wait_gone(TOP_RULE);
    let picker = driver.wait_visible("task-005");
    assert_order(&picker, &["task-002", "task-005"]);
    driver.assert_no_command("mention");
    // The first row sends to every session the name matches, so task-005 is two rows down.
    driver.press(Key::Down);
    driver.press(Key::Down);
    driver.press(Key::Enter);
    let mention = driver.wait_command("mention");
    assert_eq!(mention["text"], "keep the CSV path");
    let target = &mention["target"];
    assert!(
        *target == json!({"target": "task", "value": "task-005"})
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

fn submits(driver: &TestDriver) -> usize {
    driver
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
        .timed(conversation_turn("t-001", "send me", "Sent.", 5));
    let mut driver = TestDriver::launch(script)
        .ui("keys = \"vim\"")
        .home_file(path, &history)
        .start();
    driver.wait_visible("-- INSERT --");
    driver.submit("send me");
    driver.wait_visible("Sent\\.");

    driver.send("draft");
    wait_input(&driver, "draft");
    driver.press(Key::Esc);
    driver.wait_visible(MODE_IDLE);
    driver.press(Key::Char('/'));
    let screen = driver.wait_visible("Search prompts · everywhere");
    expect_row(&screen, "^ {4}3d ago {3}deploy the web app");
    expect_row(&screen, "^  ❯ \\S+( ago)? +send me");
    expect_row(&screen, "│ ⌕ Filter history…");
    expect_row(
        &screen,
        "^  ↑/↓ to navigate · Enter to use · Esc to cancel · ctrl\\+s to scope · Esc i / for slash commands$",
    );
    assert!(
        row_of(&screen, MODE_IDLE).is_none(),
        "the search stands in for the input and the bottom line"
    );

    // Typing filters; the input keeps its draft until a prompt is picked.
    driver.send("dwa");
    let screen = driver.wait_visible("│ ⌕ dwa");
    expect_row(&screen, "^  ❯ 3d ago   deploy the web app");
    assert!(
        row_of(&screen, "ago +send me").is_none(),
        "{}",
        screen.visible.join("\n")
    );

    // Enter puts it in the input and sends nothing.
    driver.press(Key::Enter);
    let screen = driver.wait_visible(MODE_IDLE);
    assert_eq!(input_rows(&screen), ["❯ deploy the web app"]);
    assert_eq!(submits(&driver), 1, "only the first prompt went out");

    // Ctrl+S narrows to this project, and Esc leaves the input as it was.
    driver.press(Key::Char('/'));
    driver.wait_visible("Search prompts · everywhere");
    driver.press(Key::Ctrl('s'));
    let screen = driver.wait_visible("Search prompts · this project");
    assert!(row_of(&screen, "deploy the web app").is_none());
    expect_row(&screen, "^  ❯ \\S+( ago)? +send me");
    driver.press(Key::Esc);
    let screen = driver.wait_visible(MODE_IDLE);
    assert_eq!(input_rows(&screen), ["❯ deploy the web app"]);
    assert_eq!(submits(&driver), 1);
}

#[test]
fn ctrl_r_searches_prompt_history_with_default_keys() {
    let (path, history) = other_repo_history();
    let mut driver = TestDriver::launch(Script::idle())
        .home_file(path, &history)
        .start();
    driver.wait_for(TOP_RULE);
    driver.wait_visible(MODE_IDLE);
    driver.send("half typed");
    wait_input(&driver, "half typed");

    driver.press(Key::Ctrl('r'));
    let screen = driver.wait_visible("Search prompts · everywhere");
    expect_row(&screen, "^  ❯ 3d ago   deploy the web app");
    let hints = expect_row(&screen, "^  ↑/↓ to navigate");
    assert!(
        !screen.visible[hints].contains("Esc i /"),
        "the way to `/` commands is for vim keys"
    );
    driver.press(Key::Esc);
    let screen = driver.wait_visible(MODE_IDLE);
    assert_eq!(input_rows(&screen), ["❯ half typed"]);

    driver.press(Key::Ctrl('r'));
    driver.wait_visible("Search prompts");
    driver.send("web");
    driver.wait_visible("│ ⌕ web");
    driver.press(Key::Enter);
    let screen = wait_input(&driver, "deploy the web app");
    assert_eq!(input_rows(&screen), ["❯ deploy the web app"]);
    driver.assert_no_command("submit");
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
    let mut driver = TestDriver::launch(Script::idle()).start();
    driver.wait_for(TOP_RULE);
    driver.wait_visible(MODE_IDLE);
    driver.send_at_once("wor\r\rbar");
    let screen = wait_input(&driver, "bar");
    assert_eq!(input_rows(&screen), ["❯ wor", "", "  bar"]);
    std::thread::sleep(std::time::Duration::from_millis(300));
    driver.assert_no_command("submit");
}

/// Terminal.app types a dropped file's path out as keys in one write. The path starts with
/// `/`, so every key also runs the commands list, which a debug build takes a while over; the
/// keys still came together, so the path is one paste and becomes its picture's chip.
#[test]
fn a_dropped_picture_typed_out_as_keys_becomes_its_chip() {
    let mut driver = TestDriver::launch(Script::idle()).start();
    driver.wait_for(TOP_RULE);
    driver.wait_visible(MODE_IDLE);
    let picture = driver
        .scratch()
        .join("Screenshot 2026-10-08 at 10.07.05 PM.png");
    std::fs::write(&picture, b"\x89PNG\r\n\x1a\n").unwrap();
    let typed = picture.display().to_string().replace(' ', "\\ ");
    assert!(typed.starts_with('/'), "{typed}");

    driver.send_at_once(&typed);
    let screen = wait_input(&driver, "[Image #1]");
    assert_eq!(input_rows(&screen), ["❯ [Image #1]"]);
    driver.assert_no_command("submit");
}

#[test]
fn a_paste_typed_out_as_keys_never_picks_a_prompt_in_the_search() {
    let (path, history) = other_repo_history();
    let (_, mine) = this_repo_history(&["Should this be a workflow?"]);
    let mut driver = TestDriver::launch(Script::idle())
        .ui("keys = \"vim\"")
        .home_file(path, &format!("{history}{mine}"))
        .start();
    driver.wait_visible("-- INSERT --");
    driver.press(Key::Esc);
    driver.wait_visible(MODE_IDLE);
    driver.press(Key::Char('/'));
    driver.wait_visible("Search prompts · everywhere");

    driver.send_at_once("wor\r\rbar");
    let screen = driver.wait_visible("│ ⌕ wor bar");
    expect_row(&screen, "Search prompts · everywhere");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let screen = driver.screen();
    expect_row(&screen, "│ ⌕ wor bar");
    driver.assert_no_command("submit");

    // The search is still yours: Esc leaves it with the input as it was.
    driver.press(Key::Esc);
    let screen = driver.wait_visible(MODE_IDLE);
    let input = input_rows(&screen);
    assert!(input[0].starts_with("❯ Try "), "still empty: {input:?}");
    driver.assert_no_command("submit");
}

/// The footer's Esc i / reaches the `/` commands, so it shows only where it does, and works
/// when Esc and i arrive together, as Alt+i.
#[test]
fn esc_i_slash_from_the_search_opens_the_slash_menu() {
    let (path, history) = other_repo_history();
    let mut driver = TestDriver::launch(Script::idle())
        .ui("keys = \"vim\"")
        .home_file(path, &history)
        .start();
    driver.wait_visible("-- INSERT --");

    // Ctrl+R in INSERT goes back to INSERT, where Esc i / would type, so no hint.
    driver.press(Key::Ctrl('r'));
    let screen = driver.wait_visible("Search prompts · everywhere");
    let hints = expect_row(&screen, "^  ↑/↓ to navigate");
    assert!(
        !screen.visible[hints].contains("Esc i /"),
        "{}",
        screen.visible[hints]
    );
    driver.press(Key::Esc);
    driver.wait_visible("-- INSERT --");

    driver.press(Key::Esc);
    driver.wait_visible(MODE_IDLE);
    driver.press(Key::Char('/'));
    driver.wait_visible("Esc i / for slash commands");
    driver.send_at_once("\x1bi");
    let screen = driver.wait_visible("-- INSERT --");
    assert!(row_of(&screen, "Search prompts").is_none());
    let input = input_rows(&screen);
    assert!(input[0].starts_with("❯ Try "), "nothing typed: {input:?}");
    driver.send("/");
    driver.wait_visible("^  /clear ");
}
