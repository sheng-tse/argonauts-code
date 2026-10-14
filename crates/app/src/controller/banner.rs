//! A CLI's start-up screen, read once in a hidden terminal: for a CLI that names who is
//! signed in nowhere else, as Antigravity CLI does. It is started with no arguments, sent
//! nothing at all, read until its screen has said who is signed in and stopped changing its
//! mind, or the time is up, and closed. Whatever the CLI asks on that screen stays unanswered.

use std::io::Read;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// How long a start-up screen has to say who is signed in.
pub(crate) const BANNER_TIMEOUT: Duration = Duration::from_secs(8);
/// How long a reading has to stay the same before it is taken. A CLI may draw a loading frame
/// first and its banner a line at a time, and what a half-drawn screen says isn't its answer.
const SETTLED_FOR: Duration = Duration::from_millis(300);
/// The hidden terminal's size, wide enough for a banner's longest line.
const COLUMNS: u16 = 120;
const ROWS: u16 = 40;
/// How often the screen is looked at while nothing new comes.
const LOOK_EVERY: Duration = Duration::from_millis(200);

/// Starts `program` in `cwd` in a hidden terminal with `env` set and reads its screen as text,
/// handing each new state of it to `read`. What `read` finds is taken once the screen has
/// said the same for [`SETTLED_FOR`]; when `timeout` passes first, the last thing it found is.
/// The program is closed either way. Blocking, so it runs off the async threads.
pub(crate) fn read_banner<T: PartialEq>(
    program: &Path,
    cwd: &Path,
    env: &[(&str, &str)],
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
    for (name, value) in env {
        command.env(name, value);
    }
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
    // What the screen says now, and since when it has said exactly that.
    let mut reading: Option<(T, Instant)> = None;
    loop {
        let now = Instant::now();
        let left = end.saturating_duration_since(now);
        let settles_in = reading
            .as_ref()
            .map(|(_, since)| (*since + SETTLED_FOR).saturating_duration_since(now));
        if left.is_zero() || settles_in.is_some_and(|wait| wait.is_zero()) {
            break;
        }
        match received.recv_timeout(left.min(settles_in.unwrap_or(LOOK_EVERY))) {
            Ok(bytes) => {
                screen.process(&bytes);
                reading = match (reading.take(), read(&screen.screen().contents())) {
                    (Some((was, since)), Some(says)) if was == says => Some((was, since)),
                    (_, says) => says.map(|says| (says, Instant::now())),
                };
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    reading.map(|(says, _)| says)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    use argonauts_adapter_antigravity_cli::AntigravityCli;
    use argonauts_core::Adapter;
    use argonauts_core::adapter::BannerAccount;

    /// A fake CLI that plays `frames` as a terminal program draws them, each a full screen
    /// after `pause` seconds, then waits as an interactive CLI does. It notes each variable of
    /// `env_seen` it was started with in a file beside it.
    fn fake_cli(dir: &Path, frames: &[(&str, &str)], env_seen: &[&str]) -> std::path::PathBuf {
        let path = dir.join("cli");
        let mut script = String::from("#!/bin/sh\n");
        for name in env_seen {
            script.push_str(&format!(
                "printf '%s=%s\\n' {name} \"${name}\" >> '{}'\n",
                dir.join("env").display()
            ));
        }
        for (pause, frame) in frames {
            let drawn = frame.replace('\n', "\\r\\n");
            script.push_str(&format!("sleep {pause}\nprintf '\\033[2J\\033[H{drawn}'\n"));
        }
        script.push_str("sleep 30\n");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// agy's first frame, drawn for a few milliseconds while it loads a saved login.
    const LOADING: &str =
        " Welcome to the Antigravity CLI.\n You are currently not signed in.\n\n ⣷  Signing in…";
    /// Its banner, as 1.3.3 draws it.
    const BANNER: &str =
        " Antigravity CLI 1.3.3\n you@example.com (Google AI Pro)\n Gemini 3.8 Flash (High)\n /";
    /// What it asks when no login is saved.
    const LOGIN_CHOICE: &str = " Welcome to the Antigravity CLI.\n You are currently not signed in.\n\n Select login method:\n > 1. Google OAuth\n   2. API key";
    /// What it asks first in a folder it hasn't trusted.
    const TRUST_QUESTION: &str = " Do you trust the contents of this project?\n\n > 1. Yes, trust this folder\n   2. No, exit";

    fn agy_says(frames: &[(&str, &str)], timeout: Duration) -> Option<BannerAccount> {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_cli(dir.path(), frames, &[]);
        let agy = AntigravityCli;
        read_banner(
            &program,
            dir.path(),
            agy.updater_off_env(),
            timeout,
            |screen| agy.banner_account(screen),
        )
    }

    #[test]
    fn the_screen_is_read_until_it_names_the_account_and_the_cli_is_closed() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_cli(dir.path(), &[("0", "  Banner\n  you@example.com\n")], &[]);
        let started = Instant::now();
        let found = read_banner(
            &program,
            dir.path(),
            &[],
            Duration::from_secs(10),
            |screen| {
                screen
                    .lines()
                    .find(|line| line.contains('@'))
                    .map(|line| line.trim().to_owned())
            },
        );
        assert_eq!(found.as_deref(), Some("you@example.com"));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "it stops reading once the screen says, and closes the CLI"
        );
    }

    #[test]
    fn a_screen_that_never_says_gives_up_when_the_time_is_up() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_cli(dir.path(), &[("0", "  Banner\n")], &[]);
        let found = read_banner(
            &program,
            dir.path(),
            &[],
            Duration::from_millis(600),
            |screen| screen.contains('@').then_some(()),
        );
        assert_eq!(found, None);
    }

    #[test]
    fn agys_loading_frame_is_waited_out_and_its_banner_read() {
        let found = agy_says(&[("0", LOADING), ("0.02", BANNER)], Duration::from_secs(10));
        assert_eq!(
            found,
            Some(BannerAccount::SignedIn {
                account: "you@example.com".to_owned(),
                plan: Some("Google AI Pro".to_owned()),
            })
        );
        // A saved login that takes its time is waited for just the same.
        let slow = agy_says(&[("0", LOADING), ("0.9", BANNER)], Duration::from_secs(10));
        assert!(
            matches!(slow, Some(BannerAccount::SignedIn { .. })),
            "{slow:?}"
        );
    }

    #[test]
    fn a_plan_drawn_after_the_address_is_still_read() {
        let address_only = " Antigravity CLI 1.3.3\n you@example.com";
        let found = agy_says(
            &[("0", LOADING), ("0.02", address_only), ("0.1", BANNER)],
            Duration::from_secs(10),
        );
        assert_eq!(
            found,
            Some(BannerAccount::SignedIn {
                account: "you@example.com".to_owned(),
                plan: Some("Google AI Pro".to_owned()),
            })
        );
    }

    #[test]
    fn agy_asking_for_a_login_method_is_signed_out() {
        let found = agy_says(
            &[("0", LOADING), ("0.02", LOGIN_CHOICE)],
            Duration::from_secs(10),
        );
        assert_eq!(found, Some(BannerAccount::SignedOut));
    }

    #[test]
    fn a_loading_frame_that_never_ends_is_not_signed_out() {
        let found = agy_says(&[("0", LOADING)], Duration::from_millis(700));
        assert_eq!(found, None);
    }

    #[test]
    fn a_folder_agy_has_not_trusted_is_said_and_never_answered() {
        let dir = tempfile::tempdir().unwrap();
        // The fake CLI notes anything typed at its question.
        let typed = dir.path().join("typed");
        let path = dir.path().join("cli");
        let drawn = TRUST_QUESTION.replace('\n', "\\r\\n");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '\\033[2J\\033[H{drawn}'\nread answer\nprintf '%s' \"$answer\" > '{}'\n",
                typed.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let agy = AntigravityCli;
        let started = Instant::now();
        let found = read_banner(
            &path,
            dir.path(),
            agy.updater_off_env(),
            Duration::from_secs(10),
            |screen| agy.banner_account(screen),
        );
        assert_eq!(found, Some(BannerAccount::UntrustedFolder));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the question is recognised, not waited out"
        );
        assert!(!typed.exists(), "nothing was typed at agy's question");
    }

    #[test]
    fn the_hidden_start_keeps_agys_own_updater_off() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_cli(
            dir.path(),
            &[("0", BANNER)],
            &["AGY_CLI_DISABLE_AUTO_UPDATE"],
        );
        let agy = AntigravityCli;
        let found = read_banner(
            &program,
            dir.path(),
            agy.updater_off_env(),
            Duration::from_secs(10),
            |screen| agy.banner_account(screen),
        );
        assert!(
            matches!(found, Some(BannerAccount::SignedIn { .. })),
            "{found:?}"
        );
        let seen = std::fs::read_to_string(dir.path().join("env")).unwrap();
        assert_eq!(seen.trim(), "AGY_CLI_DISABLE_AUTO_UPDATE=true");
    }
}
