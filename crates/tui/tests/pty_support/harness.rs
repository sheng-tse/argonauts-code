//! The harness binary in a pty, with a private home, its script, and the commands it recorded.

use std::cell::RefCell;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use argonauts_core::app::UiCommand;
use argonauts_testkit::{Key, Pty, PtyCommand, Screen};
use regex::{Regex, RegexBuilder};
use serde_json::Value;
use tempfile::TempDir;

use super::screen::{bells, titles};
use super::script::Script;

/// The longest any single wait may take.
pub const WAIT: Duration = Duration::from_secs(5);
pub const ROWS: u16 = 32;
pub const COLS: u16 = 100;

const POLL: Duration = Duration::from_millis(20);
/// How long a lone Esc gets to be read before the next key. Read together, `ESC ESC` is one
/// Esc and `ESC d` is Alt+d, as in any terminal app.
const ESC_SETTLE: Duration = Duration::from_millis(60);

/// Variables from the developer's shell that would change what the screen draws.
const CLEARED_ENV: &[&str] = &[
    "NO_COLOR",
    "FORCE_COLOR",
    "CLICOLOR_FORCE",
    "COLORTERM",
    "ARGONAUTS_SCREEN_READER",
    "ARGONAUTS_SESSION",
    "VISUAL",
    "EDITOR",
    "TMUX",
    "TERM_PROGRAM",
    "LC_ALL",
];

/// How to start the harness: the script, the terminal size, `[ui]` settings, and extra
/// environment.
pub struct Launch {
    script: Script,
    rows: u16,
    cols: u16,
    ui: Vec<String>,
    env: Vec<(String, OsString)>,
    name: Option<String>,
    shell: Option<Shell>,
}

/// Runs the harness from a shell, which prints a line first, as a prompt would, and can start
/// it a second time in the same terminal once the first one quits.
struct Shell {
    before: String,
    then: Option<Script>,
}

impl Launch {
    pub fn size(mut self, rows: u16, cols: u16) -> Self {
        self.rows = rows;
        self.cols = cols;
        self
    }

    /// Lines of `[ui]` TOML, such as `keys = "vim"` or `[panel]\nagents = true`.
    pub fn ui(mut self, toml: &str) -> Self {
        self.ui.push(toml.to_owned());
        self
    }

    pub fn env(mut self, name: &str, value: impl Into<OsString>) -> Self {
        self.env.push((name.to_owned(), value.into()));
        self
    }

