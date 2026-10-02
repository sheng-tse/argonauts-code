//! The fullscreen screen, `ui.screen = "fullscreen"` and the default: the app takes the
//! terminal's alternate screen with the header at the top and the input at the bottom, the
//! conversation scrolls inside it, and every way out gives the main screen back as the shell
//! left it.

mod pty_support;

use argonauts_core::app::AppEvent;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use pty_support::*;

const HEADER: &str = r"Argonauts v\d";
const SHELL_LINE: &str = "$ argonauts";
const ENTER: &str = "\x1b[?1049h";
const LEAVE: &str = "\x1b[?1049l";
const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1006h";
const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1000l";
const BELOW: &str = r"rows? below · pgdn to scroll$";
const WHEEL_UP: &str = "\x1b[<64;10;5M";
const WHEEL_DOWN: &str = "\x1b[<65;10;5M";

fn notes(range: std::ops::RangeInclusive<u32>) -> Vec<AppEvent> {
    range
        .map(|n| notice("info", &format!("note {n:02}")))
        .collect()
}

fn raw_text(harness: &Harness) -> String {
    String::from_utf8_lossy(&harness.raw()).into_owned()
}

/// Whether the last screen switch the app wrote left the terminal on the alternate screen.
fn on_alternate(harness: &Harness) -> bool {
    let raw = raw_text(harness);
    match (raw.rfind(ENTER), raw.rfind(LEAVE)) {
        (Some(enter), Some(leave)) => enter > leave,
        (Some(_), None) => true,
        _ => false,
    }
}

/// The input's top rule, the input, its bottom rule, and the mode line on the last row.
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
        screen.visible.len() - rule,
        4,
        "only the mode line is under the input:\n{}",
        screen.visible_text()
    );
}

/// Started from a shell that printed [`SHELL_LINE`], and exited by now: the main screen shows
/// that line, then `after`, and nothing else, and the mouse is the terminal's again.
fn assert_shell_as_it_was(harness: &Harness, after: &[&str]) {
    assert!(!on_alternate(harness), "the alternate screen was left");
    let raw = raw_text(harness);
    let off = raw.rfind(MOUSE_OFF).expect("the mouse is given back");
    assert!(off > raw.rfind(MOUSE_ON).unwrap_or(0));
    let screen = harness.screen();
    assert!(screen.scrollback.is_empty(), "{}", screen.text());
    let mut expected = vec![SHELL_LINE.to_owned()];
    expected.extend(after.iter().map(|line| (*line).to_owned()));
    let shown: Vec<&str> = screen
        .visible
        .iter()
        .map(String::as_str)
        .filter(|row| !row.is_empty())
        .collect();
    assert_eq!(shown, expected, "{}", screen.visible_text());
    assert_eq!(screen.visible[0], SHELL_LINE, "{}", screen.visible_text());
}

fn start_from_shell(script: Script) -> Harness {
    Harness::launch(script).after_shell_line(SHELL_LINE).start()
}

#[test]
fn the_start_takes_the_alternate_screen_with_the_header_on_top() {
    let harness = start_from_shell(Script::idle());
    let screen = harness.wait_visible(MODE_IDLE);
    assert!(on_alternate(&harness));
    assert!(raw_text(&harness).contains(MOUSE_ON));
    assert!(screen.visible[0].is_empty(), "{}", screen.visible_text());
    assert_eq!(expect_row(&screen, HEADER), 1, "{}", screen.visible_text());
    assert_input_at_bottom(&screen);
    // The shell's line stays on the main screen, out of view, and nothing went to scrollback.
    assert!(screen.scrollback.is_empty(), "{}", screen.text());
    assert!(row_of(&screen, r"^\$ argonauts$").is_none());
}

