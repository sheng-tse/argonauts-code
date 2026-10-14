//! Reattaching to a run: the condensed replay instead of every event, no bells for history,
//! and the full history one Ctrl+O away.

mod pty_support;

use argonauts_core::app::AppEvent;
use pty_support::*;

fn replayed(tool_calls: usize) -> (RunScript, Vec<Agent>, Script) {
    let (run, crew, events) = long_replayed_run(tool_calls);
    let script = Script::new()
        .at(0, welcome_with_runs(1))
        .at(0, bottom_line())
        .all(0, events)
        .at(0, mode_working(RUN));
    (run, crew, script)
}

#[test]
fn reattach_long_run_prints_summaries_only() {
    let (_, _, script) = replayed(40);
    let driver = TestDriver::start(script);
    driver.wait_visible(&format!("※ recap: {REPLAY_RECAP}"));
    let screen = driver.wait_visible(WORKING);
    for milestone in REPLAY_MILESTONES {
        assert_eq!(
            count_rows(&screen, &regex::escape(milestone)),
            1,
            "{milestone}:\n{}",
            screen.text()
        );
    }
    assert!(
        screen
            .scrollback
            .iter()
            .all(|row| !row.contains("replayed_")),
        "no replayed tool call goes to the scrollback:\n{}",
        screen.text()
    );
    let rule = expect_row(&screen, TOP_RULE);
    let replayed_rows = screen.visible[..rule]
        .iter()
        .filter(|row| row.contains("replayed_"))
        .count();
    assert!(
        replayed_rows <= 3,
        "at most each live block's latest call:\n{}",
        screen.text()
    );
}

#[test]
fn replayed_events_ring_no_bell() {
    let (mut run, crew, mut events) = long_replayed_run(3);
    // Keep the question open at the end of the history, so its question shows but stays quiet.
    let done = events.pop();
    let condensed = events.pop();
    assert!(matches!(done, Some(AppEvent::ReplayDone { .. })));
    events.push(run.question_approval("approval-002", &crew[0]));
    events.extend(condensed);
    events.push(replay_done(RUN, run.next_seq() - 1));
    let script = Script::new()
        .at(0, welcome_with_runs(1))
        .at(0, bottom_line())
        .all(0, events)
        .at(0, mode_working(RUN))
        .await_signal("live")
        .at(0, run.merge_approval("approval-003"));
    let driver = TestDriver::start(script);
    driver.wait_visible(&format!("※ recap: {REPLAY_RECAP}"));
    driver.wait_visible("Keep a CSV fallback\\?");
    // Still open, the question takes the keys, though it never rings.
    driver.wait_visible("❯ 1\\. Yes, keep it");
    assert_eq!(driver.bells(), 0, "history never rings");
    driver.signal("live");
    driver.wait_until("the bell for the new task", |_| driver.bells() > 0);
    assert_eq!(driver.bells(), 1);
}

#[test]
fn resume_shows_condensed_replay() {
    let (_, _, script) = replayed(5);
    let driver = TestDriver::launch(script).size(48, 100).start();
    driver.wait_visible(&format!("※ recap: {REPLAY_RECAP}"));
    let screen = driver.wait_visible(WORKING);
    assert_order(
        &screen,
        &[
            "Add a --json flag to export",
            "Add a --json flag to the export command",
            "The JSON writer sits beside the CSV one\\.",
            REPLAY_MILESTONES[0],
            REPLAY_MILESTONES[1],
            REPLAY_MILESTONES[2],
            "※ recap: ",
            WORKING_LINE,
            TOP_RULE,
        ],
    );
    assert_eq!(count_rows(&screen, "※ recap: "), 1, "{}", screen.text());
}

#[test]
fn ctrl_o_opens_full_history_after_resume() {
    let (_, _, script) = replayed(2);
    // The overlay holds a still-working agent's calls through its block, which it has when
    // agents show as they work; otherwise its own view has them.
    let mut driver = TestDriver::launch(script)
        .ui("agents_in_conversation = \"as_they_work\"")
        .start();
    driver.wait_visible(&format!("※ recap: {REPLAY_RECAP}"));
    driver.wait_visible(WORKING);
    driver.press(Key::Ctrl('o'));
    let history = driver.wait_gone(TOP_RULE);
    let calls = history
        .visible
        .iter()
        .filter(|row| row.contains("Read") && row.contains("replayed_"))
        .count();
    assert!(
        calls >= 3,
        "the replayed calls are all there:\n{}",
        history.visible_text()
    );
    driver.press(Key::Esc);
    let main = driver.wait_visible(TOP_RULE);
    assert!(
        main.scrollback.iter().all(|row| !row.contains("replayed_")),
        "and the main screen stays condensed:\n{}",
        main.text()
    );
}