    /// The name screen dumps use; the test's own name by default.
    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.to_owned());
        self
    }

    /// A shell prints `line` before the harness starts, as a prompt would.
    pub fn after_shell_line(mut self, line: &str) -> Self {
        self.shell.get_or_insert_with(|| Shell::new(line)).before = line.to_owned();
        self
    }

    /// Once the first run quits, the shell starts the harness again with `script`.
    pub fn then_restart(mut self, script: Script) -> Self {
        self.shell
            .get_or_insert_with(|| Shell::new("$ argonauts"))
            .then = Some(script);
        self
    }

    pub fn start(self) -> Harness {
        let dir = tempfile::tempdir().expect("making the harness folder");
        let home = dir.path().join("home");
        let work = dir.path().join("work");
        let signals = dir.path().join("signals");
        for folder in [&home, &work, &signals] {
            fs::create_dir_all(folder).expect("making the harness folders");
        }
        let script = dir.path().join("script.jsonl");
        fs::write(&script, self.script.to_jsonl()).expect("writing the script");
        let commands = dir.path().join("commands.jsonl");

        let binary = env!("CARGO_BIN_EXE_argonauts-tui-harness");
        let command = match &self.shell {
            None => PtyCommand::new(binary).args([script.as_os_str()]),
            Some(shell) => {
                let again = dir.path().join("again.jsonl");
                let then = shell
                    .then
                    .as_ref()
                    .map(Script::to_jsonl)
                    .unwrap_or_default();
                fs::write(&again, then).expect("writing the second script");
                let restart = if shell.then.is_some() {
                    r#"; printf '%s\r\n' "$4"; "$1" "$3""#
                } else {
                    ""
                };
                let line = format!(r#"printf '%s\r\n' "$4"; "$1" "$2"{restart}"#);
                PtyCommand::new("/bin/sh").args([
                    OsString::from("-c"),
                    OsString::from(line),
                    OsString::from("sh"),
                    OsString::from(binary),
                    script.clone().into_os_string(),
                    again.into_os_string(),
                    OsString::from(&shell.before),
                ])
            }
        };
        let mut command = command
            .cwd(&work)
            .size(self.rows, self.cols)
            .env("HOME", home.as_os_str())
            .env("ARGONAUTS_HOME", home.join(".argonauts").as_os_str())
            .env("XDG_CONFIG_HOME", home.join(".config").as_os_str())
            .env("LANG", "en_US.UTF-8")
            // Clock times on screen, such as a reset time, read the same on every machine.
            .env("TZ", "UTC")
            .env("HARNESS_UI", self.ui.join("\n"))
            .env("HARNESS_COMMANDS", commands.as_os_str())
            .env("HARNESS_SIGNALS", signals.as_os_str());
        for name in CLEARED_ENV {
            command = command.env_remove(name);
        }
        command = command.envs(self.env);
        let pty = Pty::spawn(command).expect("starting the harness in a pty");
        let name = self.name.unwrap_or_else(test_name);
        Harness {
            pty,
            dir,
            commands,
            signals,
            name,
            taken: RefCell::new(Vec::new()),
        }
    }
}

impl Shell {
    fn new(before: &str) -> Self {
        Self {
            before: before.to_owned(),
            then: None,
        }
    }
}

fn test_name() -> String {
    thread::current()
        .name()
        .filter(|name| *name != "main")
        .map(|name| name.rsplit("::").next().unwrap_or(name).to_owned())
        .unwrap_or_else(|| "pty".to_owned())
}

/// The running harness. Its screen is dumped under `$ARGONAUTS_SHOTS_DIR` when it's dropped,
/// even when the test failed, and the process is killed.
pub struct Harness {
    pty: Pty,
    dir: TempDir,
    commands: PathBuf,
    signals: PathBuf,
    name: String,
    /// Which recorded commands `wait_command` already returned.
    taken: RefCell<Vec<bool>>,
}

impl Harness {
    /// 100 by 32, default settings.
    pub fn start(script: Script) -> Self {
        Self::launch(script).start()
    }

    pub fn launch(script: Script) -> Launch {
        Launch {
            script,
            rows: ROWS,
            cols: COLS,
            ui: Vec::new(),
            env: Vec::new(),
            name: None,
            shell: None,
        }
    }

    pub fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    pub fn work_dir(&self) -> PathBuf {
        self.dir.path().join("work")
    }

    /// A scratch folder for the test's own files.
    pub fn scratch(&self) -> &Path {
        self.dir.path()
    }

    pub fn send(&mut self, text: &str) {
        self.pty.send(text).expect("typing into the harness");
    }

    pub fn press(&mut self, key: Key) {
        self.pty.press(key).expect("pressing a key");
        if key == Key::Esc {
            thread::sleep(ESC_SETTLE);
        }
    }

    pub fn press_all(&mut self, keys: &[Key]) {
        for key in keys {
            self.press(*key);
        }
    }

    /// Pastes the way a terminal does when the app turned bracketed paste on.
    pub fn paste(&mut self, text: &str) {
        self.send(&format!("\x1b[200~{text}\x1b[201~"));
    }

    pub fn screen(&self) -> Screen {
        self.pty.screen()
    }

    pub fn visible(&self) -> String {
        self.screen().visible_text()
    }

    /// Scrollback and visible rows.
    pub fn text(&self) -> String {
        self.screen().text()
    }

