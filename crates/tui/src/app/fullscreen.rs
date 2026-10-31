//! The conversation with the whole alternate screen, for `ui.screen = "fullscreen"`. What the
//! inline screen prints into the terminal's scrollback stays in [`Scrollback`] instead, and the
//! conversation scrolls inside the app with the wheel, PgUp, and PgDn. New output follows the
//! bottom unless you scrolled up.
//!
//! The app takes the mouse as SGR reports, with motion while a button is down. Alternate
//! scroll mode would leave the mouse alone, but it turns the wheel into ↑ and ↓, which in the
//! conversation walk the prompt history. So the app selects text itself, as [`Selecting`] does, and
//! the terminal's own selection still works with its bypass key held, such as Shift.

use std::io::{self, Write};
use std::ops::Range;
use std::time::Instant;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

use super::selection::{Point, Selecting};
use super::sticky;
use super::{Ui, UiAction, compose_docked, compose_live};
use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::text::{hard_wrap_spans, sanitize, truncate_line};
use crate::base::theme::Role;
use crate::frame::state::AppState;
use crate::terminal::paint_rows;
use crate::terminal::setup;

/// Rows one notch of the wheel moves, as most terminals scroll their own scrollback.
pub const WHEEL_ROWS: usize = 3;

#[derive(Debug, Default)]
pub struct Scrollback {
    /// Every row printed so far, at the width of the frame that printed it.
    printed: Vec<Line<'static>>,
    /// Where the header's rows are in `printed`, so a changed header takes their place.
    header: Option<Range<usize>>,
    /// `printed` cut to `width`, the way a terminal wraps rows wider than its screen.
    wrapped: Vec<Line<'static>>,
    width: u16,
    /// How many rows of `printed` are in `wrapped`.
    wrapped_from: usize,
    /// Rows scrolled up from the end. Zero follows new output.
    offset: usize,
    /// The rows the last frame had, so rows that arrive below while you read move the offset
    /// by as many and the view stays still.
    last_total: usize,
    /// Rows arrived below since you scrolled up.
    unseen: bool,
    /// As of the last frame.
    page: usize,
    max_offset: usize,
    /// Text selected with the mouse, on whatever the screen shows.
    selecting: Selecting,
    /// While the button is down on a view that follows new output, the row the view began at,
    /// so the text under a drag holds still while output streams in.
    held: Option<usize>,
    /// The row the last frame's view began at.
    start: usize,
    /// Where each of your prompts starts in `printed`, with its words.
    prompts: Vec<(usize, String)>,
    /// Where each row of `printed` starts in `wrapped`.
    wrapped_at: Vec<usize>,
    /// Where the prompt the last frame kept at the top starts in `wrapped`.
    sticky: Option<usize>,
    /// The row of `wrapped` the next frame starts at, asked for by a jump to a prompt.
    jump_to: Option<usize>,
    /// `ui.autoscroll` is off: scrolling back down to the bottom leaves the view there rather
    /// than following new output, which only sending brings back, as in Claude Code.
    pub holds_still: bool,
    /// Scrolled back down with `holds_still`: at the bottom, but new output leaves the rows
    /// in view where they are.
    resting: bool,
    /// The rows of `wrapped` and the live rows after them the last frame showed, and how many
    /// rows it put above them, where a kept prompt sits.
    shown: Range<usize>,
    kept_above: usize,
}

impl Scrollback {
    pub fn new() -> Self {
        Self::default()
    }

    /// `header` is where the header sits in `rows`, if they hold it.
    pub fn push(&mut self, rows: Vec<Line<'static>>, header: Option<Range<usize>>) {
        if let Some(header) = header {
            let at = self.printed.len();
            self.header = Some(at + header.start..at + header.end);
        }
        self.printed.extend(rows);
    }

    /// Where the rows the next [`push`](Self::push) brings hold your prompts, with their
    /// words, as the history's drain found them.
    pub fn mark_prompts(&mut self, prompts: Vec<(Range<usize>, String)>) {
        let at = self.printed.len();
        self.prompts.extend(
            prompts
                .into_iter()
                .filter_map(|(rows, text)| Some((at + rows.start, sticky::words(&text)?))),
        );
    }

