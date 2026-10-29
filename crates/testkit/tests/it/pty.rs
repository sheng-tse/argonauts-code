use std::time::Duration;

use argonauts_testkit::pty::Placeholder;
use argonauts_testkit::{Key, Pty, PtyCommand};

const WAIT: Duration = Duration::from_secs(10);

#[test]
fn types_reads_the_screen_and_keeps_scrollback() {
    let script = "i=1; while [ $i -le 40 ]; do echo line-$i; i=$((i+1)); done; \
                  printf 'name? '; read name; echo \"hello $name\"; read _";
    let mut pty = Pty::spawn(PtyCommand::new("/bin/sh").args(["-c", script]).size(10, 40)).unwrap();
    pty.wait_for("name\\?", WAIT).unwrap();
    pty.send("argo").unwrap();
    pty.press(Key::Enter).unwrap();
    let screen = pty.wait_for("hello argo", WAIT).unwrap();
    assert_eq!(screen.visible.len(), 10);
    assert!(
        screen.scrollback.iter().any(|line| line == "line-1"),
        "{:?}",
        screen.scrollback
    );
    assert!(screen.text().contains("line-40"));

    let dir = tempfile::tempdir().unwrap();
    let dumped = pty.dump_to(dir.path(), "hello").unwrap();
    assert!(
        std::fs::read_to_string(&dumped)
            .unwrap()
            .contains("hello argo")
    );
    assert!(dir.path().join("hello.ansi").is_file());

    pty.press(Key::Enter).unwrap();
    assert_eq!(pty.wait_for_exit(WAIT).unwrap(), 0);
}

#[test]
fn answers_cursor_position_queries() {
    // Ask for the cursor position the way a full-screen app does, and print the reply.
    let script = "stty raw -echo; printf 'ab\\033[6n'; reply=$(dd bs=1 count=6 2>/dev/null); \
                  stty sane; printf '\\n[%s]\\n' \"$(printf '%s' \"$reply\" | tr -d '\\033')\"";
    let mut pty = Pty::spawn(PtyCommand::new("/bin/sh").args(["-c", script]).size(10, 40)).unwrap();
    pty.wait_for("\\[\\[1;3R\\]", WAIT).unwrap();
    pty.wait_for_exit(WAIT).unwrap();
}

#[test]
fn a_missing_pattern_times_out_with_the_screen() {
    let pty = Pty::spawn(PtyCommand::new("/bin/sh").args(["-c", "echo shown; sleep 5"])).unwrap();
    let err = pty
        .wait_for("never shown", Duration::from_millis(300))
        .unwrap_err();
    assert!(err.to_string().contains("shown"), "{err}");
}

#[test]
fn keeps_rows_scrolled_off_a_region_at_the_top() {
    // History printed above a live row that stays put, as an inline app prints it.
    let script = "printf '\\033[10;1Hlive\\033[1;6r\\033[6;1H'; i=1; \
                  while [ $i -le 12 ]; do printf '\\nhistory-%s' $i; i=$((i+1)); done; \
                  printf '\\033[r\\033[9;1Hdone'; read _";
    let mut pty = Pty::spawn(PtyCommand::new("/bin/sh").args(["-c", script]).size(10, 40)).unwrap();
    let screen = pty.wait_for("done", WAIT).unwrap();
    let history: Vec<String> = (1..=6).map(|n| format!("history-{n}")).collect();
    assert!(
        screen.scrollback.ends_with(&history),
        "{:?}",
        screen.scrollback
    );
    assert_eq!(screen.visible[0], "history-7", "{}", screen.text());
    assert_eq!(screen.visible[5], "history-12", "{}", screen.text());
    assert_eq!(screen.visible[9], "live", "{}", screen.text());
    pty.press(Key::Enter).unwrap();
    assert_eq!(pty.wait_for_exit(WAIT).unwrap(), 0);
}

#[test]
fn a_new_cell_size_tells_the_program_even_at_the_same_rows_and_columns() {
    let script = "n=0; trap 'n=$((n+1)); echo \"told $n\"' WINCH; echo ready; \
                  while :; do sleep 0.1; done";
    let mut pty = Pty::spawn(PtyCommand::new("/bin/sh").args(["-c", script]).size(10, 40)).unwrap();
    pty.wait_for("ready", WAIT).unwrap();
    assert_eq!(pty.pixel_size().unwrap(), (400, 200));

    // A font size change: the same rows and columns, bigger cells.
    pty.resize_with_cell(10, 40, (12, 26)).unwrap();
    pty.wait_for("told 1", WAIT).unwrap();
    assert_eq!(pty.pixel_size().unwrap(), (480, 260));

    // A plain resize keeps the cells as they are now.
    pty.resize(12, 50).unwrap();
    pty.wait_for("told 2", WAIT).unwrap();
    assert_eq!(pty.pixel_size().unwrap(), (600, 312));
}

#[test]
fn a_test_can_set_the_pixels_a_cell() {
    let pty = Pty::spawn(
        PtyCommand::new("/bin/sh")
            .args(["-c", "read _"])
            .size(38, 120)
            .cell_pixels(9, 21),
    )
    .unwrap();
    assert_eq!(pty.pixel_size().unwrap(), (1080, 798));
}

