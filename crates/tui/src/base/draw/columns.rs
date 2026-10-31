//! Aligned columns for the rows that list agents: the workflow view, the agent view's list, the
//! agent panel, and the agent panel. Every cell is padded to its column's widest, numbers sit
//! right-aligned, and the time ends at the row's edge, as in Claude Code's workflow rows. No
//! `·` joins the columns, so none floats after the padding.

use ratatui::text::{Line, Span};

use crate::base::text::{spans_width, truncate_spans};

/// Cells between two columns, and the least between the last column and the edge.
pub const GAP: usize = 2;

/// A cell's spans, before padding.
pub type Cell = Vec<Span<'static>>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Right,
}

/// One column, as wide as its widest cell once fitted. Zero leaves it out of every row, gap
/// and all, so a column that gave way never leaves a hole in one row only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Column {
    pub width: usize,
    pub align: Align,
}

impl Column {
    pub fn left(width: usize) -> Self {
        Self {
            width,
            align: Align::Left,
        }
    }

    pub fn right(width: usize) -> Self {
        Self {
            width,
            align: Align::Right,
        }
    }

    /// As wide as the widest of `cells`.
    pub fn widest<'a>(align: Align, cells: impl IntoIterator<Item = &'a Cell>) -> Self {
        let width = cells.into_iter().map(|cell| spans_width(cell)).max();
        Self {
            width: width.unwrap_or(0),
            align,
        }
    }
}

/// One way the columns give up room, tried in order until they fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Give {
    /// Cut the column with `…`, by only what's needed and to no less than `min` cells.
    Cut { column: usize, min: usize },
    /// Take a few cells off the column, at most `most`, and only when that alone makes the
    /// row fit, so a column never loses cells to a squeeze that drops another anyway.
    Trim {
        column: usize,
        most: usize,
        min: usize,
    },
    /// Leave the column out.
    Drop(usize),
}

/// The cells the shown columns take, with the gaps between them.
pub fn used(columns: &[Column]) -> usize {
    let shown = columns.iter().filter(|column| column.width > 0).count();
    columns.iter().map(|column| column.width).sum::<usize>() + GAP * shown.saturating_sub(1)
}

/// Fits `columns` into `room` cells, giving way as `gives` says in order. Past them, the first
/// column still shown is cut to what's left.
pub fn fit(columns: &mut [Column], room: usize, gives: &[Give]) {
    for give in gives {
        let over = used(columns).saturating_sub(room);
        if over == 0 {
            return;
        }
        match *give {
            Give::Cut { column, min } => {
                let column = &mut columns[column];
                column.width = column.width.saturating_sub(over).max(min.min(column.width));
            }
            Give::Trim { column, most, min } => {
                let column = &mut columns[column];
                if over <= most && column.width >= min + over {
                    column.width -= over;
                }
            }
            Give::Drop(column) => columns[column].width = 0,
        }
    }
    let over = used(columns).saturating_sub(room);
    if over > 0
        && let Some(first) = columns.iter_mut().find(|column| column.width > 0)
    {
        first.width = first.width.saturating_sub(over);
    }
}

/// `cell` padded to `column`, or cut with `…` when it's wider.
pub fn aligned(cell: &[Span<'static>], column: Column) -> Cell {
    let cell = truncate_spans(cell, column.width);
    let pad = Span::raw(" ".repeat(column.width.saturating_sub(spans_width(&cell))));
    match column.align {
        Align::Left => cell.into_iter().chain([pad]).collect(),
        Align::Right => [pad].into_iter().chain(cell).collect(),
    }
}

/// One row: `opening`, then each shown column's cell, `GAP` apart, then `edge` ending at `width`.
/// A row too narrow for the edge after its columns leaves it out.
pub fn line(
    opening: Cell,
    columns: &[Column],
    cells: &[Cell],
    edge: Option<Cell>,
    width: usize,
) -> Line<'static> {
    let mut spans = opening;
    let mut first = true;
    for (column, cell) in columns.iter().zip(cells) {
        if column.width == 0 {
            continue;
        }
        if !first {
            spans.push(Span::raw(" ".repeat(GAP)));
        }
        first = false;
        spans.extend(aligned(cell, *column));
    }
    if let Some(edge) = edge {
        let room = width.saturating_sub(spans_width(&spans));
        let edge_width = spans_width(&edge);
        if room >= edge_width + if first { 0 } else { GAP } {
            spans.push(Span::raw(" ".repeat(room - edge_width)));
            spans.extend(edge);
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(text: &str) -> Cell {
        vec![Span::raw(text.to_owned())]
    }

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn cells_pad_to_their_column_and_numbers_sit_right() {
        let columns = [Column::left(8), Column::right(6), Column::right(5)];
        let row = |cells: [&str; 3], edge: &str| {
            let cells: Vec<Cell> = cells.into_iter().map(cell).collect();
            text(&line(cell("  "), &columns, &cells, Some(cell(edge)), 32))
        };
        assert_eq!(
            row(["judge", "339k", "$0.12"], "57s"),
            "  judge       339k  $0.12    57s"
        );
        assert_eq!(
            row(["reviewer", "16k", ""], " 7s"),
            "  reviewer     16k            7s"
        );
    }

    #[test]
    fn a_column_that_gave_way_leaves_no_gap() {
        let columns = [Column::left(5), Column::right(0), Column::right(3)];
        let cells = [cell("judge"), cell("339k"), cell("$1")];
        assert_eq!(
            text(&line(Vec::new(), &columns, &cells, None, 20)),
            "judge   $1"
        );
    }

    #[test]
    fn gives_apply_in_order_until_the_row_fits() {
        let model = 1;
        let tokens = 2;
        let gives = [
            Give::Trim {
                column: model,
                most: 4,
                min: 8,
            },
            Give::Drop(tokens),
            Give::Cut {
                column: model,
                min: 8,
            },
        ];
        let wide = [Column::left(10), Column::left(20), Column::right(8)];
        // 10 + 20 + 8 and two gaps make 42: two cells short trims the model.
        let mut columns = wide;
        fit(&mut columns, 40, &gives);
        assert_eq!(columns.map(|c| c.width), [10, 18, 8]);
        // Further short, the tokens go and the model keeps its width.
        let mut columns = wide;
        fit(&mut columns, 32, &gives);
        assert_eq!(columns.map(|c| c.width), [10, 20, 0]);
        // Then the model is cut, down to its least, and then the label.
        let mut columns = wide;
        fit(&mut columns, 24, &gives);
        assert_eq!(columns.map(|c| c.width), [10, 12, 0]);
        let mut columns = wide;
        fit(&mut columns, 16, &gives);
        assert_eq!(columns.map(|c| c.width), [6, 8, 0]);
        assert_eq!(used(&columns), 16);
    }

    #[test]
    fn a_cell_wider_than_its_column_is_cut() {
        let cut = aligned(&cell("Gemini 3.6 Pro on agy"), Column::left(12));
        assert_eq!(text(&Line::from(cut)), "Gemini 3.6 …");
    }
}
