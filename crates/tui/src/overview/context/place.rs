//! Text set at fixed columns, so related numbers end in line down the whole Context tab.

use ratatui::text::Span;

use crate::kit::Drawn;
use crate::text::{spans_width, truncate_spans};

/// The least blank cells between two pieces of a row.
pub(super) const GAP: usize = 2;

/// One piece of a row: spans that start at a column, or end just before one.
#[derive(Debug, Clone)]
pub(super) enum Piece {
    At(usize, Vec<Span<'static>>),
    Ending(usize, Vec<Span<'static>>),
}

pub(super) fn at(column: usize, spans: Vec<Span<'static>>) -> Piece {
    Piece::At(column, spans)
}

pub(super) fn ending(column: usize, spans: Vec<Span<'static>>) -> Piece {
    Piece::Ending(column, spans)
}

/// The pieces on one row, in order. A piece that would touch the one before it starts two
/// cells after it instead, so text is pushed right and never drawn over.
pub(super) fn place(pieces: impl IntoIterator<Item = Piece>) -> Vec<Span<'static>> {
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut end = 0;
    for piece in pieces {
        let (wanted, spans) = match piece {
            Piece::At(column, spans) => (column, spans),
            Piece::Ending(column, spans) => (column.saturating_sub(spans_width(&spans)), spans),
        };
        let width = spans_width(&spans);
        if width == 0 {
            continue;
        }
        let start = if row.is_empty() {
            wanted
        } else {
            wanted.max(end + GAP)
        };
        if start > end {
            row.push(Span::raw(" ".repeat(start - end)));
        }
        row.extend(spans);
        end = start + width;
    }
    row
}

/// Two lists side by side, a row of pieces each: both halves' pieces placed together, so the
/// right half keeps its own columns whatever the left half holds.
pub(super) fn beside(left: Vec<Vec<Piece>>, right: Vec<Vec<Piece>>) -> Drawn {
    let rows = left.len().max(right.len());
    let (mut left, mut right) = (left.into_iter(), right.into_iter());
    let mut drawn = Drawn::new();
    for _ in 0..rows {
        let pieces = left
            .next()
            .unwrap_or_default()
            .into_iter()
            .chain(right.next().unwrap_or_default());
        drawn.push(place(pieces));
    }
    drawn
}

/// Lists one under another, a blank row between them.
pub(super) fn stacked(lists: impl IntoIterator<Item = Vec<Vec<Piece>>>) -> Drawn {
    let mut drawn = Drawn::new();
    for list in lists.into_iter().filter(|list| !list.is_empty()) {
        if drawn.height() > 0 {
            drawn.push(Vec::new());
        }
        for row in list {
            drawn.push(place(row));
        }
    }
    drawn
}

/// `text` in at most `cells`, ending in `…` where it was cut.
pub(super) fn clip(text: &str, cells: usize) -> String {
    truncate_spans(&[Span::raw(text.to_owned())], cells)
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// Where one list sits: the column it starts at, and the one its numbers end before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Block {
    pub start: usize,
    pub end: usize,
}

impl Block {
    pub fn cells(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
}

/// The columns the tab's sections share. At 118 cells and wider they are the columns the
/// tab was designed on: the content five cells in, the legend at 47, the left-hand numbers
/// ending at 61, the right-hand lists from 66 with their numbers ending at 110, and the edge
/// at 117. A narrower panel keeps one list a row, and one narrower than the legend needs
/// sets the legend under the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Columns {
    pub width: usize,
    pub left: Block,
    /// The right-hand lists, where the panel has room for two side by side.
    pub right: Option<Block>,
    /// The legend beside the grid, where it fits there.
    pub legend: Option<usize>,
    /// The right edge of what is set flush right.
    pub edge: usize,
}

impl Columns {
    /// The width the tab was designed at. A wider panel keeps these columns.
    pub const DESIGNED: usize = 118;
    const INDENT: usize = 5;
    const LEGEND: usize = 47;
    /// The legend's glyph, name, tokens and share.
    const LEGEND_CELLS: usize = 42;
    const LEFT_CELLS: usize = 56;

    pub fn for_width(width: usize) -> Self {
        if width >= Self::DESIGNED {
            return Self {
                width,
                left: Block {
                    start: Self::INDENT,
                    end: Self::INDENT + Self::LEFT_CELLS,
                },
                right: Some(Block {
                    start: 66,
                    end: 110,
                }),
                legend: Some(Self::LEGEND),
                edge: Self::DESIGNED - 1,
            };
        }
        let beside = width > Self::LEGEND + Self::LEGEND_CELLS;
        let start = if beside { Self::INDENT } else { 2 };
        let edge = width.saturating_sub(1);
        Self {
            width,
            left: Block {
                start,
                end: (start + Self::LEFT_CELLS).min(edge).max(start),
            },
            right: None,
            legend: beside.then_some(Self::LEGEND),
            edge,
        }
    }

    pub fn indent(&self) -> usize {
        self.left.start
    }

    /// Whether the tab is at its designed columns, with room for what only they hold.
    pub fn designed(&self) -> bool {
        self.right.is_some()
    }

    /// Where the right-hand list goes: beside the left one, or in its place under it.
    pub fn second(&self) -> Block {
        self.right.unwrap_or(self.left)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::plain_text;
    use ratatui::text::Line;

    fn text(row: Vec<Span<'static>>) -> String {
        plain_text(&[Line::from(row)])
    }

    #[test]
    fn pieces_start_and_end_at_their_columns() {
        let row = place([
            at(2, vec![Span::raw("Bash")]),
            ending(14, vec![Span::raw("180k")]),
            ending(22, vec![Span::raw("18.0%")]),
        ]);
        assert_eq!(text(row), "  Bash    180k   18.0%");
    }

    #[test]
    fn a_piece_that_would_touch_the_last_is_pushed_right() {
        let row = place([
            at(0, vec![Span::raw("a-long-tool-name")]),
            at(10, vec![Span::raw("server")]),
            ending(20, vec![Span::raw("1.2k")]),
        ]);
        assert_eq!(text(row), "a-long-tool-name  server  1.2k");
    }

    #[test]
    fn an_empty_piece_takes_no_room() {
        let row = place([
            at(1, vec![Span::raw("x")]),
            at(4, Vec::new()),
            at(5, vec![Span::raw("y")]),
        ]);
        assert_eq!(text(row), " x   y");
    }

    #[test]
    fn the_designed_columns_hold_from_118_cells_up() {
        for width in [118, 140, 200] {
            let columns = Columns::for_width(width);
            assert_eq!(columns.left, Block { start: 5, end: 61 });
            assert_eq!(
                columns.right,
                Some(Block {
                    start: 66,
                    end: 110
                })
            );
            assert_eq!((columns.legend, columns.edge), (Some(47), 117));
        }
    }

    #[test]
    fn a_narrower_panel_keeps_one_list_a_row() {
        let medium = Columns::for_width(100);
        assert_eq!(medium.right, None);
        assert_eq!((medium.legend, medium.edge), (Some(47), 99));
        assert_eq!(medium.second(), medium.left);
        let narrow = Columns::for_width(60);
        assert_eq!(narrow.legend, None, "the legend goes under the grid");
        assert_eq!(narrow.left, Block { start: 2, end: 58 });
        assert_eq!(Columns::for_width(40).left.end, 39);
    }
}
