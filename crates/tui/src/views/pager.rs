//! The diff and raw pagers, and a plain text pager for a choice's full body.

use argonauts_core::api::{DiffView, RawView};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::text::{hard_wrap_spans, sanitize, sanitize_line, spans_width};
use crate::theme::{Role, Theme};

use super::list::{self, Nav};
use super::panes::{self, Footer, Header, Side};
use super::raw_rows;
use super::{View, ViewCtx, ViewOutcome};

#[derive(Debug)]
pub struct Pager {
    pub title: String,
    pub content: PagerContent,
    /// Rows scrolled from the top.
    pub offset: usize,
    /// Rows the last frame showed, for paging.
    page: usize,
    /// Rows the content took at the last frame's width.
    rows: usize,
    /// The raw log's cursor, its unfolded events, and its search.
    raw: Box<RawCursor>,
}

/// Where the raw log's cursor is, by event, and what it found.
#[derive(Debug, Default)]
struct RawCursor {
    event: usize,
    open: std::collections::BTreeSet<usize>,
    /// Each event's type, read once.
    types: Vec<Option<String>>,
    /// What you're typing after `/`.
    typing: Option<String>,
    /// The last search, for `n` and `N`.
    query: String,
    /// The row the cursor's event starts on at the last frame.
    row: usize,
}

#[derive(Debug)]
pub enum PagerContent {
    Diff(DiffView),
    Raw(RawView),
    Text(Vec<String>),
}

impl Pager {
    pub fn diff(diff: DiffView) -> Self {
        Self::with(String::from("Diff"), PagerContent::Diff(diff))
    }

    pub fn raw(raw: RawView) -> Self {
        let types = raw
            .lines
            .iter()
            .map(|line| raw_rows::type_of(line))
            .collect();
        let mut pager = Self::with(String::from("Raw"), PagerContent::Raw(raw));
        pager.raw.types = types;
        pager
    }

    pub fn text(title: impl Into<String>, lines: Vec<String>) -> Self {
        Self::with(title.into(), PagerContent::Text(lines))
    }

    fn with(title: String, content: PagerContent) -> Self {
        Self {
            title,
            content,
            offset: 0,
            page: 1,
            rows: 0,
            raw: Box::default(),
        }
    }

    fn header(&self, theme: &Theme) -> Header {
        let label = |text: String| Line::from(Span::styled(text, theme.style(Role::Label)));
        match &self.content {
            PagerContent::Diff(diff) => {
                let stat = &diff.stat;
                let files = panes::count(stat.files_changed as usize, "file");
                Header {
                    title: label(format!(
                        "{} · {}",
                        self.title,
                        sanitize_line(diff.card.as_str())
                    )),
                    description: format!(
                        "{files} changed · +{} −{}",
                        stat.insertions, stat.deletions
                    ),
                    summary: sanitize_line(&diff.path.to_string_lossy()).into_owned(),
                }
            }
            PagerContent::Raw(raw) => {
                let lines = panes::count(raw.lines.len(), "line");
                let description = if raw.truncated {
                    format!("The agent's own lines, unparsed · the last {lines}")
                } else {
                    format!("The agent's own lines, unparsed · {lines}")
                };
                Header {
                    title: label(format!(
                        "{} · {}",
                        self.title,
                        sanitize_line(raw.session.as_str())
                    )),
                    description,
                    summary: sanitize_line(&raw.path.to_string_lossy()).into_owned(),
                }
            }
            PagerContent::Text(lines) => Header {
                title: label(sanitize_line(&self.title).into_owned()),
                description: String::new(),
                summary: panes::count(lines.len(), "line"),
            },
        }
    }

