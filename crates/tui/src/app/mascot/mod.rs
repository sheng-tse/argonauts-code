//! The header's mascot: drawn as a picture where the terminal draws one, and an ultra look's
//! frames swapped while the header still shows, block cells or picture alike. On the inline
//! screen, once the header goes up into scrollback nothing is drawn again, and the last frame
//! stays. The full screen draws its history again each frame, so it places the picture where
//! the header's cells are as it draws, and takes it off while they are out of view.

use std::io::{self, Write};
use std::time::Instant;

use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

use super::Ui;
use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::mascot::{Graphics, Look, Mascot, ROWS, Sent, WIDTH};
use crate::base::theme::{Brightness, MascotDrawing, Theme};
use crate::cells::cell::{HistoryCell, MARK_COL, MARK_MIN_WIDTH, SessionHeader};

/// The kitty protocol's first picture id: high, so it stays clear of ids other programs in the
/// same terminal pick from 1.
const FIRST_ID: u32 = 0x4d41_0000;
/// The mascot's first row among the header's rows, under the blank one on top.
const FIRST_ROW: u16 = 1;

#[derive(Debug)]
pub(super) struct HeaderMascot {
    /// How the terminal takes a picture, when it takes one.
    graphics: Option<Graphics>,
    /// Pixels a cell, as the terminal reports them.
    cell: Option<(u16, u16)>,
    /// The picture on screen.
    sent: Option<Sent>,
    /// The header's mascot and look, and when they printed, which an ultra look's frames count
    /// from.
    shown: Option<(Mascot, Look, Instant)>,
    /// The frame the block cells show.
    frame: usize,
    next_id: u32,
    /// Where the full screen shows the picture, and which of its pictures.
    placed: Option<(Position, u32)>,
    /// The full screen was given away, as to a program you ran, and the terminal may have let
    /// its pictures go, so they go out again on its next frame.
    resend: bool,
    /// What the full screen sends with its next frame before it places the picture, such as
    /// the frames to keep.
    waiting: Vec<u8>,
    /// Inline, after a resize: the picture goes in the header's cells again on the next frame,
    /// drawn anew when the cell size changed.
    again: Option<Again>,
    /// Reads the cell size the terminal reports now.
    measure: fn() -> Option<(u16, u16)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Again {
    /// The same pictures, placed again.
    Place,
    /// New pictures, for a new cell size.
    Redraw,
}

impl Default for HeaderMascot {
    fn default() -> Self {
        Self {
            graphics: None,
            cell: None,
            sent: None,
            shown: None,
            frame: 0,
            next_id: FIRST_ID,
            placed: None,
            resend: false,
            waiting: Vec::new(),
            again: None,
            // The crate's own tests never read the terminal they run in.
            measure: if cfg!(test) {
                || None
            } else {
                crate::terminal::setup::cell_size
            },
        }
    }
}

impl HeaderMascot {
    /// Whether the mascot goes out as a picture with `theme`'s settings.
    fn pictures(&self, theme: &Theme) -> bool {
        theme.mascot == MascotDrawing::Auto && self.graphics.is_some() && self.cell.is_some()
    }
}

/// What the full screen sends after it draws, so the picture shows where the header is: the
/// cell it starts from, if it shows, and the escapes.
pub(super) type Placing = Option<(Option<Position>, Vec<u8>)>;

impl<W: Write> Ui<W> {
    /// The terminal's cell in pixels, where it said, which pictures sent now are drawn for.
    pub(super) fn picture_cell(&self) -> Option<(u16, u16)> {
        self.mascot.cell
    }

    /// The terminal takes pictures through `graphics`, at `cell` pixels a cell, so the header's
    /// mascot is drawn as one where `ui.mascot` lets it.
    pub fn use_pictures(&mut self, graphics: Option<Graphics>, cell: Option<(u16, u16)>) {
        self.mascot.graphics = graphics;
        self.mascot.cell = cell.filter(|(width, height)| *width > 0 && *height > 0);
        self.fit_mascot_to_theme();
    }

