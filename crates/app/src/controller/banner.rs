//! A CLI's start-up screen, read once in a hidden terminal: for a CLI that names who is
//! signed in nowhere else, as Antigravity CLI does. It is started with no arguments, sent
//! nothing at all, read until its screen says who is signed in or the time is up, and closed.

use std::io::Read;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// How long a start-up screen has to say who is signed in.
pub(crate) const BANNER_TIMEOUT: Duration = Duration::from_secs(8);
/// The hidden terminal's size, wide enough for a banner's longest line.
const COLUMNS: u16 = 120;
const ROWS: u16 = 40;
/// How often the screen is looked at while nothing new comes.
const LOOK_EVERY: Duration = Duration::from_millis(200);

/// Starts `program` in `cwd` in a hidden terminal and reads its screen as text, handing each
/// new state of it to `read` until `read` finds what it looks for or `timeout` passes. The
/// program is closed either way. Blocking, so it runs off the async threads.
pub(crate) fn read_banner<T>(
    program: &Path,
    cwd: &Path,
    timeout: Duration,
    read: impl Fn(&str) -> Option<T>,
) -> Option<T> {
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLUMNS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .ok()?;
    let mut command = CommandBuilder::new(program);
    command.cwd(cwd);
    command.env("TERM", "xterm-256color");
    let mut child = pty.slave.spawn_command(command).ok()?;
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader().ok()?;
    let (sent, received) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        while let Ok(read) = reader.read(&mut chunk) {
            if read == 0 || sent.send(chunk[..read].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut screen = vt100::Parser::new(ROWS, COLUMNS, 0);
    let end = Instant::now() + timeout;
    let mut found = None;
    while found.is_none() {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match received.recv_timeout(left.min(LOOK_EVERY)) {
            Ok(bytes) => {
                screen.process(&bytes);
                found = read(&screen.screen().contents());
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in that draws a start-up screen, then waits as an interactive CLI does.
    fn stand_in(dir: &Path, screen: &str) -> std::path::PathBuf {
        let path = dir.join("cli");
        std::fs::write(&path, format!("#!/bin/sh\nprintf '{screen}'\nsleep 30\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn the_screen_is_read_until_it_names_the_account_and_the_cli_is_closed() {
        let dir = tempfile::tempdir().unwrap();
        let program = stand_in(
            dir.path(),
            "\\033[2J\\033[H  Banner\\r\\n  you@example.com\\r\\n",
        );
        let started = Instant::now();
        let found = read_banner(&program, dir.path(), Duration::from_secs(10), |screen| {
            screen
                .lines()
                .find(|line| line.contains('@'))
                .map(|line| line.trim().to_owned())
        });
        assert_eq!(found.as_deref(), Some("you@example.com"));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "it stops reading once the screen says, and closes the CLI"
        );
    }

    #[test]
    fn a_screen_that_never_says_gives_up_when_the_time_is_up() {
        let dir = tempfile::tempdir().unwrap();
        let program = stand_in(dir.path(), "  Banner\\r\\n");
        let found = read_banner(&program, dir.path(), Duration::from_millis(600), |screen| {
            screen.contains('@').then_some(())
        });
        assert_eq!(found, None);
    }
}