    /// The screen row the header's row `row` showed at in the last frame, while it and the
    /// `rows` rows under it all showed, each whole on a row of its own.
    pub fn header_rows_at(&self, row: usize, rows: usize) -> Option<u16> {
        let header = self.header.as_ref()?;
        let first = header.start + row;
        let last = first + rows.checked_sub(1)?;
        if last >= header.end || self.wrapped_from <= last {
            return None;
        }
        let top = *self.wrapped_at.get(first)?;
        let below = self
            .wrapped_at
            .get(last + 1)
            .copied()
            .unwrap_or(self.wrapped.len());
        let whole = below - top == rows;
        let shows = self.shown.start <= top && top + rows <= self.shown.end;
        (whole && shows)
            .then(|| u16::try_from(top - self.shown.start + self.kept_above).ok())
            .flatten()
    }

    /// How many rows have printed so far, where the next [`push`](Self::push) starts.
    pub fn printed_len(&self) -> usize {
        self.printed.len()
    }

    /// Where printed rows `first..first + rows` showed in the last frame: the screen row the
    /// first sat on, above the screen once it scrolled past, and the screen rows of them in
    /// view. `None` while none shows or any of them wraps onto more rows than one.
    pub fn printed_rows_at(&self, first: usize, rows: usize) -> Option<(i32, Range<u16>)> {
        let last = first + rows.checked_sub(1)?;
        if self.wrapped_from <= last {
            return None;
        }
        let top = *self.wrapped_at.get(first)?;
        let below = self
            .wrapped_at
            .get(last + 1)
            .copied()
            .unwrap_or(self.wrapped.len());
        if below - top != rows {
            return None;
        }
        let (start, end) = (top.max(self.shown.start), below.min(self.shown.end));
        if start >= end {
            return None;
        }
        let on_screen = |row: usize| u16::try_from(row - self.shown.start + self.kept_above).ok();
        let first_at =
            i32::try_from(top + self.kept_above).ok()? - i32::try_from(self.shown.start).ok()?;
        Some((first_at, on_screen(start)?..on_screen(end)?))
    }

    /// The header printed last now reads as `lines`, rows that only change how they look, as
    /// an ultra look's moving light does: they take the old ones' place where they show,
    /// without wrapping the rest again.
    pub fn redraw_header(&mut self, lines: Vec<Line<'static>>) {
        let Some(range) = self.header.clone() else {
            return;
        };
        let wrapped_alone = range.end <= self.wrapped_from
            && range.clone().all(|row| {
                let at = self.wrapped_at[row];
                let next = self.wrapped_at.get(row + 1).copied();
                next.unwrap_or(self.wrapped.len()) == at + 1
            });
        let fits = lines.len() == range.len()
            && lines
                .iter()
                .all(|line| line.width() <= usize::from(self.width));
        if !(wrapped_alone && fits) {
            self.replace_header(lines);
            return;
        }
        for (row, line) in range.zip(lines) {
            self.wrapped[self.wrapped_at[row]] = line.clone();
            self.printed[row] = line;
        }
    }

    /// The header printed last now reads as `lines`.
    pub fn replace_header(&mut self, lines: Vec<Line<'static>>) {
        let Some(range) = self.header.clone() else {
            return;
        };
        let start = range.start;
        let count = lines.len();
        let (end, grown) = (range.end, count as isize - range.len() as isize);
        self.printed.splice(range, lines);
        self.header = Some(start..start + count);
        for (row, _) in self.prompts.iter_mut().filter(|(row, _)| *row >= end) {
            *row = row.saturating_add_signed(grown);
        }
        self.wrapped.clear();
        self.wrapped_from = 0;
    }

    /// Ctrl+↑, or a click on the prompt kept at the top: the next frame starts where that
    /// prompt does. False when no prompt is kept there.
    pub fn jump_to_prompt(&mut self) -> bool {
        self.jump_to = self.sticky;
        self.jump_to.is_some()
    }

    /// Whether the screen's `row` holds the prompt kept at the top.
    pub fn holds_prompt(&self, row: u16) -> bool {
        row == 0 && self.sticky.is_some()
    }

    /// The last of your prompts that starts above `row` of `wrapped`, out of sight, so its
    /// reply is what shows from there: where it starts, and its words.
    fn answered_at(&self, row: usize) -> Option<(usize, &str)> {
        self.prompts
            .iter()
            .filter_map(|(at, words)| Some((*self.wrapped_at.get(*at)?, words.as_str())))
            .take_while(|(at, _)| *at < row)
            .last()
    }

