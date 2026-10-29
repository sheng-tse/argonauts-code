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
use regex::{Regex, RegexBuilder};

use self::emulator::Emulator;
use crate::deadline;
pub use crate::screen::Placeholder;
use crate::screen::{Grid, Rgb};

mod emulator;
mod pictures;

/// Where `dump` writes screens, when set.
pub const SHOTS_ENV: &str = "ARGONAUTS_SHOTS_DIR";

const SCROLLBACK: usize = 5000;

const POLL: Duration = Duration::from_millis(20);

/// How long, at most, the reader gets after the program exits to take the last it wrote.
const DRAIN: Duration = Duration::from_secs(2);

/// Variables that name the terminal the tests run in, or a multiplexer around it. The program
/// runs in this emulator, an xterm that draws no pictures, and must not take them for its own
/// terminal's. A test that plays another terminal sets them itself.
const OUTER_TERMINAL: &[&str] = &[
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERMINFO",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_BIN_DIR",
    "GHOSTTY_SHELL_FEATURES",
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "WEZTERM_PANE",
    "WEZTERM_EXECUTABLE",
    "WEZTERM_UNIX_SOCKET",
    "ITERM_SESSION_ID",
    "VTE_VERSION",
    "WT_SESSION",
    "TMUX",
    "TMUX_PANE",
    "STY",
    "ZELLIJ",
    "ZELLIJ_SESSION_NAME",
];

/// Pixels a cell, width then height, unless a test sets its own. A real terminal reports its
/// size in pixels beside its rows and columns, and the app finds its cell size from them.
const CELL_PIXELS: (u16, u16) = (10, 20);

/// Enter this soon after typed text could be a line break in a paste that a terminal without
/// bracketed paste types out key by key, which the app reads as part of the text. The app's
/// window is 120 ms.
const ENTER_GAP: Duration = Duration::from_millis(150);
/// The longest Enter waits for the app to show the typed text first. Text the screen never
/// shows as typed waits this long.
const ENTER_WAIT: Duration = Duration::from_secs(2);
/// How much of the typed text's last word Enter looks for on the screen.
const TAIL_CHARS: usize = 8;
/// How long a lone Esc gets to be read before the next key. Read together, `ESC ESC` is one
/// Esc and `ESC d` is Alt+d, as in any terminal app.
const ESC_SETTLE: Duration = Duration::from_millis(60);

#[derive(Debug, Clone)]
pub struct PtyCommand {
    program: PathBuf,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    env: Vec<(String, OsString)>,
    env_remove: Vec<String>,
    rows: u16,
    cols: u16,
    cell: (u16, u16),
    answers: bool,
    pictures: bool,
    colours: OwnColours,
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
            cell: CELL_PIXELS,
            answers: true,
            pictures: false,
            colours: OwnColours::default(),
        }
    }

    /// A terminal that answers none of the app's queries, as some don't, so the app waits out
    /// its own timeouts at start.
    pub fn unanswered(mut self) -> Self {
        self.answers = false;
        self
    }

    /// A terminal that takes the kitty protocol's pictures, as Ghostty and kitty do: it answers
    /// a graphics query with OK, and keeps what it is sent in one store for each screen, so
    /// [`Pty::check_pictures`] can say whether a placeholder shows its picture.
    pub fn takes_pictures(mut self) -> Self {
        self.pictures = true;
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

    /// Pixels a cell, width then height, as a terminal's font size sets them.
    pub fn cell_pixels(mut self, width: u16, height: u16) -> Self {
        self.cell = (width, height);
        self
    }

    /// A terminal whose own text colour and background are `text` and `background`, as it
    /// tells an app that asks (OSC 10 and 11), so an app that follows the background draws its
    /// light or dark theme as it would there. Unset, they are a dark xterm's.
    pub fn own_colours(mut self, text: Rgb, background: Rgb) -> Self {
        self.colours = OwnColours { text, background };
        self
    }
}

/// The window size a terminal reports: its rows and columns, and the pixels they cover.
fn window(rows: u16, cols: u16, (width, height): (u16, u16)) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: cols.saturating_mul(width),
        pixel_height: rows.saturating_mul(height),
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
    /// Pixels a cell, width then height.
    cell: (u16, u16),
    /// Text typed since the last Enter.
    typed: Option<Typed>,
}