    /// Waits until the scrollback or the visible rows match. `^` and `$` match at each row.
    pub fn wait_for(&self, pattern: &str) -> Screen {
        self.pty
            .wait_for(&format!("(?m){pattern}"), WAIT)
            .unwrap_or_else(|err| panic!("{}: {err:#}", self.name))
    }

    /// Waits until the visible rows match, ignoring the scrollback.
    pub fn wait_visible(&self, pattern: &str) -> Screen {
        let regex = regex(pattern);
        self.wait_until(&format!("{pattern:?} on screen"), |screen| {
            regex.is_match(&screen.visible_text())
        })
    }

    /// The screen once the visible rows match, or `None` after `timeout`, for a step that
    /// may or may not be needed.
    pub fn poll_visible(&self, pattern: &str, timeout: Duration) -> Option<Screen> {
        let regex = regex(pattern);
        let deadline = Instant::now() + timeout.min(WAIT);
        loop {
            let screen = self.screen();
            if regex.is_match(&screen.visible_text()) {
                return Some(screen);
            }
            if self.gave_up(deadline).is_some() {
                return None;
            }
            thread::sleep(POLL);
        }
    }

    /// Waits until the visible rows no longer match. A blank screen doesn't count, since it's
    /// the moment between the chat and a view drawing itself.
    pub fn wait_gone(&self, pattern: &str) -> Screen {
        let regex = regex(pattern);
        self.wait_until(&format!("{pattern:?} gone from the screen"), |screen| {
            let text = screen.visible_text();
            !text.trim().is_empty() && !regex.is_match(&text)
        })
    }

    pub fn wait_until(&self, what: &str, done: impl Fn(&Screen) -> bool) -> Screen {
        let deadline = Instant::now() + WAIT;
        loop {
            let screen = self.screen();
            if done(&screen) {
                return self.settled(screen, &done, deadline);
            }
            if let Some(why) = self.gave_up(deadline) {
                // Read the screen again: the last output may have landed after the check.
                panic!(
                    "{}: {why} waiting for {what}; the screen was:\n{}",
                    self.name,
                    self.text()
                );
            }
            thread::sleep(POLL);
        }
    }

    /// Why a wait should stop: its time is up, or the harness is gone and nothing will change.
    /// A frame can be read half drawn, so the screen is taken once two polls agree, as long
    /// as it still matches.
    fn settled(
        &self,
        mut screen: Screen,
        done: &impl Fn(&Screen) -> bool,
        deadline: Instant,
    ) -> Screen {
        while Instant::now() < deadline {
            thread::sleep(POLL);
            let next = self.screen();
            if !done(&next) {
                break;
            }
            if next.visible == screen.visible {
                return next;
            }
            screen = next;
        }
        screen
    }