    /// Zero while following new output.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Moves the view `rows` up, or down when negative. True when it moved.
    pub fn scroll(&mut self, rows: isize) -> bool {
        let target = self.offset.saturating_add_signed(rows).min(self.max_offset);
        if target == self.offset {
            return false;
        }
        self.offset = target;
        if target == 0 {
            self.unseen = false;
            self.resting = self.holds_still;
        }
        true
    }

    /// Back to the bottom, following new output again. True when it moved.
    pub fn follow(&mut self) -> bool {
        let moved = self.offset > 0;
        self.offset = 0;
        self.unseen = false;
        self.resting = false;
        moved
    }

    /// The mouse button went down: a view following new output holds still under it.
    pub fn hold(&mut self) {
        if self.offset == 0 {
            self.held = Some(self.start);
        }
    }

    /// The button came up: the view follows new output again. True when it was held.
    pub fn let_go(&mut self) -> bool {
        self.held.take().is_some()
    }

    /// Rows PgUp and PgDn move, keeping a row of what was in view.
    pub fn page(&self) -> usize {
        self.page.max(1)
    }

    /// The `rows` rows of the conversation in view: the printed history, then `trailing`, the
    /// live rows that continue it. Content shorter than the screen starts at the top, under
    /// the header. A view scrolled up ends with a row saying what is below it.
    pub fn window(
        &mut self,
        trailing: &[Line<'static>],
        rows: usize,
        ctx: &RenderCtx<'_>,
    ) -> Vec<Line<'static>> {
        self.rewrap(ctx.width);
        let total = self.wrapped.len() + trailing.len();
        // Scrolled up, the last rows say what is below, under a blank row when there's room,
        // so the history above them can scroll as far as the top.
        let foot = if rows >= 4 { 2 } else { 1 };
        if total > self.last_total {
            if self.offset > 0 {
                self.offset += total - self.last_total;
                self.unseen = true;
            } else if self.resting && self.held.is_none() && rows >= 2 {
                // The rows in view stay put, as they would scrolled up.
                let end = self.start + (rows - foot);
                self.offset = total.saturating_sub(end);
                self.unseen = self.offset > 0;
                self.resting = self.offset == 0;
            }
        }
        self.last_total = total;
        self.max_offset = if total <= rows || rows < 2 {
            0
        } else {
            total - (rows - foot)
        };
        self.offset = self.offset.min(self.max_offset);
        if let Some(row) = self.jump_to.take() {
            self.offset = total
                .saturating_sub(row + (rows - foot))
                .min(self.max_offset);
        }
        if self.offset == 0 {
            self.unseen = false;
        }
        self.page = rows.saturating_sub(2);

        let shown = if self.offset > 0 { rows - foot } else { rows };
        let end = total - self.offset;
        let start = match self.held {
            Some(held) if self.offset == 0 => held.min(end.saturating_sub(shown)),
            _ => end.saturating_sub(shown),
        };
        let end = end.min(start + shown);
        self.start = start;
        let mut lines: Vec<Line<'static>> = self
            .wrapped
            .iter()
            .chain(trailing)
            .skip(start)
            .take(end - start)
            .cloned()
            .collect();
        // Scrolled up past a prompt, the prompt being answered sits above the view, which
        // keeps its top row and gives up its last ones.
        let answered = (self.offset > 0 && lines.len() > sticky::ROWS)
            .then(|| self.answered_at(start))
            .flatten()
            .map(|(at, words)| (at, words.to_owned()));
        self.sticky = answered.as_ref().map(|(at, _)| *at);
        self.shown = start..start + lines.len();
        self.kept_above = 0;
        if let Some((_, words)) = &answered {
            lines.truncate(lines.len() - sticky::ROWS);
            lines.splice(0..0, [sticky::prompt_row(words, ctx), sticky::rule(ctx)]);
            self.shown.end -= sticky::ROWS;
            self.kept_above = sticky::ROWS;
        }
        if self.offset > 0 {
            if foot == 2 {
                lines.push(Line::default());
            }
            // While a prompt is kept at the top, the last row says how to get back to it,
            // unless new output arrived below, which matters more.
            lines.push(match answered {
                Some(_) if !self.unseen => sticky::hint(ctx),
                _ => self.below(ctx),
            });
        }
        lines
    }

