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
use super::raw_log::{RawKey, RawLog};
use super::{View, ViewCtx, ViewOutcome};

#[derive(Debug)]
pub struct Pager {
    pub title: String,
    pub content: PagerContent,
    /// Rows scrolled from the top; the raw log keeps its own.
    pub offset: usize,
    /// Rows the last frame showed, for paging.
    page: usize,
    /// Rows the content took at the last frame's width.
    rows: usize,
}

#[derive(Debug)]
pub enum PagerContent {
    Diff(DiffView),
    Raw(Box<RawLog>),
    Text(Vec<String>),
}

impl Pager {
    pub fn diff(diff: DiffView) -> Self {
        Self::with(String::from("Diff"), PagerContent::Diff(diff))
    }

    pub fn raw(raw: RawView) -> Self {
        Self::with(
            String::from("Raw"),
            PagerContent::Raw(Box::new(RawLog::new(raw))),
        )
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
                        sanitize_line(diff.task.as_str())
                    )),
                    description: format!(
                        "{files} changed · +{} −{}",
                        stat.insertions, stat.deletions
                    ),
                    summary: sanitize_line(&diff.path.to_string_lossy()).into_owned(),
                }
            }
            PagerContent::Raw(log) => Header {
                title: label(format!(
                    "{} · {}",
                    self.title,
                    sanitize_line(log.view.session.as_str())
                )),
                description: log.description(),
                summary: sanitize_line(&log.view.path.to_string_lossy()).into_owned(),
            },
            PagerContent::Text(lines) => Header {
                title: label(sanitize_line(&self.title).into_owned()),
                description: String::new(),
                summary: panes::count(lines.len(), "line"),
            },
        }
    }

    /// Every row at `width`, long lines wrapped so nothing is lost. The raw log draws its own.
    fn rows(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let width = width.max(1);
        let mut rows = Vec::new();
        match &self.content {
            PagerContent::Diff(diff) => {
                for line in sanitize(&diff.text).lines() {
                    push_diff_line(&mut rows, line, width, theme);
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
            PagerContent::Raw(_) => {}
        }
        if rows.is_empty() {
            let empty = match &self.content {
                PagerContent::Diff(_) => "No changes",
                _ => "Nothing to show",
            };
            rows.push(Line::from(Span::styled(empty, theme.style(Role::Dim))));
        }
        rows
    }

    /// Where the rows are scrolled to, the raw log's own included.
    pub fn top(&self) -> usize {
        match &self.content {
            PagerContent::Raw(log) => log.offset,
            _ => self.offset,
        }
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

        if let PagerContent::Raw(log) = &mut self.content {
            let mark = log.draw(buf, panes.right, theme);
            let mut hints = log.hints();
            if !log.typing() {
                hints.push((panes::ESC_BACK.into(), 0));
            }
            panes::draw_footer(buf, panes.footer, Footer::Hints(hints), &mark, theme);
            return;
        }
        let width = usize::from(panes.right.width).max(1);
        let height = usize::from(panes.right.height);
        self.page = height.max(1);
        let rows = self.rows(width, theme);
        self.rows = rows.len();
        self.offset = self.offset.min(rows.len().saturating_sub(height));
        panes::put_lines(buf, panes.right, &rows[self.offset..]);

        let marks = panes::Marks::for_theme(theme);
        let mark = position_mark(self.offset, height, rows.len(), marks.down);
        let hints = Footer::hints(&[
            ("↑↓ scroll", 0),
            ("pgup pgdn page", 1),
            (panes::ESC_BACK, 0),
        ]);
        panes::draw_footer(buf, panes.footer, hints, &mark, theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        if let PagerContent::Raw(log) = &mut self.content
            && log.typing()
        {
            log.key(key);
            return ViewOutcome::Consumed;
        }
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        if matches!(key.code, KeyCode::Esc | KeyCode::Left | KeyCode::Char('q')) {
            return ViewOutcome::Close;
        }
        if let PagerContent::Raw(log) = &mut self.content {
            return match log.key(key) {
                RawKey::Consumed => ViewOutcome::Consumed,
                RawKey::Ignored => ViewOutcome::Ignored,
            };
        }
        if key.code == KeyCode::Char(' ') {
            self.scroll(Nav::By(isize::try_from(self.page).unwrap_or(isize::MAX)));
            return ViewOutcome::Consumed;
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