#[test]
fn the_conversation_scrolls_inside_the_app() {
    let script = Script::idle()
        .all(0, notes(1..=60))
        .await_signal("more")
        .all(0, notes(61..=64));
    let mut harness = Harness::start(script);
    let screen = harness.wait_visible("note 60");
    assert!(screen.scrollback.is_empty(), "{}", screen.text());
    assert!(row_of(&screen, "note 01").is_none());
    assert!(
        expect_row(&screen, "note 60") < expect_row(&screen, TOP_RULE),
        "{}",
        screen.visible_text()
    );
    harness.shot("bottom");

    // PgUp goes back a page at a time, as far as the header.
    harness.press(Key::PageUp);
    let screen = harness.wait_visible(BELOW);
    assert!(
        row_of(&screen, "note 60").is_none(),
        "{}",
        screen.visible_text()
    );
    assert_input_at_bottom(&screen);
    for _ in 0..4 {
        harness.press(Key::PageUp);
    }
    let screen = harness.wait_visible(HEADER);
    assert!(
        row_of(&screen, "note 01").is_some(),
        "{}",
        screen.visible_text()
    );
    harness.shot("top");

    // Output that arrives while you read back leaves the view where it is.
    let before = screen.visible.clone();
    harness.signal("more");
    let screen = harness.wait_visible("New output below");
    let rule = expect_row(&screen, TOP_RULE);
    assert_eq!(screen.visible[..rule - 2], before[..rule - 2]);
    assert!(row_of(&screen, "note 64").is_none());

    // The wheel scrolls too, and back at the bottom new output shows again.
    harness.send(WHEEL_DOWN);
    for _ in 0..40 {
        harness.send(WHEEL_DOWN);
    }
    let screen = harness.wait_visible("note 64");
    assert!(
        row_of(&screen, BELOW).is_none(),
        "{}",
        screen.visible_text()
    );
    harness.send(WHEEL_UP);
    let screen = harness.wait_visible(r"3 rows below · pgdn to scroll");
    assert!(
        row_of(&screen, "note 64").is_none(),
        "{}",
        screen.visible_text()
    );
    harness.press(Key::PageDown);
    harness.wait_gone(BELOW);
    harness.wait_visible("note 64");
}

#[test]
fn sending_a_message_jumps_back_to_the_bottom() {
    let script = Script::idle().all(0, notes(1..=60));
    let mut harness = Harness::start(script);
    harness.wait_visible("note 60");
    harness.press(Key::PageUp);
    harness.wait_visible(BELOW);
    harness.submit("hello");
    let screen = harness.wait_visible("note 60");
    assert!(
        row_of(&screen, BELOW).is_none(),
        "{}",
        screen.visible_text()
    );
}

#[test]
fn ctrl_o_shows_the_full_transcript_and_comes_back() {
    let mut harness = Harness::start(Script::idle().all(0, notes(1..=3)));
    harness.wait_visible("note 03");
    harness.press(Key::Ctrl('o'));
    harness.wait_visible("Showing detailed transcript");
    harness.press(Key::Esc);
    let screen = harness.wait_gone("Showing detailed transcript");
    assert!(on_alternate(&harness));
    assert!(
        row_of(&screen, "note 03").is_some(),
        "{}",
        screen.visible_text()
    );
    assert_input_at_bottom(&screen);
}

#[test]
fn ctrl_c_twice_gives_the_shell_back_as_it_was() {
    let mut harness = start_from_shell(Script::idle().all(0, notes(1..=3)));
    harness.wait_visible("note 03");
    harness.press(Key::Ctrl('c'));
    harness.wait_visible("Press Ctrl-C again to exit");
    harness.press(Key::Ctrl('c'));
    assert_eq!(harness.wait_exit(), 0);
    harness.shot("after");
    assert_shell_as_it_was(&harness, &[]);
}

#[test]
fn slash_exit_gives_the_shell_back_as_it_was() {
    let mut harness = start_from_shell(Script::idle().all(0, notes(1..=3)));
    harness.wait_visible("note 03");
    harness.send("/exit");
    harness.press(Key::Enter);
    assert_eq!(harness.wait_exit(), 0);
    assert_shell_as_it_was(&harness, &[]);
}

