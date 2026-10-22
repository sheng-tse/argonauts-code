//! The conversation on the main screen: history in the terminal's scrollback, the input pinned to
//! the bottom, streaming answers, Esc, and the screen's shape as Claude Code draws its own.

use crate::pty_support;
use pty_support::*;
use serde_json::json;

const HI_ANSWER: &str = "Hi! I'm the main agent (claude-code/haiku). Ask me about this repo, \
                         or describe a change and I'll run it as Captain or with the crew.";

/// Idle, then an answer to each prompt as the test sends it.
fn conversation_script(turns: &[(&str, &str)]) -> Script {
    turns
        .iter()
        .enumerate()
        .fold(Script::idle(), |script, (i, (prompt, answer))| {
            let turn = format!("t-{:03}", i + 1);
            script
                .await_command("submit")
                .timed(conversation_turn(&turn, prompt, answer, 5))
        })
}

fn long_answer(topic: &str) -> String {
    (1..=12)
        .map(|n| format!("- {topic} point {n}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Each pattern matches exactly one row of the scrollback and screen together, in this order.
fn assert_rows_once_in_order(screen: &Screen, patterns: &[String]) {
    let rows: Vec<&String> = screen.scrollback.iter().chain(&screen.visible).collect();
    let mut from = 0;
    for pattern in patterns {
        assert_eq!(
            count_rows(screen, pattern),
            1,
            "{pattern} once:\n{}",
            screen.text()
        );
        let regex = regex::Regex::new(pattern).unwrap();
        let Some(at) = rows[from..].iter().position(|row| regex.is_match(row)) else {
            panic!("{pattern} out of order:\n{}", screen.text());
        };
        from += at + 1;
    }
}

#[test]
fn history_goes_to_scrollback() {
    let turns = [
        ("first question", long_answer("alpha")),
        ("second question", long_answer("beta")),
        ("third question", long_answer("gamma")),
    ];
    let script: Vec<(&str, &str)> = turns.iter().map(|(p, a)| (*p, a.as_str())).collect();
    let mut driver = TestDriver::launch(conversation_script(&script))
        .inline()
        .start();
    driver.wait_for(TOP_RULE);
    for (prompt, topic) in [
        ("first question", "alpha"),
        ("second question", "beta"),
        ("third question", "gamma"),
    ] {
        driver.submit(prompt);
        driver.wait_for(&format!("{topic} point 12"));
        driver.wait_visible(MODE_IDLE);
    }
    let screen = driver.screen();
    let scrollback = screen.scrollback.join("\n");
    assert!(scrollback.contains("❯ first question"), "{}", screen.text());
    assert!(scrollback.contains("alpha point 1"), "{}", screen.text());
    // Rows scrolled out of a partial scroll region never reach some terminals' scrollback, this
    // one included, so a row lost that way breaks the order.
    let conversation: Vec<String> = [
        ("first question", "alpha"),
        ("second question", "beta"),
        ("third question", "gamma"),
    ]
    .iter()
    .flat_map(|(prompt, topic)| {
        let answer = (1..=12).map(move |n| format!("{topic} point {n}$"));
        std::iter::once(format!("^❯ {prompt}$")).chain(answer)
    })
    .collect();
    assert_rows_once_in_order(&screen, &conversation);
    // Only the live region redraws, so it's never left behind in the scrollback.
    assert_eq!(count_rows(&screen, MODE_IDLE), 1, "{}", screen.text());
    assert_eq!(count_rows(&screen, TOP_RULE), 1, "{}", screen.text());
}

#[test]
fn input_box_pinned_under_blank_rows() {
    let mut driver = TestDriver::start(conversation_script(&[("hi", "Short answer.")]));
    driver.wait_for(TOP_RULE);
    driver.submit("hi");
    driver.wait_visible(TURN_END);
    let screen = driver.wait_visible(MODE_IDLE);
    let turn_end = expect_row(&screen, TURN_END);
    let rule = expect_row(&screen, TOP_RULE);
    assert!(
        rule > turn_end + 5
            && screen.visible[turn_end + 1..rule]
                .iter()
                .all(|row| row.trim().is_empty()),
        "blank rows between the conversation and the input:\n{}",
        screen.visible_text()
    );
    assert!(
        screen.visible[rule + 1].starts_with('❯'),
        "the input is right under the rule:\n{}",
        screen.visible_text()
    );
    assert_eq!(
        blank_rows_at_bottom(&screen),
        0,
        "the input sits at the bottom, as in Claude Code:\n{}",
        screen.visible_text()
    );
}

#[test]
fn streaming_text_word_by_word() {
    let answer = "Streaming arrives one word at a time until the very last word, finish.";
    let script = Script::idle()
        .await_command("submit")
        .timed(conversation_turn("t-001", "stream", answer, 120));
    let mut driver = TestDriver::start(script);
    driver.wait_for(TOP_RULE);
    driver.submit("stream please");
    let partial = driver.wait_until("part of the answer", |screen| {
        let text = screen.visible_text();
        text.contains("Streaming") && !text.contains("finish.")
    });
    driver.shot("partial");
    let screen = driver.wait_visible("finish\\.");
    assert_eq!(
        count_rows(&screen, "Streaming arrives"),
        1,
        "{}",
        screen.text()
    );
    assert!(partial.visible_text().contains("Streaming"));
    driver.wait_visible(TURN_END);
}

#[test]
fn esc_interrupts_conversation_turn() {
    let mut turn = vec![
        (0, mode_answering("t-001")),
        (0, turn_started("t-001", "explain")),
    ];
    turn.extend(
        words("The export command first reads")
            .iter()
            .enumerate()
            .map(|(i, w)| (10 * i as u64, text_delta("t-001", w))),
    );
    let script = Script::idle()
        .await_command("submit")
        .timed(turn)
        .await_command("cancel_turn")
        .at(0, turn_cancelled("t-001"))
        .at(0, mode_idle());
    let mut driver = TestDriver::start(script);
    driver.wait_for(TOP_RULE);
    driver.submit("explain the export");
    driver.wait_visible("first reads");
    driver.press(Key::Esc);
    driver.wait_command("cancel_turn");
    let screen = driver.wait_visible("⎿  Interrupted · What should Argonauts Code do instead\\?");
    let partial = expect_row(&screen, "The export command first reads");
    let interrupted = expect_row(
        &screen,
        "Interrupted · What should Argonauts Code do instead",
    );
    assert_eq!(
        interrupted,
        partial + 1,
        "right under the partial answer:\n{}",
        screen.visible_text()
    );
    assert!(
        row_of(&screen, TURN_END).is_none(),
        "no turn end:\n{}",
        screen.visible_text()
    );
}

#[test]
fn main_screen_rows_come_in_order() {
    let mut driver = TestDriver::start(conversation_script(&[("hi", HI_ANSWER)]));
    driver.wait_for(TOP_RULE);
    driver.submit("hi");
    driver.wait_visible(TURN_END);
    let screen = driver.wait_visible(MODE_IDLE);
    assert_order(
        &screen,
        &[
            "Argonauts Code",
            "^❯ hi$",
            &format!("^{BULLET} Hi! I'm the main agent"),
            // At this width the answer ends on the second row.
            "^  \\S.*with the crew\\.$",
            TURN_END,
            TOP_RULE,
            INPUT_ROW,
            "^─+$",
            MODE_IDLE,
        ],
    );
    let answer = expect_row(&screen, "Hi! I'm the main agent");
    assert!(
        screen.visible[answer - 1].is_empty(),
        "a blank row before the answer"
    );
    let status = expect_row(&screen, STATUS_IDLE);
    assert_eq!(
        status + 1 + blank_rows_at_bottom(&screen),
        screen.visible.len()
    );
    assert_frames(&screen, COLS);
}

#[test]
fn idle_screen_keeps_its_layout() {
    let mut driver = TestDriver::start(conversation_script(&[("hi", HI_ANSWER)]));
    driver.wait_for(TOP_RULE);
    driver.submit("hi");
    driver.wait_visible(TURN_END);
    let screen = driver.wait_visible(MODE_IDLE);
    let rule = expect_row(&screen, TOP_RULE);
    let rows = &screen.visible[rule..rule + 4];
    assert_eq!(
        width(&rows[0]),
        usize::from(COLS),
        "the top rule spans the width"
    );
    assert!(rows[0].ends_with(" Council · Haiku ─"), "{rows:?}");
    assert!(rows[1].starts_with("❯"), "{rows:?}");
    assert!(
        is_rule(&rows[2]) && width(&rows[2]) == usize::from(COLS),
        "{rows:?}"
    );
    // The bottom line is the bottom row, with each CLI's plan usage against its right edge. At
    // 100 columns the chips take the room of the footer's hint.
    assert!(
        rows[3].starts_with("  ǁ supervised mode on (shift+tab to cycle)  "),
        "{rows:?}"
    );
    assert_eq!(expect_row(&screen, STATUS_IDLE), rule + 3, "{rows:?}");
    assert_eq!(width(&rows[3]), usize::from(COLS), "{rows:?}");
    assert_eq!(
        rule + 4 + blank_rows_at_bottom(&screen),
        screen.visible.len(),
        "{rows:?}"
    );
    let turn_end = expect_row(&screen, TURN_END);
    assert!(screen.visible[turn_end].contains(" · done "));
}

#[test]
fn busy_screen_keeps_its_layout() {
    let (_, _, events) = crew_run(3);
    let script = Script::idle().all(0, events);
    let driver = TestDriver::launch(script)
        .ui("[panel]\nagents = true")
        .start();
    driver.wait_visible("^    ○ task-003 ");
    let screen = driver.wait_visible("←  3 agents · ↓  to manage");
    assert_order(
        &screen,
        &[
            WORKING_LINE,
            TOP_RULE,
            INPUT_ROW,
            "^─+$",
            // A run's agents keep going past Esc, so with only a run at work
            // the bottom line makes no offer to interrupt. At 100 columns the chips take the room
            // of the mode's first two hints.
            "^  ǁ supervised mode on( \\(shift\\+tab to cycle\\))?( · \\? for shortcuts)? · ←  3 agents · ↓  to manage( {2,}\\S.*)?$",
            // The list is Claude Code's: your conversation as `main`, then a row per run,
            // named by its workflow and title, with its agents
            // under it, each by its name, what it's doing, and how long and how many tokens.
            // This recording is from before agents had labels, so each goes by its task.
            &format!("^  {BULLET} main$"),
            "^  ◯ Council · Add a --json flag to export +▱+ +0/3 · ",
            &format!(
                "^    ○ task-002 +Update\\(src/export\\.rs\\) +{LISTED_FOR} · ↓  [\\d.]+k tokens$"
            ),
            &format!(
                "^    ○ task-003 +Update\\(src/export\\.rs\\) +{LISTED_FOR} · ↓  [\\d.]+k tokens$"
            ),
            &format!(
                "^    ○ task-004 +Update\\(src/export\\.rs\\) +{LISTED_FOR} · ↓  [\\d.]+k tokens$"
            ),
        ],
    );
    // Agents at work show in the list under the input; the conversation gets each once it finishes.
    assert!(
        !screen
            .visible
            .iter()
            .any(|row| row.starts_with(&format!("{BULLET} Opus on Claude Code · worker"))),
        "{}",
        screen.visible_text()
    );
    let working = expect_row(&screen, WORKING_LINE);
    assert!(
        screen.visible[working + 1].is_empty(),
        "a blank row under the working line"
    );
    assert!(
        !screen.visible[working].contains('$'),
        "spend lives on the bottom line, not the working line"
    );
    assert_frames(&screen, COLS);
}

#[test]
fn terminal_title_glyph_and_session_title() {
    let (_, _, events) = crew_run(1);
    let script = Script::idle().all(0, events);
    let driver = TestDriver::start(script);
    let title = driver.wait_title("Add a --json flag to export");
    let glyph = title.chars().next().unwrap_or(' ');
    assert!(
        !glyph.is_alphanumeric() && title.chars().nth(1) == Some(' '),
        "the title starts with a state glyph: {title:?}"
    );
}

#[test]
fn recap_after_long_run() {
    let mut run = RunScript::default();
    let script = Script::idle().all(
        0,
        [
            status_with(json!({"run": RUN, "title": "Add a --json flag to export"})),
            mode_working(RUN),
            run.started("Add a --json flag to export", "crew"),
            run.finished("merged", "4 tasks, 7 files, +120 -18"),
            recap("Export now takes --json, all 4 tasks merged, and the tests pass."),
            mode_idle(),
        ],
    );
    let driver = TestDriver::start(script);
    let screen = driver.wait_visible("※ recap: Export now takes --json");
    // The note may wrap onto the next row.
    driver.wait_visible(r"disable recaps in\s+/config");
    assert_order(&screen, &["※ recap: Export now takes --json", TOP_RULE]);
    assert_eq!(count_rows(&screen, "※ recap"), 1, "{}", screen.text());
}

/// Esc with the main agent's own subagents running stops its turn alone: the
/// line Esc leaves shows under what it wrote, the subagent stays in the list, still going, and
/// with only the subagent at work there is no working line and nothing more for Esc to stop.
#[test]
fn esc_stops_the_main_turn_and_leaves_its_subagents_running() {
    let subagent = |status: &str, doing: &str| {
        conversation_agent(
            "t-001",
            json!({"type": "subagent", "update": {"id": "toolu_a", "parent": null,
                "kind": "subagent", "label": "Count slowly", "prompt": null, "status": status,
                "last_message": doing, "log_uri": null, "steps_visible": true,
                "tokens": 11823}}),
        )
    };
    let background = conversation_agent(
        "t-001",
        json!({"type": "background_tasks", "running": ["task-a"]}),
    );
    let mut turn = vec![
        (0, mode_answering("t-001")),
        (0, turn_started("t-001", "count in the background")),
        (0, subagent("running", "Counting to four hundred")),
        (0, background),
    ];
    turn.extend(
        words("The count runs on its own while I")
            .iter()
            .enumerate()
            .map(|(i, w)| (10 * i as u64, text_delta("t-001", w))),
    );
    let stopped = [
        json!({"type": "error", "category": "interrupted", "message": "interrupted",
               "retryable": false}),
        json!({"type": "result", "text": null, "structured": null, "is_error": true}),
    ];
    let script = Script::idle()
        .await_command("submit")
        .timed(turn)
        .await_command("cancel_turn")
        .all(
            0,
            stopped
                .map(|event| conversation_agent("t-001", event))
                .to_vec(),
        );
    // Wide enough that the bottom line leaves the mode its `esc to interrupt`.
    let mut driver = TestDriver::launch(script).size(ROWS, 160).start();
    driver.wait_for(TOP_RULE);
    driver.submit("count in the background");
    driver.wait_visible("on its own while I");
    driver.wait_visible("esc to interrupt");
    driver.press(Key::Esc);
    driver.wait_command("cancel_turn");
    let screen = driver.wait_visible("⎿  Interrupted · What should Argonauts Code do instead\\?");
    let partial = expect_row(&screen, "The count runs on its own while I");
    let interrupted = expect_row(
        &screen,
        "Interrupted · What should Argonauts Code do instead",
    );
    assert_eq!(interrupted, partial + 1, "{}", screen.visible_text());
    // Its subagent is still listed and going, with its time and the tokens Claude Code counted
    // for it, and the main agent rests.
    let screen = driver.wait_gone("esc to interrupt");
    let row = expect_row(
        &screen,
        "○ Count slowly +Counting to four hundred +\\d+s · ↓  11.8k tokens",
    );
    assert!(row > interrupted, "{}", screen.visible_text());
    driver.press(Key::Esc);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let cancels = driver
        .command_types()
        .iter()
        .filter(|kind| *kind == "cancel_turn")
        .count();
    assert_eq!(cancels, 1, "nothing is left for Esc to stop");
    expect_row(&driver.screen(), "○ Count slowly +Counting to four hundred");
}