    /// After any resize, even one that keeps the columns and rows, as a font size change can:
    /// the cell size the terminal reports now, which pictures sent from now on take (one it
    /// can't tell leaves the last), and the picture taken off and placed again where the
    /// header's cells are now, drawn anew for a new cell size. The full screen places it as it
    /// draws the next frame; inline, it goes back while the header's rows still show.
    pub(super) fn measure_cells(&mut self) {
        if self.mascot.graphics.is_none() {
            return;
        }
        let measured = (self.mascot.measure)().filter(|(width, height)| *width > 0 && *height > 0);
        let redraw = measured.is_some_and(|cell| Some(cell) != self.mascot.cell);
        if let Some(cell) = measured {
            self.mascot.cell = Some(cell);
            self.fit_mascot_to_theme();
        }
        if self.scrollback.is_none() {
            // iTerm2 draws its picture into the cells, which the terminal moves with the text.
            let kitty = self
                .mascot
                .sent
                .as_ref()
                .is_some_and(|sent| sent.graphics == Graphics::Kitty);
            self.mascot.again = kitty.then_some(if redraw { Again::Redraw } else { Again::Place });
            return;
        }
        // The terminal may still show it where the header's cells were.
        if self.mascot.placed.take().is_some()
            && let Some(sent) = &self.mascot.sent
        {
            let hidden = sent.hide();
            self.mascot.waiting.extend(hidden);
        }
        if redraw && self.mascot.sent.is_some() {
            self.forget_mascot();
            self.mascot.resend = self.mascot.shown.is_some();
        }
    }

    /// Lets a test say what cell size the terminal reports.
    #[cfg(test)]
    pub(super) fn measure_cells_with(&mut self, measure: fn() -> Option<(u16, u16)>) {
        self.mascot.measure = measure;
    }

    /// Inline, on the first frame after a resize: the picture in the header's cells again,
    /// while its rows show. True when something went out.
    fn place_mascot_again(&mut self, now: Instant) -> bool {
        let Some(again) = self.mascot.again.take() else {
            return false;
        };
        if !self.terminal.marked_on_screen() || !self.theme.mascot_pictures {
            return false;
        }
        let column = u16::try_from(MARK_COL).unwrap_or(0);
        if again == Again::Redraw {
            let (mascot, look, since) = match self.mascot.shown {
                Some(shown) => shown,
                None => return false,
            };
            self.show_mascot(now, true);
            // Its frames still count from when the header printed.
            self.mascot.shown = Some((mascot, look, since));
            return true;
        }
        let frame = self.mascot_frame(now).map_or(0, |(_, frame, _)| frame);
        let Some(sent) = self.mascot.sent.as_mut() else {
            return false;
        };
        let row = first_row(sent.look);
        let Some(escapes) = sent.place(frame) else {
            return false;
        };
        self.terminal.send_on_marked(row, column, escapes);
        true
    }

    /// The look of the picture on screen, which says the rows it takes: the normal one while
    /// none shows.
    pub(super) fn picture_look(&self) -> Look {
        self.mascot
            .sent
            .as_ref()
            .map_or(Look::Normal, |sent| sent.look)
    }

    /// How the terminal takes pictures and the pixels a cell, when it takes them.
    pub(super) fn picture_terminal(&self) -> Option<(Graphics, (u16, u16))> {
        self.mascot.graphics.zip(self.mascot.cell)
    }

    /// Tells the header whether a picture takes its mascot's cells, from `ui.mascot` and the
    /// terminal.
    pub(super) fn fit_mascot_to_theme(&mut self) {
        self.theme.mascot_pictures = self.mascot.pictures(&self.theme);
    }

    /// The header printed: its mascot goes in as a picture where it is one. Inline, the one
    /// before went up into scrollback with its header and stays there.
    pub(super) fn header_printed(&mut self, now: Instant) {
        if self.scrollback.is_none() {
            self.mascot.sent = None;
        }
        self.show_mascot(now, true);
    }

    /// The header changed where it shows: a new mascot or look replaces the picture.
    pub(super) fn header_changed(&mut self, now: Instant) {
        self.show_mascot(now, false);
    }

