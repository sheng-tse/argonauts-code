//! The row layout follows Codex's `diff_render.rs` (Apache-2.0): a right-aligned line number,
//! the marker, and the text wrapped under itself so continuation rows keep the row color. Rows
//! wrap where a token ends when they can, and a live preview cuts them with `…` instead.

use std::ops::Range;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::words::changed_words;
use super::{DiffKind, DiffLine, DiffView};
use crate::cell::layout::{indent, right_margin};
use crate::render::RenderCtx;
use crate::text::{sanitize_line, truncate_spans, wrap_code_spans};
use crate::theme::{ColorLevel, Role, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fill {
    /// In the transcript.
    ShortOfEdge,
    /// Inside a card.
    ToEdge,
}

pub(super) fn rows(
    view: &DiffView,
    ctx: &RenderCtx<'_>,
    col: usize,
    fill: Fill,
) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let digits = view
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter_map(DiffLine::number)
        .map(|n| n.to_string().len())
        .max()
        .unwrap_or(1);
    let width = usize::from(ctx.width);
    let end = match fill {
        Fill::ShortOfEdge => width - right_margin(width),
        Fill::ToEdge => width,
    };
    let layout = Layout {
        col,
        gutter: digits + 1,
        end,
        fills: matches!(theme.level, ColorLevel::TrueColor | ColorLevel::Ansi256),
        live: ctx.live,
    };
    let mut out = Vec::new();
    for (i, hunk) in view.hunks.iter().enumerate() {
        if i > 0 {
            let gap = format!("{}{}", " ".repeat(layout.gutter - 1), theme.glyphs.hunk_gap);
            out.push(Line::from(vec![
                indent(col),
                Span::styled(gap, theme.style(Role::Dim)),
            ]));
        }
        let changed = changed_words(&hunk.lines);
        for (line, words) in hunk.lines.iter().zip(changed.iter()) {
            out.extend(layout.line_rows(line, words, theme));
        }
    }
    out
}

struct Layout {
    col: usize,
    gutter: usize,
    /// Where a row's text and fill end.
    end: usize,
    /// Off for 16 color and colorless terminals, which can't show background fills well.
    fills: bool,
    live: bool,
}

impl Layout {
    fn line_rows(
        &self,
        line: &DiffLine,
        words: &[Range<usize>],
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let (marker, role, row, word) = match line.kind {
            DiffKind::Removed => (
                "-",
                Role::RemovedMarker,
                Some(Role::RemovedRow),
                Role::RemovedWord,
            ),
            DiffKind::Added => (
                "+",
                Role::AddedMarker,
                Some(Role::AddedRow),
                Role::AddedWord,
            ),
            DiffKind::Context => (" ", Role::Dim, None, Role::Text),
        };
        let (fill, text_style, word_style) = match row {
            Some(row) if self.fills => {
                let fill = Style::new().bg(theme.style(row).bg.unwrap_or_default());
                let text = fill.patch(theme.style(Role::DiffText));
                let word_bg = theme.style(word).bg;
                (
                    fill,
                    text,
                    Style {
                        bg: word_bg,
                        ..text
                    },
                )
            }
            // Without fills the row says what it is with colored text and underlined words.
            Some(_) => {
                let mark = theme.style(role);
                (Style::new(), mark, mark.add_modifier(Modifier::UNDERLINED))
            }
            None => {
                let text = theme.style(Role::Text);
                (Style::new(), text, text)
            }
        };
        let mark_style = fill.patch(theme.style(role));
        let text = sanitize_line(&line.text);
        let spans = split_words(&text, words, text_style, word_style);
        let text_col = self.col + self.gutter + 2;
        let avail = self.end.saturating_sub(text_col).max(1);
        let number = line.number().map(|n| n.to_string()).unwrap_or_default();
        let chunks = if self.live {
            vec![truncate_spans(&spans, avail)]
        } else {
            wrap_code_spans(&spans, avail)
        };
        chunks
            .into_iter()
            .enumerate()
            .map(|(i, chunk)| {
                let mut row = vec![indent(self.col)];
                if i == 0 {
                    row.push(Span::styled(
                        format!("{number:>w$}", w = self.gutter),
                        mark_style,
                    ));
                    row.push(Span::styled(" ", fill));
                    row.push(Span::styled(marker, mark_style));
                } else {
                    row.push(Span::styled(" ".repeat(self.gutter + 2), fill));
                }
                let used = text_col + chunk.iter().map(Span::width).sum::<usize>();
                row.extend(chunk);
                if fill.bg.is_some() && used < self.end {
                    row.push(Span::styled(" ".repeat(self.end - used), fill));
                }
                Line::from(row)
            })
            .collect()
    }
}

/// `words` are byte ranges into `text`.
fn split_words(
    text: &str,
    words: &[Range<usize>],
    plain: Style,
    word: Style,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut at = 0;
    for range in words {
        let (start, end) = (range.start.min(text.len()), range.end.min(text.len()));
        if start < at || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        if start > at {
            spans.push(Span::styled(text[at..start].to_owned(), plain));
        }
        if end > start {
            spans.push(Span::styled(text[start..end].to_owned(), word));
        }
        at = end;
    }
    if at < text.len() || spans.is_empty() {
        spans.push(Span::styled(text[at..].to_owned(), plain));
    }
    spans
}
