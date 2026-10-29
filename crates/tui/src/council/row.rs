//! A row built by column, as the talk lays itself out: each piece put where it goes, a later
//! piece over an earlier one, the gaps left blank.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use super::faces::Faces;
use super::words::{self, Piece};
use crate::theme::Theme;

#[derive(Debug, Clone, PartialEq)]
enum Cell {
    Empty,
    Glyph(String, Style),
    /// The second cell of a two-cell glyph.
    Spill,
}

#[derive(Debug, Clone, Default)]
pub struct Row {
    cells: Vec<Cell>,
    /// A fill under the whole row, as a picked message's band in the transcript.
    fill: Option<Style>,
}

impl Row {
    pub fn new() -> Self {
        Self::default()
    }

    /// `span` from `col`, over anything there; returns the column after it.
    pub fn put(&mut self, col: usize, span: Span<'static>) -> usize {
        let mut at = col;
        for ch in span.content.chars() {
            let size = ch.width().unwrap_or(0);
            if size == 0 {
                if let Some(Cell::Glyph(text, _)) =
                    at.checked_sub(1).and_then(|i| self.cells.get_mut(i))
                {
                    text.push(ch);
                }
                continue;
            }
            self.set(at, Cell::Glyph(ch.to_string(), span.style));
            if size == 2 {
                self.set(at + 1, Cell::Spill);
            }
            at += size;
        }
        at
    }

    fn set(&mut self, at: usize, cell: Cell) {
        if self.cells.len() <= at {
            self.cells.resize(at + 1, Cell::Empty);
        }
        // Covering half of a two-cell glyph leaves its other half blank.
        if self.cells[at] == Cell::Spill && at > 0 && cell != Cell::Spill {
            self.cells[at - 1] = Cell::Empty;
        }
        if let Some(Cell::Spill) = self.cells.get(at + 1)
            && matches!(self.cells[at], Cell::Glyph(..))
        {
            self.cells[at + 1] = Cell::Empty;
        }
        self.cells[at] = cell;
    }

    /// Spans one after another from `col`; returns the column after the last.
    pub fn put_all(&mut self, col: usize, spans: impl IntoIterator<Item = Span<'static>>) -> usize {
        spans.into_iter().fold(col, |at, span| self.put(at, span))
    }

    /// The same as [`Row::put_all`], said where a piece goes over something already there.
    pub fn overlay(&mut self, col: usize, spans: Vec<Span<'static>>) -> usize {
        self.put_all(col, spans)
    }

    /// Pieces one after another from `col`; returns the column after the last.
    pub fn pieces(&mut self, col: usize, pieces: &[Piece], faces: &Faces, theme: &Theme) -> usize {
        self.put_all(col, words::spans(pieces, faces, theme))
    }

    pub fn with(mut self, col: usize, span: Span<'static>) -> Self {
        self.put(col, span);
        self
    }

    pub fn filled(mut self, style: Style) -> Self {
        self.fill = Some(style);
        self
    }

    /// The column after its last cell.
    pub fn end(&self) -> usize {
        self.cells.len()
    }

    /// The row as a line no wider than `width`, neighbouring cells of one style in one span.
    pub fn line(self, width: usize) -> Line<'static> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut text = String::new();
        let mut style: Option<Style> = None;
        let mut used = 0;
        for cell in self.cells.into_iter() {
            let (glyph, cell_style, size) = match cell {
                Cell::Spill => continue,
                Cell::Empty => (" ".to_owned(), Style::default(), 1),
                Cell::Glyph(glyph, cell_style) => {
                    let size = glyph
                        .chars()
                        .map(|ch| ch.width().unwrap_or(0))
                        .sum::<usize>();
                    (glyph, cell_style, size)
                }
            };
            if used + size > width {
                break;
            }
            used += size;
            if style != Some(cell_style) && !text.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(&mut text),
                    style.unwrap_or_default(),
                ));
            }
            style = Some(cell_style);
            text.push_str(&glyph);
        }
        if !text.is_empty() {
            spans.push(Span::styled(text, style.unwrap_or_default()));
        }
        // Trailing blanks of no style go, as every other row of the transcript ends at its
        // last character.
        if let Some(last) = spans.last_mut()
            && last.style == Style::default()
        {
            let kept = last.content.trim_end().to_owned();
            used -= last.content.len() - kept.len();
            last.content = kept.into();
            if last.content.is_empty() {
                spans.pop();
            }
        }
        let mut line = Line::from(spans);
        if let Some(fill) = self.fill {
            if used < width {
                line.spans.push(Span::raw(" ".repeat(width - used)));
            }
            line = line.style(fill);
        }
        line
    }
}

/// `pieces` centred in `width`, as Messages centres its own lines.
pub fn centred(pieces: &[Piece], width: usize, faces: &Faces, theme: &Theme) -> Row {
    let size = words::width(pieces, faces, theme);
    let mut row = Row::new();
    row.pieces(width.saturating_sub(size) / 2, pieces, faces, theme);
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn pieces_land_by_column_and_a_later_one_covers_an_earlier() {
        let mut row = Row::new();
        row.put(4, Span::raw("abcdef"));
        row.put(6, Span::raw("XY"));
        row.put(0, Span::raw("✔"));
        assert_eq!(text(&row.line(20)), "✔   abXYef");
    }
}
