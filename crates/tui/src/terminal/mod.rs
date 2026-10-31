//! The terminal laid out like Claude Code's: history fills down from the top row, the live
//! region stays pinned to the bottom rows, and only the live region redraws. Live rows that
//! continue the transcript, such as a streaming answer, can trail right under history instead.
//! History that reaches the live region scrolls up into the normal scrollback, and so does
//! whatever the shell showed before the start.
//!
//! The design follows Codex's `custom_terminal.rs`, `insert_history.rs`, and the draw path in
//! `tui.rs` (Apache-2.0), which derive from ratatui's `Terminal` (MIT). It keeps two buffers for
//! the viewport and writes only the cells that changed, and it prints history in the same
//! synchronized update as the frame, so the viewport never flickers. It writes to any `io::Write`,
//! so tests can point it at a `vt100::Parser`.

mod clipboard;
mod flush;
mod insert;
mod kept;
mod progress;
pub mod setup;
mod sgr;
pub mod title;

#[cfg(test)]
mod tests;

use std::io::{self, Write};
use std::ops::Range;

use crossterm::cursor::{Hide, MoveTo, RestorePosition, SavePosition, Show};
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal::{
    BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Position, Rect, Size};
use ratatui::text::Line;

pub use flush::flush_diff;
pub use insert::{Inserted, insert_lines, write_line};

use insert::{columns, physical_rows};
use kept::Kept;

#[derive(Debug)]
pub struct InlineTerminal<W: Write> {
    writer: W,
    /// Every row from the end of history to the bottom of the screen. The live region draws
    /// its pinned rows at the bottom and any trailing rows at the top.
    viewport: Rect,
    screen: Size,
    /// The row under the last history row on screen, where the next one prints.
    history_end: u16,
    /// The rows above `history_end` still show what the shell printed before the start. They
    /// go up into scrollback before anything else draws.
    shell_on_screen: bool,
    pending_history: Vec<Line<'static>>,
    /// Where `pending_history` turns the page: the history on screen goes up into scrollback
    /// before the rows from here print, at the top.
    page_break: Option<usize>,
    /// Which of `pending_history` to mark once they print.
    pending_mark: Option<Range<usize>>,
    /// History rows a later frame may print over while they still show, such as the header.
    marked: Option<Marked>,
    /// Rows to print over the marked ones on the next frame.
    pending_redraw: Option<Vec<Line<'static>>>,
    /// Rows a redraw had beyond the marked ones, printed under the history since, so a header
    /// that grew taller says its new rows once.
    spilled: Vec<Line<'static>>,
    /// Escapes to send at a cell of the marked rows on the next frame, such as a picture, by
    /// the row among them and the column.
    pending_on_marked: Option<(u16, u16, Vec<u8>)>,
    /// Escapes to send at a cell of a row of `pending_history` once it prints, such as a
    /// picture to draw under its words: the row, the column, the escapes.
    pending_on_rows: Vec<(usize, u16, Vec<u8>)>,
    /// The frame being drawn and the one on screen, both covering the viewport.
    buffers: [Buffer; 2],
    current: usize,
    /// Where the cursor rested after the last frame, if it still does.
    resting: Option<Position>,
    cursor_hidden: bool,
    /// The buffer of the frame on screen still holds what the viewport shows, as nothing has
    /// written over it since.
    viewport_known: bool,
    /// A row to clear from before the next frame, after a resize.
    clear_from: Option<u16>,
    /// The rows the live region drew last frame: its trailing rows and its pinned ones.
    live_rows: [Range<u16>; 2],
    title: Option<String>,
    /// Whether the progress bar last sent shows work, once one was sent.
    progress: Option<bool>,
    /// What each row of the alternate screen shows, or `None` where it isn't known, so an
    /// overlay frame rewrites only the rows that changed.
    screen_rows: Vec<Option<Line<'static>>>,
    /// What a view drew on the alternate screen last, so the next one writes only changed cells.
    screen_buffer: Option<Buffer>,
    /// One frame's bytes, written in a single piece so it reaches the terminal whole.
    scratch: Vec<u8>,
    /// Everything written between [`begin_frame`](Self::begin_frame) and
    /// [`end_frame`](Self::end_frame), sent as one synchronized update.
    batch: Vec<u8>,
    batching: bool,
    frame_bytes: u64,
    /// Pictures each screen gets before the first cells drawn on it, such as the logos.
    kept: Kept,
}

/// Marked history rows, counted up from the end of history: history stays one unbroken run of
/// rows above `history_end` as it scrolls, so this finds them wherever they moved.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Marked {
    rows: u16,
    /// History rows printed under them since.
    below: u16,
    /// How many columns each marked line and each line printed under them takes, so a
    /// terminal that rewraps them at another width can be followed. `None` once so many
    /// printed under them that they are far up in scrollback.
    widths: Option<Widths>,
    /// Found again after the terminal rewrapped them, which pictures may follow but text no
    /// longer prints over.
    rewrapped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Widths {
    marked: Vec<usize>,
    below: Vec<usize>,
}

/// The most lines under the marked ones whose widths are kept.
const WIDTHS_KEPT: usize = 4096;

/// The rows a line `columns` wide takes at `width`.
fn rows_at(columns: usize, width: u16) -> u16 {
    u16::try_from(columns.max(1).div_ceil(usize::from(width.max(1)))).unwrap_or(u16::MAX)
}

