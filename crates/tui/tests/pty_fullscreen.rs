//! The fullscreen screen, `ui.screen = "fullscreen"` and the default: the app takes the
//! terminal's alternate screen with the header at the top and the input at the bottom, the
//! conversation scrolls inside it, and every way out gives the main screen back as the shell
//! left it.

mod pty_support;

use argonauts_core::app::AppEvent;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use pty_support::*;

const HEADER: &str = r"Argonauts Code v\d";
const SHELL_LINE: &str = "$ argonauts";
const ENTER: &str = "\x1b[?1049h";
const LEAVE: &str = "\x1b[?1049l";
const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";
const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";
const BELOW: &str = r"rows? below · pgdn to scroll$";
const WHEEL_UP: &str = "\x1b[<64;10;5M";
const WHEEL_DOWN: &str = "\x1b[<65;10;5M";
const PASTE_ON: &str = "\x1b[?2004h";
const PASTE_OFF: &str = "\x1b[?2004l";
const FOCUS_ON: &str = "\x1b[?1004h";
const FOCUS_OFF: &str = "\x1b[?1004l";
const CURSOR_DEFAULT: &str = "\x1b[0 q";

fn notes(range: std::ops::RangeInclusive<u32>) -> Vec<AppEvent> {
    range
        .map(|n| notice("info", &format!("note {n:02}")))
        .collect()
}

fn raw_text(driver: &TestDriver) -> String {
    String::from_utf8_lossy(&driver.raw()).into_owned()
}

/// Whether the last screen switch the app wrote left the terminal on the alternate screen.
fn on_alternate(driver: &TestDriver) -> bool {
    let raw = raw_text(driver);
    match (raw.rfind(ENTER), raw.rfind(LEAVE)) {
        (Some(enter), Some(leave)) => enter > leave,
        (Some(_), None) => true,
        _ => false,
    }
}

/// The input's top rule, the input, its bottom rule, and the bottom line on the last row.
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
        "only the bottom line is under the input:\n{}",
        screen.visible_text()
    );
}

