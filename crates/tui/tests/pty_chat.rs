//! The chat on the main screen: history in the terminal's scrollback, the input pinned to the
//! bottom, streaming answers, Esc, and the screen's shape against Claude Code's.

mod pty_support;

use pty_support::*;
use serde_json::json;

const HI_ANSWER: &str = "Hi! I'm the default agent (claude-code/haiku). Ask me about this repo, \
                         or describe a change and I'll run it solo or with the crew.";

/// Idle, then an answer to each prompt as the test sends it.
fn chat_script(turns: &[(&str, &str)]) -> Script {
    turns
        .iter()
        .enumerate()
        .fold(Script::idle(), |script, (i, (prompt, answer))| {
            let turn = format!("t-{:03}", i + 1);
            script
                .await_command("submit")
                .timed(chat_turn(&turn, prompt, answer, 5))
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
    let mut harness = Harness::start(chat_script(&script));
    harness.wait_for(TOP_RULE);
    for (prompt, topic) in [
        ("first question", "alpha"),
        ("second question", "beta"),
        ("third question", "gamma"),
    ] {
        harness.submit(prompt);
        harness.wait_for(&format!("{topic} point 12"));
        harness.wait_visible(MODE_IDLE);
    }
    let screen = harness.screen();
    let scrollback = screen.scrollback.join("\n");
    assert!(scrollback.contains("❯ first question"), "{}", screen.text());
    assert!(scrollback.contains("alpha point 1"), "{}", screen.text());
    // Rows scrolled out of a partial scroll region never reach some terminals' scrollback, this
    // one included, so a row lost that way breaks the order.
    let chat: Vec<String> = [
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
    assert_rows_once_in_order(&screen, &chat);
    // Only the live region redraws, so it's never left behind in the scrollback.
    assert_eq!(count_rows(&screen, MODE_IDLE), 1, "{}", screen.text());
    assert_eq!(count_rows(&screen, TOP_RULE), 1, "{}", screen.text());
}

#[test]
fn input_box_pinned_under_blank_rows() {
    let mut harness = Harness::start(chat_script(&[("hi", "Short answer.")]));
    harness.wait_for(TOP_RULE);
    harness.submit("hi");
    harness.wait_visible(TURN_END);
    let screen = harness.wait_visible(MODE_IDLE);
    let turn_end = expect_row(&screen, TURN_END);
    let rule = expect_row(&screen, TOP_RULE);
    assert!(
        rule > turn_end + 5
            && screen.visible[turn_end + 1..rule]
                .iter()
                .all(|row| row.trim().is_empty()),
        "blank rows between the chat and the input:\n{}",
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
        .timed(chat_turn("t-001", "stream", answer, 120));
    let mut harness = Harness::start(script);
    harness.wait_for(TOP_RULE);
    harness.submit("stream please");
    let partial = harness.wait_until("part of the answer", |screen| {
        let text = screen.visible_text();
        text.contains("Streaming") && !text.contains("finish.")
    });
    harness.shot("partial");
    let screen = harness.wait_visible("finish\\.");
    assert_eq!(
        count_rows(&screen, "Streaming arrives"),
        1,
        "{}",
        screen.text()
    );
    assert!(partial.visible_text().contains("Streaming"));
    harness.wait_visible(TURN_END);
}

#[test]
fn esc_interrupts_chat_turn() {
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
    let mut harness = Harness::start(script);
    harness.wait_for(TOP_RULE);
    harness.submit("explain the export");
    harness.wait_visible("first reads");
    harness.press(Key::Esc);
    harness.wait_command("cancel_turn");
    let screen = harness.wait_visible("⎿  Interrupted · What should Argonauts do instead\\?");
    let partial = expect_row(&screen, "The export command first reads");
    let interrupted = expect_row(&screen, "Interrupted · What should Argonauts do instead");
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
    let mut harness = Harness::start(chat_script(&[("hi", HI_ANSWER)]));
    harness.wait_for(TOP_RULE);
    harness.submit("hi");
    harness.wait_visible(TURN_END);
    let screen = harness.wait_visible(MODE_IDLE);
    assert_order(
        &screen,
        &[
            "Argonauts",
            "^❯ hi$",
            &format!("^{BULLET} Hi! I'm the default agent"),
            // At this width the answer ends on the second row.
            "^  \\S.*with the crew\\.$",
            TURN_END,
            TOP_RULE,
            INPUT_ROW,
            "^─+$",
            MODE_IDLE,
        ],
    );
    let answer = expect_row(&screen, "Hi! I'm the default agent");
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
    let mut harness = Harness::start(chat_script(&[("hi", HI_ANSWER)]));
    harness.wait_for(TOP_RULE);
    harness.submit("hi");
    harness.wait_visible(TURN_END);
    let screen = harness.wait_visible(MODE_IDLE);
    let rule = expect_row(&screen, TOP_RULE);
    let rows = &screen.visible[rule..rule + 4];
    assert_eq!(
        width(&rows[0]),
        usize::from(COLS),
        "the top rule spans the width"
    );
    assert!(rows[0].ends_with(" peer-crew ─"), "{rows:?}");
    assert!(rows[1].starts_with("❯"), "{rows:?}");
    assert!(
        is_rule(&rows[2]) && width(&rows[2]) == usize::from(COLS),
        "{rows:?}"
    );
    // The mode line is the bottom row, with what fits of the quotas and spend at its right.
    assert!(
        rows[3].starts_with("  ǁ supervised mode on (shift+tab to cycle) · ? for shortcuts  "),
        "{rows:?}"
    );
    assert!(
        rows[3].ends_with("  Claude 5h 71% used · $0.00"),
        "{rows:?}"
    );
    assert_eq!(width(&rows[3]), usize::from(COLS) - 1, "{rows:?}");
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
    let harness = Harness::launch(script).ui("[panel]\nagents = true").start();
    harness.wait_visible("◐ GPT 5.6 on Codex");
    let screen = harness.wait_visible("esc to interrupt · ← 3 agents · ↓ to manage");
    assert_order(
        &screen,
        &[
            &format!("^{BULLET} Opus on Claude Code · owner · card-002 \\(Add the JSON writer\\)"),
            "^  ⎿  ",
            WORKING_LINE,
            TOP_RULE,
            INPUT_ROW,
            "^─+$",
            "^  ǁ supervised mode on \\(shift\\+tab to cycle\\) · esc to interrupt · ← 3 agents · ↓ to manage( {2,}\\S.*)?$",
            // The panel has a row per run, named by its workflow and title, with its agents
            // under it, and the kit keeps Claude Code's space
            // after the arrow. An agent's row is its circle, name, card, bar, and stats, and a
            // name too long for the column is cut.
            "^  ○ peer-crew · Add a --json flag to ex\\S*… +0/3 agents · ",
            &format!("^    ◐ Opus on Claude Code +card-\\d+ +{ELAPSED} · ↓ "),
            &format!("^    ◐ GPT 5\\.6 on Codex +card-\\d+ +{ELAPSED} · ↓ "),
            &format!("^    ◐ Gemini 3\\.6 Pro on Antig\\S*… +card-\\d+ +{ELAPSED} · ↓ "),
        ],
    );
    let working = expect_row(&screen, WORKING_LINE);
    assert!(
        screen.visible[working + 1].is_empty(),
        "a blank row under the working line"
    );
    assert!(
        !screen.visible[working].contains('$'),
        "spend lives on the mode line, not the working line"
    );
    assert_frames(&screen, COLS);
}

#[test]
fn terminal_title_glyph_and_session_title() {
    let (_, _, events) = crew_run(1);
    let script = Script::idle().all(0, events);
    let harness = Harness::start(script);
    let title = harness.wait_title("Add a --json flag to export");
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
            run.finished("merged", "4 cards, 7 files, +120 -18"),
            recap("Export now takes --json, all 4 cards merged, and the tests pass."),
            mode_idle(),
        ],
    );
    let harness = Harness::start(script);
    let screen = harness.wait_visible("※ recap: Export now takes --json");
    // The note may wrap onto the next row.
    harness.wait_visible(r"disable recaps in\s+/config");
    assert_order(&screen, &["※ recap: Export now takes --json", TOP_RULE]);
    assert_eq!(count_rows(&screen, "※ recap"), 1, "{}", screen.text());
}
