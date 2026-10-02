//! Tables draw in box characters. Columns shrink to fit by wrapping their cells, widest first,
//! but never below their longest word (capped) so values stay readable. A table that still
//! doesn't fit stacks each row as a record of `Header: value` lines.

use std::ops::Range;

use pulldown_cmark::Alignment;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::text::{spans_width, str_width, wrap_spans};
use crate::theme::{Role, Theme};

/// Each column's frame cells besides its text: the border on its left and a space each side.
const COLUMN_FRAME: usize = 3;
/// The narrowest a column shrinks to, unless its content is narrower.
const MIN_COLUMN: usize = 3;
/// A long word only protects its column up to this width.
const WORD_FLOOR_CAP: usize = 12;

type Cell = Vec<Span<'static>>;

#[derive(Debug)]
pub(super) struct TableBuilder {
    aligns: Vec<Alignment>,
    rows: Vec<Vec<Cell>>,
    row: Vec<Cell>,
    cell: Cell,
    source: Range<usize>,
}

impl TableBuilder {
    pub(super) fn new(aligns: Vec<Alignment>, source: Range<usize>) -> Self {
        Self {
            aligns,
            rows: Vec::new(),
            row: Vec::new(),
            cell: Vec::new(),
            source,
        }
    }

    /// Where the table sits in the source, for showing it as written.
    pub(super) fn source(&self) -> Range<usize> {
        self.source.clone()
    }

    pub(super) fn start_row(&mut self) {
        self.row.clear();
    }

    pub(super) fn start_cell(&mut self) {
        self.cell.clear();
    }

    pub(super) fn push(&mut self, span: Span<'static>) {
        self.cell.push(span);
    }

    pub(super) fn end_cell(&mut self) {
        self.row.push(std::mem::take(&mut self.cell));
    }

    pub(super) fn end_row(&mut self) {
        self.rows.push(std::mem::take(&mut self.row));
    }