#[test]
fn a_message_that_says_exit_or_quit_leaves() {
    for word in ["exit", " quit "] {
        let mut harness = start_from_shell(Script::idle());
        harness.wait_visible(MODE_IDLE);
        harness.send(word);
        harness.press(Key::Enter);
        assert_eq!(harness.wait_exit(), 0, "{word:?}");
        harness.assert_no_command("submit");
        assert_shell_as_it_was(&harness, &[]);
    }
}

#[test]
fn runs_that_keep_going_leave_one_line_naming_how_to_watch_them() {
    let (_, _, events) = crew_run(1);
    let mut harness = start_from_shell(Script::idle().all(0, events));
    harness.wait_visible("esc to interrupt");
    harness.send("/exit");
    harness.press(Key::Enter);
    assert_eq!(harness.wait_exit(), 0);
    assert_shell_as_it_was(
        &harness,
        &["run-01 keeps going · watch it with: argonauts resume run-01"],
    );
}

#[test]
fn a_panic_gives_the_terminal_back_before_its_message() {
    let script = Script::idle()
        .await_signal("boom")
        .panic("the harness crashed on purpose");
    let mut harness = start_from_shell(script);
    harness.wait_visible(MODE_IDLE);
    assert!(on_alternate(&harness));
    harness.signal("boom");
    harness.wait_exit();
    assert!(!on_alternate(&harness));
    let raw = raw_text(&harness);
    let message = raw
        .find("the harness crashed on purpose")
        .expect("the panic printed");
    assert!(
        raw.rfind(LEAVE).is_some_and(|left| left < message),
        "{raw:?}"
    );
    let screen = harness.screen();
    assert_eq!(screen.visible[0], SHELL_LINE, "{}", screen.visible_text());
    assert!(
        row_of(&screen, HEADER).is_none(),
        "{}",
        screen.visible_text()
    );
    assert!(row_of(&screen, "crashed on purpose").is_some());
}

#[test]
fn sigterm_gives_the_terminal_back() {
    let mut harness = Harness::start(Script::idle().all(0, notes(1..=3)));
    harness.wait_visible("note 03");
    assert!(on_alternate(&harness));
    let pid = i32::try_from(harness.pid()).expect("a pid fits an i32");
    kill(Pid::from_raw(pid), Signal::SIGTERM).expect("signalling the harness");
    harness.wait_exit();
    assert!(!on_alternate(&harness));
    let screen = harness.screen();
    assert!(
        screen.visible.iter().all(String::is_empty),
        "{}",
        screen.visible_text()
    );
}

#[test]
fn the_inline_screen_never_takes_the_alternate_screen() {
    let mut harness = Harness::launch(Script::idle().all(0, notes(1..=3)))
        .inline()
        .after_shell_line(SHELL_LINE)
        .start();
    harness.wait_visible("note 03");
    assert!(!raw_text(&harness).contains(ENTER));
    assert!(!raw_text(&harness).contains(MOUSE_ON));
    harness.press(Key::Ctrl('c'));
    harness.wait_visible("Press Ctrl-C again to exit");
    harness.press(Key::Ctrl('c'));
    assert_eq!(harness.wait_exit(), 0);
    // The chat stays in the terminal, under the shell's line.
    let text = harness.text();
    assert!(text.contains(SHELL_LINE), "{text}");
    assert!(text.contains("note 03"), "{text}");
    assert_eq!(count_rows(&harness.screen(), HEADER), 1, "{text}");
}

#[test]
fn the_frame_holds_at_every_width_while_scrolled() {
    for cols in [80, 100, 120, 200] {
        let mut harness = Harness::launch(Script::idle().all(0, notes(1..=60)))
            .size(ROWS, cols)
            .name(&format!("fullscreen_scrolled_{cols}"))
            .start();
        harness.wait_visible("note 60");
        harness.press(Key::PageUp);
        let screen = harness.wait_visible(BELOW);
        assert_frames(&screen, cols);
        assert_input_at_bottom(&screen);
        let rule = expect_row(&screen, TOP_RULE);
        assert_eq!(width(&screen.visible[rule]), usize::from(cols));
    }
}