    fn gave_up(&self, deadline: Instant) -> Option<&'static str> {
        if Instant::now() >= deadline {
            Some("timed out")
        } else if self.exited() {
            Some("the harness exited")
        } else {
            None
        }
    }

    /// The pty driver only tells that the program exited through a failed wait, so this asks
    /// for a pattern that never matches, with no time to wait.
    fn exited(&self) -> bool {
        self.pty
            .wait_for(r"[^\s\S]", Duration::ZERO)
            .is_err_and(|err| err.to_string().starts_with("the program exited"))
    }

    /// Every command the screen sent so far, oldest first.
    pub fn commands(&self) -> Vec<UiCommand> {
        self.command_values()
            .into_iter()
            .map(|value| {
                serde_json::from_value(value.clone()).unwrap_or_else(|err| {
                    panic!("recorded a command that isn't one: {err}\n{value}")
                })
            })
            .collect()
    }

    /// The recorded commands as JSON, the way the harness wrote them.
    pub fn command_values(&self) -> Vec<Value> {
        let text = fs::read_to_string(&self.commands).unwrap_or_default();
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("the harness writes JSON lines"))
            .collect()
    }

    /// The `type` of every command so far.
    pub fn command_types(&self) -> Vec<String> {
        self.command_values()
            .iter()
            .map(|value| command_type(value).to_owned())
            .collect()
    }

    /// Waits for the oldest command of this `type` that no earlier wait returned.
    pub fn wait_command(&self, kind: &str) -> Value {
        self.wait_command_where(kind, |_| true)
    }

    /// Like `wait_command`, for the oldest one that also passes `check`.
    pub fn wait_command_where(&self, kind: &str, check: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let values = self.command_values();
            let mut taken = self.taken.borrow_mut();
            taken.resize(values.len(), false);
            let found = values
                .iter()
                .enumerate()
                .find(|(i, value)| !taken[*i] && command_type(value) == kind && check(value));
            if let Some((i, value)) = found {
                taken[i] = true;
                return value.clone();
            }
            drop(taken);
            if let Some(why) = self.gave_up(deadline) {
                panic!(
                    "{}: {why} waiting for a {kind:?} command; the screen sent {:?}; the screen \
                     was:\n{}",
                    self.name,
                    values,
                    self.text()
                );
            }
            thread::sleep(POLL);
        }
    }

    /// Types a line and presses Enter, then waits for the screen to send it as `Submit`.
    /// Every command sent before it has arrived by then, so a test can also use it to check
    /// that something else wasn't sent.
    pub fn submit(&mut self, text: &str) -> Value {
        self.send(text);
        self.press(Key::Enter);
        self.wait_command_where("submit", |value| value["text"] == text)
    }

    pub fn assert_no_command(&self, kind: &str) {
        let types = self.command_types();
        assert!(
            !types.iter().any(|t| t == kind),
            "{}: the screen sent {kind:?}; it sent {types:?}",
            self.name
        );
    }

    /// Lets a script line `await_signal` with this name go ahead.
    pub fn signal(&self, name: &str) {
        fs::write(self.signals.join(name), b"").expect("writing a signal file");
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.pty.resize(rows, cols).expect("resizing the pty");
    }

    /// Everything the harness wrote, escape sequences included.
    pub fn raw(&self) -> Vec<u8> {
        self.pty.raw()
    }

    /// Bells rung so far. A bell that ends a title sequence doesn't count.
    pub fn bells(&self) -> usize {
        bells(&self.raw())
    }

    /// Every terminal title set so far, oldest first.
    pub fn titles(&self) -> Vec<String> {
        titles(&self.raw())
    }

    pub fn wait_title(&self, pattern: &str) -> String {
        let regex = regex(pattern);
        let deadline = Instant::now() + WAIT;
        loop {
            let titles = self.titles();
            if let Some(title) = titles.iter().rev().find(|title| regex.is_match(title)) {
                return title.clone();
            }
            if let Some(why) = self.gave_up(deadline) {
                panic!(
                    "{}: {why} waiting for a title matching {pattern:?}; titles were {titles:?}; \
                     the screen was:\n{}",
                    self.name,
                    self.text()
                );
            }
            thread::sleep(POLL);
        }
    }

    pub fn wait_exit(&mut self) -> u32 {
        self.pty
            .wait_for_exit(WAIT)
            .unwrap_or_else(|err| panic!("{}: {err:#}", self.name))
    }

    /// Saves this moment's screen as `<test>-<step>` when `$ARGONAUTS_SHOTS_DIR` is set.
    pub fn shot(&self, step: &str) {
        self.pty
            .dump(&format!("{}-{step}", self.name))
            .expect("dumping the screen");
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // A failed dump mustn't hide the test's own failure.
        let _ = self.pty.dump(&self.name);
        self.pty.kill();
    }
}

pub fn command_type(value: &Value) -> &str {
    value["type"].as_str().unwrap_or_default()
}

/// Patterns run over the joined rows, so `^` and `$` match at each row.
fn regex(pattern: &str) -> Regex {
    RegexBuilder::new(pattern)
        .multi_line(true)
        .build()
        .unwrap_or_else(|err| panic!("bad pattern {pattern:?}: {err}"))
}
