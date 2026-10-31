//! vt100 with the scrollback xterm and iTerm2 keep. vt100 drops a row that scrolls off a scroll
//! region, but those terminals keep it when the region starts at the top row. That is how an
//! inline app prints history above the live rows it keeps out of the scroll, so a test has to
//! see those rows the way you would.
//!
//! A second parser runs one byte ahead of vt100 to say what each byte is about to do. Before a
//! byte that can scroll, the rows it would push off the top are copied out.
//!
//! Like those terminals, it shows a frame the app wraps in a synchronized update
//! (`CSI ? 2026 h` … `CSI ? 2026 l`) only once the frame is whole, so a test never reads one
//! half drawn, however long the app stalls in the middle of it.
//!
//! A terminal set to take pictures keeps the kitty protocol's pictures as Ghostty does, one store
//! for each screen, and pictures placed on the main screen go up with its text when rows scroll
//! off its top (see [`super::pictures`]).

use std::collections::VecDeque;

use super::pictures::{Pictures, Stores};
use crate::screen::Grid;

pub(super) struct Emulator {
    parser: vt100::Parser,
    spy: vte::Parser,
    /// The main screen's scroll region as rows `top..=bottom`, which vt100 doesn't show.
    region: (u16, u16),
    scrollback: VecDeque<String>,
    limit: usize,
    /// Where the cursor was when vt100 last caught up, and how many characters were printed
    /// since. Each one moves the cursor two columns at most, which says when the next could
    /// wrap without asking vt100 after every character.
    cursor: Option<(u16, u16)>,
    printed: u16,
    /// What the screen showed when an unfinished synchronized update began.
    held: Option<Held>,
    pictures: Pictures,
}

struct Held {
    screen: vt100::Screen,
    /// Rows the update has pushed into the scrollback so far, shown once it ends.
    pushed: usize,
    /// The pictures each screen held then, which the screen shown draws from.
    stores: Stores,
}

/// What a byte does, as far as scrolling, whole frames and pictures care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Print,
    /// LF, VT, and FF all move down a row, and scroll at the region's bottom.
    LineFeed,
    /// `CSI n S`.
    ScrollUp(u16),
    /// `CSI top ; bottom r`, with 0 for a missing number.
    Region(u16, u16),
    /// `ESC c`.
    Reset,
    /// `CSI 2 J`, which clears the screen whole.
    Clear,
    /// `CSI ? … h` or `l` with the modes that matter here: `CSI ? 2026 h` begins a
    /// synchronized update and `CSI ? 2026 l` ends it, and `CSI ? 1049 h` enters the alternate
    /// screen, clearing it.
    Modes {
        on: bool,
        synchronized: bool,
        alternate: bool,
    },
    /// The `\` that ends a kitty graphics command.
    Graphics,
    /// Anything else, which may move the cursor.
    Other,
}

/// The private mode of synchronized updates.
const SYNCHRONIZED: u16 = 2026;
/// The private mode of the alternate screen that clears it on the way in, and with it the
/// screen's pictures.
const ALTERNATE_CLEARED: u16 = 1049;

#[derive(Default)]
struct Spy {
    action: Option<Action>,
}

impl vte::Perform for Spy {
    fn print(&mut self, _c: char) {
        self.action = Some(Action::Print);
    }

    fn execute(&mut self, byte: u8) {
        self.action = Some(match byte {
            b'\n' | 0x0b | 0x0c => Action::LineFeed,
            _ => Action::Other,
        });
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        let mut numbers = params
            .iter()
            .map(|param| param.first().copied().unwrap_or(0));
        let mut next = || numbers.next().unwrap_or(0);
        self.action = Some(match (intermediates, action) {
            ([], 'S') => Action::ScrollUp(next().max(1)),
            ([], 'r') => {
                let top = next();
                Action::Region(top, next())
            }
            ([], 'J') if next() == 2 => Action::Clear,
            ([b'?'], 'h' | 'l') => {
                let (mut synchronized, mut alternate) = (false, false);
                for mode in numbers {
                    synchronized |= mode == SYNCHRONIZED;
                    alternate |= mode == ALTERNATE_CLEARED;
                }
                if synchronized || alternate {
                    Action::Modes {
                        on: action == 'h',
                        synchronized,
                        alternate,
                    }
                } else {
                    Action::Other
                }
            }
            _ => Action::Other,
        });
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        self.action = Some(if intermediates.is_empty() && byte == b'c' {
            Action::Reset
        } else {
            Action::Other
        });
    }