    /// The rows of each event unfolded in the raw log, by its place.
    fn opened_rows(
        &self,
        raw: &RawView,
        width: usize,
        theme: &Theme,
    ) -> std::collections::BTreeMap<usize, Vec<Line<'static>>> {
        self.raw
            .open
            .iter()
            .filter_map(|&i| Some((i, raw_rows::unfolded(raw.lines.get(i)?, width, theme))))
            .collect()
    }

    /// How many rows the raw log takes, one per event and the unfolded ones beneath, and the
    /// row its cursor is on.
    fn raw_layout(
        &self,
        raw: &RawView,
        opened: &std::collections::BTreeMap<usize, Vec<Line<'static>>>,
    ) -> (usize, usize) {
        let under =
            |before: usize| -> usize { opened.range(..before).map(|(_, rows)| rows.len()).sum() };
        let total = raw.lines.len() + under(raw.lines.len());
        let cursor = self.raw.event + under(self.raw.event);
        (total, cursor)
    }

    /// The raw log's rows from the offset, `height` of them at most.
    fn raw_window(
        &self,
        raw: &RawView,
        opened: &std::collections::BTreeMap<usize, Vec<Line<'static>>>,
        width: usize,
        theme: &Theme,
        height: usize,
    ) -> Vec<Line<'static>> {
        let column = raw_rows::type_column(&self.raw.types);
        let mut shown = Vec::with_capacity(height);
        let mut row = 0;
        for (i, line) in raw.lines.iter().enumerate() {
            if shown.len() >= height {
                break;
            }
            let under = opened.get(&i).map_or(&[][..], Vec::as_slice);
            if row + 1 + under.len() <= self.offset {
                row += 1 + under.len();
                continue;
            }
            if row >= self.offset {
                let kind = self.raw.types.get(i).and_then(Option::as_deref);
                let selected = i == self.raw.event;
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

    /// Every row at `width`, long lines wrapped so nothing is lost; the raw log one row per
    /// event. Also the row the raw log's cursor is on.
    fn rows(&self, width: usize, theme: &Theme) -> (Vec<Line<'static>>, usize) {
        let width = width.max(1);
        let mut rows = Vec::new();
        let mut cursor = 0;
        match &self.content {
            PagerContent::Diff(diff) => {
                for line in sanitize(&diff.text).lines() {
                    push_diff_line(&mut rows, line, width, theme);
                }
            }
            PagerContent::Raw(raw) => {
                let column = raw_rows::type_column(&self.raw.types);
                for (i, line) in raw.lines.iter().enumerate() {
                    if i == self.raw.event {
                        cursor = rows.len();
                    }
                    let kind = self.raw.types.get(i).and_then(Option::as_deref);
                    let selected = i == self.raw.event;
                    rows.push(raw_rows::event_row(
                        line, kind, column, width, selected, theme,
                    ));
                    if self.raw.open.contains(&i) {
                        rows.extend(raw_rows::unfolded(line, width, theme));
                    }
                }
            }
            PagerContent::Text(lines) => {
                let text = theme.style(Role::Text);
                for line in lines {
                    for part in sanitize(line).split('\n') {
                        push_wrapped(&mut rows, &[Span::styled(part.to_owned(), text)], width);
                    }
                }
            }
        }
        if rows.is_empty() {
            let empty = match &self.content {
                PagerContent::Diff(_) => "No changes",
                PagerContent::Raw(_) => "No lines yet",
                PagerContent::Text(_) => "Nothing to show",
            };
            rows.push(Line::from(Span::styled(empty, theme.style(Role::Dim))));
        }
        (rows, cursor)
    }
}

fn push_wrapped(rows: &mut Vec<Line<'static>>, spans: &[Span<'_>], width: usize) {
    rows.extend(hard_wrap_spans(spans, width).into_iter().map(Line::from));
}