    fn rewrap(&mut self, width: u16) {
        if width != self.width {
            self.width = width;
            self.wrapped.clear();
            self.wrapped_from = 0;
        }
        self.wrapped_at.truncate(self.wrapped_from);
        for line in &self.printed[self.wrapped_from..] {
            self.wrapped_at.push(self.wrapped.len());
            wrap_into(&mut self.wrapped, line, width);
        }
        self.wrapped_from = self.printed.len();
    }

    fn below(&self, ctx: &RenderCtx<'_>) -> Line<'static> {
        let what = if self.unseen {
            "New output below".to_owned()
        } else {
            let unit = if self.offset == 1 { "row" } else { "rows" };
            format!("{} {unit} below", self.offset)
        };
        // The same down arrow token counts use, with its ASCII form.
        let arrow = ctx.theme.glyphs.tokens;
        let gap = crate::base::theme::glyphs::gap_after(arrow);
        let line = Line::from(Span::styled(
            format!("  {arrow}{gap}{what} · pgdn to scroll"),
            ctx.theme.style(Role::Dim),
        ));
        truncate_line(&line, usize::from(ctx.width))
    }
}

/// Ctrl+Shift+C, which terminals report with the letter in either case, or Cmd+C.
fn is_copy_key(key: &KeyEvent) -> bool {
    let c = matches!(key.code, KeyCode::Char('c' | 'C'));
    let ctrl_shift = key
        .modifiers
        .contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    let command = key.modifiers == KeyModifiers::SUPER;
    c && (ctrl_shift || command)
}

/// A row wider than the screen, after the screen got narrower, goes on in the next rows.
fn wrap_into(out: &mut Vec<Line<'static>>, line: &Line<'static>, width: u16) {
    if line.width() <= usize::from(width) {
        out.push(line.clone());
        return;
    }
    for spans in hard_wrap_spans(&line.spans, usize::from(width)) {
        let mut row = Line::from(spans).style(line.style);
        row.alignment = line.alignment;
        out.push(row);
    }
}

impl<W: Write> Ui<W> {
    /// From the next frame the app has the whole alternate screen, and the conversation
    /// scrolls inside it.
    pub fn use_fullscreen(&mut self) {
        if self.scrollback.is_none() {
            self.scrollback = Some(Scrollback::new());
        }
        // The full screen draws its history again each frame, so its mascot is block cells.
        self.fit_mascot_to_theme();
        self.dirty = true;
    }

    pub fn is_fullscreen(&self) -> bool {
        self.scrollback.is_some()
    }

    /// How far the conversation is scrolled up, or `None` on the inline screen.
    pub fn scroll_offset(&self) -> Option<usize> {
        self.scrollback.as_ref().map(Scrollback::offset)
    }

