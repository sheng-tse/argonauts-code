//! The order and the panic hook follow Codex's `tui.rs` (Apache-2.0): modes go on in one
//! order and come off in the reverse one, and a panic restores the terminal before its message
//! prints, so a crash never leaves the shell in raw mode.

use std::io::{self, IsTerminal, Write};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::cursor::{self, SetCursorStyle, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{self, LeaveAlternateScreen};
use crossterm::{execute, queue};
use ratatui::layout::{Position, Size};

/// Whether keyboard flags were pushed, so the panic hook knows to pop them.
static KEYBOARD_PUSHED: AtomicBool = AtomicBool::new(false);
/// Whether the terminal tells Ctrl+Enter from Ctrl+J, through the keyboard protocol asked for at
/// start. Without it Ctrl+Enter arrives as a new line, as in Claude Code.
pub fn keyboard_protocol() -> bool {
    KEYBOARD_PUSHED.load(Ordering::Relaxed)
}

/// Whether the app or an overlay has the alternate screen, so the panic hook and
/// [`TerminalModes::restore`] know to leave it.
static ALTERNATE_SCREEN: AtomicBool = AtomicBool::new(false);

/// Whether the inline screen reports the mouse, as while a map shows, so the panic hook and
/// [`TerminalModes::restore`] know to turn it off.
static MOUSE_REPORTED: AtomicBool = AtomicBool::new(false);

/// Set once the panic hook gave the terminal back, so a screen still running draws nothing more.
static GIVEN_BACK: AtomicBool = AtomicBool::new(false);

/// Clicks, the wheel, and motion while a button is down, reported as SGR mouse events, so the
/// app can select text. The terminal's own selection still works with its bypass key held,
/// such as Shift.
pub(crate) const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";
/// Turns off every kind of mouse reporting the app uses, a map's motion and pixels among
/// them, and alternate scroll mode.
pub(crate) const MOUSE_OFF: &str =
    "\x1b[?1016l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?1007l";

/// Raw mode, bracketed paste, focus events, and keyboard disambiguation where the terminal
/// supports it. Dropping it restores the terminal.
#[derive(Debug)]
pub struct TerminalModes {
    active: bool,
    keyboard_pushed: bool,
}

impl TerminalModes {
    /// Colors are forced on in crossterm, because the theme already honors `NO_COLOR` and
    /// crossterm's own handling would reset bold and dim along with colors.
    pub fn enable() -> io::Result<Self> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(io::Error::other(
                "the interactive UI needs a terminal: stdin and stdout must both be a tty",
            ));
        }
        crossterm::style::force_color_output(true);
        terminal::enable_raw_mode()?;
        // From here on, dropping `modes` undoes whatever succeeded.
        let mut modes = Self {
            active: true,
            keyboard_pushed: false,
        };
        execute!(io::stdout(), EnableBracketedPaste, EnableFocusChange)?;
        // Without the kitty protocol, Shift+Enter looks like Enter. Terminals that don't know
        // it don't answer the query, and keep working the old way.
        if matches!(terminal::supports_keyboard_enhancement(), Ok(true)) {
            execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
            modes.keyboard_pushed = true;
            KEYBOARD_PUSHED.store(true, Ordering::Relaxed);
        }
        Ok(modes)
    }

    /// For a terminal that went away: nothing is restored, now or on drop.
    pub fn abandon(mut self) {
        self.active = false;
    }

    /// Safe to call twice.
    pub fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        let mut stdout = io::stdout();
        let mut first_error = None;
        let mut keep = |result: io::Result<()>| {
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        };
        // Normally the screen left already. After an error it may not have.
        if ALTERNATE_SCREEN.swap(false, Ordering::Relaxed) {
            keep(execute!(stdout, Print(MOUSE_OFF), LeaveAlternateScreen));
        }
        if MOUSE_REPORTED.swap(false, Ordering::Relaxed) {
            keep(execute!(stdout, Print(MOUSE_OFF)));
        }
        if self.keyboard_pushed {
            keep(execute!(stdout, PopKeyboardEnhancementFlags));
            self.keyboard_pushed = false;
            KEYBOARD_PUSHED.store(false, Ordering::Relaxed);
        }
        keep(execute!(stdout, DisableFocusChange, DisableBracketedPaste));
        keep(terminal::disable_raw_mode());
        keep(execute!(stdout, Show, SetCursorStyle::DefaultUserShape));
        first_error.map_or(Ok(()), Err)
    }

    pub fn is_active(&self) -> bool {
        self.active
    }
}

