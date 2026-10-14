//! Keys on the main screen: Ctrl+C, Ctrl+G, `?`, the arrows into the agent panel and the
//! views, and handing the terminal to an agent's own UI.

mod pty_support;

use std::os::unix::fs::PermissionsExt;

use pty_support::*;
use serde_json::json;

fn crew_with_panel(agents: usize) -> (RunScript, Vec<Agent>, TestDriver) {
    let (run, crew, events) = crew_run(agents);
    let driver = TestDriver::launch(Script::idle().all(0, events))
        .ui("[panel]\nagents = true")
        .start();
    driver.wait_visible("↓ to manage");
    (run, crew, driver)
}

/// The input row, right under the top rule.
fn input_row(driver: &TestDriver) -> String {
    let screen = driver.screen();
    let rule = expect_row(&screen, TOP_RULE);
    screen.visible.get(rule + 1).cloned().unwrap_or_default()
}

fn wait_input(driver: &TestDriver, text: &str) {
    driver.wait_until(&format!("the input to read {text:?}"), |screen| {
        row_of(screen, TOP_RULE)
            .and_then(|rule| screen.visible.get(rule + 1))
            .is_some_and(|row| row.trim_end() == format!("❯ {text}").trim_end())
    });
}

#[test]
fn ctrl_c_clears_then_exits_twice() {
    let (_, _, events) = crew_run(1);
    let mut driver = TestDriver::start(Script::idle().all(0, events));
    driver.wait_visible(WORKING);
    driver.send("a draft to throw away");
    driver.wait_visible("❯ a draft to throw away");
    driver.press(Key::Ctrl('c'));
    driver.wait_gone("a draft to throw away");
    driver.wait_visible("Press Ctrl-C again to exit · the run keeps going");
    driver.press(Key::Ctrl('c'));
    assert_eq!(driver.wait_exit(), 0);
    // The main screen comes back with one line under it: what keeps going, and how to watch.
    let text = driver.text();
    assert_eq!(
        text.trim(),
        "run-01 keeps going · watch it with: argonauts resume run-01"
    );
}

#[test]
fn ctrl_c_twice_on_the_inline_screen_leaves_what_keeps_going_in_scrollback() {
    let (_, _, events) = crew_run(1);
    let mut driver = TestDriver::launch(Script::idle().all(0, events))
        .inline()
        .start();
    driver.wait_visible(WORKING);
    driver.press(Key::Ctrl('c'));
    driver.wait_visible("Press Ctrl-C again to exit · the run keeps going");
    driver.press(Key::Ctrl('c'));
    assert_eq!(driver.wait_exit(), 0);
    let text = driver.text();
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
    let mut driver = TestDriver::launch(Script::idle())
        .env("EDITOR", editor.as_os_str())
        .start();
    driver.wait_visible(MODE_IDLE);
    driver.send("first draft");
    driver.wait_visible("❯ first draft");
    driver.press(Key::Ctrl('g'));
    wait_input(&driver, "rewritten in the editor");
    driver.press(Key::Enter);
    driver.wait_command_where("submit", |c| c["text"] == "rewritten in the editor");
}

#[test]
fn keyboard_map_table() {
    let mut driver = TestDriver::start(Script::idle());
    driver.wait_visible(MODE_IDLE);
    driver.send("?");
    let screen = driver.wait_visible("ctrl \\+ o");
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
        "in place of the bottom line"
    );
    driver.press(Key::Esc);
    driver.wait_visible(MODE_IDLE);
    driver.wait_gone("ctrl \\+ o");
    assert!(
        !input_row(&driver).contains('?'),
        "`?` isn't typed into the input"
    );
}

#[test]
fn navigation_keys_consistent() {
    let (_, _, mut driver) = crew_with_panel(2);
    // The run's row comes first, so the first agent is two rows down.
    driver.press(Key::Down);
    driver.press(Key::Down);
    driver.press(Key::Enter);
    let session = driver.wait_gone(TOP_RULE);
    assert!(
        session.visible_text().contains("Opus on Claude Code"),
        "{}",
        session.visible_text()
    );
    driver.press(Key::Left);
    let crew = driver.wait_visible("All");
    assert!(
        crew.visible_text().contains("GPT 5.6 on Codex"),
        "{}",
        crew.visible_text()
    );
    driver.press(Key::Right);
    driver.wait_visible("Opus on Claude Code");
    driver.press(Key::Esc);
    driver.press(Key::Esc);
    // Back at the list with the agent still selected, the rule names who a message is for.
    driver.wait_visible("^─+ task-002 ─$");
    driver.press(Key::Esc);
    driver.wait_visible(TOP_RULE);
    driver.press(Key::Left);
    driver.wait_gone(TOP_RULE);
    driver.wait_visible("All");
    driver.press(Key::Esc);
    driver.wait_visible(TOP_RULE);
}