    /// The conversation from the top, scrolled where you left it, and the live region pinned
    /// to the bottom rows, laid out as the inline screen lays them.
    pub(super) fn draw_fullscreen(
        &mut self,
        now: Instant,
        state: Option<&AppState>,
    ) -> io::Result<()> {
        self.resend_mascot(now);
        self.animate_full_screen_mascot(now);
        let dock = self.dock_lines(now, state);
        let screen = self.terminal.screen_size();
        let ctx = RenderCtx::new(screen.width, &self.theme, Clock::new(self.started, now))
            .with_rows(screen.height);
        let blocks = self.model.history.live_blocks(&ctx);
        // Two rows stay above the live region, so some of the conversation always shows.
        let max_height = screen.height.saturating_sub(2);
        let view = match dock {
            Some(dock) => compose_docked(
                &blocks,
                &self.model.history,
                &self.model.bottom,
                &ctx,
                max_height,
                dock,
            ),
            None => compose_live(
                &blocks,
                &self.model.history,
                &self.model.bottom,
                &ctx,
                max_height,
            ),
        };
        let (trailing, pinned) = view.lines.split_at(view.trailing.min(view.lines.len()));
        let pinned_rows = u16::try_from(pinned.len())
            .unwrap_or(u16::MAX)
            .min(screen.height);
        let top = screen.height - pinned_rows;
        let Some(scrollback) = self.scrollback.as_mut() else {
            return Ok(());
        };
        scrollback.holds_still = !self.model.screen.autoscroll;
        let conversation = scrollback.window(trailing, usize::from(top), &ctx);
        let mascot_at = super::mascot::mascot_cell(scrollback);

        let mut buffer = Buffer::empty(Rect::new(0, 0, screen.width, screen.height));
        paint_rows(
            &mut buffer,
            Rect::new(0, 0, screen.width, top),
            &conversation,
        );
        paint_rows(
            &mut buffer,
            Rect::new(0, top, screen.width, pinned_rows),
            pinned,
        );
        scrollback
            .selecting
            .paint(&mut buffer, self.theme.style(Role::Selection));
        let trailing_rows = u16::try_from(trailing.len()).unwrap_or(u16::MAX);
        let cursor = view.cursor.and_then(|(column, row)| {
            let row = top.checked_add(row.checked_sub(trailing_rows)?)?;
            (row < screen.height).then_some(Position::new(column, row))
        });
        let placing = self.place_mascot(now, mascot_at);
        let map = self.place_map(now);
        self.terminal.draw_buffer_resting(&buffer, cursor)?;
        self.send_placing(placing)?;
        self.send_map_placing(map)?;
        self.place_council_pictures()
    }

    /// PgUp and PgDn scroll the conversation. False on the inline screen, where the terminal
    /// scrolls its own scrollback.
    pub(super) fn scroll_key(&mut self, key: &KeyEvent) -> bool {
        let Some(scrollback) = self.scrollback.as_mut() else {
            return false;
        };
        // Ctrl+↑ goes to where the prompt kept at the top starts, and does nothing else then.
        if key.code == KeyCode::Up && key.modifiers == KeyModifiers::CONTROL {
            let jumped =
                self.views.is_empty() && self.overlay.is_none() && scrollback.jump_to_prompt();
            self.dirty |= jumped;
            return jumped;
        }
        if !key.modifiers.is_empty() {
            return false;
        }
        let page = isize::try_from(scrollback.page()).unwrap_or(isize::MAX);
        let moved = match key.code {
            KeyCode::PageUp => scrollback.scroll(page),
            KeyCode::PageDown => scrollback.scroll(-page),
            _ => return false,
        };
        self.dirty |= moved;
        true
    }

    /// With text selected, Esc lets go of it and does nothing else. Ctrl+Shift+C, or Cmd+C
    /// where the terminal passes it on, copies it and keeps it, as Claude Code's do. Any other
    /// key, Ctrl+C too, lets go of it on its way, so Ctrl+C still stops a command or interrupts
    /// work: the text was copied when the button came up, unless `ui.copy_on_select` is off.
    /// `None` when the key goes on.
    pub(super) fn selection_key(&mut self, event: &Event) -> Option<Vec<UiAction>> {
        let Event::Key(key) = event else {
            return None;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return None;
        }
        let scrollback = self.scrollback.as_mut()?;
        // A release the terminal never reported can't keep the view from following.
        self.dirty |= scrollback.let_go();
        if is_copy_key(key)
            && let Some(text) = scrollback.selecting.text()
        {
            return Some(vec![UiAction::Copy(text.to_owned())]);
        }
        if !scrollback.selecting.clear() {
            return None;
        }
        self.dirty = true;
        (key.code == KeyCode::Esc).then(Vec::new)
    }

    /// A resize moves everything on screen, so a selection lets go.
    pub(super) fn clear_selection(&mut self) {
        if let Some(scrollback) = self.scrollback.as_mut() {
            scrollback.selecting.clear();
            scrollback.let_go();
        }
    }

    /// Shows the selection on a frame about to be drawn.
    pub(super) fn paint_selection(&mut self, frame: &mut Buffer) {
        if let Some(scrollback) = self.scrollback.as_mut() {
            let style = self.theme.style(Role::Selection);
            scrollback.selecting.paint(frame, style);
        }
    }

    /// What you send shows at once, so the view goes back to the bottom.
    pub(super) fn follow_conversation(&mut self) {
        if self.scrollback.as_mut().is_some_and(Scrollback::follow) {
            self.dirty = true;
        }
    }