    fn show_mascot(&mut self, now: Instant, printed: bool) {
        let (mascot, look) = match self.model.history.header() {
            Some(header) => header.mascot(self.theme.brightness == Brightness::Light),
            // Before the header prints, the greeting's argonaut takes its place.
            None if self.model.history.greets_with_a_picture() => {
                (Mascot::Argonaut, Look::new(None, false))
            }
            None => return,
        };
        let same = self
            .mascot
            .shown
            .is_some_and(|(was, was_look, _)| (was, was_look) == (mascot, look));
        if !printed && same {
            return;
        }
        self.mascot.shown = Some((mascot, look, now));
        self.mascot.frame = 0;
        if !self.theme.mascot_pictures {
            return;
        }
        // A header too narrow for its mascot starts its words where the picture would go.
        if !self.mascot_has_room() {
            self.forget_mascot();
            return;
        }
        let (Some(graphics), Some(cell)) = (self.mascot.graphics, self.mascot.cell) else {
            return;
        };
        let before = self.mascot.sent.take();
        let look_before = before.as_ref().map_or(look, |sent| sent.look);
        let mut escapes = before.map(|sent| sent.forget()).unwrap_or_default();
        if self.scrollback.is_some() {
            // The full screen places it as it draws, wherever the header is by then.
            if let Some((at, _)) = self.mascot.placed.take()
                && graphics == Graphics::Iterm
            {
                self.terminal.redraw_cells(mascot_area(at, look_before));
            }
            if let Some((sent, stored)) =
                Sent::stored(graphics, mascot, look, cell, self.mascot.next_id)
            {
                self.mascot.next_id = sent.next_id();
                escapes.extend(stored);
                self.mascot.sent = Some(sent);
            }
            self.mascot.waiting.extend(escapes);
            return;
        }
        if let Some((sent, first)) = Sent::first(graphics, mascot, look, cell, self.mascot.next_id)
        {
            self.mascot.next_id = sent.next_id();
            escapes.extend(first);
            self.mascot.sent = Some(sent);
        }
        let column = u16::try_from(MARK_COL).unwrap_or(0);
        self.terminal
            .send_on_marked(first_row(look), column, escapes);
    }

    /// Whether the screen is wide enough for the header to draw its mascot's cells.
    fn mascot_has_room(&self) -> bool {
        usize::from(self.terminal.screen_size().width) >= MARK_MIN_WIDTH
    }

    /// Takes the picture off the screen and out of the terminal: the full screen with its
    /// next frame, the inline screen now.
    fn forget_mascot(&mut self) {
        let Some(sent) = self.mascot.sent.take() else {
            return;
        };
        let escapes = sent.forget();
        if self.scrollback.is_some() {
            if let Some((at, _)) = self.mascot.placed.take()
                && sent.graphics == Graphics::Iterm
            {
                self.terminal.redraw_cells(mascot_area(at, sent.look));
            }
            self.mascot.waiting.extend(escapes);
        } else {
            // Nothing to fail on that a later frame could put right.
            let _ = self.terminal.send_escapes(None, &escapes);
        }
    }

    /// After a resize, a header too narrow for its mascot loses the picture, and on the full
    /// screen, where the header is drawn again at the new width, one wide enough gets it back.
    pub(super) fn fit_mascot_to_width(&mut self) {
        if !self.theme.mascot_pictures {
            return;
        }
        if !self.mascot_has_room() {
            self.forget_mascot();
        } else if self.scrollback.is_some()
            && self.mascot.sent.is_none()
            && self.mascot.shown.is_some()
        {
            self.mascot.resend = true;
        }
    }

    /// The frame an ultra look shows now, while its header is on screen and things may move.
    fn mascot_frame(&self, now: Instant) -> Option<(Mascot, usize, Instant)> {
        let (mascot, look, since) = self.mascot.shown?;
        if !look.is_ultra() || self.theme.reduced_motion || !self.header_shows() {
            return None;
        }
        let clock = Clock::new(since, now);
        let frame = clock.frame(mascot.frame_time(), mascot.frames());
        Some((mascot, frame, clock.next_frame_at(mascot.frame_time())))
    }

    /// When the next frame of the header's mascot is due.
    pub(super) fn next_mascot_frame(&self, now: Instant) -> Option<Instant> {
        self.mascot_frame(now).map(|(_, _, next)| next)
    }

    /// Whether the mascot's rows are on screen: every one of the marked rows inline, and on
    /// the full screen in the conversation the last frame showed.
    fn header_shows(&self) -> bool {
        match &self.scrollback {
            Some(scrollback) => scrollback
                .header_rows_at(usize::from(FIRST_ROW), ROWS)
                .is_some(),
            None => self.terminal.marked_on_screen(),
        }
    }

    /// On the full screen, the block cells' light moves on while the header shows, in the rows
    /// the next frame draws. A picture's frame goes in as it is placed.
    pub(super) fn animate_full_screen_mascot(&mut self, now: Instant) {
        if self.mascot.sent.is_some() {
            return;
        }
        let Some((_, frame, _)) = self.mascot_frame(now) else {
            return;
        };
        if frame == self.mascot.frame {
            return;
        }
        self.mascot.frame = frame;
        if let Some(lines) = self.header_lines(now)
            && let Some(scrollback) = self.scrollback.as_mut()
        {
            scrollback.redraw_header(lines);
        }
    }