    fn hook(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _c: char) {
        self.action = Some(Action::Other);
    }

    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {
        self.action = Some(Action::Other);
    }
}

impl Emulator {
    pub(super) fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        Self {
            // The scrollback lives here, so vt100 keeps none of its own.
            parser: vt100::Parser::new(rows, cols, 0),
            spy: vte::Parser::new(),
            region: (0, rows.saturating_sub(1)),
            scrollback: VecDeque::new(),
            limit: scrollback,
            cursor: None,
            printed: 0,
            held: None,
            pictures: Pictures::default(),
        }
    }

    /// A terminal that keeps the kitty protocol's pictures, as Ghostty and kitty do, its cells
    /// `cell` pixels.
    pub(super) fn takes_pictures(mut self, cell: (u16, u16)) -> Self {
        self.pictures = Pictures::taking(cell);
        self
    }

    /// The terminal's cells are `cell` pixels from now on, as after a font size change.
    pub(super) fn use_cell(&mut self, cell: (u16, u16)) {
        self.pictures.use_cell(cell);
    }

    /// The screen as the program has drawn it so far, which is where the cursor is.
    pub(super) fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// The screen a person would see: during an unfinished synchronized update, the one from
    /// before it.
    pub(super) fn shown(&self) -> &vt100::Screen {
        self.held
            .as_ref()
            .map_or(self.parser.screen(), |held| &held.screen)
    }

    /// The cells a person would see, a placeholder showing its picture when the stores the shown
    /// screen draws from hold it, and a picture placed on the screen over the cells it covers.
    pub(super) fn cells(&self) -> Grid {
        let screen = self.shown();
        let stores = self.shown_stores();
        let alternate = screen.alternate_screen();
        let mut cells = Grid::read(screen, &|id| self.pictures.shows(stores, alternate, id));
        self.pictures.cover(&mut cells, stores);
        cells
    }

    /// What keeps a picture the program meant to show from showing, one line each: placeholders
    /// on the shown screen whose picture it doesn't hold, and placements that found no picture.
    pub(super) fn picture_problems(&self) -> Vec<String> {
        self.pictures.problems(&self.cells(), self.shown_stores())
    }

    /// Whether a placement found no picture, which no later output makes right.
    pub(super) fn misplaced_a_picture(&self) -> bool {
        self.pictures.misplaced_any()
    }

    fn shown_stores(&self) -> &Stores {
        self.held
            .as_ref()
            .map_or(self.pictures.stores(), |held| &held.stores)
    }

    /// The scrollback a person would see, oldest first, trailing spaces trimmed. Rows an
    /// unfinished synchronized update pushed up aren't in it yet.
    pub(super) fn scrollback(&self) -> impl Iterator<Item = &str> {
        let pushed = self.held.as_ref().map_or(0, |held| held.pushed);
        let shown = self.scrollback.len().saturating_sub(pushed);
        self.scrollback.iter().take(shown).map(String::as_str)
    }

    /// Shows what the program drew, even mid-update, as a terminal does once the program is
    /// gone.
    pub(super) fn release(&mut self) {
        self.held = None;
    }

    /// As vt100 resizes a region: one that reached the bottom still does.
    pub(super) fn set_size(&mut self, rows: u16, cols: u16) {
        let (old_rows, _) = self.parser.screen().size();
        let (mut top, mut bottom) = self.region;
        if bottom == old_rows.saturating_sub(1) || bottom >= rows {
            bottom = rows.saturating_sub(1);
        }
        if bottom < top {
            top = 0;
        }
        self.region = (top, bottom);
        self.parser.screen_mut().set_size(rows, cols);
        self.cursor = None;
        // A held screen has the old size; the program draws the new one whole.
        self.held = None;
    }

    pub(super) fn process(&mut self, bytes: &[u8]) {
        let mut fed = 0;
        for at in 0..bytes.len() {
            let mut spy = Spy::default();
            self.spy.advance(&mut spy, &bytes[at..=at]);
            let action = if self.pictures.scan(bytes[at]) {
                Some(Action::Graphics)
            } else {
                spy.action
            };
            let Some(action) = action else {
                continue;
            };
            match action {
                Action::Other => self.cursor = None,
                Action::Print if !self.may_wrap() => self.printed += 1,
                _ => {
                    self.parser.process(&bytes[fed..at]);
                    self.apply(action, bytes[at]);
                    fed = at + 1;
                }
            }
        }
        self.parser.process(&bytes[fed..]);
    }

    /// Whether the next character could start at the last column or past it, where it wraps.
    fn may_wrap(&self) -> bool {
        let (_, cols) = self.parser.screen().size();
        self.cursor.is_none_or(|(_, col)| {
            col.saturating_add(self.printed.saturating_mul(2)) >= cols.saturating_sub(1)
        })
    }

    /// Feeds the byte that completes `action`, with vt100 caught up to just before it.
    fn apply(&mut self, action: Action, byte: u8) {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let (row, col) = screen.cursor_position();
        let kept_here = !screen.alternate_screen() && self.region.0 == 0;
        let at_bottom = kept_here && row == self.region.1;
        match action {
            Action::LineFeed if at_bottom => self.keep_top_rows(1),
            Action::ScrollUp(count) if kept_here => {
                self.keep_top_rows(count.min(rows));
            }
            Action::Print if at_bottom && col >= cols.saturating_sub(1) => {
                let top = self.row_text(0);
                self.parser.process(&[byte]);
                // A wrap starts the row again, so the cursor ends up left of where it was.
                let (_, after) = self.parser.screen().cursor_position();
                if after < col {
                    self.push(top);
                    self.pictures.scroll(false, self.region.1, 1);
                }
                self.cursor = Some(self.parser.screen().cursor_position());
                self.printed = 0;
                return;
            }
            Action::Region(top, bottom) if !screen.alternate_screen() => {
                self.region = region(top, bottom, rows);
            }
            Action::Reset => {
                self.region = (0, rows.saturating_sub(1));
                self.scrollback.clear();
                self.held = None;
                self.pictures.empty(false);
                self.pictures.empty(true);
            }
            Action::Clear => self.pictures.empty(screen.alternate_screen()),
            Action::Modes {
                on,
                synchronized,
                alternate,
            } => {
                if synchronized && !on {
                    self.held = None;
                } else if synchronized && self.held.is_none() {
                    self.held = Some(Held {
                        screen: screen.clone(),
                        pushed: 0,
                        stores: self.pictures.stores().clone(),
                    });
                }
                if alternate && on {
                    self.pictures.empty(true);
                }
            }
            Action::Graphics => self
                .pictures
                .run(screen.alternate_screen(), screen.cursor_position()),
            _ => {}
        }
        self.parser.process(&[byte]);
        self.cursor = Some(self.parser.screen().cursor_position());
        self.printed = 0;
    }

    /// Copies the rows `count` scrolls push off a region starting at the top. Past the
    /// region's own rows, only the blank rows it pulled in go.
    fn keep_top_rows(&mut self, count: u16) {
        let (_, cols) = self.parser.screen().size();
        let region_rows = usize::from(self.region.1) + 1;
        let rows: Vec<String> = self
            .parser
            .screen()
            .rows(0, cols)
            .take(region_rows.min(usize::from(count)))
            .map(|row| row.trim_end().to_owned())
            .collect();
        let blanks = usize::from(count).saturating_sub(rows.len());
        for row in rows
            .into_iter()
            .chain(std::iter::repeat_n(String::new(), blanks))
        {
            self.push(row);
        }
        self.pictures.scroll(false, self.region.1, count);
    }

    fn row_text(&self, row: usize) -> String {
        let (_, cols) = self.parser.screen().size();
        self.parser
            .screen()
            .rows(0, cols)
            .nth(row)
            .map(|text| text.trim_end().to_owned())
            .unwrap_or_default()
    }

    fn push(&mut self, row: String) {
        if let Some(held) = &mut self.held {
            held.pushed += 1;
        }
        self.scrollback.push_back(row);
        while self.scrollback.len() > self.limit {
            self.scrollback.pop_front();
        }
    }
}

