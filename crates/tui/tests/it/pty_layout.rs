//! The screen's shape, as in Claude Code's: the header sits at the top, and the input is pinned
//! to the bottom rows. On the inline screen, what the shell showed goes up into the scrollback,
//! history fills down from the header, then scrolls into the scrollback, and a restart in the
//! same terminal shows one header. `pty_fullscreen.rs` covers the alternate screen.

use crate::pty_support;
use argonauts_core::app::AppEvent;
use argonauts_testkit::screen::{Grid, Look, REAL_SIZES, laws};
use pty_support::*;
use regex::Regex;

const HEADER: &str = r"Argonauts Code v\d";
const SHELL_LINE: &str = "$ argonauts";

fn visible_count(screen: &Screen, pattern: &str) -> usize {
    let pattern = Regex::new(pattern).expect("a valid pattern");
    screen
        .visible
        .iter()
        .filter(|row| pattern.is_match(row))
        .count()
}

fn notes(range: std::ops::RangeInclusive<u32>) -> Vec<AppEvent> {
    range
        .map(|n| notice("info", &format!("note {n:02}")))
        .collect()
}

/// The input's top rule, the input, its bottom rule, then the bottom line ends the screen.
fn assert_input_at_bottom(screen: &Screen) {
    let rule = expect_row(screen, TOP_RULE);
    assert!(
        screen.visible[rule + 1].starts_with('❯'),
        "{}",
        screen.visible_text()
    );
    assert!(
        is_rule(&screen.visible[rule + 2]),
        "{}",
        screen.visible_text()
    );
    assert_eq!(
        blank_rows_at_bottom(screen),
        0,
        "the live region ends on the last row:\n{}",
        screen.visible_text()
    );
    assert!(
        screen.visible.len() - rule == 4,
        "only the bottom line is under the input:\n{}",
        screen.visible_text()
    );
}

#[test]
fn the_start_shows_the_header_on_top_and_the_input_at_the_bottom() {
    let driver = TestDriver::launch(Script::idle())
        .inline()
        .after_shell_line(SHELL_LINE)
        .start();
    // The input and the header can reach the terminal in frames of their own.
    let screen = driver.wait_until("the header and the input", |screen| {
        row_of(screen, HEADER).is_some() && row_of(screen, MODE_IDLE).is_some()
    });
    // As in Claude Code, the top row stays blank and the header starts under it.
    assert_eq!(
        expect_row(&screen, HEADER),
        1,
        "the header starts on the second row:\n{}",
        screen.visible_text()
    );
    assert!(screen.visible[0].is_empty(), "{}", screen.visible_text());
    assert_input_at_bottom(&screen);

    // The shell's line went up into the scrollback. Nothing erased it.
    assert_eq!(
        count_rows(&screen, r"^\$ argonauts$"),
        1,
        "{}",
        screen.text()
    );
    assert!(
        !screen.visible.iter().any(|row| row == SHELL_LINE),
        "{}",
        screen.visible_text()
    );
    let raw = String::from_utf8_lossy(&driver.raw()).into_owned();
    assert!(!raw.contains("\x1b[3J"), "the scrollback is never erased");
    assert!(
        !raw.contains("\x1b[2J"),
        "some terminals copy a cleared screen to scrollback"
    );
}

#[test]
fn history_fills_under_the_header_then_scrolls_into_the_scrollback() {
    let script = Script::idle()
        .all(50, notes(1..=3))
        .await_signal("more")
        .all(0, notes(4..=40));
    let driver = TestDriver::launch(script).inline().start();
    let screen = driver.wait_visible("note 03");
    let header = expect_row(&screen, HEADER);
    let first = expect_row(&screen, "note 01");
    let last = expect_row(&screen, "note 03");
    let rule = expect_row(&screen, TOP_RULE);
    assert!(header < first && first < last, "{}", screen.visible_text());
    assert!(
        last + 5 < rule,
        "blank rows between history and the input:\n{}",
        screen.visible_text()
    );
    assert!(
        screen.visible[last + 1..rule]
            .iter()
            .all(|row| row.trim().is_empty()),
        "{}",
        screen.visible_text()
    );
    assert_input_at_bottom(&screen);

    driver.signal("more");
    let screen = driver.wait_visible("note 40");
    assert_input_at_bottom(&screen);
    assert_eq!(expect_row(&screen, TOP_RULE), rule, "the input stays put");
    let last = expect_row(&screen, "note 40");
    assert!(
        screen.visible[last + 1..rule]
            .iter()
            .all(|row| row.trim().is_empty()),
        "history ends right above the live region:\n{}",
        screen.visible_text()
    );
    assert!(
        last + 3 >= rule,
        "a full screen leaves no gap:\n{}",
        screen.visible_text()
    );
    // Every note shows once, in order, and the oldest are in the scrollback.
    let rows: Vec<&String> = screen.scrollback.iter().chain(&screen.visible).collect();
    let places: Vec<usize> = (1..=40)
        .map(|n| {
            let note = format!("note {n:02}");
            let hits: Vec<usize> = (0..rows.len())
                .filter(|&i| rows[i].trim_end().ends_with(&note))
                .collect();
            assert_eq!(hits.len(), 1, "{note} once:\n{}", screen.text());
            hits[0]
        })
        .collect();
    assert!(
        places.windows(2).all(|pair| pair[0] < pair[1]),
        "{}",
        screen.text()
    );
    assert!(screen.scrollback.iter().any(|row| row.contains("note 01")));
    assert_eq!(count_rows(&screen, HEADER), 1, "{}", screen.text());
    assert!(
        row_of(&screen, HEADER).is_none(),
        "the header scrolled away"
    );
}