    /// On the full screen, what puts the picture where the header's cells show at `at`, with
    /// the frame due now, or takes it off while they don't: nothing when it already shows so.
    /// iTerm2 draws a picture into the cells, so where it was is written again by the frame
    /// about to draw, which this has to come before.
    pub(super) fn place_mascot(&mut self, now: Instant, at: Option<Position>) -> Placing {
        let frame = self.mascot_frame(now).map_or(0, |(_, frame, _)| frame);
        let mut escapes = std::mem::take(&mut self.mascot.waiting);
        let Some(sent) = self.mascot.sent.as_mut() else {
            return (!escapes.is_empty()).then_some((None, escapes));
        };
        let frame = if sent.look.is_ultra() { frame } else { 0 };
        // Keyed by the picture due, not the clock's frame: frames that look alike place nothing
        // again, so iTerm2 gets at most its 18 a cycle and kitty's protocol keeps a still part.
        let wanted = at.map(|at| (at, sent.due_picture(frame)));
        if wanted == self.mascot.placed {
            return (!escapes.is_empty()).then_some((None, escapes));
        }
        let was = self.mascot.placed.take();
        escapes.extend(match (sent.graphics, wanted) {
            (_, Some(_)) => sent.place(frame).unwrap_or_default(),
            (Graphics::Kitty, None) => sent.hide(),
            (Graphics::Iterm, None) => Vec::new(),
        });
        if sent.graphics == Graphics::Iterm
            && let Some((before, _)) = was
            && Some(before) != at
        {
            self.terminal.redraw_cells(mascot_area(before, sent.look));
        }
        self.mascot.placed = wanted;
        Some((at, escapes))
    }

    /// Sends what [`Self::place_mascot`] made, after the frame drew.
    pub(super) fn send_placing(&mut self, placing: Placing) -> io::Result<()> {
        match placing {
            Some((at, escapes)) => self.terminal.send_escapes(at, &escapes),
            None => Ok(()),
        }
    }

    /// The full screen comes back after it was given away: its picture goes out again.
    pub(super) fn mascot_given_back(&mut self) {
        self.mascot.placed = None;
        self.mascot.resend = self.mascot.sent.is_some();
    }

    /// On the full screen's first frame after it came back, the picture goes out again.
    pub(super) fn resend_mascot(&mut self, now: Instant) {
        if std::mem::take(&mut self.mascot.resend) {
            self.show_mascot(now, true);
        }
    }

    /// Swaps in the frame due now, as a picture or as block cells, or after a resize puts the
    /// picture in the header's cells again. True when something went out.
    pub(super) fn animate_mascot(&mut self, now: Instant) -> bool {
        if self.place_mascot_again(now) {
            return true;
        }
        let Some((_, frame, _)) = self.mascot_frame(now) else {
            return false;
        };
        if let Some(sent) = &mut self.mascot.sent {
            let Some(escapes) = sent.show(frame) else {
                return false;
            };
            let column = u16::try_from(MARK_COL).unwrap_or(0);
            let row = first_row(sent.look);
            self.terminal.send_on_marked(row, column, escapes);
            return true;
        }
        if frame == self.mascot.frame {
            return false;
        }
        self.mascot.frame = frame;
        let Some(lines) = self.header_lines(now) else {
            return false;
        };
        self.terminal.redraw_marked(lines);
        true
    }

    /// The header's rows with the frame due now.
    fn header_lines(&self, now: Instant) -> Option<Vec<Line<'static>>> {
        let header: &SessionHeader = self.model.history.header()?;
        let (_, _, since) = self.mascot.shown?;
        let screen = self.terminal.screen_size();
        let ctx = RenderCtx::new(screen.width, &self.theme, Clock::new(since, now))
            .with_rows(screen.height);
        Some(header.lines(&ctx))
    }
}

/// The cell a picture of `look` starts from on the full screen, while every row it takes
/// shows in the conversation the last frame drew.
pub(super) fn mascot_cell(
    scrollback: &super::fullscreen::Scrollback,
    look: Look,
) -> Option<Position> {
    let first = usize::from(first_row(look));
    let row = scrollback.header_rows_at(first, look.picture_rows())?;
    Some(Position::new(u16::try_from(MARK_COL).ok()?, row))
}

/// The header row a picture of `look` starts in: the mascot's first, or for an ultra look the
/// blank one above, which its mark rises into.
fn first_row(look: Look) -> u16 {
    let above = u16::try_from(look.rows_above()).unwrap_or(0);
    FIRST_ROW.saturating_sub(above)
}

/// The cells a picture of `look` takes with its first cell at `at`.
fn mascot_area(at: Position, look: Look) -> Rect {
    let size = |cells: usize| u16::try_from(cells).unwrap_or(u16::MAX);
    Rect::new(at.x, at.y, size(WIDTH), size(look.picture_rows()))
}

#[cfg(test)]
mod tests;