/// Text typed and not yet sent with Enter.
struct Typed {
    at: Instant,
    /// How many bytes the app had drawn by then.
    drawn: usize,
    /// The end of the text, and how many times the screen showed it before the text was typed.
    /// `None` for a lone key, or text whose end the screen wouldn't show as typed.
    tail: Option<(String, usize)>,
}

/// Up to [`TAIL_CHARS`] of the text's last word, which a screen showing the text shows as
/// typed: a person's draft wraps between words, and control keys show as something else.
fn tail_of(text: &str) -> Option<String> {
    let word = text.split_whitespace().last()?;
    if word.chars().any(char::is_control) {
        return None;
    }
    let skip = word.chars().count().saturating_sub(TAIL_CHARS);
    Some(word.chars().skip(skip).collect())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// macOS's `openpty` now and then fails when several test threads call it at once.
static OPENING: Mutex<()> = Mutex::new(());

impl Pty {
    pub fn spawn(command: PtyCommand) -> anyhow::Result<Self> {
        let size = window(command.rows, command.cols, command.cell);
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
        for name in OUTER_TERMINAL {
            builder.env_remove(name);
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
        let mut emulator = Emulator::new(command.rows, command.cols, SCROLLBACK);
        if command.pictures {
            emulator = emulator.takes_pictures();
        }
        let terminal = Arc::new(Mutex::new(Terminal {
            emulator,
            raw: Vec::new(),
            closed: false,
        }));
        let answers = command.answers;
        let responder = Responder {
            pictures: command.pictures,
            colours: command.colours,
            ..Responder::default()
        };
        let reader = {
            let terminal = terminal.clone();
            let writer = writer.clone();
            thread::Builder::new()
                .name("argonauts-pty-reader".to_owned())
                .spawn(move || read_output(reader, &terminal, &writer, answers, responder))
                .context("starting the pty reader")?
        };
        Ok(Self {
            child,
            master: pair.master,
            writer,
            terminal,
            reader: Some(reader),
            size: (command.rows, command.cols),
            cell: command.cell,
            typed: None,
        })
    }

    /// Types text the way a person does: the text at once, and each `\r` as Enter once the app
    /// has the text, so the Enter sends it rather than joining it as a pasted line break.
    pub fn send(&mut self, text: &str) -> anyhow::Result<()> {
        let mut parts = text.split('\r').peekable();
        while let Some(part) = parts.next() {
            if !part.is_empty() {
                let tail = tail_of(part).map(|tail| {
                    let shown = self.visible_text().matches(tail.as_str()).count();
                    (tail, shown)
                });
                self.write(part.as_bytes())?;
                self.typed = Some(Typed {
                    at: Instant::now(),
                    drawn: self.drawn(),
                    tail,
                });
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
            Key::Char(_) => {
                self.typed = Some(Typed {
                    at: Instant::now(),
                    drawn: self.drawn(),
                    tail: None,
                });
            }
            _ => {}
        }
        self.write(&key.bytes())?;
        if key == Key::Esc {
            thread::sleep(ESC_SETTLE);
        }
        Ok(())
    }

    /// Waits until [`ENTER_GAP`] after the screen showed the last typed text, so the app reads
    /// the next Enter as a key of its own. Any output won't do: a spinner's next frame can come
    /// before the app has read the text, and the text and Enter read together are a paste.
    fn settle_typing(&mut self) {
        let Some(typed) = self.typed.take() else {
            return;
        };
        let deadline = typed.at + deadline::scaled(ENTER_WAIT);
        let mut looked_at = typed.drawn;
        let mut shown_at = None;
        loop {
            let now = Instant::now();
            let drawn = self.drawn();
            if shown_at.is_none() && drawn > looked_at {
                looked_at = drawn;
                if self.shows(&typed) {
                    shown_at = Some(now);
                }
            }
            let settled = shown_at.is_some_and(|at| now >= at + ENTER_GAP);
            if settled || now >= deadline {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Whether the screen shows the typed text: its end once more than before, or for a lone
    /// key, anything drawn since.
    fn shows(&self, typed: &Typed) -> bool {
        match &typed.tail {
            Some((tail, before)) => self.visible_text().matches(tail.as_str()).count() > *before,
            None => true,
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
    /// row, are in the scrollback, as xterm and iTerm2 keep them. A frame drawn as one
    /// synchronized update shows only once it is whole, as in those terminals.
    pub fn screen(&self) -> Screen {
        let terminal = lock(&self.terminal);
        Screen {
            scrollback: terminal.emulator.scrollback().map(str::to_owned).collect(),
            visible: visible_rows(&terminal),
        }
    }

    /// The screen a person would see, cell by cell, with the colours the program set and which
    /// placeholders show their picture. A frame drawn as one synchronized update shows only
    /// once it is whole.
    pub fn cells(&self) -> Grid {
        lock(&self.terminal).emulator.cells()
    }

    /// Fails when a picture the program meant to show doesn't: a placeholder on the screen a
    /// person sees names a picture that screen doesn't hold, or a placement was sent for a
    /// picture its screen didn't hold. kitty and Ghostty keep one store for each screen, and
    /// empty the alternate one each time the program enters it with `CSI ? 1049 h`.
    pub fn check_pictures(&self) -> anyhow::Result<()> {
        let terminal = lock(&self.terminal);
        let problems = terminal.emulator.picture_problems();
        if problems.is_empty() {
            return Ok(());
        }
        drop(terminal);
        bail!(
            "pictures don't show: {}; the screen was:\n{}",
            problems.join("; "),
            self.screen().visible_text()
        )
    }

    /// Waits until every placeholder on the screen shows its picture, as after a switch of
    /// screens the program sends its pictures again. A placement that found no picture fails
    /// at once, since no later output makes it right.
    pub fn wait_for_pictures(&self, timeout: Duration) -> anyhow::Result<()> {
        let deadline = deadline::after(timeout);
        loop {
            let misplaced = lock(&self.terminal).emulator.misplaced_a_picture();
            let checked = self.check_pictures();
            if checked.is_ok() || misplaced || Instant::now() >= deadline {
                return checked;
            }
            thread::sleep(POLL);
        }
    }

    /// The visible rows alone, cheaper than [`Self::screen`] with its scrollback.
    fn visible_text(&self) -> String {
        visible_rows(&lock(&self.terminal)).join("\n")
    }

    /// Everything the program wrote, escape sequences included.
    pub fn raw(&self) -> Vec<u8> {
        lock(&self.terminal).raw.clone()
    }

    /// Waits until the screen, scrollback included, matches `pattern`. The error shows the
    /// screen as it was.
    ///
    /// The scrollback keeps every line the program ever printed there, so this suits what
    /// stays, such as a line in the transcript. A state the screen shows only for a while, or
    /// one it showed earlier too, needs [`Self::wait_for_visible`] or [`Self::wait_until`].
    pub fn wait_for(&self, pattern: &str, timeout: Duration) -> anyhow::Result<Screen> {
        let regex = Regex::new(pattern).with_context(|| format!("bad pattern {pattern:?}"))?;
        self.poll(
            &format!("{pattern:?}"),
            deadline::after(timeout),
            |screen| regex.is_match(&screen.text()),
        )
    }

    /// Waits until the visible rows, leaving out the scrollback, match `pattern`, where `^`
    /// and `$` match at each row. The screen comes back once two looks in a row agree, so a
    /// frame read half drawn isn't taken.
    pub fn wait_for_visible(&self, pattern: &str, timeout: Duration) -> anyhow::Result<Screen> {
        let regex = RegexBuilder::new(pattern)
            .multi_line(true)
            .build()
            .with_context(|| format!("bad pattern {pattern:?}"))?;
        self.wait_until(&format!("{pattern:?} on screen"), timeout, |screen| {
            regex.is_match(&screen.visible_text())
        })
    }

    /// Waits until `done` holds for the screen, `what` naming it for the error. Like
    /// [`Self::wait_for_visible`], it returns once two looks in a row agree.
    pub fn wait_until(
        &self,
        what: &str,
        timeout: Duration,
        done: impl Fn(&Screen) -> bool,
    ) -> anyhow::Result<Screen> {
        let deadline = deadline::after(timeout);
        'look: loop {
            let mut screen = self.poll(what, deadline, &done)?;
            while Instant::now() < deadline {
                thread::sleep(POLL);
                let next = self.screen();
                if !done(&next) {
                    continue 'look;
                }
                if next.visible == screen.visible {
                    return Ok(next);
                }
                screen = next;
            }
            return Ok(screen);
        }
    }

    fn poll(
        &self,
        what: &str,
        deadline: Instant,
        done: impl Fn(&Screen) -> bool,
    ) -> anyhow::Result<Screen> {
        loop {
            let screen = self.screen();
            if done(&screen) {
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
                    "{why} waiting for {what}; the screen was:\n{}",
                    screen.text()
                );
            }
            thread::sleep(POLL);
        }
    }

    pub fn wait_for_exit(&mut self, timeout: Duration) -> anyhow::Result<u32> {
        let deadline = deadline::after(timeout);
        loop {
            if let Some(status) = self.child.try_wait().context("checking the program")? {
                self.drain();
                return Ok(status.exit_code());
            }
            if Instant::now() >= deadline {
                bail!(
                    "the program didn't exit in {:?}; the screen was:\n{}",
                    deadline::scaled(timeout),
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
        let deadline = deadline::after(DRAIN);
        while !lock(&self.terminal).closed && Instant::now() < deadline {
            thread::sleep(POLL);
        }
    }

    /// Resizes the window, keeping the pixels a cell.
    pub fn resize(&mut self, rows: u16, cols: u16) -> anyhow::Result<()> {
        self.resize_with_cell(rows, cols, self.cell)
    }

    /// Resizes the window with new pixels a cell, as a font size change does. The rows and
    /// columns may stay as they were: the size in pixels still changes, so the program is told.
    pub fn resize_with_cell(
        &mut self,
        rows: u16,
        cols: u16,
        cell: (u16, u16),
    ) -> anyhow::Result<()> {
        self.master
            .resize(window(rows, cols, cell))
            .map_err(|err| anyhow!("resizing the pty: {err}"))?;
        lock(&self.terminal).emulator.set_size(rows, cols);
        self.size = (rows, cols);
        self.cell = cell;
        Ok(())
    }

    /// The size in pixels the program reads with its rows and columns, width then height.
    pub fn pixel_size(&self) -> anyhow::Result<(u16, u16)> {
        let size = self
            .master
            .get_size()
            .map_err(|err| anyhow!("reading the pty's size: {err}"))?;
        Ok((size.pixel_width, size.pixel_height))
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
    mut responder: Responder,
) {
    let mut buffer = [0u8; 8192];
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
    let mut terminal = lock(terminal);
    terminal.emulator.release();
    terminal.closed = true;
}

/// The screen a terminal `cols` × `rows` that takes pictures, as Ghostty does, shows once it has
/// read `bytes`.
pub(crate) fn read_bytes(cols: u16, rows: u16, bytes: &[u8]) -> Grid {
    let mut emulator = Emulator::new(rows, cols, 0).takes_pictures();
    emulator.process(bytes);
    emulator.release();
    emulator.cells()
}

/// The rows the terminal shows, top to bottom, trailing spaces trimmed.
fn visible_rows(terminal: &Terminal) -> Vec<String> {
    let screen = terminal.emulator.shown();
    let (rows, cols) = screen.size();
    screen
        .rows(0, cols)
        .take(usize::from(rows))
        .map(|line| line.trim_end().to_owned())
        .collect()
}

/// Spots terminal queries in the output stream, even split across reads, and builds replies.
#[derive(Default)]
struct Responder {
    /// The start of a query cut off by the end of the last read.
    tail: Vec<u8>,
    /// Graphics queries are answered with OK.
    pictures: bool,
    colours: OwnColours,
}

/// A terminal's own text colour and background, which it tells an app that asks.
#[derive(Debug, Clone, Copy)]
struct OwnColours {
    text: Rgb,
    background: Rgb,
}

impl Default for OwnColours {
    /// A dark xterm's.
    fn default() -> Self {
        Self {
            text: Rgb(0xc8, 0xcd, 0xd8),
            background: Rgb(0x1e, 0x21, 0x30),
        }
    }
}

/// The start of a kitty graphics command, which a graphics query is.
const GRAPHICS: &[u8] = b"\x1b_G";

/// Cursor position report request; the reply depends on where the cursor is.
const CURSOR_QUERY: &[u8] = b"\x1b[6n";

/// Queries with fixed replies, as xterm gives them.
const QUERIES: [(&[u8], &[u8]); 3] = [
    (b"\x1b[c", b"\x1b[?1;2c"),
    (b"\x1b[0c", b"\x1b[?1;2c"),
    (b"\x1b[?u", b"\x1b[?0u"),
];

/// Queries for the terminal's own colours, OSC 10 for its text colour and OSC 11 for its
/// background: the query, its number, and the ending its answer takes.
const COLOUR_QUERIES: [(&[u8], u8, &[u8]); 3] = [
    (b"\x1b]10;?\x07", 10, b"\x07"),
    (b"\x1b]11;?\x07", 11, b"\x07"),
    (b"\x1b]11;?\x1b\\", 11, b"\x1b\\"),
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
            } else if let Some(&(query, number, end)) =
                COLOUR_QUERIES.iter().find(|(q, ..)| rest.starts_with(q))
            {
                replies.extend(self.colour_answer(number, end));
                at += query.len();
            } else if self.pictures && rest.starts_with(GRAPHICS) {
                let Some(end) = rest.windows(2).position(|pair| pair == b"\x1b\\") else {
                    self.tail = rest.to_vec();
                    break;
                };
                replies.extend(graphics_answer(&rest[..end]));
                at += end + 2;
            } else if is_cut_query(rest) || (self.pictures && GRAPHICS.starts_with(rest)) {
                self.tail = rest.to_vec();
                break;
            } else {
                at += 1;
            }
        }
        replies
    }

    /// The answer to OSC `number`'s question, in xterm's sixteen bits a channel.
    fn colour_answer(&self, number: u8, end: &[u8]) -> Vec<u8> {
        let Rgb(red, green, blue) = match number {
            10 => self.colours.text,
            _ => self.colours.background,
        };
        let mut answer = format!(
            "\x1b]{number};rgb:{red:02x}{red:02x}/{green:02x}{green:02x}/{blue:02x}{blue:02x}"
        )
        .into_bytes();
        answer.extend_from_slice(end);
        answer
    }
}

/// OK to a graphics query, `a=q`, under the id it asked with. Other graphics commands, which
/// send pictures quietly, have no answer.
fn graphics_answer(command: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(&command[GRAPHICS.len()..]);
    let keys = text.split(';').next().unwrap_or_default();
    let mut keys = keys.split(',');
    if !keys.clone().any(|key| key == "a=q") {
        return Vec::new();
    }
    match keys.find_map(|key| key.strip_prefix("i=")) {
        Some(id) => format!("\x1b_Gi={id};OK\x1b\\").into_bytes(),
        None => Vec::new(),
    }
}

fn is_cut_query(rest: &[u8]) -> bool {
    rest.first() == Some(&0x1b)
        && std::iter::once(CURSOR_QUERY)
            .chain(QUERIES.iter().map(|(query, _)| *query))
            .chain(COLOUR_QUERIES.iter().map(|(query, ..)| *query))
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
    fn a_terminal_that_takes_pictures_answers_the_graphics_query() {
        let query = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c";
        let mut responder = Responder {
            pictures: true,
            ..Responder::default()
        };
        let (first, rest) = query.split_at(2);
        assert_eq!(responder.replies(first, (0, 0)), b"");
        assert_eq!(
            responder.replies(rest, (0, 0)),
            b"\x1b_Gi=31;OK\x1b\\\x1b[?1;2c"
        );
        // A picture sent quietly has no answer.
        let quiet = b"\x1b_Ga=t,f=100,q=2,i=5,m=0;iVBORw==\x1b\\";
        assert_eq!(responder.replies(quiet, (0, 0)), b"");
        // Nor does the query, in a terminal that takes no pictures.
        assert_eq!(Responder::default().replies(query, (0, 0)), b"\x1b[?1;2c");
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

    #[test]
    fn a_terminal_tells_its_own_colours_in_the_ending_it_was_asked_in() {
        let dark = Responder::default();
        assert_eq!(
            dark.colour_answer(11, b"\x1b\\"),
            b"\x1b]11;rgb:1e1e/2121/3030\x1b\\"
        );
        let mut white = Responder {
            colours: OwnColours {
                text: Rgb(0, 0, 0),
                background: Rgb(0xff, 0xff, 0xff),
            },
            ..Responder::default()
        };
        assert!(white.replies(b"\x1b]11;", (0, 0)).is_empty());
        assert_eq!(
            white.replies(b"?\x1b\\\x1b]10;?\x07", (0, 0)),
            b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1b]10;rgb:0000/0000/0000\x07"
        );
    }
}
