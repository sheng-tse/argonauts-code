//! Runs a program in a pseudo-terminal and reads its screen through a vt100 emulator. The
//! emulator answers the queries a full-screen app sends a real terminal, so apps don't hang.

use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use regex::Regex;

use self::emulator::Emulator;

mod emulator;

/// Where `dump` writes screens, when set.
pub const SHOTS_ENV: &str = "ARGONAUTS_SHOTS_DIR";

const SCROLLBACK: usize = 5000;

const POLL: Duration = Duration::from_millis(20);

/// How long, at most, the reader gets after the program exits to take the last it wrote.
const DRAIN: Duration = Duration::from_secs(2);

/// Enter this soon after typed text could be a line break in a paste that a terminal without
/// bracketed paste types out key by key, which the app reads as part of the text. The app's
/// window is 120 ms.
const ENTER_GAP: Duration = Duration::from_millis(150);
/// The longest Enter waits for the app to draw the typed text first.
const ENTER_WAIT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct PtyCommand {
    program: PathBuf,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    env: Vec<(String, OsString)>,
    env_remove: Vec<String>,
    rows: u16,
    cols: u16,
    answers: bool,
}

impl PtyCommand {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            rows: 32,
            cols: 100,
            answers: true,
        }
    }

    /// A terminal that answers none of the app's queries, as some don't, so the app waits out
    /// its own timeouts at start.
    pub fn unanswered(mut self) -> Self {
        self.answers = false;
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn envs<I>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (String, OsString)>,
    {
        self.env.extend(vars);
        self
    }

    pub fn env(mut self, name: &str, value: impl Into<OsString>) -> Self {
        self.env.push((name.to_owned(), value.into()));
        self
    }

    pub fn env_remove(mut self, name: &str) -> Self {
        self.env_remove.push(name.to_owned());
        self
    }

    pub fn size(mut self, rows: u16, cols: u16) -> Self {
        self.rows = rows;
        self.cols = cols;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Enter,
    Esc,
    Tab,
    ShiftTab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Ctrl(char),
    Char(char),
}

impl Key {
    pub fn bytes(self) -> Vec<u8> {
        match self {
            Key::Enter => b"\r".to_vec(),
            Key::Esc => b"\x1b".to_vec(),
            Key::Tab => b"\t".to_vec(),
            Key::ShiftTab => b"\x1b[Z".to_vec(),
            Key::Backspace => b"\x7f".to_vec(),
            Key::Up => b"\x1b[A".to_vec(),
            Key::Down => b"\x1b[B".to_vec(),
            Key::Right => b"\x1b[C".to_vec(),
            Key::Left => b"\x1b[D".to_vec(),
            Key::Home => b"\x1b[H".to_vec(),
            Key::End => b"\x1b[F".to_vec(),
            Key::PageUp => b"\x1b[5~".to_vec(),
            Key::PageDown => b"\x1b[6~".to_vec(),
            Key::Ctrl(letter) => {
                let byte = (letter.to_ascii_lowercase() as u8) & 0x1f;
                vec![byte]
            }
            Key::Char(c) => c.to_string().into_bytes(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// Oldest first.
    pub scrollback: Vec<String>,
    /// Top to bottom, trailing spaces trimmed.
    pub visible: Vec<String>,
}

impl Screen {
    /// Scrollback and visible rows as one text.
    pub fn text(&self) -> String {
        let mut lines = self.scrollback.clone();
        lines.extend(self.visible.iter().cloned());
        lines.join("\n")
    }

    pub fn visible_text(&self) -> String {
        self.visible.join("\n")
    }
}

struct Terminal {
    emulator: Emulator,
    raw: Vec<u8>,
    closed: bool,
}

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// Killed on drop.
pub struct Pty {
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    writer: SharedWriter,
    terminal: Arc<Mutex<Terminal>>,
    reader: Option<JoinHandle<()>>,
    size: (u16, u16),
    /// When text was last typed, and how many bytes the app had drawn by then.
    typed: Option<(Instant, usize)>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// macOS's `openpty` now and then fails when several test threads call it at once.
static OPENING: Mutex<()> = Mutex::new(());

impl Pty {
    pub fn spawn(command: PtyCommand) -> anyhow::Result<Self> {
        let size = PtySize {
            rows: command.rows,
            cols: command.cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = {
            let _one_at_a_time = lock(&OPENING);
            native_pty_system().openpty(size)
        }
        .map_err(|err| anyhow!("opening a pty: {err}"))?;
        let mut builder = CommandBuilder::new(&command.program);
        builder.args(&command.args);
        if let Some(cwd) = &command.cwd {
            builder.cwd(cwd);
        }
        builder.env("TERM", "xterm-256color");
        builder.env("COLUMNS", command.cols.to_string());
        builder.env("LINES", command.rows.to_string());
        for name in &command.env_remove {
            builder.env_remove(name);
        }
        for (name, value) in &command.env {
            builder.env(name, value);
        }
        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|err| anyhow!("starting {}: {err}", command.program.display()))?;
        // Only the child should hold the slave side, so its exit closes our reader.
        drop(pair.slave);
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| anyhow!("reading the pty: {err}"))?;
        let writer: SharedWriter = Arc::new(Mutex::new(
            pair.master
                .take_writer()
                .map_err(|err| anyhow!("writing the pty: {err}"))?,
        ));
        let terminal = Arc::new(Mutex::new(Terminal {
            emulator: Emulator::new(command.rows, command.cols, SCROLLBACK),
            raw: Vec::new(),
            closed: false,
        }));
        let answers = command.answers;
        let reader = {
            let terminal = terminal.clone();
            let writer = writer.clone();
            thread::Builder::new()
                .name("argonauts-pty-reader".to_owned())
                .spawn(move || read_output(reader, &terminal, &writer, answers))
                .context("starting the pty reader")?
        };
        Ok(Self {
            child,
            master: pair.master,
            writer,
            terminal,
            reader: Some(reader),
            size: (command.rows, command.cols),
            typed: None,
        })
    }

    /// Types text the way a person does: the text at once, and each `\r` as Enter once the app
    /// has the text, so the Enter sends it rather than joining it as a pasted line break.
    pub fn send(&mut self, text: &str) -> anyhow::Result<()> {
        let mut parts = text.split('\r').peekable();
        while let Some(part) = parts.next() {
            if !part.is_empty() {
                self.write(part.as_bytes())?;
                self.typed = Some((Instant::now(), self.drawn()));
            }
            if parts.peek().is_some() {
                self.press(Key::Enter)?;
            }
        }
        Ok(())
    }

    /// Writes the bytes in one go, as a terminal without bracketed paste delivers a paste.
    pub fn send_at_once(&mut self, text: &str) -> anyhow::Result<()> {
        self.typed = None;
        self.write(text.as_bytes())
    }

    pub fn press(&mut self, key: Key) -> anyhow::Result<()> {
        match key {
            Key::Enter => self.settle_typing(),
            Key::Char(_) => self.typed = Some((Instant::now(), self.drawn())),
            _ => {}
        }
        self.write(&key.bytes())
    }

    /// Waits until [`ENTER_GAP`] after the app drew the last typed text, so it reads the next
    /// Enter as a key of its own.
    fn settle_typing(&mut self) {
        let Some((typed_at, drawn)) = self.typed.take() else {
            return;
        };
        let mut drew_at = None;
        loop {
            let now = Instant::now();
            if drew_at.is_none() && self.drawn() > drawn {
                drew_at = Some(now);
            }
            let settled = drew_at.is_some_and(|at| now >= at + ENTER_GAP);
            if settled || now >= typed_at + ENTER_WAIT {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn drawn(&self) -> usize {
        lock(&self.terminal).raw.len()
    }

    fn write(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        let mut writer = lock(&self.writer);
        writer.write_all(bytes).context("typing into the pty")?;
        writer.flush().context("typing into the pty")
    }

    /// Rows scrolled off the top of the screen, or off a scroll region starting at the top
    /// row, are in the scrollback, as xterm and iTerm2 keep them.
    pub fn screen(&self) -> Screen {
        let terminal = lock(&self.terminal);
        let scrollback = terminal.emulator.scrollback().map(str::to_owned).collect();
        let screen = terminal.emulator.screen();
        let (rows, cols) = screen.size();
        let visible = screen
            .rows(0, cols)
            .take(usize::from(rows))
            .map(|line| line.trim_end().to_owned())
            .collect();
        Screen {
            scrollback,
            visible,
        }
    }

    /// Everything the program wrote, escape sequences included.
    pub fn raw(&self) -> Vec<u8> {
        lock(&self.terminal).raw.clone()
    }

    /// Waits until the screen, scrollback included, matches `pattern`. The error shows the
    /// screen as it was.
    pub fn wait_for(&self, pattern: &str, timeout: Duration) -> anyhow::Result<Screen> {
        let regex = Regex::new(pattern).with_context(|| format!("bad pattern {pattern:?}"))?;
        let deadline = Instant::now() + timeout;
        loop {
            let screen = self.screen();
            if regex.is_match(&screen.text()) {
                return Ok(screen);
            }
            let closed = lock(&self.terminal).closed;
            if closed || Instant::now() >= deadline {
                let why = if closed {
                    "the program exited"
                } else {
                    "timed out"
                };
                bail!(
                    "{why} waiting for {pattern:?}; the screen was:\n{}",
                    screen.text()
                );
            }
            thread::sleep(POLL);
        }
    }

    pub fn wait_for_exit(&mut self, timeout: Duration) -> anyhow::Result<u32> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().context("checking the program")? {
                self.drain();
                return Ok(status.exit_code());
            }
            if Instant::now() >= deadline {
                bail!(
                    "the program didn't exit in {timeout:?}; the screen was:\n{}",
                    self.screen().text()
                );
            }
            thread::sleep(POLL);
        }
    }

    /// Waits for the reader to take the last of the output, which a program that has exited
    /// can still have in the terminal. A program it started may keep the terminal open, so
    /// the wait is short.
    fn drain(&self) {
        let deadline = Instant::now() + DRAIN;
        while !lock(&self.terminal).closed && Instant::now() < deadline {
            thread::sleep(POLL);
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> anyhow::Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| anyhow!("resizing the pty: {err}"))?;
        lock(&self.terminal).emulator.set_size(rows, cols);
        self.size = (rows, cols);
        Ok(())
    }

    /// Saves the screen as `<name>.txt` (text), `<name>.ansi` (everything the program wrote,
    /// for `ttyshot.py` to render), and `<name>.json` (the size) under `$ARGONAUTS_SHOTS_DIR`.
    /// Does nothing when that isn't set.
    pub fn dump(&self, name: &str) -> anyhow::Result<Option<PathBuf>> {
        let Some(dir) = std::env::var_os(SHOTS_ENV).filter(|d| !d.is_empty()) else {
            return Ok(None);
        };
        self.dump_to(Path::new(&dir), name).map(Some)
    }

    /// Saves the screen under `dir`, whatever the environment says.
    pub fn dump_to(&self, dir: &Path, name: &str) -> anyhow::Result<PathBuf> {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let base = dir.join(name);
        let (rows, cols) = self.size;
        fs::write(base.with_extension("txt"), self.screen().text() + "\n")
            .context("writing the text dump")?;
        fs::write(base.with_extension("ansi"), self.raw()).context("writing the ANSI dump")?;
        let meta = serde_json::json!({ "rows": rows, "cols": cols });
        fs::write(base.with_extension("json"), meta.to_string()).context("writing the size")?;
        Ok(base.with_extension("txt"))
    }

    /// The program's process id, for a test that signals it.
    pub fn pid(&self) -> Option<u32> {
        self.child.process_id()
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.kill();
        }
        // The reader ends when the pty closes; don't wait on it if the child left it open.
        drop(self.reader.take());
    }
}

/// Feeds output to the emulator and answers terminal queries, until the pty closes.
fn read_output(
    mut reader: Box<dyn Read + Send>,
    terminal: &Mutex<Terminal>,
    writer: &SharedWriter,
    answers: bool,
) {
    let mut buffer = [0u8; 8192];
    let mut responder = Responder::default();
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let chunk = &buffer[..n];
                let replies = {
                    let mut terminal = lock(terminal);
                    terminal.raw.extend_from_slice(chunk);
                    terminal.emulator.process(chunk);
                    let cursor = terminal.emulator.screen().cursor_position();
                    responder.replies(chunk, cursor)
                };
                if answers && !replies.is_empty() {
                    let mut writer = lock(writer);
                    let _ = writer.write_all(&replies);
                    let _ = writer.flush();
                }
            }
        }
    }
    lock(terminal).closed = true;
}

/// Spots terminal queries in the output stream, even split across reads, and builds replies.
#[derive(Default)]
struct Responder {
    /// The start of a query cut off by the end of the last read.
    tail: Vec<u8>,
}

/// Cursor position report request; the reply depends on where the cursor is.
const CURSOR_QUERY: &[u8] = b"\x1b[6n";

/// Queries with fixed replies, as a dark xterm gives them.
const QUERIES: [(&[u8], &[u8]); 6] = [
    (b"\x1b[c", b"\x1b[?1;2c"),
    (b"\x1b[0c", b"\x1b[?1;2c"),
    (b"\x1b[?u", b"\x1b[?0u"),
    (b"\x1b]10;?\x07", b"\x1b]10;rgb:c8c8/cdcd/d8d8\x07"),
    (b"\x1b]11;?\x07", b"\x1b]11;rgb:1e1e/2121/3030\x07"),
    (b"\x1b]11;?\x1b\\", b"\x1b]11;rgb:1e1e/2121/3030\x1b\\"),
];

impl Responder {
    fn replies(&mut self, chunk: &[u8], cursor: (u16, u16)) -> Vec<u8> {
        let mut data = std::mem::take(&mut self.tail);
        data.extend_from_slice(chunk);
        let mut replies = Vec::new();
        let mut at = 0;
        while at < data.len() {
            let rest = &data[at..];
            if rest.starts_with(CURSOR_QUERY) {
                let (row, col) = cursor;
                replies.extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
                at += CURSOR_QUERY.len();
            } else if let Some((query, reply)) = QUERIES.iter().find(|(q, _)| rest.starts_with(q)) {
                replies.extend_from_slice(reply);
                at += query.len();
            } else if is_cut_query(rest) {
                self.tail = rest.to_vec();
                break;
            } else {
                at += 1;
            }
        }
        replies
    }
}

fn is_cut_query(rest: &[u8]) -> bool {
    rest.first() == Some(&0x1b)
        && std::iter::once(CURSOR_QUERY)
            .chain(QUERIES.iter().map(|(query, _)| *query))
            .any(|query| query.len() > rest.len() && query.starts_with(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_keys_are_control_codes() {
        assert_eq!(Key::Ctrl('c').bytes(), [3]);
        assert_eq!(Key::Ctrl('T').bytes(), [20]);
        assert_eq!(Key::ShiftTab.bytes(), b"\x1b[Z");
    }

    #[test]
    fn the_responder_answers_queries_split_across_reads() {
        let mut responder = Responder::default();
        assert!(responder.replies(b"hello \x1b[", (0, 0)).is_empty());
        assert_eq!(responder.replies(b"6n more", (4, 9)), b"\x1b[5;10R");
        assert!(responder.replies(b"plain", (0, 0)).is_empty());
        assert_eq!(responder.replies(b"\x1b[c", (0, 0)), b"\x1b[?1;2c");
        assert!(
            responder.replies(b"x", (0, 0)).is_empty(),
            "no second answer from the tail"
        );
    }
}