/// A unified diff line in its colors: added and removed rows on their backgrounds, filled to
/// the pane's edge, hunk headers in the accent, and file headers bold.
fn push_diff_line(rows: &mut Vec<Line<'static>>, line: &str, width: usize, theme: &Theme) {
    let file_header = [
        "diff ",
        "index ",
        "--- ",
        "+++ ",
        "new file",
        "deleted file",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix));
    let (row, marker) = if file_header {
        let style = theme
            .style(Role::Bright)
            .add_modifier(ratatui::style::Modifier::BOLD);
        push_wrapped(rows, &[Span::styled(line.to_owned(), style)], width);
        return;
    } else if line.starts_with("@@") {
        push_wrapped(
            rows,
            &[Span::styled(line.to_owned(), theme.style(Role::Accent))],
            width,
        );
        return;
    } else if line.starts_with('+') {
        (theme.style(Role::AddedRow), theme.style(Role::AddedMarker))
    } else if line.starts_with('-') {
        (
            theme.style(Role::RemovedRow),
            theme.style(Role::RemovedMarker),
        )
    } else {
        push_wrapped(
            rows,
            &[Span::styled(line.to_owned(), theme.style(Role::Text))],
            width,
        );
        return;
    };
    let (sign, rest) = line.split_at(1);
    let text = theme.style(Role::DiffText).patch(row);
    let spans = [
        Span::styled(sign.to_owned(), marker.patch(row)),
        Span::styled(rest.to_owned(), text),
    ];
    for wrapped in hard_wrap_spans(&spans, width) {
        rows.push(filled(wrapped, width, row));
    }
}

/// The row padded to `width` in `style`, so its background runs to the pane's edge.
fn filled(mut spans: Vec<Span<'static>>, width: usize, style: Style) -> Line<'static> {
    let pad = width.saturating_sub(spans_width(&spans));
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), style));
    }
    Line::from(spans)
}

/// `1-24 of 34 ↓`, or empty when everything fits.
pub(crate) fn position_mark(offset: usize, shown: usize, total: usize, down: &str) -> String {
    if total <= shown {
        return String::new();
    }
    let last = (offset + shown).min(total);
    let more = if last < total {
        format!(" {down}")
    } else {
        String::new()
    };
    format!("{}-{last} of {total}{more}", offset + 1)
}

impl View for Pager {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let panes = panes::split(area, 0);
        panes::draw_header(buf, panes.header, &self.header(theme), theme);
        panes::draw_box(buf, &panes, ["", ""], Side::Right, theme);

        let width = usize::from(panes.right.width).max(1);
        let height = usize::from(panes.right.height);
        self.page = height.max(1);
        // A raw log can hold thousands of events, so only the rows on screen are built.
        let (total, cursor, opened) = match &self.content {
            PagerContent::Raw(raw) => {
                let opened = self.opened_rows(raw, width, theme);
                let (total, cursor) = self.raw_layout(raw, &opened);
                (total, cursor, Some(opened))
            }
            _ => (0, 0, None),
        };
        let all = match opened {
            Some(_) => None,
            None => Some(self.rows(width, theme).0),
        };
        let total = all.as_ref().map_or(total, Vec::len);
        self.rows = total;
        if self.is_raw() && cursor != self.raw.row {
            // The cursor moved, so the rows follow it.
            self.raw.row = cursor;
            if cursor < self.offset {
                self.offset = cursor;
            } else if cursor >= self.offset + self.page {
                self.offset = cursor + 1 - self.page;
            }
        }
        self.offset = self.offset.min(total.saturating_sub(height));
        match (&all, &self.content, opened) {
            (Some(rows), ..) => panes::put_lines(buf, panes.right, &rows[self.offset..]),
            (None, PagerContent::Raw(raw), Some(opened)) => {
                let shown = self.raw_window(raw, &opened, width, theme, height);
                panes::put_lines(buf, panes.right, &shown);
            }
            _ => {}
        }