impl Drop for TerminalModes {
    fn drop(&mut self) {
        if self.active {
            let _ = self.restore();
        }
    }
}

pub(crate) fn note_alternate_screen(active: bool) {
    ALTERNATE_SCREEN.store(active, Ordering::Relaxed);
}

pub(crate) fn note_mouse(active: bool) {
    MOUSE_REPORTED.store(active, Ordering::Relaxed);
}

/// Whether a panic already gave the terminal back to the shell.
pub(crate) fn given_back_after_panic() -> bool {
    GIVEN_BACK.load(Ordering::Relaxed)
}

/// Installing it twice is harmless.
pub fn install_panic_hook() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_after_panic();
            previous(info);
        }));
    });
}

/// Best effort, since the panic may have come from anywhere, even mid-frame.
fn restore_after_panic() {
    GIVEN_BACK.store(true, Ordering::Relaxed);
    let mut stdout = io::stdout();
    if ALTERNATE_SCREEN.swap(false, Ordering::Relaxed) {
        let _ = queue!(stdout, Print(MOUSE_OFF), LeaveAlternateScreen);
    }
    if MOUSE_REPORTED.swap(false, Ordering::Relaxed) {
        let _ = queue!(stdout, Print(MOUSE_OFF));
    }
    if KEYBOARD_PUSHED.swap(false, Ordering::Relaxed) {
        let _ = queue!(stdout, PopKeyboardEnhancementFlags);
    }
    let _ = queue!(
        stdout,
        SetAttribute(Attribute::Reset),
        DisableFocusChange,
        DisableBracketedPaste,
        Show,
        SetCursorStyle::DefaultUserShape,
    );
    let _ = stdout.write_all(b"\r\n");
    let _ = stdout.flush();
    let _ = terminal::disable_raw_mode();
}

/// Falls back to under the last row when the terminal doesn't answer the cursor query, so
/// whatever the screen shows counts as printed and nothing paints over it.
pub fn probe() -> io::Result<(Size, Position)> {
    let (size, cursor) = probe_cursor()?;
    Ok((size, cursor.unwrap_or(Position::new(0, size.height))))
}

/// Terminals that don't answer the cursor query take crossterm's two second timeout to say so.
pub fn probe_cursor() -> io::Result<(Size, Option<Position>)> {
    let (columns, rows) = terminal::size()?;
    let cursor = cursor::position().ok().map(|(x, y)| Position::new(x, y));
    Ok((Size::new(columns, rows), cursor))
}

/// Throws away input the terminal still holds, such as keys a child program didn't read, so
/// they don't land in the prompt when the screen comes back.
pub fn flush_stdin() {
    #[cfg(unix)]
    {
        use nix::sys::termios::{FlushArg, tcflush};
        // Nothing to flush when stdin isn't a terminal.
        let _ = tcflush(io::stdin(), FlushArg::TCIFLUSH);
    }
}

/// Asks the terminal for its background color (OSC 11) and waits up to `timeout` for the
/// answer: `Some(true)` for a light background, `None` when it didn't say in time. Raw mode
/// must be on, and nothing else may read input meanwhile.
pub fn probe_background(timeout: std::time::Duration) -> Option<bool> {
    #[cfg(unix)]
    {
        read_background(timeout)
    }
    #[cfg(not(unix))]
    {
        let _ = timeout;
        None
    }
}

#[cfg(unix)]
fn read_background(timeout: std::time::Duration) -> Option<bool> {
    let answer = ask(b"\x1b]11;?\x1b\\", osc_complete, timeout)?;
    parse_background(&answer).map(is_light)
}

/// Pixels a cell, from the size in pixels the terminal reports with its size (TIOCGWINSZ).
/// `None` when it reports none, as some terminals and anything not a terminal do.
pub fn cell_size() -> Option<(u16, u16)> {
    let size = terminal::window_size().ok()?;
    cell_of((size.width, size.height), (size.columns, size.rows))
}

fn cell_of((width, height): (u16, u16), (columns, rows): (u16, u16)) -> Option<(u16, u16)> {
    if columns == 0 || rows == 0 {
        return None;
    }
    Some((width / columns, height / rows)).filter(|(width, height)| *width > 0 && *height > 0)
}

