//! Reattaching to a run: the condensed replay instead of every event, no bells for history,
//! and the full history one Ctrl+O away.

mod pty_support;

use argonauts_core::app::AppEvent;
use pty_support::*;

fn replayed(tool_calls: usize) -> (RunScript, Vec<Agent>, Script) {
    let (run, crew, events) = long_replayed_run(tool_calls);
    let script = Script::new()
        .at(0, welcome_with_runs(1))
        .at(0, status_line())
        .all(0, events)
        .at(0, mode_working(RUN));
    (run, crew, script)
}

#[test]
fn reattach_long_run_prints_summaries_only() {
    let (_, _, script) = replayed(40);
    let harness = Harness::start(script);
    harness.wait_visible(&format!("※ recap: {REPLAY_RECAP}"));
    let screen = harness.wait_visible("esc to interrupt");
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
    // Keep the question open at the end of the history, so its card shows but stays quiet.
    let done = events.pop();
    let condensed = events.pop();
    assert!(matches!(done, Some(AppEvent::ReplayDone { .. })));
    events.push(run.question_checkpoint("cp-002", &crew[0]));
    events.extend(condensed);
    events.push(replay_done(RUN, run.next_seq() - 1));
    let script = Script::new()
        .at(0, welcome_with_runs(1))
        .at(0, status_line())
        .all(0, events)
        .at(0, mode_working(RUN))
        .await_signal("live")
        .at(0, run.merge_checkpoint("cp-003"));
    let harness = Harness::start(script);
    harness.wait_visible(&format!("※ recap: {REPLAY_RECAP}"));
    harness.wait_visible("Keep a CSV fallback\\?");
    // Still open, the question takes the keys, though it never rings.
    harness.wait_visible("❯ 1\\. Yes, keep it");
    assert_eq!(harness.bells(), 0, "history never rings");
    harness.signal("live");
    harness.wait_until("the bell for the new card", |_| harness.bells() > 0);
    assert_eq!(harness.bells(), 1);
}

#[test]
fn resume_shows_condensed_replay() {
    let (_, _, script) = replayed(5);
    let harness = Harness::launch(script).size(48, 100).start();
    harness.wait_visible(&format!("※ recap: {REPLAY_RECAP}"));
    let screen = harness.wait_visible("esc to interrupt");
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
    let mut harness = Harness::start(script);
    harness.wait_visible(&format!("※ recap: {REPLAY_RECAP}"));
    harness.wait_visible("esc to interrupt");
    harness.press(Key::Ctrl('o'));
    let history = harness.wait_gone(TOP_RULE);
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
    harness.press(Key::Esc);
    let main = harness.wait_visible(TOP_RULE);
    assert!(
        main.scrollback.iter().all(|row| !row.contains("replayed_")),
        "and the main screen stays condensed:\n{}",
        main.text()
    );
}
