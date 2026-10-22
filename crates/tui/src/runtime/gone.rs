//! When the terminal goes away, the app goes too.
//!
//! Crossterm's reader never says so: on end of file or an I/O error it reads again, forever.
//! A process with no controlling terminal, as a capture tool starts it, gets no SIGHUP either.
//! So the screen asks the terminal itself every so often. And a terminal that is still open but
//! no longer read blocks every write, which keeps the screen from ever handling a stop signal,
//! so a thread of its own ends the process when one isn't handled in time.

use std::io::{self, IsTerminal};
use std::os::fd::AsFd;
use std::sync::Once;
use std::time::Duration;

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use signal_hook::consts::{SIGHUP, SIGTERM};
use signal_hook::iterator::Signals;

/// How often the screen checks that its terminal is still there.
pub(super) const CHECK_EVERY: Duration = Duration::from_millis(500);

/// How long the app gets to close after SIGTERM or SIGHUP before the process ends anyway.
const GRACE: Duration = Duration::from_secs(3);

/// The screen's terminal hung up, so nothing more can be shown or typed.
#[derive(Debug, thiserror::Error)]
#[error("the terminal went away")]
pub struct TerminalGone;

/// Which of stdin and stdout are terminals, taken while the terminal is there: on Linux, closing
/// a terminal's window hangs up its other side, which then stops being a terminal at all.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Terminals {
    stdin: bool,
    stdout: bool,
}

impl Terminals {
    pub(super) fn now() -> Self {
        Self {
            stdin: io::stdin().is_terminal(),
            stdout: io::stdout().is_terminal(),
        }
    }

    /// Whether the terminal on stdin or stdout hung up, which is what closing its window does.
    /// A stream that wasn't a terminal says nothing either way.
    pub(super) fn gone(self) -> bool {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut fds = Vec::with_capacity(2);
        if self.stdin {
            fds.push(PollFd::new(stdin.as_fd(), PollFlags::POLLIN));
        }
        if self.stdout {
            fds.push(PollFd::new(stdout.as_fd(), PollFlags::POLLOUT));
        }
        if fds.is_empty() || poll(&mut fds, PollTimeout::ZERO).is_err() {
            return false;
        }
        let hung_up = PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL;
        fds.iter().any(|fd| {
            fd.revents()
                .is_some_and(|events| events.intersects(hung_up))
        })
    }
}

/// After a first SIGTERM or SIGHUP the app has [`GRACE`] to close the way `/exit` does; then,
/// or at a second one, the process ends at once. Armed once, for the process's own terminal.
pub(super) fn arm_backstop() {
    static ARMED: Once = Once::new();
    ARMED.call_once(|| {
        let Ok(mut signals) = Signals::new([SIGTERM, SIGHUP]) else {
            return;
        };
        let watcher = std::thread::Builder::new()
            .name("argonauts-stop-backstop".to_owned())
            .spawn(move || {
                let mut first = true;
                for signal in signals.forever() {
                    if !first {
                        end_now(signal);
                    }
                    first = false;
                    let _ = std::thread::Builder::new()
                        .name("argonauts-stop-grace".to_owned())
                        .spawn(move || {
                            std::thread::sleep(GRACE);
                            end_now(signal);
                        });
                }
            });
        drop(watcher);
    });
}

/// A bare `_exit`, without flushing anything: a blocked terminal would block the flush too.
fn end_now(signal: i32) -> ! {
    signal_hook::low_level::exit(128 + signal)
}