/// Pixels a cell: from the size the terminal reports, else from its answer to CSI 16 t,
/// waited for up to `timeout`. Raw mode must be on, and nothing else may read input meanwhile.
pub fn probe_cell_size(timeout: std::time::Duration) -> Option<(u16, u16)> {
    if let Some(cell) = cell_size() {
        return Some(cell);
    }
    #[cfg(unix)]
    {
        let done = |answer: &[u8]| answer.contains(&b't');
        parse_cell_size(&ask(b"\x1b[16t", done, timeout)?)
    }
    #[cfg(not(unix))]
    {
        let _ = timeout;
        None
    }
}

/// `ESC [ 6 ; height ; width t`, the answer to CSI 16 t, as width and height.
pub fn parse_cell_size(answer: &[u8]) -> Option<(u16, u16)> {
    let text = std::str::from_utf8(answer).ok()?;
    let body = &text[text.find("\x1b[6;")? + 4..];
    let (body, _) = body.split_once('t')?;
    let (height, width) = body.split_once(';')?;
    let (width, height) = (width.parse().ok()?, height.parse().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

/// Sends `query` and reads the terminal's answer until `done` says it is whole or `timeout`
/// passes. `None` when stdin or stdout isn't a terminal.
#[cfg(unix)]
fn ask(
    query: &[u8],
    done: impl Fn(&[u8]) -> bool,
    timeout: std::time::Duration,
) -> Option<Vec<u8>> {
    use std::time::Instant;

    use nix::sys::termios::{SetArg, SpecialCharacterIndices, tcgetattr, tcsetattr};

    let stdin = io::stdin();
    if !stdin.is_terminal() || !io::stdout().is_terminal() {
        return None;
    }
    let saved = tcgetattr(&stdin).ok()?;
    // Reads return after a tenth of a second without input, so the deadline holds.
    let mut polling = saved.clone();
    polling.control_chars[SpecialCharacterIndices::VMIN as usize] = 0;
    polling.control_chars[SpecialCharacterIndices::VTIME as usize] = 1;
    tcsetattr(&stdin, SetArg::TCSANOW, &polling).ok()?;

    let deadline = Instant::now() + timeout;
    let mut answer = Vec::new();
    let mut stdout = io::stdout();
    let asked = stdout.write_all(query).and_then(|()| stdout.flush());
    while asked.is_ok() && Instant::now() < deadline && !done(&answer) {
        let mut chunk = [0u8; 64];
        match nix::unistd::read(&stdin, &mut chunk) {
            Ok(read) => answer.extend_from_slice(&chunk[..read]),
            Err(nix::errno::Errno::EINTR) => {}
            Err(_) => break,
        }
    }
    let _ = tcsetattr(&stdin, SetArg::TCSANOW, &saved);
    Some(answer)
}

/// Whether the terminal reports the mouse in pixels when asked (SGR-Pixels, mode 1016), by its
/// answer to DECRQM. A terminal that doesn't know the mode, or doesn't answer within `timeout`,
/// reports cells, which its environment alone can't tell: a terminal started from Ghostty keeps
/// Ghostty's variables. The question goes with DA1, which every terminal answers after it, so
/// the wait ends at that answer. Raw mode must be on, and nothing else may read input meanwhile.
pub fn probe_pixel_mouse(timeout: std::time::Duration) -> bool {
    #[cfg(unix)]
    {
        ask(b"\x1b[?1016$p\x1b[c", attributes_said, timeout)
            .is_some_and(|answer| knows_mode(&answer, 1016))
    }
    #[cfg(not(unix))]
    {
        let _ = timeout;
        false
    }
}

/// Whether `answer` holds the terminal's answer to DA1, `CSI ? <attributes> c`.
fn attributes_said(answer: &[u8]) -> bool {
    answer.windows(3).enumerate().any(|(at, start)| {
        start == b"\x1b[?"
            && answer[at + 3..]
                .iter()
                .find(|byte| !byte.is_ascii_digit() && **byte != b';')
                == Some(&b'c')
    })
}

/// Reads DECRQM's `CSI ? <mode> ; <state> $ y`: a state of 1 to 4 means the terminal knows
/// the mode, 0 that it doesn't.
pub fn knows_mode(answer: &[u8], mode: u16) -> bool {
    let text = String::from_utf8_lossy(answer);
    let asked = format!("[?{mode};");
    let Some(start) = text.find(&asked) else {
        return false;
    };
    let rest = &text[start + asked.len()..];
    rest.find("$y")
        .and_then(|end| rest[..end].parse::<u8>().ok())
        .is_some_and(|state| (1..=4).contains(&state))
}

/// An OSC answer ends with BEL or ST.
fn osc_complete(bytes: &[u8]) -> bool {
    bytes.ends_with(b"\x07") || bytes.ends_with(b"\x1b\\")
}

/// Reads `ESC ] 11 ; rgb:RRRR/GGGG/BBBB` (with one to four hex digits per channel) into 8-bit
/// channels.
pub fn parse_background(answer: &[u8]) -> Option<(u8, u8, u8)> {
    let text = std::str::from_utf8(answer).ok()?;
    let start = text.find("]11;")? + 4;
    let body = &text[start..];
    let end = body.find(['\x07', '\x1b']).unwrap_or(body.len());
    let color = body[..end].strip_prefix("rgb:")?;
    let mut channels = color.split('/').map(|hex| {
        let digits = u32::try_from(hex.len())
            .ok()
            .filter(|n| (1..=4).contains(n))?;
        let value = u32::from_str_radix(hex, 16).ok()?;
        let max = (1u32 << (4 * digits)) - 1;
        u8::try_from(value * 255 / max).ok()
    });
    let rgb = (channels.next()??, channels.next()??, channels.next()??);
    channels.next().is_none().then_some(rgb)
}

/// Relative luminance past the middle, the way terminals pick light themes.
pub fn is_light((r, g, b): (u8, u8, u8)) -> bool {
    let luma = 0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b);
    luma > 127.5
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_terminal_that_knows_pixel_mouse_reports_says_so() {
        assert!(super::knows_mode(b"\x1b[?1016;2$y", 1016));
        assert!(super::knows_mode(b"junk\x1b[?1016;1$y", 1016));
        assert!(
            !super::knows_mode(b"\x1b[?1016;0$y", 1016),
            "not recognized"
        );
        assert!(!super::knows_mode(b"\x1b[?1004;2$y", 1016), "another mode");
        assert!(!super::knows_mode(b"", 1016), "no answer");
        assert!(super::attributes_said(b"\x1b[?1016;2$y\x1b[?62;22c"));
        assert!(super::attributes_said(b"\x1b[?1;2c"));
        assert!(
            !super::attributes_said(b"\x1b[?1016;2$y"),
            "DECRQM's answer alone"
        );
    }

    #[test]
    fn a_cell_is_the_pixels_over_the_cells() {
        assert_eq!(super::cell_of((1062, 1344), (118, 64)), Some((9, 21)));
        assert_eq!(super::cell_of((0, 0), (118, 64)), None);
        assert_eq!(super::cell_of((1062, 1344), (0, 0)), None);
    }

    #[test]
    fn the_answer_to_csi_16_t_is_height_then_width() {
        assert_eq!(super::parse_cell_size(b"\x1b[6;42;18t"), Some((18, 42)));
        assert_eq!(super::parse_cell_size(b"junk\x1b[6;21;9t"), Some((9, 21)));
        assert_eq!(super::parse_cell_size(b"\x1b[6;0;9t"), None);
        assert_eq!(super::parse_cell_size(b""), None);
    }

    use super::*;

    #[test]
    fn reads_the_background_answer() {
        assert_eq!(
            parse_background(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\"),
            Some((255, 255, 255))
        );
        assert_eq!(
            parse_background(b"\x1b]11;rgb:1e1e/1e1e/2e2e\x07"),
            Some((30, 30, 46))
        );
        assert_eq!(
            parse_background(b"\x1b]11;rgb:f/8/0\x07"),
            Some((255, 136, 0))
        );
        assert_eq!(parse_background(b"\x1b]11;rgb:zz/00/00\x07"), None);
        assert_eq!(parse_background(b"\x1b]10;rgb:ff/ff/ff\x07"), None);
        assert_eq!(parse_background(b""), None);
        assert!(is_light((250, 250, 245)));
        assert!(!is_light((30, 30, 46)));
    }
}