/// The region `CSI top ; bottom r` sets, as vt100 reads it: a missing number is the screen's
/// edge, and a region of one row or less is the whole screen.
fn region(top: u16, bottom: u16, rows: u16) -> (u16, u16) {
    let last = rows.saturating_sub(1);
    let top = top.max(1) - 1;
    let bottom = if bottom == 0 { rows } else { bottom };
    let bottom = bottom.saturating_sub(1).min(last);
    if top < bottom {
        (top, bottom)
    } else {
        (0, last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(range: std::ops::RangeInclusive<u32>) -> String {
        range.map(|n| format!("line-{n}\r\n")).collect()
    }

    fn kept(emulator: &Emulator) -> Vec<&str> {
        emulator.scrollback().collect()
    }

    fn visible(emulator: &Emulator) -> Vec<String> {
        let (_, cols) = emulator.screen().size();
        emulator
            .screen()
            .rows(0, cols)
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    /// Rows 1 to 4 scroll and the last two are a live region, as an inline app sets them.
    fn inline_app() -> Emulator {
        let mut emulator = Emulator::new(6, 20, 100);
        emulator.process(b"\x1b[5;1Hlive one\x1b[6;1Hlive two");
        emulator.process(b"\x1b[1;4r\x1b[4;1H");
        emulator
    }

    #[test]
    fn rows_off_the_whole_screen_are_kept_in_order() {
        let mut emulator = Emulator::new(4, 20, 100);
        emulator.process(lines(1..=6).as_bytes());
        assert_eq!(kept(&emulator), ["line-1", "line-2", "line-3"]);
        assert_eq!(visible(&emulator), ["line-4", "line-5", "line-6", ""]);
    }

    #[test]
    fn rows_off_a_region_at_the_top_are_kept_and_the_live_rows_stay() {
        let mut emulator = inline_app();
        for n in 1..=6 {
            emulator.process(format!("\r\nhistory-{n}").as_bytes());
        }
        emulator.process(b"\x1b[r");
        assert_eq!(kept(&emulator), ["", "", "", "", "history-1", "history-2"]);
        assert_eq!(
            visible(&emulator),
            [
                "history-3",
                "history-4",
                "history-5",
                "history-6",
                "live one",
                "live two"
            ]
        );
    }

    #[test]
    fn rows_off_a_region_below_the_top_are_lost_as_in_xterm() {
        let mut emulator = Emulator::new(6, 20, 100);
        emulator.process(b"header\x1b[2;4r\x1b[4;1H");
        emulator.process(lines(1..=5).as_bytes());
        assert!(kept(&emulator).is_empty(), "{:?}", kept(&emulator));
        assert_eq!(visible(&emulator)[0], "header");
    }

    #[test]
    fn scroll_up_keeps_as_many_rows_as_it_scrolls() {
        let mut emulator = inline_app();
        emulator.process(b"\x1b[1;1Ha\x1b[2;1Hb\x1b[3;1Hc\x1b[4;1Hd");
        emulator.process(b"\x1b[2S");
        assert_eq!(kept(&emulator), ["a", "b"]);
        emulator.process(b"\x1b[9S");
        assert_eq!(kept(&emulator), ["a", "b", "c", "d", "", "", "", ""]);
        assert_eq!(visible(&emulator)[4..], ["live one", "live two"]);
    }

    #[test]
    fn a_wrap_at_the_regions_bottom_keeps_the_top_row() {
        let mut emulator = inline_app();
        emulator.process(b"\x1b[1;1Hfirst\x1b[4;1H");
        emulator.process(b"abcdefghijklmnopqrstuvwxyz");
        assert_eq!(kept(&emulator), ["first"]);
        assert_eq!(visible(&emulator)[2..4], ["abcdefghijklmnopqrst", "uvwxyz"]);
        // A row that fills to the edge and stops there doesn't wrap.
        emulator.process(b"\r\x1b[K01234567890123456789");
        assert_eq!(kept(&emulator), ["first"]);
    }

    #[test]
    fn wide_characters_wrap_one_column_early() {
        let mut emulator = Emulator::new(2, 5, 100);
        emulator.process("\x1b[2;1Habcd界".as_bytes());
        assert_eq!(kept(&emulator), [""]);
        assert_eq!(visible(&emulator), ["abcd", "界"]);
    }

    #[test]
    fn the_alternate_screen_keeps_nothing() {
        let mut emulator = Emulator::new(4, 20, 100);
        emulator.process(b"\x1b[?1049h");
        emulator.process(lines(1..=8).as_bytes());
        emulator.process(b"\x1b[1;2r\x1b[2;1H\n\n\x1b[r\x1b[?1049l");
        assert!(kept(&emulator).is_empty(), "{:?}", kept(&emulator));
    }

    #[test]
    fn a_region_set_on_the_alternate_screen_leaves_the_main_one_alone() {
        let mut emulator = Emulator::new(4, 20, 100);
        emulator.process(b"\x1b[?1049h\x1b[2;3r\x1b[?1049l");
        emulator.process(lines(1..=5).as_bytes());
        assert_eq!(kept(&emulator), ["line-1", "line-2"]);
    }

    #[test]
    fn bytes_split_anywhere_read_the_same() {
        let mut stream = b"\x1b[5;1Hlive\x1b[1;4r\x1b[4;1H".to_vec();
        for n in 1..=9 {
            stream.extend_from_slice(format!("\r\nrow-{n} ünï 界").as_bytes());
        }
        stream.extend_from_slice(b"\x1b[3S\x1b[r");
        let mut whole = Emulator::new(6, 12, 100);
        whole.process(&stream);
        let mut bytewise = Emulator::new(6, 12, 100);
        for byte in &stream {
            bytewise.process(std::slice::from_ref(byte));
        }
        assert_eq!(kept(&whole), kept(&bytewise));
        assert_eq!(visible(&whole), visible(&bytewise));
        assert!(kept(&whole).contains(&"row-1 ünï 界"), "{:?}", kept(&whole));
    }

    fn shown(emulator: &Emulator) -> Vec<String> {
        let (_, cols) = emulator.shown().size();
        emulator
            .shown()
            .rows(0, cols)
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    #[test]
    fn a_synchronized_frame_shows_once_it_is_whole() {
        let mut emulator = Emulator::new(3, 20, 100);
        emulator.process(b"\x1b[?2026h\x1b[1;1Hfirst\x1b[?2026l");
        assert_eq!(shown(&emulator), ["first", "", ""]);
        emulator.process(b"\x1b[?2026h\x1b[1;1Hsecond\x1b[2;1Hhalf");
        assert_eq!(shown(&emulator), ["first", "", ""], "a frame half drawn");
        assert_eq!(visible(&emulator), ["second", "half", ""]);
        emulator.process(b" done\x1b[?2026l");
        assert_eq!(shown(&emulator), ["second", "half done", ""]);
    }

    #[test]
    fn rows_a_synchronized_frame_pushes_up_show_with_it() {
        let mut emulator = Emulator::new(2, 20, 100);
        emulator.process(b"\x1b[?2026h");
        emulator.process(lines(1..=4).as_bytes());
        assert!(kept(&emulator).is_empty(), "{:?}", kept(&emulator));
        emulator.process(b"\x1b[?2026l");
        assert_eq!(kept(&emulator), ["line-1", "line-2", "line-3"]);
    }

    #[test]
    fn a_frame_left_unfinished_shows_once_released_or_resized() {
        let mut emulator = Emulator::new(2, 20, 100);
        emulator.process(b"\x1b[?2026hlost");
        assert_eq!(shown(&emulator), ["", ""]);
        emulator.release();
        assert_eq!(shown(&emulator), ["lost", ""]);
        emulator.process(b"\x1b[?2026h\r\nmore");
        emulator.set_size(3, 20);
        assert_eq!(shown(&emulator), ["lost", "more", ""]);
    }

    #[test]
    fn the_scrollback_holds_its_limit() {
        let mut emulator = Emulator::new(2, 20, 3);
        emulator.process(lines(1..=10).as_bytes());
        assert_eq!(kept(&emulator), ["line-7", "line-8", "line-9"]);
    }

    #[test]
    fn a_full_reset_forgets_the_region_and_the_scrollback() {
        let mut emulator = inline_app();
        emulator.process(b"\r\nx\r\ny\r\nz\r\nw\r\nv");
        assert!(!kept(&emulator).is_empty());
        emulator.process(b"\x1bc");
        assert!(kept(&emulator).is_empty());
        emulator.process(lines(1..=8).as_bytes());
        assert_eq!(kept(&emulator), ["line-1", "line-2", "line-3"]);
    }

    #[test]
    fn a_resize_keeps_a_region_that_reached_the_bottom() {
        let mut emulator = Emulator::new(4, 20, 100);
        emulator.set_size(6, 20);
        emulator.process(lines(1..=7).as_bytes());
        assert_eq!(kept(&emulator), ["line-1", "line-2"]);
    }

    #[test]
    fn regions_read_like_vt100() {
        assert_eq!(region(1, 4, 10), (0, 3));
        assert_eq!(region(0, 0, 10), (0, 9));
        assert_eq!(region(3, 0, 10), (2, 9));
        assert_eq!(region(2, 40, 10), (1, 9));
        assert_eq!(region(5, 5, 10), (0, 9));
        assert_eq!(region(9, 3, 10), (0, 9));
    }
}