/// Started from a shell that printed [`SHELL_LINE`], and exited by now: the main screen shows
/// that line, then `after`, and nothing else, and the mouse is the terminal's again.
fn assert_shell_as_it_was(driver: &TestDriver, after: &[&str]) {
    assert!(!on_alternate(driver), "the alternate screen was left");
    let raw = raw_text(driver);
    let off = raw.rfind(MOUSE_OFF).expect("the mouse is given back");
    assert!(off > raw.rfind(MOUSE_ON).unwrap_or(0));
    let screen = driver.screen();
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

fn start_from_shell(script: Script) -> TestDriver {
    TestDriver::launch(script)
        .after_shell_line(SHELL_LINE)
        .start()
}

#[test]
fn the_start_takes_the_alternate_screen_with_the_header_on_top() {
    let driver = start_from_shell(Script::idle());
    let screen = driver.wait_visible(MODE_IDLE);
    assert!(on_alternate(&driver));
    assert!(raw_text(&driver).contains(MOUSE_ON));
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
    let mut driver = TestDriver::start(script);
    let screen = driver.wait_visible("note 60");
    assert!(screen.scrollback.is_empty(), "{}", screen.text());
    assert!(row_of(&screen, "note 01").is_none());
    assert!(
        expect_row(&screen, "note 60") < expect_row(&screen, TOP_RULE),
        "{}",
        screen.visible_text()
    );
    driver.shot("bottom");

    // PgUp goes back a page at a time, as far as the header.
    driver.press(Key::PageUp);
    let screen = driver.wait_visible(BELOW);
    assert!(
        row_of(&screen, "note 60").is_none(),
        "{}",
        screen.visible_text()
    );
    assert_input_at_bottom(&screen);
    for _ in 0..4 {
        driver.press(Key::PageUp);
    }
    let screen = driver.wait_visible(HEADER);
    assert!(
        row_of(&screen, "note 01").is_some(),
        "{}",
        screen.visible_text()
    );
    driver.shot("top");

    // Output that arrives while you read back leaves the view where it is.
    let before = screen.visible.clone();
    driver.signal("more");
    let screen = driver.wait_visible("New output below");
    let rule = expect_row(&screen, TOP_RULE);
    assert_eq!(screen.visible[..rule - 2], before[..rule - 2]);
    assert!(row_of(&screen, "note 64").is_none());

    // The wheel scrolls too, and back at the bottom new output shows again.
    driver.send(WHEEL_DOWN);
    for _ in 0..40 {
        driver.send(WHEEL_DOWN);
    }
    let screen = driver.wait_visible("note 64");
    assert!(
        row_of(&screen, BELOW).is_none(),
        "{}",
        screen.visible_text()
    );
    driver.send(WHEEL_UP);
    let screen = driver.wait_visible(r"3 rows below · pgdn to scroll");
    assert!(
        row_of(&screen, "note 64").is_none(),
        "{}",
        screen.visible_text()
    );
    driver.press(Key::PageDown);
    driver.wait_gone(BELOW);
    driver.wait_visible("note 64");
}

#[test]
fn sending_a_message_jumps_back_to_the_bottom() {
    let script = Script::idle().all(0, notes(1..=60));
    let mut driver = TestDriver::start(script);
    driver.wait_visible("note 60");
    driver.press(Key::PageUp);
    driver.wait_visible(BELOW);
    driver.submit("hello");
    let screen = driver.wait_visible("note 60");
    assert!(
        row_of(&screen, BELOW).is_none(),
        "{}",
        screen.visible_text()
    );
}

#[test]
fn ctrl_o_shows_the_full_transcript_and_comes_back() {
    let mut driver = TestDriver::start(Script::idle().all(0, notes(1..=3)));
    driver.wait_visible("note 03");
    driver.press(Key::Ctrl('o'));
    driver.wait_visible("Showing detailed transcript");
    driver.press(Key::Esc);
    let screen = driver.wait_gone("Showing detailed transcript");
    assert!(on_alternate(&driver));
    assert!(
        row_of(&screen, "note 03").is_some(),
        "{}",
        screen.visible_text()
    );
    assert_input_at_bottom(&screen);
}

#[test]
fn ctrl_c_twice_gives_the_shell_back_as_it_was() {
    let mut driver = start_from_shell(Script::idle().all(0, notes(1..=3)));
    driver.wait_visible("note 03");
    driver.press(Key::Ctrl('c'));
    driver.wait_visible("Press Ctrl-C again to exit");
    driver.press(Key::Ctrl('c'));
    assert_eq!(driver.wait_exit(), 0);
    driver.shot("after");
    assert_shell_as_it_was(&driver, &[]);
}

#[test]
fn slash_exit_gives_the_shell_back_as_it_was() {
    let mut driver = start_from_shell(Script::idle().all(0, notes(1..=3)));
    driver.wait_visible("note 03");
    driver.send("/exit");
    driver.press(Key::Enter);
    assert_eq!(driver.wait_exit(), 0);
    assert_shell_as_it_was(&driver, &[]);
}

#[test]
fn a_message_that_says_exit_or_quit_leaves() {
    for word in ["exit", " quit "] {
        let mut driver = start_from_shell(Script::idle());
        driver.wait_visible(MODE_IDLE);
        driver.send(word);
        driver.press(Key::Enter);
        assert_eq!(driver.wait_exit(), 0, "{word:?}");
        driver.assert_no_command("submit");
        assert_shell_as_it_was(&driver, &[]);
    }
}

#[test]
fn runs_that_keep_going_leave_one_line_naming_how_to_watch_them() {
    let (_, _, events) = crew_run(1);
    let mut driver = start_from_shell(Script::idle().all(0, events));
    driver.wait_visible(WORKING);
    driver.send("/exit");
    driver.press(Key::Enter);
    assert_eq!(driver.wait_exit(), 0);
    assert_shell_as_it_was(
        &driver,
        &["run-01 keeps going · watch it with: argonauts resume run-01"],
    );
}

#[test]
fn a_panic_gives_the_terminal_back_before_its_message() {
    let script = Script::idle()
        .await_signal("boom")
        .panic("the CLI crashed on purpose");
    let mut driver = start_from_shell(script);
    driver.wait_visible(MODE_IDLE);
    assert!(on_alternate(&driver));
    driver.signal("boom");
    driver.wait_exit();
    assert!(!on_alternate(&driver));
    let raw = raw_text(&driver);
    let message = raw
        .find("the CLI crashed on purpose")
        .expect("the panic printed");
    assert!(
        raw.rfind(LEAVE).is_some_and(|left| left < message),
        "{raw:?}"
    );
    let screen = driver.screen();
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
    let mut driver = TestDriver::start(Script::idle().all(0, notes(1..=3)));
    driver.wait_visible("note 03");
    assert!(on_alternate(&driver));
    let pid = i32::try_from(driver.pid()).expect("a pid fits an i32");
    kill(Pid::from_raw(pid), Signal::SIGTERM).expect("signalling the CLI");
    driver.wait_exit();
    assert!(!on_alternate(&driver));
    let screen = driver.screen();
    assert!(
        screen.visible.iter().all(String::is_empty),
        "{}",
        screen.visible_text()
    );
}

#[test]
fn the_inline_screen_never_takes_the_alternate_screen() {
    let mut driver = TestDriver::launch(Script::idle().all(0, notes(1..=3)))
        .inline()
        .after_shell_line(SHELL_LINE)
        .start();
    driver.wait_visible("note 03");
    assert!(!raw_text(&driver).contains(ENTER));
    assert!(!raw_text(&driver).contains(MOUSE_ON));
    driver.press(Key::Ctrl('c'));
    driver.wait_visible("Press Ctrl-C again to exit");
    driver.press(Key::Ctrl('c'));
    assert_eq!(driver.wait_exit(), 0);
    // The conversation stays in the terminal, under the shell's line.
    let text = driver.text();
    assert!(text.contains(SHELL_LINE), "{text}");
    assert!(text.contains("note 03"), "{text}");
    assert_eq!(count_rows(&driver.screen(), HEADER), 1, "{text}");
}

/// A stop signal closes the inline screen the way `/exit` does: the terminal gets its modes
/// back, and a run that keeps going says how to watch it.
#[test]
fn stop_signals_give_the_inline_screen_the_terminal_back() {
    for signal in [Signal::SIGTERM, Signal::SIGHUP, Signal::SIGINT] {
        let (_, _, run) = crew_run(1);
        let script = Script::idle().all(0, notes(1..=3)).all(0, run);
        let mut driver = TestDriver::launch(script)
            .inline()
            .name(&format!("inline_{signal}"))
            .start();
        driver.wait_visible(WORKING);
        let pid = i32::try_from(driver.pid()).expect("a pid fits an i32");
        kill(Pid::from_raw(pid), signal).expect("signalling the CLI");
        assert_eq!(driver.wait_exit(), 0, "{signal} closes the screen cleanly");
        let raw = raw_text(&driver);
        assert!(!raw.contains(ENTER), "{signal}: {raw:?}");
        for (on, off) in [(PASTE_ON, PASTE_OFF), (FOCUS_ON, FOCUS_OFF)] {
            let given_back = raw
                .rfind(off)
                .is_some_and(|at| at > raw.rfind(on).unwrap_or(0));
            assert!(given_back, "{signal} turns {on:?} off: {raw:?}");
        }
        assert!(raw.ends_with(CURSOR_DEFAULT), "{signal}: {raw:?}");
        // As after any other exit, the conversation stays in the terminal.
        let text = driver.text();
        assert!(text.contains("note 03"), "{signal}: {text}");
        for line in [
            "run-01 keeps going · 1 agent working",
            "Reattach with: argonauts resume run-01",
        ] {
            assert!(text.contains(line), "{signal}: {text}");
        }
    }
}

/// A stop signal while the app still waits on a terminal that never answers its start-up
/// queries, with raw mode already on, waits for the screen, which gives the terminal back.
#[test]
fn a_stop_signal_during_the_start_up_probes_gives_the_terminal_back() {
    const KEYBOARD_QUERY: &str = "\x1b[?u";
    let mut driver = TestDriver::launch(Script::idle().all(0, notes(1..=3)))
        .inline()
        .unanswered()
        .start();
    driver.wait_until("the keyboard query, sent in raw mode", |_| {
        raw_text(&driver).contains(KEYBOARD_QUERY)
    });
    let pid = i32::try_from(driver.pid()).expect("a pid fits an i32");
    kill(Pid::from_raw(pid), Signal::SIGTERM).expect("signalling the CLI");
    assert_eq!(driver.wait_exit(), 0, "the screen closed it");
    let raw = raw_text(&driver);
    for (on, off) in [(PASTE_ON, PASTE_OFF), (FOCUS_ON, FOCUS_OFF)] {
        let given_back = raw
            .rfind(off)
            .is_some_and(|at| at > raw.rfind(on).unwrap_or(0));
        assert!(given_back, "turns {on:?} off: {raw:?}");
    }
    assert!(raw.ends_with(CURSOR_DEFAULT), "{raw:?}");
}

/// Ctrl+C in a program the screen handed the terminal to reaches the app too, since they share
/// the terminal's foreground group. It is the program's, so the app comes back and stays.
#[test]
fn ctrl_c_in_a_handed_over_program_leaves_the_app_running() {
    for inline in [true, false] {
        let run = AppEvent::RunInTerminal {
            argv: ["/bin/sh", "-c", "trap '' INT; kill -INT 0"]
                .map(str::to_owned)
                .to_vec(),
            cwd: std::env::temp_dir(),
        };
        let launch = TestDriver::launch(Script::idle().at(0, notice("info", "note 01")).at(0, run));
        let launch = if inline { launch.inline() } else { launch };
        let mut driver = launch
            .name(&format!("handed_over_ctrl_c_inline_{inline}"))
            .start();
        driver.wait_visible("Back from");
        driver.submit("still here");
        driver.press(Key::Ctrl('c'));
        driver.wait_visible("Press Ctrl-C again to exit");
        driver.press(Key::Ctrl('c'));
        assert_eq!(driver.wait_exit(), 0);
    }
}

#[test]
fn the_frame_holds_at_every_width_while_scrolled() {
    for cols in [80, 100, 120, 200] {
        let mut driver = TestDriver::launch(Script::idle().all(0, notes(1..=60)))
            .size(ROWS, cols)
            .name(&format!("fullscreen_scrolled_{cols}"))
            .start();
        driver.wait_visible("note 60");
        driver.press(Key::PageUp);
        let screen = driver.wait_visible(BELOW);
        assert_frames(&screen, cols);
        assert_input_at_bottom(&screen);
        let rule = expect_row(&screen, TOP_RULE);
        assert_eq!(width(&screen.visible[rule]), usize::from(cols));
    }
}

/// Standard base64 with padding, as OSC 52 carries it.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let at = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (at(0) << 16) | (at(1) << 8) | at(2);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[test]
fn a_drag_copies_the_text_through_the_terminal_and_the_system() {
    use std::os::unix::fs::PermissionsExt;

    // Fakes of the system's copy commands, so the test never touches the real clipboard.
    let fake = tempfile::tempdir().unwrap();
    let clipboard = fake.path().join("clipboard.txt");
    for name in ["pbcopy", "wl-copy", "xclip"] {
        let tool = fake.path().join(name);
        let body = format!("#!/bin/sh\ncat > '{}'\n", clipboard.display());
        std::fs::write(&tool, body).unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(fake.path().to_path_buf()).chain(std::env::split_paths(&inherited)),
    )
    .unwrap();

    let answer = "First line of the answer.\n\n- one\n- two";
    let script = Script::idle()
        .await_command("submit")
        .timed(conversation_turn("t-001", "hi", answer, 5));
    let mut driver = TestDriver::launch(script).env("PATH", path).start();
    driver.wait_visible(MODE_IDLE);
    driver.submit("hi");
    let screen = driver.wait_visible("- two");
    let first = expect_row(&screen, "First line of the answer");
    let last = expect_row(&screen, "- two");

    // SGR reports count from 1: press on the answer's first letter, drag, and let go.
    driver.send(&format!("\x1b[<0;3;{}M", first + 1));
    driver.send(&format!("\x1b[<32;6;{}M", first + 2));
    driver.send(&format!("\x1b[<32;9;{}M", last + 1));
    driver.send(&format!("\x1b[<0;9;{}m", last + 1));
    driver.wait_visible("Copied");
    driver.shot("copied");
    assert_eq!(std::fs::read_to_string(&clipboard).unwrap(), answer);
    let osc52 = format!("\x1b]52;c;{}\x07", base64(answer.as_bytes()));
    assert!(
        raw_text(&driver).contains(&osc52),
        "OSC 52 carries the text"
    );

    // A click selects nothing and copies nothing.
    std::fs::remove_file(&clipboard).unwrap();
    driver.send(&format!("\x1b[<0;3;{}M", first + 1));
    driver.send(&format!("\x1b[<0;3;{}m", first + 1));
    // Keys come in order, so once this is sent the click was taken.
    driver.submit("still here");
    assert!(!clipboard.exists());
}
