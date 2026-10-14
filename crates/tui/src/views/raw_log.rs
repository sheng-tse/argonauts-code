//! A session's raw log, moved through by event: ↑↓ and the page keys move the cursor, Enter
//! unfolds the event under it, `/` searches, and `n` and `N` find the next and the one before.
//! The raw pager and the agent view's Raw tab both draw it.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::api::RawView;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::theme::{Role, Theme};

use super::list::{self, Nav};
use super::panes::{self, Marks};
use super::raw_rows;

#[derive(Debug)]
pub struct RawLog {
    pub view: RawView,
    /// The event the cursor is on.
    event: usize,
    open: BTreeSet<usize>,
    /// Each event's type, read once.
    types: Vec<Option<String>>,
    /// What you're typing after `/`.
    typing: Option<String>,
    /// The last search, for `n` and `N`.
    query: String,
    /// The row the cursor's event started on at the last frame.
    row: usize,
    /// An event just unfolded, so the next frame brings its rows into view.
    unfolding: bool,
    /// Rows scrolled from the top.
    pub offset: usize,
    /// Rows the last frame showed, for paging.
    page: usize,
}

/// What a key did to the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawKey {
    Consumed,
    /// Not a key the log takes, such as Esc or Tab: the view around it decides.
    Ignored,
}

/// One event's rows each, keyed by its place: the unfolded ones.
type Opened = BTreeMap<usize, Vec<Line<'static>>>;

impl RawLog {
    pub fn new(view: RawView) -> Self {
        let types = view
            .lines
            .iter()
            .map(|line| raw_rows::type_of(line))
            .collect();
        Self {
            view,
            event: 0,
            open: BTreeSet::new(),
            types,
            typing: None,
            query: String::new(),
            row: 0,
            unfolding: false,
            offset: 0,
            page: 1,
        }
    }

    /// The same session's log read again: the cursor stays on its event and what was unfolded
    /// stays open, as far as the new log reaches.
    pub fn reread(self, view: RawView) -> Self {
        let count = view.lines.len();
        Self {
            event: self.event.min(count.saturating_sub(1)),
            open: self.open.into_iter().filter(|i| *i < count).collect(),
            query: self.query,
            offset: self.offset,
            ..Self::new(view)
        }
    }

    /// `The agent's own lines, unparsed · 42 lines`.
    pub fn description(&self) -> String {
        let lines = panes::count(self.view.lines.len(), "line");
        if self.view.truncated {
            format!("The agent's own lines, unparsed · the last {lines}")
        } else {
            format!("The agent's own lines, unparsed · {lines}")
        }
    }

    /// While a search is being typed, every key goes to it.
    pub fn typing(&self) -> bool {
        self.typing.is_some()
    }

    /// Draws the rows that fit in `area`, keeping the cursor's event in view, and returns the
    /// position mark for the footer, such as `1-24 of 34 ↓`.
    pub fn draw(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme) -> String {
        let width = usize::from(area.width).max(1);
        let height = usize::from(area.height);
        self.page = height.max(1);
        if self.view.lines.is_empty() {
            let empty = Line::from(Span::styled("No lines yet", theme.style(Role::Dim)));
            panes::put_lines(buf, area, &[empty]);
            return String::new();
        }
        // A raw log can hold thousands of events, so only the rows on screen are built.
        let opened = self.opened_rows(width, theme);
        let under =
            |before: usize| -> usize { opened.range(..before).map(|(_, rows)| rows.len()).sum() };
        let total = self.view.lines.len() + under(self.view.lines.len());
        let cursor = self.event + under(self.event);
        if cursor != self.row {
            // The cursor moved, so the rows follow it.
            self.row = cursor;
            if cursor < self.offset {
                self.offset = cursor;
            } else if cursor >= self.offset + self.page {
                self.offset = cursor + 1 - self.page;
            }
        }
        if std::mem::take(&mut self.unfolding) {
            let rows = opened.get(&self.event).map_or(0, Vec::len);
            let end = cursor + 1 + rows;
            if end > self.offset + self.page {
                self.offset = (end - self.page).min(cursor);
            }
        }
        self.offset = self.offset.min(total.saturating_sub(height));
        let shown = self.window(&opened, width, theme, height);
        panes::put_lines(buf, area, &shown);
        let marks = Marks::for_theme(theme);
        super::pager::position_mark(self.offset, height, total, marks.down)
    }