    /// The table in a box, or as records when the box doesn't fit in `width` columns. `None`
    /// only when there is no room at all.
    pub(super) fn layout(&self, width: usize, theme: &Theme) -> Option<Vec<Vec<Span<'static>>>> {
        if width == 0 {
            return None;
        }
        let columns = self
            .aligns
            .len()
            .max(self.rows.iter().map(Vec::len).max().unwrap_or(0));
        if columns == 0 || self.rows.is_empty() {
            return Some(Vec::new());
        }
        let Some(widths) = self.column_widths(columns, width) else {
            return Some(self.records(width));
        };
        let widths = self.fitted(widths);
        let frame = Frame::for_theme(theme);
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let mut out = vec![frame.rule(&widths, frame.top)];
        for (i, row) in self.rows.iter().enumerate() {
            let header = i == 0;
            out.extend(self.row_lines(row, &widths, header.then_some(bold), &frame));
            if header && self.rows.len() > 1 {
                out.push(frame.rule(&widths, frame.middle));
            }
        }
        out.push(frame.rule(&widths, frame.bottom));
        Some(out)
    }

    /// The narrow layout: each row as a record, a `Header: value` line per cell, with a blank
    /// line between records.
    fn records(&self, width: usize) -> Vec<Cell> {
        let Some((header, body)) = self.rows.split_first() else {
            return Vec::new();
        };
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let label = |col: usize| -> Cell {
            let mut label = header.get(col).cloned().unwrap_or_default();
            for span in &mut label {
                span.style = span.style.patch(bold);
            }
            label
        };
        // Wrapped values hang under their label, when there is room for that.
        let hang: Vec<Span<'static>> = if width >= 8 {
            vec![Span::raw("  ")]
        } else {
            Vec::new()
        };
        if body.is_empty() {
            return (0..header.len())
                .flat_map(|col| wrap_spans(&label(col), width, &[], &hang))
                .map(|line| line.spans)
                .collect();
        }
        let mut out = Vec::new();
        for (i, row) in body.iter().enumerate() {
            if i > 0 {
                out.push(Vec::new());
            }
            for (col, cell) in row.iter().enumerate() {
                let mut spans = label(col);
                if spans_width(&spans) > 0 {
                    spans.push(Span::styled(":", bold));
                    spans.push(Span::raw(" "));
                }
                spans.extend(cell.iter().cloned());
                let lines = wrap_spans(&spans, width, &[], &hang);
                out.extend(lines.into_iter().map(|line| line.spans));
            }
        }
        out
    }

    fn column_widths(&self, columns: usize, width: usize) -> Option<Vec<usize>> {
        let cell_at = |row: &Vec<Cell>, col: usize| row.get(col).map_or(0, |c| spans_width(c));
        let mut widths: Vec<usize> = (0..columns)
            .map(|col| {
                self.rows
                    .iter()
                    .map(|row| cell_at(row, col))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let floors: Vec<usize> = (0..columns)
            .map(|col| {
                let word = self
                    .rows
                    .iter()
                    .filter_map(|row| row.get(col))
                    .map(|cell| longest_word(cell))
                    .max()
                    .unwrap_or(0);
                word.clamp(MIN_COLUMN, WORD_FLOOR_CAP).min(widths[col])
            })
            .collect();
        let frame = COLUMN_FRAME * columns + 1;
        let floor_total: usize = floors.iter().sum::<usize>() + frame;
        if floor_total > width {
            return None;
        }
        let mut total: usize = widths.iter().sum::<usize>() + frame;
        while total > width {
            let (col, _) = widths
                .iter()
                .zip(&floors)
                .enumerate()
                .map(|(col, (w, floor))| (col, w.saturating_sub(*floor)))
                .max_by_key(|(_, slack)| *slack)
                .filter(|(_, slack)| *slack > 0)?;
            widths[col] -= 1;
            total -= 1;
        }
        Some(widths)
    }

    /// Narrows each column to what its wrapped cells really take, so the box is no wider than
    /// the text.
    fn fitted(&self, mut widths: Vec<usize>) -> Vec<usize> {
        for (col, width) in widths.iter_mut().enumerate() {
            let used = self
                .rows
                .iter()
                .filter_map(|row| row.get(col))
                .flat_map(|cell| wrap_spans(cell, *width, &[], &[]))
                .map(|line| spans_width(&line.spans))
                .max()
                .unwrap_or(0);
            *width = used.min(*width);
        }
        widths
    }

    fn row_lines(
        &self,
        row: &[Cell],
        widths: &[usize],
        patch: Option<Style>,
        frame: &Frame,
    ) -> Vec<Cell> {
        let wrapped: Vec<Vec<Line<'static>>> = widths
            .iter()
            .enumerate()
            .map(|(col, width)| {
                let mut cell = row.get(col).cloned().unwrap_or_default();
                if let Some(patch) = patch {
                    for span in &mut cell {
                        span.style = span.style.patch(patch);
                    }
                }
                wrap_spans(&cell, *width, &[], &[])
            })
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        (0..height)
            .map(|line| {
                let mut spans: Cell = vec![frame.edge()];
                for (col, width) in widths.iter().enumerate() {
                    spans.push(Span::raw(" "));
                    let content = wrapped[col]
                        .get(line)
                        .map(|l| l.spans.clone())
                        .unwrap_or_default();
                    let room = width.saturating_sub(spans_width(&content));
                    let (left, right) = match self.aligns.get(col) {
                        Some(Alignment::Right) => (room, 0),
                        Some(Alignment::Center) => (room / 2, room - room / 2),
                        _ => (0, room),
                    };
                    if left > 0 {
                        spans.push(Span::raw(" ".repeat(left)));
                    }
                    spans.extend(content);
                    spans.push(Span::raw(" ".repeat(right + 1)));
                    spans.push(frame.edge());
                }
                spans
            })
            .collect()
    }
}

/// The box's pieces. Joints run left, middle, right. ASCII glyph sets get `+`, `-`, and `|`.
struct Frame {
    across: &'static str,
    down: &'static str,
    top: [&'static str; 3],
    middle: [&'static str; 3],
    bottom: [&'static str; 3],
    style: Style,
}

impl Frame {
    fn for_theme(theme: &Theme) -> Self {
        let style = theme.style(Role::Dim);
        if theme.glyphs.rule.is_ascii() {
            return Self {
                across: "-",
                down: "|",
                top: ["+"; 3],
                middle: ["+"; 3],
                bottom: ["+"; 3],
                style,
            };
        }
        Self {
            across: "─",
            down: "│",
            top: ["┌", "┬", "┐"],
            middle: ["├", "┼", "┤"],
            bottom: ["└", "┴", "┘"],
            style,
        }
    }

    fn edge(&self) -> Span<'static> {
        Span::styled(self.down, self.style)
    }

    /// A line across the box, each column as wide as its text and the spaces beside it.
    fn rule(&self, widths: &[usize], [left, middle, right]: [&'static str; 3]) -> Cell {
        let columns: Vec<String> = widths
            .iter()
            .map(|width| self.across.repeat(width + 2))
            .collect();
        let text = format!("{left}{}{right}", columns.join(middle));
        vec![Span::styled(text, self.style)]
    }
}

fn longest_word(cell: &[Span<'_>]) -> usize {
    let text: String = cell.iter().map(|span| span.content.as_ref()).collect();
    text.split_whitespace().map(str_width).max().unwrap_or(0)
}
