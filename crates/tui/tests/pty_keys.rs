//! Keys on the main screen: Ctrl+C, Ctrl+G, `?`, the arrows into the agent panel and the
//! views, and handing the terminal to an agent's own UI.

mod pty_support;

use std::os::unix::fs::PermissionsExt;

use pty_support::*;
use serde_json::json;

fn crew_with_panel(agents: usize) -> (RunScript, Vec<Agent>, Harness) {
    let (run, crew, events) = crew_run(agents);
    let harness = Harness::launch(Script::idle().all(0, events))
        .ui("[panel]\nagents = true")
        .start();
    harness.wait_visible("↓ to manage");
    (run, crew, harness)
}

/// The input row, right under the top rule.
fn input_row(harness: &Harness) -> String {
    let screen = harness.screen();
    let rule = expect_row(&screen, TOP_RULE);
    screen.visible.get(rule + 1).cloned().unwrap_or_default()
}

fn wait_input(harness: &Harness, text: &str) {
    harness.wait_until(&format!("the input to read {text:?}"), |screen| {
        row_of(screen, TOP_RULE)
            .and_then(|rule| screen.visible.get(rule + 1))
            .is_some_and(|row| row.trim_end() == format!("❯ {text}").trim_end())
    });
}

#[test]
fn ctrl_c_clears_then_exits_twice() {
    let (_, _, events) = crew_run(1);
    let mut harness = Harness::start(Script::idle().all(0, events));
    harness.wait_visible("esc to interrupt");
    harness.send("a draft to throw away");
    harness.wait_visible("❯ a draft to throw away");
    harness.press(Key::Ctrl('c'));
    harness.wait_gone("a draft to throw away");
    harness.wait_visible("Press Ctrl-C again to exit · the run keeps going");
    harness.press(Key::Ctrl('c'));
    assert_eq!(harness.wait_exit(), 0);
    // The main screen comes back with one line under it: what keeps going, and how to watch.
    let text = harness.text();
    assert_eq!(
        text.trim(),
        "run-01 keeps going · watch it with: argonauts resume run-01"
    );
}

#[test]
fn ctrl_c_twice_on_the_inline_screen_leaves_what_keeps_going_in_scrollback() {
    let (_, _, events) = crew_run(1);
    let mut harness = Harness::launch(Script::idle().all(0, events))
        .inline()
        .start();
    harness.wait_visible("esc to interrupt");
    harness.press(Key::Ctrl('c'));
    harness.wait_visible("Press Ctrl-C again to exit · the run keeps going");
    harness.press(Key::Ctrl('c'));
    assert_eq!(harness.wait_exit(), 0);
    let text = harness.text();
    assert!(text.contains("run-01 keeps going"), "{text}");
    assert!(text.contains("Reattach with: argonauts"), "{text}");
}