fn rows_of(widths: &[usize], width: u16) -> u16 {
    widths
        .iter()
        .map(|columns| rows_at(*columns, width))
        .fold(0, u16::saturating_add)
}

/// The columns `row` of `buffer` takes up to its last cell that isn't blank, as a terminal
/// rewrapping it would count them.
fn drawn_columns(buffer: &Buffer, row: u16) -> usize {
    let area = buffer.area;
    (area.left()..area.right())
        .rev()
        .find(|&x| buffer[(x, row)].symbol() != " ")
        .map_or(0, |x| usize::from(x - area.left()) + 1)
}

#[derive(Debug)]
pub struct Frame<'a> {
    /// The rows pinned to the bottom of the screen, counted from the screen's top left.
    pub area: Rect,
    /// The rows right under history, counted from the screen's top left. Empty unless the draw
    /// asked for some.
    pub trailing: Rect,
    pub buffer: &'a mut Buffer,
    /// Counted from the screen's top left.
    pub cursor: Option<Position>,
}

impl Frame<'_> {
    /// Paints the pinned rows. Each line's own style fills its whole row before its spans
    /// draw, so a styled blank line still paints.
    pub fn paint_lines(&mut self, lines: &[Line<'_>]) {
        paint_rows(self.buffer, self.area, lines);
    }

    /// Paints the rows that trail right under history.
    pub fn paint_trailing(&mut self, lines: &[Line<'_>]) {
        paint_rows(self.buffer, self.trailing, lines);
    }

    /// `col` and `row` are within the pinned rows.
    pub fn set_cursor(&mut self, col: u16, row: u16) {
        self.cursor = Some(Position::new(
            self.area.x.saturating_add(col),
            self.area.y.saturating_add(row),
        ));
    }
}

impl<W: Write> InlineTerminal<W> {
    /// `cursor` is where the shell left it. The rows above it, and its own row when it sits
    /// after text, scroll up into scrollback before the first frame, so the screen starts
    /// blank with nothing erased.
    pub fn new(writer: W, screen: Size, cursor: Position) -> Self {
        let viewport = Rect::new(0, screen.height, screen.width, 0);
        Self {
            writer,
            viewport,
            screen,
            history_end: row_under(cursor).min(screen.height),
            shell_on_screen: true,
            pending_history: Vec::new(),
            page_break: None,
            pending_mark: None,
            marked: None,
            pending_redraw: None,
            spilled: Vec::new(),
            pending_on_marked: None,
            pending_on_rows: Vec::new(),
            buffers: [Buffer::empty(viewport), Buffer::empty(viewport)],
            current: 0,
            resting: None,
            cursor_hidden: false,
            viewport_known: false,
            clear_from: None,
            live_rows: [0..0, 0..0],
            title: None,
            progress: None,
            screen_rows: Vec::new(),
            screen_buffer: None,
            scratch: Vec::new(),
            batch: Vec::new(),
            batching: false,
            frame_bytes: 0,
            kept: Kept::default(),
        }
    }

    pub fn screen_size(&self) -> Size {
        self.screen
    }

    /// Bytes frames have written so far, which tells whether a frame changed anything on screen.
    pub fn frame_bytes(&self) -> u64 {
        self.frame_bytes
    }

    /// Where the cursor rested after the last frame, for tests that act as the terminal.
    #[cfg(test)]
    pub(crate) fn resting(&self) -> Option<Position> {
        self.resting
    }

    pub fn viewport(&self) -> Rect {
        self.viewport
    }

    /// The row the next history row prints on.
    pub fn history_end(&self) -> u16 {
        self.history_end
    }

    pub fn writer(&self) -> &W {
        &self.writer
    }

    pub fn writer_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    pub fn pending_history(&self) -> &[Line<'static>] {
        &self.pending_history
    }

    /// The rows must already fit the screen width.
    pub fn insert_history(&mut self, lines: Vec<Line<'static>>) {
        self.pending_history.extend(lines);
    }

    /// Like [`insert_history`](Self::insert_history), and remembers where `marked` of the lines
    /// print, so [`redraw_marked`](Self::redraw_marked) can print over them later.
    pub fn insert_marked_history(&mut self, lines: Vec<Line<'static>>, marked: Range<usize>) {
        let offset = self.pending_history.len();
        self.pending_mark = Some(offset + marked.start..offset + marked.end);
        self.pending_history.extend(lines);
        self.spilled.clear();
    }

    /// Prints `lines` over the marked history rows on the next frame, while every one of them is
    /// still on screen. Rows already in scrollback can't change, so then nothing prints.
    pub fn redraw_marked(&mut self, lines: Vec<Line<'static>>) {
        self.pending_redraw = Some(lines);
    }

    /// Sends `escapes` with the cursor at `column` of the marked rows' `row`, on the next frame,
    /// while that row still shows, such as a picture to draw in the header's cells. Sent again
    /// before that frame, they add to what waits.
    pub fn send_on_marked(&mut self, row: u16, column: u16, escapes: Vec<u8>) {
        match &mut self.pending_on_marked {
            Some((at_row, at_column, waiting)) if (*at_row, *at_column) == (row, column) => {
                waiting.extend(escapes);
            }
            pending => *pending = Some((row, column, escapes)),
        }
    }

    /// Sends `escapes` with the cursor at `column` of row `row` of the history waiting to
    /// print, counting rows inserted after this call too, on the frame that prints it, while
    /// that row shows. The escapes leave the cursor where they found it.
    pub fn send_on_history_row(&mut self, row: usize, column: u16, escapes: Vec<u8>) {
        self.pending_on_rows.push((row, column, escapes));
    }

    /// Whether every marked row is still on screen, where a later frame can reach them.
    pub fn marked_on_screen(&self) -> bool {
        self.marked_top().is_some()
    }

    /// The screen row of the first marked row, while every one of them shows.
    fn marked_top(&self) -> Option<u16> {
        let marked = self.marked.as_ref()?;
        let top = i32::from(self.history_end) - i32::from(marked.below) - i32::from(marked.rows);
        u16::try_from(top).ok()
    }

    /// History inserted after this starts a fresh screen: what printed before goes up into
    /// scrollback first, the way the shell's rows do at the start.
    pub fn turn_page(&mut self) {
        self.page_break = Some(self.pending_history.len());
    }

    /// Draws one frame of `height` rows pinned to the bottom, in a synchronized update. A frame
    /// that changes nothing writes nothing.
    pub fn draw(&mut self, height: u16, paint: impl FnOnce(&mut Frame<'_>)) -> io::Result<()> {
        self.draw_live(0, height, paint)
    }

    /// Like [`draw`](Self::draw), with `trailing` more rows right under history, so a
    /// streaming answer reads on from what already printed. When the screen can't hold both,
    /// history scrolls up and they meet.
    pub fn draw_live(
        &mut self,
        trailing: u16,
        height: u16,
        paint: impl FnOnce(&mut Frame<'_>),
    ) -> io::Result<()> {
        if self.screen.width == 0 || self.screen.height == 0 {
            return Ok(());
        }
        let mut out = std::mem::take(&mut self.scratch);
        out.clear();
        // Everything goes to memory first, so nothing below can fail halfway through a frame.
        let mut result = self.draw_into(&mut out, trailing, height, paint);
        if result.is_ok() && !out.is_empty() {
            result = self.write_frame(&out);
            if result.is_err() {
                // Part of the frame may be on screen: repaint everything next time.
                self.invalidate();
            }
        }
        self.scratch = out;
        result
    }

    /// Writes cells, or escapes placed among them, after the kept pictures the screen shown
    /// lacks, so a placeholder never names a picture its screen doesn't hold.
    fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        let kept = self.kept.due();
        if kept.is_empty() && frame.is_empty() {
            return Ok(());
        }
        self.frame_bytes += (kept.len() + frame.len()) as u64;
        if self.batching {
            self.batch.extend_from_slice(kept);
            self.batch.extend_from_slice(frame);
            return Ok(());
        }
        if kept.is_empty() {
            return self.write_synchronized(frame);
        }
        let mut bytes = kept.to_vec();
        bytes.extend_from_slice(frame);
        self.write_synchronized(&bytes)
    }

    fn write_synchronized(&mut self, bytes: &[u8]) -> io::Result<()> {
        let written = self.write_whole(bytes);
        if written.is_err() {
            // The pictures in it may not have arrived.
            self.kept.let_go();
        }
        written
    }

    fn write_whole(&mut self, bytes: &[u8]) -> io::Result<()> {
        queue!(self.writer, BeginSynchronizedUpdate)?;
        self.writer.write_all(bytes)?;
        queue!(self.writer, EndSynchronizedUpdate)?;
        self.writer.flush()
    }

    /// Keeps the pictures `escapes` send, such as the numbered logos', for every screen: each
    /// gets them right before the first cells drawn on it from now on, and the alternate screen
    /// again after every entry, since entering it empties its store.
    pub fn keep_pictures(&mut self, escapes: &[u8]) {
        self.kept.add(escapes);
    }

    /// Keeps the logos' pictures `escapes` send, as [`Self::keep_pictures`] does, in place of
    /// any kept before, such as the ones drawn for the cell size before a font size change.
    pub fn keep_logos(&mut self, escapes: &[u8]) {
        self.kept.renew_logos(escapes);
    }

    /// Whether rows draw the kept pictures now. While none does, as with `ui.logos` off, no
    /// screen is sent them; once rows draw them again, each screen gets what it lacks.
    pub fn rows_draw_kept_pictures(&mut self, drawn: bool) {
        self.kept.used(drawn);
    }

    /// Sends now the kept pictures the screen shown lacks, as after a frame that drew a
    /// picture's placeholders for the first time.
    pub fn send_kept_pictures(&mut self) -> io::Result<()> {
        self.write_frame(&[])
    }

    /// The screens may have let go of the kept pictures, as after a program you ran drew on
    /// the terminal, so each gets them again before its next cells.
    pub fn forget_kept_pictures(&mut self) {
        self.kept.let_go();
    }

    /// Writes `bytes` on their own, or as part of the frame being built.
    fn emit(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.batching {
            self.batch.extend_from_slice(bytes);
            return Ok(());
        }
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    /// Until [`end_frame`](Self::end_frame), every write waits, so a frame that sets the title,
    /// switches screens, prints history, and redraws still reaches the terminal as one
    /// synchronized update.
    pub fn begin_frame(&mut self) {
        self.batch.clear();
        self.batching = true;
    }

    /// Sends what the frame wrote, if anything, in one synchronized update and one flush.
    pub fn end_frame(&mut self) -> io::Result<()> {
        if !std::mem::replace(&mut self.batching, false) || self.batch.is_empty() {
            return Ok(());
        }
        let batch = std::mem::take(&mut self.batch);
        let result = self.write_synchronized(&batch);
        self.batch = batch;
        if result.is_err() {
            // Part of the frame may be on screen: repaint everything next time.
            self.invalidate();
        }
        result
    }

    /// Rings the terminal bell.
    pub fn bell(&mut self) -> io::Result<()> {
        self.emit(b"\x07")
    }

    /// Puts `text` on the system clipboard, where the terminal allows OSC 52.
    pub fn set_clipboard(&mut self, text: &str) -> io::Result<()> {
        self.emit(clipboard::osc52(text).as_bytes())
    }

    fn draw_into(
        &mut self,
        out: &mut Vec<u8>,
        trailing: u16,
        height: u16,
        paint: impl FnOnce(&mut Frame<'_>),
    ) -> io::Result<()> {
        let screen = self.screen;
        self.scroll_shell_away(out)?;
        if let Some(row) = self.clear_from.take() {
            queue!(out, MoveTo(0, row), Clear(ClearType::FromCursorDown))?;
            self.invalidate();
        }

        let height = height.min(screen.height);
        let trailing = trailing.min(screen.height - height);
        let live = trailing + height;
        let mut pending = std::mem::take(&mut self.pending_history);
        let mut mark = self.pending_mark.take();
        let mut on_rows = std::mem::take(&mut self.pending_on_rows);
        let mut cleared = false;
        if let Some(at) = self.page_break.take() {
            let fresh = pending.split_off(at.min(pending.len()));
            // Rows before the break go up into scrollback, where they can't change.
            mark = mark
                .filter(|mark| mark.start >= at)
                .map(|mark| mark.start - at..mark.end - at);
            on_rows.retain(|(row, _, _)| *row >= at);
            for (row, _, _) in &mut on_rows {
                *row -= at;
            }
            self.insert_pending(out, &pending, live, None)?;
            self.scroll_history_away(out)?;
            cleared = true;
            pending = fresh;
        }
        let marked_rows = match &mark {
            Some(mark) => Some(mark.len()),
            None => self.marked.as_ref().map(|marked| usize::from(marked.rows)),
        };
        if let Some(rows) = marked_rows {
            self.spill_taller_redraw(&mut pending, rows);
        }
        cleared |= self.insert_pending(out, &pending, live, mark)?;
        if self.history_end > screen.height - live {
            // A taller live region pushes the history rows it would cover up into scrollback.
            scroll_screen_up(out, self.history_end - (screen.height - live), screen)?;
            self.history_end = screen.height - live;
            cleared = true;
        }
        self.send_on_printed_rows(out, &pending, on_rows)?;
        pending.clear();
        self.pending_history = pending;
        if let Some(lines) = self.pending_redraw.take() {
            self.print_over_marked(out, &lines)?;
        }
        if let Some((row, column, escapes)) = self.pending_on_marked.take()
            && let Some(top) = self.marked_top()
            && self.marked.as_ref().is_some_and(|marked| row < marked.rows)
        {
            queue!(out, MoveTo(column, top + row))?;
            out.extend_from_slice(&escapes);
            self.resting = None;
        }

        let area = Rect::new(
            0,
            self.history_end,
            screen.width,
            screen.height - self.history_end,
        );
        let old = self.viewport;
        if !cleared && area.y > old.y && area.bottom() == old.bottom() && area.width == old.width {
            // History printed over the region's top rows. The rows below are still on screen
            // as drawn, so only what changes there gets written.
            self.crop_viewport(area);
        } else if cleared || area != old {
            // Everything above the new top is history by now, and whatever a scroll left of the
            // old region sits below it.
            if area.y < screen.height {
                queue!(out, MoveTo(0, area.y), Clear(ClearType::FromCursorDown))?;
            }
            self.set_viewport(area);
            self.invalidate();
        }

        let pinned = Rect::new(0, screen.height - height, screen.width, height);
        let trailing = Rect::new(0, area.y, screen.width, trailing);
        self.erase_freed_rows(
            out,
            [
                trailing.top()..trailing.bottom(),
                pinned.top()..pinned.bottom(),
            ],
        )?;
        let (current, previous) = self.buffers_mut();
        current.resize(area);
        current.reset();
        let mut frame = Frame {
            area: pinned,
            trailing,
            buffer: &mut *current,
            cursor: None,
        };
        paint(&mut frame);
        let cursor = frame.cursor;

        let before = out.len();
        flush_diff(out, previous, current)?;
        if out.len() > before {
            self.resting = None;
        }
        if !self.cursor_hidden {
            queue!(out, Hide)?;
            self.cursor_hidden = true;
        }
        // The composer draws its own block cursor. The real one stays hidden, resting where
        // typing happens so input methods put their popups there. Without an input it rests on
        // the region's last row, so a resize can always tell how far the terminal moved it.
        let rest =
            cursor.or_else(|| (pinned.height > 0).then(|| Position::new(0, pinned.bottom() - 1)));
        if let Some(position) = rest
            && self.resting != Some(position)
        {
            queue!(out, MoveTo(position.x, position.y))?;
            self.resting = Some(position);
        }
        self.swap_buffers();
        self.viewport_known = true;
        Ok(())
    }

    /// `cursor` is where the terminal reports the cursor now, if it could be asked; after a
    /// reflow it tells where history moved. The live region goes back to the bottom rows, and
    /// printed history is left as it is.
    pub fn resize(&mut self, screen: Size, cursor: Option<Position>) -> io::Result<()> {
        let mut end = i32::from(self.history_end);
        // Codex's heuristic: terminals that rewrap scrollback, or push rows into it as the
        // screen shrinks, move the cursor with them, so history moved as far as the cursor did.
        if let (Some(reported), Some(resting)) = (cursor, self.resting)
            && reported.y != resting.y
        {
            end += i32::from(reported.y) - i32::from(resting.y);
        }
        if screen.width != self.screen.width {
            // The terminal may rewrap history: the marked rows are found again from where it
            // says the cursor went, or not at all.
            self.marked = cursor.and_then(|cursor| self.rewrapped_marked(screen, cursor, end));
        }
        self.screen = screen;
        let end = u16::try_from(end.clamp(0, i32::from(screen.height))).unwrap_or(0);
        self.history_end = end;
        self.set_viewport(Rect::new(0, end, screen.width, screen.height - end));
        // Whatever the terminal kept of the old live region sits under history. History taller
        // than the room left scrolls up on the next frame.
        self.clear_from = (end < screen.height).then_some(end);
        self.live_rows = Default::default();
        self.invalidate();
        // The cursor stays where the terminal just said it is, so the next resize of a burst,
        // before any frame, measures from there.
        self.resting = cursor;
        Ok(())
    }

    /// The marked rows after a terminal rewrapped every row at `screen`'s width and reported
    /// its cursor at `cursor`, with history to end at `end` from now on. Between them and the
    /// cursor sit the lines printed under them and the live rows the last frame drew above the
    /// cursor, each taking the rows it needs at the new width. `None` when that can't be told,
    /// or they went up into scrollback.
    fn rewrapped_marked(&self, screen: Size, cursor: Position, end: i32) -> Option<Marked> {
        let marked = self.marked.as_ref()?;
        let widths = marked.widths.as_ref()?;
        let resting = self.resting.filter(|_| self.viewport_known)?;
        let shown = &self.buffers[1 - self.current];
        let live = shown.area;
        if !(live.top()..live.bottom()).contains(&resting.y) {
            return None;
        }
        let width = screen.width;
        let live_above = (live.top()..resting.y)
            .map(|row| rows_at(drawn_columns(shown, row), width))
            .fold(0, u16::saturating_add);
        let before_cursor = i32::from(resting.x) / i32::from(width.max(1));
        let rows = rows_of(&widths.marked, width);
        let below = rows_of(&widths.below, width);
        let top = i32::from(cursor.y)
            - before_cursor
            - i32::from(live_above)
            - i32::from(below)
            - i32::from(rows);
        let end = end.clamp(0, i32::from(screen.height));
        // Counted from history's end from now on, so they move with what prints after.
        let below = u16::try_from(end - top - i32::from(rows)).ok()?;
        (top >= 0).then(|| Marked {
            rows,
            below,
            widths: Some(widths.clone()),
            rewrapped: true,
        })
    }

    /// Call after something else wrote to the screen.
    pub fn invalidate(&mut self) {
        // A reset cell would still equal a blank one, so mark every cell as changed.
        let (_, previous) = self.buffers_mut();
        mark_all_changed(previous);
        self.viewport_known = false;
        self.resting = None;
        self.cursor_hidden = false;
        self.screen_rows.clear();
        self.screen_buffer = None;
    }

    /// Clears the live region and leaves the cursor under the last history row, so the shell
    /// prompt comes back right there.
    pub fn clear_live(&mut self) -> io::Result<()> {
        // When nothing drew yet, what the shell printed stays where it is.
        self.shell_on_screen = false;
        let screen = self.screen;
        let mut out = Vec::new();
        if screen.height > 0 && self.history_end >= screen.height {
            // History reaches the last row: one more scroll makes a blank row for the prompt.
            scroll_screen_up(&mut out, 1, screen)?;
            self.history_end = screen.height - 1;
        }
        if self.history_end < screen.height {
            queue!(
                out,
                MoveTo(0, self.history_end),
                Clear(ClearType::FromCursorDown)
            )?;
        }
        queue!(out, Show)?;
        self.emit(&out)?;
        self.set_viewport(Rect::new(0, screen.height, screen.width, 0));
        self.invalidate();
        Ok(())
    }

    /// The title last set, before sanitizing.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn set_title(&mut self, title: &str) -> io::Result<()> {
        if self.title.as_deref() == Some(title) {
            return Ok(());
        }
        let mut out = Vec::new();
        queue!(out, title::SetTitle(title::sanitize_title(title)))?;
        self.emit(&out)?;
        self.title = Some(title.to_owned());
        Ok(())
    }

    /// Shows the terminal's progress bar while `working`, and takes it off after. Only a
    /// change is written, and nothing until work first shows. True when it wrote.
    pub fn set_progress(&mut self, working: bool) -> io::Result<bool> {
        if self.progress.unwrap_or(false) == working {
            return Ok(false);
        }
        let mut out = Vec::new();
        queue!(out, progress::SetProgress { working })?;
        self.emit(&out)?;
        self.progress = Some(working);
        Ok(true)
    }

    /// History goes on under the cursor's row, after a child program printed to the screen.
    pub fn set_origin(&mut self, cursor: Position) {
        let end = row_under(cursor).min(self.screen.height);
        self.history_end = end;
        self.clear_from = (end < self.screen.height).then_some(end);
        // What the program printed sits between history and the marked rows.
        self.marked = None;
        self.invalidate();
    }

    /// History and the viewport stay untouched on the main screen.
    pub fn enter_alternate_screen(&mut self) -> io::Result<()> {
        let mut out = Vec::new();
        // The main screen starts clean even when a view opens before the first frame.
        self.scroll_shell_away(&mut out)?;
        // Alternate scroll mode turns the mouse wheel into arrow keys without capturing the
        // mouse, so selecting text still works.
        queue!(out, EnterAlternateScreen, Print("\x1b[?1007h"), Hide)?;
        self.emit(&out)?;
        setup::note_alternate_screen(true);
        self.kept.entered_alternate();
        // Some terminals keep what the alternate screen showed last time, so every row repaints.
        self.screen_rows.clear();
        self.screen_buffer = None;
        Ok(())
    }

    /// The next draw repaints the viewport.
    pub fn leave_alternate_screen(&mut self) -> io::Result<()> {
        let mut out = Vec::new();
        queue!(out, Print("\x1b[?1007l"), LeaveAlternateScreen)?;
        self.emit(&out)?;
        setup::note_alternate_screen(false);
        self.kept.left_alternate();
        self.invalidate();
        Ok(())
    }

    /// The whole app on the alternate screen, with the wheel reported, for `ui.screen =
    /// "fullscreen"`. Nothing is written to the main screen, so leaving shows it as it was.
    pub fn enter_fullscreen(&mut self) -> io::Result<()> {
        let mut out = Vec::new();
        queue!(out, EnterAlternateScreen, Print(setup::MOUSE_ON), Hide)?;
        self.emit(&out)?;
        setup::note_alternate_screen(true);
        self.kept.entered_alternate();
        // The main screen is never drawn on, so there is no shell output to move out of the way.
        self.shell_on_screen = false;
        self.cursor_hidden = true;
        self.resting = None;
        self.screen_rows.clear();
        self.screen_buffer = None;
        Ok(())
    }

    /// Back to the main screen as the shell left it, with the cursor showing.
    pub fn leave_fullscreen(&mut self) -> io::Result<()> {
        let mut out = Vec::new();
        queue!(out, Print(setup::MOUSE_OFF), LeaveAlternateScreen, Show)?;
        self.emit(&out)?;
        setup::note_alternate_screen(false);
        self.kept.left_alternate();
        self.cursor_hidden = false;
        self.resting = None;
        self.screen_rows.clear();
        self.screen_buffer = None;
        Ok(())
    }

    /// Prints `lines` where the cursor is, each on its own row, such as the one line a
    /// fullscreen exit leaves on the main screen.
    pub fn print_lines(&mut self, lines: &[Line<'_>]) -> io::Result<()> {
        let mut out = Vec::new();
        for line in lines {
            write_line(&mut out, line, self.screen.width)?;
            queue!(out, Print("\r\n"))?;
        }
        self.emit(&out)
    }

    /// Sends `escapes` on the alternate screen, from the cell `at` when given, such as a picture
    /// over the header's cells, then puts the cursor back where it rests.
    pub fn send_escapes(&mut self, at: Option<Position>, escapes: &[u8]) -> io::Result<()> {
        if escapes.is_empty() {
            return Ok(());
        }
        let mut out = Vec::new();
        if let Some(at) = at {
            queue!(out, MoveTo(at.x, at.y))?;
        }
        out.extend_from_slice(escapes);
        match self.resting {
            Some(rest) if at.is_some() => queue!(out, MoveTo(rest.x, rest.y))?,
            _ => {}
        }
        self.write_frame(&out)
    }

    /// The next alternate-screen draw writes every cell of `area` again, whatever it held, such
    /// as where a picture drawn into the cells was.
    pub fn redraw_cells(&mut self, area: Rect) {
        let Some(buffer) = self.screen_buffer.as_mut() else {
            return;
        };
        let area = area.intersection(buffer.area);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buffer[(x, y)].set_diff_option(CellDiffOption::AlwaysUpdate);
            }
        }
    }

    /// What the alternate screen shows, as the last [`draw_buffer`](Self::draw_buffer) left it.
    pub fn screen_buffer(&self) -> Option<&Buffer> {
        self.screen_buffer.as_ref()
    }

    /// Paints the whole alternate screen, writing only the rows that changed since the last
    /// call. Rows past `lines` are cleared.
    pub fn draw_screen(&mut self, lines: &[Line<'static>]) -> io::Result<()> {
        let mut out = std::mem::take(&mut self.scratch);
        out.clear();
        let mut result = self.screen_into(&mut out, lines);
        if !out.is_empty() {
            self.resting = None;
        }
        if result.is_ok() && !out.is_empty() {
            result = self.write_frame(&out);
            if result.is_err() {
                self.screen_rows.clear();
            }
        }
        self.scratch = out;
        result
    }

    /// Paints the whole alternate screen from a view's buffer, writing only the cells that
    /// changed since the last one.
    pub fn draw_buffer(&mut self, next: &Buffer) -> io::Result<()> {
        self.draw_buffer_resting(next, None)
    }

    /// Like [`draw_buffer`](Self::draw_buffer), then rests the hidden cursor at `rest`, where
    /// typing happens, so input methods put their popups there.
    pub fn draw_buffer_resting(&mut self, next: &Buffer, rest: Option<Position>) -> io::Result<()> {
        let area = Rect::new(0, 0, self.screen.width, self.screen.height);
        if area.is_empty() {
            return Ok(());
        }
        let previous = match self.screen_buffer.take() {
            Some(previous) if previous.area == area => previous,
            _ => {
                let mut blank = Buffer::empty(area);
                mark_all_changed(&mut blank);
                blank
            }
        };
        let mut next = next.clone();
        if next.area != area {
            next.resize(area);
        }
        let mut out = std::mem::take(&mut self.scratch);
        out.clear();
        let mut result = flush_diff(&mut out, &previous, &next);
        // Rows written from lines no longer describe the screen.
        self.screen_rows.clear();
        if !out.is_empty() {
            self.resting = None;
        }
        if let Some(position) = rest.filter(|position| self.resting != Some(*position))
            && result.is_ok()
        {
            result = queue!(out, MoveTo(position.x, position.y));
            self.resting = Some(position);
        }
        if result.is_ok() && !out.is_empty() {
            result = self.write_frame(&out);
        }
        if result.is_err() {
            self.resting = None;
        }
        self.screen_buffer = result.is_ok().then_some(next);
        self.scratch = out;
        result
    }

    fn screen_into(&mut self, out: &mut Vec<u8>, lines: &[Line<'static>]) -> io::Result<()> {
        self.screen_buffer = None;
        let width = self.screen.width;
        let blank = Line::default();
        self.screen_rows
            .resize(usize::from(self.screen.height), None);
        for (row, shown) in (0u16..).zip(self.screen_rows.iter_mut()) {
            let line = lines.get(usize::from(row)).unwrap_or(&blank);
            if shown.as_ref() == Some(line) {
                continue;
            }
            queue!(out, MoveTo(0, row))?;
            write_line(out, line, width)?;
            *shown = Some(line.clone());
        }
        Ok(())
    }

    /// Prints `lines` under the history on screen, marking the `mark` of them. True when that
    /// erased the live region.
    fn insert_pending(
        &mut self,
        out: &mut Vec<u8>,
        lines: &[Line<'static>],
        live: u16,
        mark: Option<Range<usize>>,
    ) -> io::Result<bool> {
        if lines.is_empty() {
            return Ok(false);
        }
        let width = self.screen.width;
        let rows = |lines: &[Line<'static>]| {
            lines
                .iter()
                .map(|line| physical_rows(line, width))
                .fold(0u16, u16::saturating_add)
        };
        let widths_of =
            |lines: &[Line<'static>]| -> Vec<usize> { lines.iter().map(columns).collect() };
        match mark.filter(|mark| mark.start < mark.end && mark.end <= lines.len()) {
            Some(mark) => {
                self.marked = Some(Marked {
                    rows: rows(&lines[mark.clone()]),
                    below: rows(&lines[mark.end..]),
                    widths: Some(Widths {
                        marked: widths_of(&lines[mark.clone()]),
                        below: widths_of(&lines[mark.end..]),
                    }),
                    rewrapped: false,
                });
            }
            None => {
                if let Some(marked) = &mut self.marked {
                    marked.below = marked.below.saturating_add(rows(lines));
                    if let Some(widths) = &mut marked.widths {
                        widths.below.extend(widths_of(lines));
                        if widths.below.len() > WIDTHS_KEPT {
                            marked.widths = None;
                        }
                    }
                }
            }
        }
        let inserted = insert_lines(out, lines, self.history_end, live, self.screen)?;
        self.history_end = inserted.end;
        self.resting = None;
        Ok(inserted.cleared)
    }

    /// Sends each of `on_rows` at its row of `printed`, the history rows that just printed and
    /// end right above `history_end`, where that row still shows.
    fn send_on_printed_rows(
        &mut self,
        out: &mut Vec<u8>,
        printed: &[Line<'static>],
        on_rows: Vec<(usize, u16, Vec<u8>)>,
    ) -> io::Result<()> {
        let width = self.screen.width;
        for (row, column, escapes) in on_rows {
            let below = printed
                .get(row..)
                .unwrap_or_default()
                .iter()
                .map(|line| physical_rows(line, width))
                .fold(0u16, u16::saturating_add);
            if let Some(at) = self.history_end.checked_sub(below)
                && row < printed.len()
            {
                queue!(out, MoveTo(column, at))?;
                out.extend_from_slice(&escapes);
                self.resting = None;
            }
        }
        Ok(())
    }

    /// A redraw with more rows than the `rows` marked, such as a header that names a CLI not
    /// ready once the checks answer, can't push the history under the marked rows down, so its
    /// rows past them print as history now. Each prints once, however often the header is
    /// drawn again.
    fn spill_taller_redraw(&mut self, pending: &mut Vec<Line<'static>>, rows: usize) {
        let Some(lines) = self.pending_redraw.as_mut() else {
            return;
        };
        if lines.len() <= rows {
            return;
        }
        for line in lines.split_off(rows) {
            if !self.spilled.contains(&line) {
                pending.push(line.clone());
                self.spilled.push(line);
            }
        }
    }

    /// Writes `lines` over the marked rows, one row each, padded with blank rows or cut to
    /// their number, so nothing under them moves. Nothing prints once any of them has gone
    /// up into scrollback, or when a line would wrap.
    fn print_over_marked(&mut self, out: &mut Vec<u8>, lines: &[Line<'static>]) -> io::Result<()> {
        let (Some(marked), Some(top)) = (self.marked.as_ref(), self.marked_top()) else {
            return Ok(());
        };
        if marked.rewrapped {
            return Ok(());
        }
        let marked_rows = marked.rows;
        let width = self.screen.width;
        if lines.iter().any(|line| physical_rows(line, width) > 1) {
            return Ok(());
        }
        let blank = Line::default();
        let rows = lines.iter().chain(std::iter::repeat(&blank));
        queue!(out, SavePosition)?;
        for (row, line) in (top..top + marked_rows).zip(rows) {
            queue!(out, MoveTo(0, row))?;
            write_line(out, line, width)?;
        }
        queue!(out, RestorePosition)?;
        self.resting = None;
        Ok(())
    }

    fn scroll_shell_away(&mut self, out: &mut Vec<u8>) -> io::Result<()> {
        if !std::mem::take(&mut self.shell_on_screen) {
            return Ok(());
        }
        self.scroll_history_away(out)
    }

    /// Line feeds at the bottom row carry every row above `history_end` up into scrollback, the
    /// one scroll every terminal keeps, and leave the screen blank. `ESC[2J` would copy the
    /// screen there twice in some terminals, and `ESC[3J` would erase the scrollback.
    fn scroll_history_away(&mut self, out: &mut Vec<u8>) -> io::Result<()> {
        if self.screen.height == 0 {
            return Ok(());
        }
        scroll_screen_up(out, self.history_end, self.screen)?;
        queue!(out, MoveTo(0, 0), Clear(ClearType::FromCursorDown))?;
        self.history_end = 0;
        self.marked = None;
        self.invalidate();
        Ok(())
    }

    /// Erases each row the live region drew last frame and leaves blank now, such as the
    /// menu's rows once it closes, and forgets what they held. The diff alone would leave a
    /// row it believes is blank already, so whatever the terminal still shows there, such as
    /// an old input rule, would stay.
    fn erase_freed_rows(&mut self, out: &mut Vec<u8>, used: [Range<u16>; 2]) -> io::Result<()> {
        let before = std::mem::replace(&mut self.live_rows, used.clone());
        let area = self.viewport;
        let mut freed = before
            .into_iter()
            .flatten()
            .filter(|row| area.top() <= *row && *row < area.bottom())
            .filter(|row| !used.iter().any(|range| range.contains(row)))
            .peekable();
        if freed.peek().is_none() {
            return Ok(());
        }
        let (_, previous) = self.buffers_mut();
        for row in freed {
            queue!(out, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
            for x in area.left()..area.right() {
                previous[(x, row)].reset();
            }
        }
        self.resting = None;
        Ok(())
    }

    /// Shrinks the viewport from the top, keeping what both buffers hold for the rows left.
    fn crop_viewport(&mut self, area: Rect) {
        for buffer in &mut self.buffers {
            let mut cropped = Buffer::empty(area);
            for y in area.top()..area.bottom() {
                for x in area.left()..area.right() {
                    cropped[(x, y)] = buffer[(x, y)].clone();
                }
            }
            *buffer = cropped;
        }
        self.viewport = area;
    }

    fn set_viewport(&mut self, area: Rect) {
        self.viewport = area;
        for buffer in &mut self.buffers {
            buffer.resize(area);
        }
    }

    fn buffers_mut(&mut self) -> (&mut Buffer, &mut Buffer) {
        let [first, second] = &mut self.buffers;
        if self.current == 0 {
            (first, second)
        } else {
            (second, first)
        }
    }

    fn swap_buffers(&mut self) {
        self.current = 1 - self.current;
    }
}

/// Each line's own style fills its whole row before its spans draw, so a styled blank line
/// still paints.
pub(crate) fn paint_rows(buffer: &mut Buffer, area: Rect, lines: &[Line<'_>]) {
    for (row, line) in (area.top()..area.bottom()).zip(lines) {
        // ratatui skips empty lines entirely, so the fill must come from the row itself.
        buffer.set_style(Rect::new(area.x, row, area.width, 1), line.style);
        buffer.set_line(area.x, row, line, area.width);
    }
}

/// The first row under what the cursor follows: its own row when it starts the row, else the
/// next.
fn row_under(cursor: Position) -> u16 {
    if cursor.x > 0 {
        cursor.y.saturating_add(1)
    } else {
        cursor.y
    }
}

fn mark_all_changed(buffer: &mut Buffer) {
    buffer.reset();
    for cell in &mut buffer.content {
        cell.set_diff_option(CellDiffOption::AlwaysUpdate);
    }
}

/// Like Codex's `grow_viewport`, it scrolls with line feeds, since some terminals drop rows
/// scrolled out with `CSI S`, and through the whole screen, since some drop rows scrolled out of
/// a region. The live region's rows scroll too, so they are cleared and drawn again right after.
fn scroll_screen_up<W: Write>(out: &mut W, rows: u16, screen: Size) -> io::Result<()> {
    if rows == 0 {
        return Ok(());
    }
    queue!(out, MoveTo(0, screen.height.saturating_sub(1)))?;
    for _ in 0..rows {
        queue!(out, Print("\r\n"))?;
    }
    Ok(())
}
