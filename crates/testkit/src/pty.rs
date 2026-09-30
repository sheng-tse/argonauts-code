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

/// Where `dump` writes screens, when set.
pub const SHOTS_ENV: &str = "ARGONAUTS_SHOTS_DIR";

const SCROLLBACK: usize = 5000;

const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone)]
pub struct PtyCommand {
    program: PathBuf,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    env: Vec<(String, OsString)>,
    env_remove: Vec<String>,
    rows: u16,
    cols: u16,
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
        }
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
    parser: vt100::Parser,
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
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Pty {
    pub fn spawn(command: PtyCommand) -> anyhow::Result<Self> {
        let size = PtySize {
            rows: command.rows,
            cols: command.cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system()
            .openpty(size)
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
            parser: vt100::Parser::new(command.rows, command.cols, SCROLLBACK),
            raw: Vec::new(),
            closed: false,
        }));
        let reader = {
            let terminal = terminal.clone();
            let writer = writer.clone();
            thread::Builder::new()
                .name("argonauts-pty-reader".to_owned())
                .spawn(move || read_output(reader, &terminal, &writer))
                .context("starting the pty reader")?
        };
        Ok(Self {
            child,
            master: pair.master,
            writer,
            terminal,
            reader: Some(reader),
            size: (command.rows, command.cols),
        })
    }

    /// Types text as if pasted key by key. `\r` is Enter.
    pub fn send(&mut self, text: &str) -> anyhow::Result<()> {
        self.write(text.as_bytes())
    }

    pub fn press(&mut self, key: Key) -> anyhow::Result<()> {
        self.write(&key.bytes())
    }

    fn write(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        let mut writer = lock(&self.writer);
        writer.write_all(bytes).context("typing into the pty")?;
        writer.flush().context("typing into the pty")
    }

    pub fn screen(&self) -> Screen {
        let mut terminal = lock(&self.terminal);
        let (rows, cols) = terminal.parser.screen().size();
        let screen = terminal.parser.screen_mut();
        screen.set_scrollback(usize::MAX);
        let depth = screen.scrollback();
        // Walk up the scrollback one line at a time; each offset shows that line on top.
        let mut scrollback = Vec::with_capacity(depth);
        for offset in (1..=depth).rev() {
            screen.set_scrollback(offset);
            if let Some(line) = screen.rows(0, cols).next() {
                scrollback.push(line.trim_end().to_owned());
            }
        }
        screen.set_scrollback(0);
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

    pub fn resize(&mut self, rows: u16, cols: u16) -> anyhow::Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| anyhow!("resizing the pty: {err}"))?;
        lock(&self.terminal)
            .parser
            .screen_mut()
            .set_size(rows, cols);
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
                    terminal.parser.process(chunk);
                    let cursor = terminal.parser.screen().cursor_position();
                    responder.replies(chunk, cursor)
                };
                if !replies.is_empty() {
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