#[test]
fn ctrl_g_opens_editor() {
    let tools = tempfile::tempdir().unwrap();
    let editor = tools.path().join("editor.sh");
    std::fs::write(
        &editor,
        "#!/bin/sh\nprintf 'rewritten in the editor' > \"$1\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut harness = Harness::launch(Script::idle())
        .env("EDITOR", editor.as_os_str())
        .start();
    harness.wait_visible(MODE_IDLE);
    harness.send("first draft");
    harness.wait_visible("❯ first draft");
    harness.press(Key::Ctrl('g'));
    wait_input(&harness, "rewritten in the editor");
    harness.press(Key::Enter);
    harness.wait_command_where("submit", |c| c["text"] == "rewritten in the editor");
}

#[test]
fn keyboard_map_table() {
    let mut harness = Harness::start(Script::idle());
    harness.wait_visible(MODE_IDLE);
    harness.send("?");
    let screen = harness.wait_visible("ctrl \\+ o");
    for key in [
        "ctrl \\+ o",
        "ctrl \\+ t",
        "ctrl \\+ g",
        "ctrl \\+ c",
        "shift \\+ tab",
        "esc",
        "/ for",
        "@ for",
    ] {
        assert!(
            row_of(&screen, key).is_some(),
            "{key}:\n{}",
            screen.visible_text()
        );
    }
    let rule = expect_row(&screen, TOP_RULE);
    let hints = expect_row(&screen, "ctrl \\+ o");
    assert!(hints > rule, "under the input:\n{}", screen.visible_text());
    assert!(
        row_of(&screen, MODE_IDLE).is_none(),
        "in place of the mode line"
    );
    harness.press(Key::Esc);
    harness.wait_visible(MODE_IDLE);
    harness.wait_gone("ctrl \\+ o");
    assert!(
        !input_row(&harness).contains('?'),
        "`?` isn't typed into the input"
    );
}

#[test]
fn navigation_keys_consistent() {
    let (_, _, mut harness) = crew_with_panel(2);
    // The run's row comes first, so the first agent is two rows down.
    harness.press(Key::Down);
    harness.press(Key::Down);
    harness.press(Key::Enter);
    let session = harness.wait_gone(TOP_RULE);
    assert!(
        session.visible_text().contains("Opus on Claude Code"),
        "{}",
        session.visible_text()
    );
    harness.press(Key::Left);
    let crew = harness.wait_visible("All");
    assert!(
        crew.visible_text().contains("GPT 5.6 on Codex"),
        "{}",
        crew.visible_text()
    );
    harness.press(Key::Right);
    harness.wait_visible("Opus on Claude Code");
    harness.press(Key::Esc);
    harness.press(Key::Esc);
    harness.wait_visible(TOP_RULE);
    harness.press(Key::Left);
    harness.wait_gone(TOP_RULE);
    harness.wait_visible("All");
    harness.press(Key::Esc);
    harness.wait_visible(TOP_RULE);
}

#[test]
fn down_focuses_agent_panel() {
    let (_, _, mut harness) = crew_with_panel(3);
    // The run's row comes first, with its agents under it, so codex is three rows down.
    harness.press(Key::Down);
    harness.press(Key::Down);
    harness.press(Key::Down);
    harness.press(Key::Enter);
    harness.wait_gone(TOP_RULE);
    // The codex agent's view opened.
    harness.wait_visible("GPT 5\\.6 on Codex");
    harness.press(Key::Esc);
    harness.wait_visible(TOP_RULE);
    harness.press(Key::Esc);
    harness.press(Key::Down);
    harness.press(Key::Down);
    // The first x asks inline, like Ctrl+C's double press, and the second stops.
    harness.press(Key::Char('x'));
    harness.wait_visible("x again to stop Opus on Claude Code · esc to keep");
    harness.press(Key::Char('x'));
    let stop = harness.wait_command("stop_agent");
    assert_eq!(
        stop,
        json!({"type": "stop_agent", "run": RUN, "session": "s-001"})
    );
}

#[test]
fn down_without_agents_walks_history() {
    let script = Script::idle()
        .await_command("submit")
        .timed(chat_turn("t-001", "first prompt", "One.", 5))
        .await_command("submit")
        .timed(chat_turn("t-002", "second prompt", "Two.", 5));
    let mut harness = Harness::start(script);
    harness.wait_visible(MODE_IDLE);
    harness.submit("first prompt");
    harness.wait_visible("One\\.");
    harness.submit("second prompt");
    harness.wait_visible("Two\\.");
    harness.wait_visible(MODE_IDLE);
    harness.press(Key::Up);
    wait_input(&harness, "second prompt");
    harness.press(Key::Up);
    wait_input(&harness, "first prompt");
    harness.press(Key::Down);
    wait_input(&harness, "second prompt");
    harness.press(Key::Down);
    let screen = harness.wait_until("the draft back", |screen| {
        row_of(screen, TOP_RULE)
            .and_then(|rule| screen.visible.get(rule + 1))
            .is_some_and(|row| !row.contains("prompt"))
    });
    assert!(row_of(&screen, "↓ to manage").is_none());
}

#[test]
fn agent_panel_rows_progress() {
    let (mut run, crew, events) = crew_run(3);
    let steps = json!([
        {"text": "Read the writer", "status": "completed"},
        {"text": "Add the change", "status": "completed"},
        {"text": "Run the tests", "status": "in_progress"},
        {"text": "Check the diff", "status": "pending"},
        {"text": "Hand it over", "status": "pending"},
    ]);
    let todos: Vec<_> = crew
        .iter()
        .map(|agent| run.agent(agent, json!({"type": "todo", "items": steps})))
        .collect();
    // Without color the bars are drawn in glyphs, so the screen's text shows them.
    let harness = Harness::launch(Script::idle().all(0, events).all(0, todos))
        .ui("[panel]\nagents = true")
        .env("NO_COLOR", "1")
        .start();
    let screen = harness.wait_visible("↓ 18\\.1k tokens");
    let mode = expect_row(&screen, "↓ to manage");
    let mut last = expect_row(&screen, "peer-crew · Add a --json flag to export ");
    assert!(last > mode, "the run's row under the mode line");
    for agent in &crew {
        // Under the run's row: the circle, the name, the card, the bar, then steps and tokens
        // in their columns, and the time at the edge. With room to spare, no name is cut.
        let pattern = format!(
            "^    ◐ {} +{} +[█░]+ +2/5 +↓ [\\d.]+k tokens +{ELAPSED}",
            regex::escape(&agent.name()),
            regex::escape(&agent.card),
        );
        let row = expect_row(&screen, &pattern);
        assert!(
            row > last,
            "rows in order under the run's row:\n{}",
            screen.visible_text()
        );
        last = row;
    }
    // Each agent's panel row, not its line in the started tree, and its bar's column in cells.
    let bars: Vec<Option<usize>> = crew
        .iter()
        .map(|agent| {
            let pattern = format!("^    ◐ {} ", regex::escape(&agent.name()));
            let row = &screen.visible[expect_row(&screen, &pattern)];
            row.chars().position(|c| c == '█' || c == '░')
        })
        .collect();
    assert!(
        bars[0].is_some() && bars.windows(2).all(|w| w[0] == w[1]),
        "bars line up: {bars:?}"
    );
}

#[test]
fn take_over_endpoint_refused() {
    let mut run = RunScript::default();
    let agent = Agent::endpoint(1);
    let script = Script::idle().all(
        0,
        [
            mode_working(RUN),
            run.started("Add a --json flag to export", "crew"),
            run.card_created(&agent),
            run.session_started(&agent, 1),
            take_over(&agent, &[], "/wt/card-002"),
        ],
    );
    let harness = Harness::start(script);
    let screen = harness
        .wait_visible(r"A direct endpoint has no UI to hand over\. /reassign the card instead\.");
    assert!(
        screen.visible_text().contains("Ollama"),
        "{}",
        screen.visible_text()
    );
    harness.wait_visible(TOP_RULE);
    harness.assert_no_command("take_over");
}

#[test]
fn return_from_take_over_shows_needs_you() {
    let mut run = RunScript::default();
    let agent = Agent::numbered(1);
    let work = tempfile::tempdir().unwrap();
    let cwd = work.path().to_string_lossy().into_owned();
    let script = Script::idle()
        .all(
            0,
            [
                mode_working(RUN),
                run.started("Add a --json flag to export", "crew"),
                run.card_created(&agent),
                run.session_started(&agent, 1),
                take_over(&agent, &["sh", "-c", "echo in the vendor ui; exit 3"], &cwd),
            ],
        )
        .await_command("take_over_ended")
        .at(0, run.take_over_return("cp-001", &agent))
        .at(0, mode_parked(RUN, "checkpoint"));
    let harness = Harness::start(script);
    // The program printed on the main screen, which the app's own screen covers again after.
    harness.wait_until("the program's output", |_| {
        String::from_utf8_lossy(&harness.raw()).contains("in the vendor ui")
    });
    let ended = harness.wait_command("take_over_ended");
    assert_eq!(ended["run"], RUN);
    assert_eq!(ended["card"], agent.card);
    assert_eq!(ended["exit_code"], 3);
    let screen = harness.wait_visible("Let the owner continue");
    // The card holds the keys, so it stands in for the input above the mode line.
    assert_order(&screen, &["◆", "Let the owner continue", "mode on"]);
    harness.wait_gone(TOP_RULE);
    harness.wait_until("the bell", |_| harness.bells() > 0);
}