#[test]
fn enter_waits_for_the_typed_text_on_screen_not_any_output() {
    // A program that draws a spinner all along and reads what was typed only after a while,
    // as a busy app does. It shows the text once it has read it, then reads the Enter, giving
    // up after 3 s.
    let script = "stty raw -echo min 0 time 30; (while :; do printf .; sleep 0.03; done) & \
                  printf 'ready\\r\\n'; sleep 0.4; first=$(dd bs=64 count=1 2>/dev/null); \
                  printf '\\r\\nshown: %s\\r\\n' \"$first\"; dd bs=64 count=1 >/dev/null 2>&1; \
                  kill $!; stty sane; printf '\\nfirst read %s bytes\\n' \"${#first}\"";
    let mut pty = Pty::spawn(PtyCommand::new("/bin/sh").args(["-c", script]).size(10, 60)).unwrap();
    pty.wait_for("ready", WAIT).unwrap();
    pty.send("hello there").unwrap();
    pty.press(Key::Enter).unwrap();
    // Read with the text, the Enter would make it 12.
    pty.wait_for("first read \\d+ bytes", WAIT).unwrap();
    assert!(
        pty.screen().text().contains("first read 11 bytes"),
        "{}",
        pty.screen().text()
    );
    pty.wait_for_exit(WAIT).unwrap();
}

#[test]
fn a_picture_sent_before_the_alternate_screen_leaves_its_placeholders_blank() {
    // A picture and its placement for placeholders, sent on the main screen; then its two
    // placeholder cells on the alternate screen; then the picture sent again there.
    let picture =
        r"\033_Ga=t,f=100,q=2,i=7,m=0;iVBORw0KGgo=\033\\\033_Ga=p,U=1,i=7,c=2,r=1,q=2\033\\";
    let cells =
        r"\033[38;2;0;0;7m\364\216\273\256\314\205\314\205\364\216\273\256\314\205\314\215\033[0m";
    let script = format!(
        "printf '{picture}\\033[?1049h\\033[1;1H{cells} one'; read _; \
         printf '{picture} two'; read _"
    );
    let mut pty = Pty::spawn(
        PtyCommand::new("/bin/sh")
            .args(["-c", &script])
            .size(5, 40)
            .takes_pictures(),
    )
    .unwrap();
    pty.wait_for_visible("one", WAIT).unwrap();
    let blank = pty.check_pictures().unwrap_err().to_string();
    assert!(
        blank.contains("placeholder for picture 7 on the alternate screen, which holds none"),
        "{blank}"
    );

    pty.press(Key::Enter).unwrap();
    pty.wait_for_visible("two", WAIT).unwrap();
    pty.wait_for_pictures(WAIT).unwrap();
    let cells = pty.cells();
    assert!(cells.alternate());
    for col in 0..2 {
        assert_eq!(
            cells.get(0, col).and_then(|cell| cell.picture),
            Some(Placeholder { id: 7, shows: true })
        );
    }
    pty.press(Key::Enter).unwrap();
    assert_eq!(pty.wait_for_exit(WAIT).unwrap(), 0);
}

/// Set on the test program when it runs as the program in an outer pty.
const INSIDE: &str = "ARGONAUTS_PTY_TEST_INSIDE";

#[test]
fn the_program_never_sees_the_terminal_the_tests_run_in() {
    // This test program runs again in a pty that says it is Ghostty, as a developer's terminal
    // would, and from there starts the program that looks.
    let outer = Pty::spawn(
        PtyCommand::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pty::looks_for_the_terminal_from_inside",
                "--ignored",
                "--nocapture",
            ])
            .env(INSIDE, "1")
            .env("TERM_PROGRAM", "ghostty")
            .env(
                "GHOSTTY_RESOURCES_DIR",
                "/Applications/Ghostty.app/Contents/Resources",
            )
            .size(20, 100),
    )
    .unwrap();
    let screen = outer
        .wait_for(r"inside: \[.*\] \[.*\] \[.*\] \[.*\]", WAIT)
        .unwrap();
    assert!(
        screen
            .text()
            .contains("inside: [] [] [xterm-256color] [kitty]"),
        "{}",
        screen.text()
    );
}

#[test]
#[ignore = "the program the test above runs"]
fn looks_for_the_terminal_from_inside() {
    if std::env::var_os(INSIDE).is_none() {
        return;
    }
    let script = "printf 'inside: [%s] [%s] [%s] [%s]\\n' \"$TERM_PROGRAM\" \
                  \"$GHOSTTY_RESOURCES_DIR\" \"$TERM\" \"$PLAYS\"";
    let mut inner = Pty::spawn(
        PtyCommand::new("/bin/sh")
            .args(["-c", script])
            .env("PLAYS", "kitty")
            .size(5, 100),
    )
    .unwrap();
    let screen = inner.wait_for("inside: \\[", WAIT).unwrap();
    inner.wait_for_exit(WAIT).unwrap();
    println!("{}", screen.visible_text());
}