#[test]
fn a_restart_shows_only_the_new_header() {
    let first = Script::idle()
        .at(0, notice("info", "first session"))
        .await_signal("quit")
        .at(0, exit());
    let second = Script::idle().at(0, notice("info", "second session"));
    let driver = TestDriver::launch(first)
        .inline()
        .after_shell_line(SHELL_LINE)
        .then_restart(second)
        .start();
    driver.wait_visible("first session");
    driver.signal("quit");
    let screen = driver.wait_until("the second start", |screen| {
        row_of(screen, "second session").is_some() && row_of(screen, MODE_IDLE).is_some()
    });

    assert_eq!(
        visible_count(&screen, HEADER),
        1,
        "one header on screen:\n{}",
        screen.visible_text()
    );
    assert_eq!(expect_row(&screen, HEADER), 1, "{}", screen.visible_text());
    assert!(
        row_of(&screen, "first session").is_none(),
        "{}",
        screen.visible_text()
    );
    assert_input_at_bottom(&screen);
    // The first session and both shell lines are in the scrollback, in order.
    assert_eq!(count_rows(&screen, "first session"), 1, "{}", screen.text());
    assert_eq!(
        count_rows(&screen, r"^\$ argonauts$"),
        2,
        "{}",
        screen.text()
    );
    assert_eq!(count_rows(&screen, HEADER), 2, "{}", screen.text());
}

#[test]
fn the_frame_holds_at_every_width() {
    for cols in [80, 100, 120, 200] {
        let script = Script::idle()
            .all(0, notes(1..=2))
            .await_command("submit")
            .timed(conversation_turn("t-001", "hi", "Short answer.", 5));
        let mut driver = TestDriver::launch(script)
            .size(ROWS, cols)
            .name(&format!("frame_{cols}"))
            .start();
        driver.wait_visible(MODE_IDLE);
        driver.submit("hi");
        driver.wait_visible(TURN_END);
        let screen = driver.wait_visible(MODE_IDLE);
        assert_frames(&screen, cols);
        assert_input_at_bottom(&screen);
        assert_eq!(expect_row(&screen, HEADER), 1, "{}", screen.visible_text());
        let rule = expect_row(&screen, TOP_RULE);
        assert_eq!(width(&screen.visible[rule]), usize::from(cols));
    }
}

#[test]
fn a_resize_keeps_the_input_at_the_bottom() {
    let mut driver = TestDriver::start(Script::idle().all(0, notes(1..=3)));
    driver.wait_visible("note 03");
    for size in REAL_SIZES {
        let (rows, cols) = (size.rows, size.cols);
        driver.resize(rows, cols);
        let screen = driver.wait_until("the live region redrawn at the bottom", |screen| {
            screen.visible.len() == usize::from(rows)
                && blank_rows_at_bottom(screen) == 0
                && row_of(screen, TOP_RULE)
                    .is_some_and(|rule| width(&screen.visible[rule]) == usize::from(cols))
        });
        assert_input_at_bottom(&screen);
        assert_frames(&screen, cols);
        assert_eq!(
            visible_count(&screen, TOP_RULE),
            1,
            "{}",
            screen.visible_text()
        );
        assert_eq!(
            visible_count(&screen, "note 03"),
            1,
            "{}",
            screen.visible_text()
        );
        assert!(
            expect_row(&screen, "note 03") < expect_row(&screen, TOP_RULE),
            "{}",
            screen.visible_text()
        );
        let grid = Grid::from_text(cols, &screen.visible);
        laws::assert_kept(&grid, &Look::new(), &format!("the idle screen at {size}"));
    }
}

/// Once the setup saves, the header on screen changes where it stands: the screen and the
/// scrollback hold one header, and it names the new workflow.
#[test]
fn a_saved_setup_changes_the_header_in_place() {
    let mut saved = serde_json::to_value(welcome()).expect("an AppEvent writes as JSON");
    saved["workflow"] = serde_json::json!("captain");
    let script = Script::idle()
        .all(50, notes(1..=3))
        .at(300, app(saved))
        .at(300, notice("info", "Saved to ~/config.toml."));
    let driver = TestDriver::start(script);
    let screen = driver.wait_visible("Saved to");

    assert_eq!(count_rows(&screen, HEADER), 1, "{}", screen.text());
    let header = expect_row(&screen, HEADER);
    assert_eq!(header, 1, "{}", screen.visible_text());
    assert!(
        screen.visible[header + 1].ends_with("· Captain"),
        "{}",
        screen.visible_text()
    );
    let first = expect_row(&screen, "note 01");
    let saved = expect_row(&screen, "Saved to");
    assert!(header < first && first < saved, "{}", screen.visible_text());
    assert_input_at_bottom(&screen);
}