    /// The footer's hints for the log, before the way out the view around it adds.
    pub fn hints(&self) -> Vec<(String, u8)> {
        match &self.typing {
            Some(typing) => vec![
                (format!("/{typing}"), 0),
                ("enter finds".into(), 0),
                ("esc cancels".into(), 0),
            ],
            None => vec![
                ("↑↓ move".into(), 0),
                ("enter unfolds".into(), 0),
                ("/ searches".into(), 1),
                ("n next".into(), 2),
            ],
        }
    }

    /// A key, already read through the vim keys when they are on.
    pub fn key(&mut self, key: KeyEvent) -> RawKey {
        if let Some(typing) = self.typing.as_mut() {
            match key.code {
                KeyCode::Esc => self.typing = None,
                KeyCode::Enter => {
                    self.query = self.typing.take().unwrap_or_default();
                    self.find(true, true);
                }
                KeyCode::Backspace => {
                    typing.pop();
                }
                KeyCode::Char(c) => typing.push(c),
                _ => {}
            }
            return RawKey::Consumed;
        }
        let last = self.view.lines.len().saturating_sub(1);
        match key.code {
            KeyCode::Enter => {
                let event = self.event;
                if !self.open.remove(&event) && !self.view.lines.is_empty() {
                    self.open.insert(event);
                    self.unfolding = true;
                }
                return RawKey::Consumed;
            }
            KeyCode::Char('/') => {
                self.typing = Some(String::new());
                return RawKey::Consumed;
            }
            KeyCode::Char('n') => {
                self.find(true, false);
                return RawKey::Consumed;
            }
            KeyCode::Char('N') => {
                self.find(false, false);
                return RawKey::Consumed;
            }
            _ => {}
        }
        let page = isize::try_from(self.page).unwrap_or(isize::MAX);
        let nav = match key.code {
            KeyCode::Char(' ') => Some(Nav::By(page)),
            _ => list::nav_key(&key, self.page),
        };
        let Some(nav) = nav else {
            return RawKey::Ignored;
        };
        self.event = match nav {
            Nav::Top => 0,
            Nav::Bottom => last,
            Nav::By(delta) => self.event.saturating_add_signed(delta).min(last),
        };
        RawKey::Consumed
    }

    /// The rows of each unfolded event, by its place.
    fn opened_rows(&self, width: usize, theme: &Theme) -> Opened {
        self.open
            .iter()
            .filter_map(|&i| {
                let line = self.view.lines.get(i)?;
                Some((i, raw_rows::unfolded(line, width, theme)))
            })
            .collect()
    }

    /// The rows from the offset, `height` of them at most.
    fn window(
        &self,
        opened: &Opened,
        width: usize,
        theme: &Theme,
        height: usize,
    ) -> Vec<Line<'static>> {
        let column = raw_rows::type_column(&self.types);
        let mut shown = Vec::with_capacity(height);
        let mut row = 0;
        for (i, line) in self.view.lines.iter().enumerate() {
            if shown.len() >= height {
                break;
            }
            let under = opened.get(&i).map_or(&[][..], Vec::as_slice);
            if row + 1 + under.len() <= self.offset {
                row += 1 + under.len();
                continue;
            }
            if row >= self.offset {
                let kind = self.types.get(i).and_then(Option::as_deref);
                let selected = i == self.event;
                shown.push(raw_rows::event_row(
                    line, kind, column, width, selected, theme,
                ));
            }
            row += 1;
            for unfolded in under {
                if row >= self.offset && shown.len() < height {
                    shown.push(unfolded.clone());
                }
                row += 1;
            }
        }
        shown
    }

    /// Moves the cursor to the next event holding the last search, or the one before; `here`
    /// lets the cursor's own event count, as a new search does.
    fn find(&mut self, forward: bool, here: bool) {
        let lines = &self.view.lines;
        let count = lines.len();
        if count == 0 || self.query.is_empty() {
            return;
        }
        let start = self.event;
        let found = (0..count)
            .map(|step| {
                let step = if here { step } else { step + 1 };
                if forward {
                    (start + step) % count
                } else {
                    (start + count * 2 - step) % count
                }
            })
            .find(|&i| raw_rows::holds(&lines[i], &self.query));
        if let Some(event) = found {
            self.event = event;
        }
    }
}