        let marks = panes::Marks::for_theme(theme);
        let mark = position_mark(self.offset, height, total, marks.down);
        let hints = match (&self.content, &self.raw.typing) {
            (PagerContent::Raw(_), Some(typing)) => {
                let typed = format!("/{typing}");
                Footer::hints(&[(&typed, 0), ("enter finds", 0), ("esc cancels", 0)])
            }
            (PagerContent::Raw(_), None) => Footer::hints(&[
                ("↑↓ move", 0),
                ("enter unfolds", 0),
                ("/ searches", 1),
                ("n next", 2),
                (panes::ESC_BACK, 0),
            ]),
            _ => Footer::hints(&[
                ("↑↓ scroll", 0),
                ("pgup pgdn page", 1),
                (panes::ESC_BACK, 0),
            ]),
        };
        panes::draw_footer(buf, panes.footer, hints, &mark, theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        if self.is_raw() {
            return self.raw_key(key, ctx);
        }
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => return ViewOutcome::Close,
            KeyCode::Char(' ') => {
                self.scroll(Nav::By(isize::try_from(self.page).unwrap_or(isize::MAX)));
                return ViewOutcome::Consumed;
            }
            _ => {}
        }
        match list::nav_key(&key, self.page) {
            Some(nav) => {
                self.scroll(nav);
                ViewOutcome::Consumed
            }
            None => ViewOutcome::Ignored,
        }
    }
}

impl Pager {
    fn scroll(&mut self, nav: Nav) {
        self.offset = list::scroll(self.offset, nav, self.rows, self.page);
    }

    fn is_raw(&self) -> bool {
        matches!(self.content, PagerContent::Raw(_))
    }

    fn raw_lines(&self) -> &[String] {
        match &self.content {
            PagerContent::Raw(raw) => &raw.lines,
            _ => &[],
        }
    }

    /// The raw log moves by event: ↑↓ and the page keys move the cursor, Enter unfolds the
    /// event under it, `/` searches, and `n` and `N` find the next and the one before.
    fn raw_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        if let Some(typing) = self.raw.typing.as_mut() {
            match key.code {
                KeyCode::Esc => self.raw.typing = None,
                KeyCode::Enter => {
                    self.raw.query = self.raw.typing.take().unwrap_or_default();
                    self.find(true, true);
                }
                KeyCode::Backspace => {
                    typing.pop();
                }
                KeyCode::Char(c) => typing.push(c),
                _ => {}
            }
            return ViewOutcome::Consumed;
        }
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        let last = self.raw_lines().len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => return ViewOutcome::Close,
            KeyCode::Enter => {
                let event = self.raw.event;
                if !self.raw.open.remove(&event) && event <= last && !self.raw_lines().is_empty() {
                    self.raw.open.insert(event);
                }
                return ViewOutcome::Consumed;
            }
            KeyCode::Char('/') => {
                self.raw.typing = Some(String::new());
                return ViewOutcome::Consumed;
            }
            KeyCode::Char('n') => {
                self.find(true, false);
                return ViewOutcome::Consumed;
            }
            KeyCode::Char('N') => {
                self.find(false, false);
                return ViewOutcome::Consumed;
            }
            _ => {}
        }
        let page = isize::try_from(self.page).unwrap_or(isize::MAX);
        let nav = match key.code {
            KeyCode::Char(' ') => Some(Nav::By(page)),
            _ => list::nav_key(&key, self.page),
        };
        let Some(nav) = nav else {
            return ViewOutcome::Ignored;
        };
        self.raw.event = match nav {
            Nav::Top => 0,
            Nav::Bottom => last,
            Nav::By(delta) => self.raw.event.saturating_add_signed(delta).min(last),
        };
        ViewOutcome::Consumed
    }

    /// Moves the cursor to the next event holding the last search, or the one before; `here`
    /// lets the cursor's own event count, as a new search does.
    fn find(&mut self, forward: bool, here: bool) {
        let lines = self.raw_lines();
        let count = lines.len();
        if count == 0 || self.raw.query.is_empty() {
            return;
        }
        let start = self.raw.event;
        let found = (0..count)
            .map(|step| {
                let step = if here { step } else { step + 1 };
                if forward {
                    (start + step) % count
                } else {
                    (start + count * 2 - step) % count
                }
            })
            .find(|&i| raw_rows::holds(&lines[i], &self.raw.query));
        if let Some(event) = found {
            self.raw.event = event;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_the_rows_shown() {
        assert_eq!(position_mark(0, 24, 34, "↓"), "1-24 of 34 ↓");
        assert_eq!(position_mark(10, 24, 34, "↓"), "11-34 of 34");
        assert_eq!(position_mark(0, 24, 20, "↓"), "");
    }
}