    /// The wheel scrolls what is on top: a view or the transcript as its ↑ and ↓ would, else
    /// the conversation. The left button selects text on any of them and copies it on release.
    pub(super) fn mouse_event(
        &mut self,
        mouse: MouseEvent,
        now: Instant,
        state: &AppState,
    ) -> Vec<UiAction> {
        let Some(scrollback) = self.scrollback.as_mut() else {
            return Vec::new();
        };
        let at = Point::new(mouse.column, mouse.row);
        // A click on the prompt kept at the top goes to where it starts.
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && self.views.is_empty()
            && self.overlay.is_none()
            && scrollback.holds_prompt(mouse.row)
        {
            self.dirty |= scrollback.jump_to_prompt();
            return Vec::new();
        }
        // Selections are made on what the screen shows, so nothing selects before a frame.
        let screen = self.terminal.screen_buffer();
        let selecting = &mut scrollback.selecting;
        let up = match mouse.kind {
            MouseEventKind::ScrollUp => true,
            MouseEventKind::ScrollDown => false,
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(screen) = screen {
                    self.dirty |= selecting.press(at, now, screen);
                    scrollback.hold();
                }
                return Vec::new();
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(screen) = screen {
                    self.dirty |= selecting.drag(at, screen);
                }
                return Vec::new();
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let copied = screen.and_then(|screen| selecting.release(at, screen));
                self.dirty |= scrollback.let_go();
                // Without copy on select the text waits for Ctrl+Shift+C.
                let copied = copied.filter(|_| self.model.screen.copy_on_select);
                return copied.map(UiAction::Copy).into_iter().collect();
            }
            _ => return Vec::new(),
        };
        if !self.views.is_empty() || self.overlay.is_some() {
            let code = if up { KeyCode::Up } else { KeyCode::Down };
            let key = Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
            let mut actions = Vec::new();
            for _ in 0..WHEEL_ROWS {
                actions.extend(self.handle_event_in(key.clone(), now, state));
            }
            return actions;
        }
        let rows = isize::try_from(WHEEL_ROWS).unwrap_or(1);
        if scrollback.scroll(if up { rows } else { -rows }) {
            self.dirty = true;
        }
        Vec::new()
    }

    /// The main screen comes back as the shell left it, with only `exit_lines` under it.
    pub(super) fn finish_fullscreen(&mut self, exit_lines: Vec<String>) -> io::Result<()> {
        self.overlay = None;
        self.views.clear();
        // After a panic the hook already left, and leaving twice would move the cursor back
        // over what it printed.
        if self.on_alternate && !setup::given_back_after_panic() {
            self.terminal.leave_fullscreen()?;
        }
        self.on_alternate = false;
        let dim = self.theme.style(Role::Dim);
        let lines: Vec<Line<'static>> = exit_lines
            .iter()
            .map(|line| Line::from(Span::styled(sanitize(line).into_owned(), dim)))
            .collect();
        self.terminal.print_lines(&lines)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::base::draw::render::Clock;
    use crate::base::text::plain_text;
    use crate::base::theme::{Glyphs, Theme};

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn rows(from: usize, to: usize) -> Vec<Line<'static>> {
        (from..to).map(|i| Line::from(format!("row {i}"))).collect()
    }

    #[test]
    fn short_content_starts_at_the_top_and_long_content_ends_at_the_bottom() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let mut scrollback = Scrollback::new();
        scrollback.push(rows(0, 3), None);
        let shown = scrollback.window(&[], 5, &ctx);
        assert_eq!(plain_text(&shown), "row 0\nrow 1\nrow 2");

