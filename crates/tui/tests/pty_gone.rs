//! When its terminal goes away, the app goes too. A capture tool starts the app with no
//! controlling terminal, so closing the terminal sends no SIGHUP and the app has to notice by
//! itself. A terminal that's still open but no longer read blocks the app's writes, and SIGTERM
//! still ends it.

mod pty_support;

use std::fs;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use nix::fcntl::{FcntlArg, FdFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::{Winsize, openpty};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use pty_support::{COLS, ROWS, Script, notice};
use tempfile::TempDir;

/// Shown once the app has drawn.
const READY: &str = "ready to go";
const DRAWN_WITHIN: Duration = Duration::from_secs(10);
/// The app's own check runs every half second, and SIGTERM's backstop waits three.
const ENDS_WITHIN: Duration = Duration::from_secs(10);

/// The driver on a pty it doesn't have as its controlling terminal, as a capture tool starts
/// the app: in a process group of its own, with the pty as its stdin, stdout, and stderr.
struct Detached {
    child: Child,
    /// The terminal's side. Dropping it closes the terminal.
    master: Option<OwnedFd>,
    output: Vec<u8>,
    _dir: TempDir,
}

impl Detached {
    fn start(script: Script) -> Self {
        let dir = tempfile::tempdir().expect("a folder for the CLI");
        let home = dir.path().join("home");
        let work = dir.path().join("work");
        for folder in [&home, &work] {
            fs::create_dir_all(folder).expect("the CLI folders");
        }
        let script_file = dir.path().join("script.jsonl");
        fs::write(&script_file, script.to_jsonl()).expect("the script");
        let size = Winsize {
            ws_row: ROWS,
            ws_col: COLS,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&size), None).expect("a pty");
        // The app mustn't hold the terminal's side open itself, or closing it closes nothing.
        fcntl(&pty.master, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).expect("keeping the pty");
        let stdio = || Stdio::from(pty.slave.try_clone().expect("the pty's terminal side"));
        let child = Command::new(env!("CARGO_BIN_EXE_argonauts-tui-driver"))
            .arg(&script_file)
            .current_dir(&work)
            .env("HOME", &home)
            .env("ARGONAUTS_HOME", home.join(".argonauts"))
            .env("TERM", "xterm-256color")
            .env("LANG", "en_US.UTF-8")
            .env("DRIVER_UI", r#"screen = "inline""#)
            .env("DRIVER_COMMANDS", dir.path().join("commands.jsonl"))
            .env_remove("TMUX")
            .stdin(stdio())
            .stdout(stdio())
            .stderr(stdio())
            .process_group(0)
            .spawn()
            .expect("starting the CLI");
        drop(pty.slave);
        Self {
            child,
            master: Some(pty.master),
            output: Vec::new(),
            _dir: dir,
        }
    }

    /// Reads what the app writes, answering its cursor question, until `text` shows.
    fn read_until(&mut self, text: &str) {
        let master = self.master.as_ref().expect("the terminal is open");
        let deadline = Instant::now() + DRAWN_WITHIN;
        let mut buf = [0u8; 8192];
        while Instant::now() < deadline {
            if String::from_utf8_lossy(&self.output).contains(text) {
                return;
            }
            let mut fds = [PollFd::new(
                std::os::fd::AsFd::as_fd(master),
                PollFlags::POLLIN,
            )];
            if poll(&mut fds, PollTimeout::from(50u16)).unwrap_or(0) == 0 {
                continue;
            }
            let Ok(read) = nix::unistd::read(master, &mut buf) else {
                break;
            };
            let chunk = &buf[..read];
            if chunk.windows(4).any(|bytes| bytes == b"\x1b[6n") {
                nix::unistd::write(master, b"\x1b[1;1R").expect("answering the cursor query");
            }
            self.output.extend_from_slice(chunk);
        }
        panic!(
            "{text:?} never showed; the app wrote:\n{}",
            String::from_utf8_lossy(&self.output)
        );
    }

    fn signal(&self, signal: Signal) {
        let pid = i32::try_from(self.child.id()).expect("a pid");
        kill(Pid::from_raw(pid), signal).expect("signalling the app");
    }

    fn ended_within(&mut self, limit: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().expect("checking the app") {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }
}

impl Drop for Detached {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn the_app_ends_when_its_terminal_closes() {
    let mut app = Detached::start(Script::idle().at(0, notice("info", READY)));
    app.read_until(READY);

    drop(app.master.take());

    assert!(
        app.ended_within(ENDS_WITHIN).is_some(),
        "the app outlived its terminal"
    );
}

#[test]
fn sigterm_ends_the_app_while_its_terminal_stops_reading() {
    let lines = (0..4000).map(|n| notice("info", &format!("line {n:04} {}", "x".repeat(60))));
    let script = Script::idle().at(0, notice("info", READY)).all(200, lines);
    let mut app = Detached::start(script);
    app.read_until(READY);

    // Nothing reads from here on, so the lines fill the terminal and the app's writes block.
    std::thread::sleep(Duration::from_secs(1));
    app.signal(Signal::SIGTERM);

    assert!(
        app.ended_within(ENDS_WITHIN).is_some(),
        "SIGTERM didn't end an app whose terminal stopped reading"
    );
}

#[test]
fn sigterm_ends_the_app_once_its_terminal_closed() {
    let mut app = Detached::start(Script::idle().at(0, notice("info", READY)));
    app.read_until(READY);

    drop(app.master.take());
    app.signal(Signal::SIGTERM);

    assert!(
        app.ended_within(ENDS_WITHIN).is_some(),
        "SIGTERM didn't end the app after its terminal closed"
    );
}
