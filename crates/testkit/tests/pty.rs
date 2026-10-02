use std::time::Duration;

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