        scrollback.push(rows(3, 8), None);
        let trailing = [Line::from("live")];
        let shown = scrollback.window(&trailing, 5, &ctx);
        assert_eq!(plain_text(&shown), "row 4\nrow 5\nrow 6\nrow 7\nlive");
    }

    #[test]
    fn scrolled_up_the_view_stays_still_while_output_arrives() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let mut scrollback = Scrollback::new();
        scrollback.push(rows(0, 20), None);
        scrollback.window(&[], 5, &ctx);
        assert!(scrollback.scroll(3));
        let shown = plain_text(&scrollback.window(&[], 5, &ctx));
        assert_eq!(
            shown,
            "row 14\nrow 15\nrow 16\n\n  ↓  3 rows below · pgdn to scroll"
        );

        scrollback.push(rows(20, 24), None);
        let shown = plain_text(&scrollback.window(&[], 5, &ctx));
        assert!(shown.starts_with("row 14\nrow 15\nrow 16\n\n"), "{shown}");
        assert!(
            shown.ends_with("New output below · pgdn to scroll"),
            "{shown}"
        );

        assert!(scrollback.scroll(-100));
        assert_eq!(scrollback.offset(), 0);
        let shown = plain_text(&scrollback.window(&[], 5, &ctx));
        assert!(shown.ends_with("row 23"), "{shown}");
    }

    /// With `ui.autoscroll` off, scrolling back down leaves the view at the bottom without
    /// following, and only sending follows new output again.
    #[test]
    fn without_autoscroll_the_bottom_holds_still_until_you_send() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let mut scrollback = Scrollback::new();
        scrollback.holds_still = true;
        scrollback.push(rows(0, 20), None);
        scrollback.window(&[], 5, &ctx);
        scrollback.push(rows(20, 22), None);
        let shown = plain_text(&scrollback.window(&[], 5, &ctx));
        assert!(
            shown.ends_with("row 21"),
            "it follows until you scroll: {shown}"
        );

        assert!(scrollback.scroll(3));
        scrollback.window(&[], 5, &ctx);
        assert!(scrollback.scroll(-3));
        let shown = plain_text(&scrollback.window(&[], 5, &ctx));
        assert!(shown.ends_with("row 21"), "{shown}");
        scrollback.push(rows(22, 26), None);
        let shown = plain_text(&scrollback.window(&[], 5, &ctx));
        assert!(shown.starts_with("row 17\nrow 18\nrow 19\n\n"), "{shown}");
        assert!(
            shown.ends_with("New output below · pgdn to scroll"),
            "{shown}"
        );

        assert!(scrollback.follow());
        scrollback.window(&[], 5, &ctx);
        scrollback.push(rows(26, 28), None);
        let shown = plain_text(&scrollback.window(&[], 5, &ctx));
        assert!(shown.ends_with("row 27"), "sending follows again: {shown}");
    }

    #[test]
    fn the_top_is_as_far_as_it_scrolls() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let mut scrollback = Scrollback::new();
        scrollback.push(rows(0, 10), None);
        scrollback.window(&[], 6, &ctx);
        assert!(scrollback.scroll(isize::MAX));
        let shown = plain_text(&scrollback.window(&[], 6, &ctx));
        assert!(
            shown.starts_with("row 0\nrow 1\nrow 2\nrow 3\n\n"),
            "{shown}"
        );
        assert!(shown.ends_with("6 rows below · pgdn to scroll"), "{shown}");
        assert!(!scrollback.scroll(1));

        let mut short = Scrollback::new();
        short.push(rows(0, 2), None);
        short.window(&[], 4, &ctx);
        assert!(!short.scroll(3), "nothing to scroll");
    }

    #[test]
    fn a_changed_header_takes_the_old_ones_place() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let mut scrollback = Scrollback::new();
        let mut first = vec![Line::default(), Line::from("old header")];
        first.extend(rows(0, 1));
        scrollback.push(first, Some(0..2));
        scrollback.push(rows(1, 2), None);
        scrollback.replace_header(vec![
            Line::default(),
            Line::from("new header"),
            Line::from("second line"),
        ]);
        let shown = plain_text(&scrollback.window(&[], 10, &ctx));
        assert_eq!(shown, "\nnew header\nsecond line\nrow 0\nrow 1");
    }

    #[test]
    fn rows_wider_than_a_narrower_screen_wrap_and_come_back_when_it_widens() {
        let theme = theme();
        let mut scrollback = Scrollback::new();
        scrollback.push(vec![Line::from("abcdefghij")], None);
        let narrow = RenderCtx::new(4, &theme, Clock::still(Instant::now()));
        assert_eq!(
            plain_text(&scrollback.window(&[], 10, &narrow)),
            "abcd\nefgh\nij"
        );
        let wide = RenderCtx::new(20, &theme, Clock::still(Instant::now()));
        assert_eq!(plain_text(&scrollback.window(&[], 10, &wide)), "abcdefghij");
    }
}
