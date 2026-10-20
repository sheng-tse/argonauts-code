//! Markdown to terminal lines, the way Claude Code draws assistant text.
//!
//! Single newlines inside a paragraph stay line breaks, so each source line maps to its own
//! rows. That keeps streaming stable: a finished line renders the same once more text arrives.
//! A heading always has a blank row after it, and a list written right under the line that
//! introduces it stays attached to it.

pub mod highlight;
mod list;
pub mod stream;
mod table;
#[cfg(test)]
mod table_golden;
mod writer;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::render::RenderCtx;
use crate::text::{fit_lines, hard_wrap_spans, sanitize, spans_width};

/// The prefixes a rendered block sits behind: one for its first row and one for every later row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Indent {
    pub first: Vec<Span<'static>>,
    pub rest: Vec<Span<'static>>,
}

impl Indent {
    pub fn new(first: Vec<Span<'static>>, rest: Vec<Span<'static>>) -> Self {
        Self { first, rest }
    }

    pub fn spaces(cols: usize) -> Self {
        let pad = vec![Span::raw(" ".repeat(cols))];
        Self {
            first: pad.clone(),
            rest: pad,
        }
    }

    /// Columns taken by the later rows' prefix, where wrapped text lines up.
    pub fn width(&self) -> usize {
        spans_width(&self.rest)
    }
}

pub fn render_markdown(source: &str, ctx: &RenderCtx<'_>, indent: &Indent) -> Vec<Line<'static>> {
    let source = sanitize(source);
    let mut lines = writer::render(&source, ctx, indent, Continuation::default());
    fit_lines(&mut lines, usize::from(ctx.width));
    lines
}

/// What a piece of Markdown continues, for a streamed answer rendered one region at a time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Continuation {
    /// Rows came before this piece, so its first row takes the later-row prefix, and its first
    /// block is separated from them like any other block.
    pub rows_before: bool,
    pub blank_before: bool,
    /// The block right before this piece was a heading, which always has a blank row after it.
    pub after_heading: bool,
}

/// Code must not reflow like prose, so each line hard-wraps and keeps every character.
pub(crate) fn code_rows(
    spans: Vec<Span<'static>>,
    width: usize,
    first: &[Span<'static>],
    rest: &[Span<'static>],
) -> Vec<Line<'static>> {
    let avail = width.saturating_sub(spans_width(first)).max(1);
    let chunks = hard_wrap_spans(&spans, avail);
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let mut row = if i == 0 {
                first.to_vec()
            } else {
                rest.to_vec()
            };
            row.extend(chunk);
            Line::from(row)
        })
        .collect()
}

pub(crate) fn pad(cols: usize) -> Span<'static> {
    Span::styled(" ".repeat(cols), Style::new())
}

#[cfg(test)]
mod tests;