#[test]
fn down_focuses_agent_panel() {
    let (_, _, mut driver) = crew_with_panel(3);
    // The run's row comes first, with its agents under it, so codex is three rows down.
    driver.press(Key::Down);
    driver.press(Key::Down);
    driver.press(Key::Down);
    driver.press(Key::Enter);
    driver.wait_gone(TOP_RULE);
    // The codex agent's view opened.
    driver.wait_visible("GPT 5\\.6 on Codex");
    driver.press(Key::Esc);
    // Back at the list, its row is still selected: the rule names it, the input offers to
    // message it, and the list's keys take the bottom line, in Claude Code's words.
    let listed = driver.wait_visible("^─+ task-003 ─$");
    let rule = expect_row(&listed, "^─+ task-003 ─$");
    assert_eq!(listed.visible[rule + 1], "❯ Message @task-003…");
    assert!(
        listed.visible[rule + 3].starts_with(
            "  ↑/↓ to select · enter to view · x to stop it · ctrl+x ctrl+k to stop all agents"
        ),
        "{listed:?}"
    );
    assert!(
        listed.visible[rule + 8].starts_with("    ● task-003 "),
        "{listed:?}"
    );
    driver.press(Key::Esc);
    driver.wait_visible(TOP_RULE);
    driver.press(Key::Down);
    driver.press(Key::Down);
    // The first x asks inline, like Ctrl+C's double press, and the second stops.
    driver.press(Key::Char('x'));
    driver.wait_visible("x again to stop task-002 · esc to keep");
    driver.press(Key::Char('x'));
    let stop = driver.wait_command("stop_agent");
    assert_eq!(
        stop,
        json!({"type": "stop_agent", "run": RUN, "session": "s-001"})
    );
}

#[test]
fn down_without_agents_walks_history() {
    let script = Script::idle()
        .await_command("submit")
        .timed(conversation_turn("t-001", "first prompt", "One.", 5))
        .await_command("submit")
        .timed(conversation_turn("t-002", "second prompt", "Two.", 5));
    let mut driver = TestDriver::start(script);
    driver.wait_visible(MODE_IDLE);
    driver.submit("first prompt");
    driver.wait_visible("One\\.");
    driver.submit("second prompt");
    driver.wait_visible("Two\\.");
    driver.wait_visible(MODE_IDLE);
    driver.press(Key::Up);
    wait_input(&driver, "second prompt");
    driver.press(Key::Up);
    wait_input(&driver, "first prompt");
    driver.press(Key::Down);
    wait_input(&driver, "second prompt");
    driver.press(Key::Down);
    let screen = driver.wait_until("the draft back", |screen| {
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
    let driver = TestDriver::launch(Script::idle().all(0, events).all(0, todos))
        .ui("[panel]\nagents = true")
        .env("NO_COLOR", "1")
        .start();
    let screen = driver.wait_visible("↓ 18\\.1k tokens");
    let mode = expect_row(&screen, "↓ to manage");
    let mut last = expect_row(&screen, "Council · Add a --json flag to export ");
    assert!(last > mode, "the run's row under the bottom line");
    for agent in &crew {
        // Under the run's row, a row of the list: the dot, the agent's name (its task, in a
        // recording from before labels), what it's doing, then its to-do count, how long, and
        // its tokens.
        let pattern = format!(
            "^    ○ {} +\\S.* +2/5 · {LISTED_FOR} · ↓ [\\d.]+k tokens$",
            regex::escape(&agent.task),
        );
        let row = expect_row(&screen, &pattern);
        assert!(
            row > last,
            "rows in order under the run's row:\n{}",
            screen.visible_text()
        );
        last = row;
    }
    // The run's row keeps the bar, and the agents' stats start in one column.
    let run_row = &screen.visible[expect_row(&screen, "Council · Add a --json flag to export ")];
    assert!(run_row.contains(['▰', '▱']), "{run_row}");
    let stats: Vec<Option<usize>> = crew
        .iter()
        .map(|agent| {
            let pattern = format!("^    ○ {} ", regex::escape(&agent.task));
            let row = &screen.visible[expect_row(&screen, &pattern)];
            row.find("2/5").map(|at| row[..at].chars().count())
        })
        .collect();
    assert!(
        stats[0].is_some() && stats.windows(2).all(|w| w[0] == w[1]),
        "stats line up: {stats:?}"
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
            run.task_created(&agent),
            run.session_started(&agent, 1),
            take_over(&agent, &[], "/wt/task-002"),
        ],
    );
    // The agent's block names the endpoint it runs on.
    let driver = TestDriver::launch(script)
        .ui("agents_in_conversation = \"as_they_work\"")
        .start();
    let screen = driver
        .wait_visible(r"A direct endpoint has no UI to hand over\. /reassign the task instead\.");
    assert!(
        screen.visible_text().contains("Ollama"),
        "{}",
        screen.visible_text()
    );
    driver.wait_visible(TOP_RULE);
    driver.assert_no_command("take_over");
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
                run.task_created(&agent),
                run.session_started(&agent, 1),
                take_over(&agent, &["sh", "-c", "echo in the agent ui; exit 3"], &cwd),
            ],
        )
        .await_command("take_over_ended")
        .at(0, run.take_over_return("approval-001", &agent))
        .at(0, mode_waiting(RUN, "approval"));
    let driver = TestDriver::start(script);
    // The program printed on the main screen, which the app's own screen covers again after.
    driver.wait_until("the program's output", |_| {
        String::from_utf8_lossy(&driver.raw()).contains("in the agent ui")
    });
    let ended = driver.wait_command("take_over_ended");
    assert_eq!(ended["run"], RUN);
    assert_eq!(ended["task"], agent.task);
    assert_eq!(ended["exit_code"], 3);
    let screen = driver.wait_visible("Let the worker continue");
    // The question holds the keys, so it stands in for the input above the bottom line.
    assert_order(&screen, &["◆", "Let the worker continue", "mode on"]);
    driver.wait_gone(TOP_RULE);
    driver.wait_until("the bell", |_| driver.bells() > 0);
}
