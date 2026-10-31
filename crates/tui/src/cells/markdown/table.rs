//! Tables draw in box characters as Claude Code draws them: a rule between every row, headers
//! centred, each cell in the middle of its row, and the borders in the text colour. Columns get
//! Claude Code's widths. A table whose cell needs more than four lines, or that is wider than its
//! room, is drawn as records of `Header: value` lines instead.

use std::ops::Range;

use pulldown_cmark::Alignment;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::base::text::{spans_width, str_width, wrap_spans};
use crate::base::theme::{Role, Theme};

/// Each column's frame cells besides its text: the border on its left and a space each side.
const COLUMN_FRAME: usize = 3;
/// The narrowest a column gets.
const MIN_COLUMN: usize = 3;
/// A table with a cell that needs more lines than this is drawn as records.
const MOST_LINES: usize = 4;
/// Rows drawn; the ones past them are counted on a line of their own.
const MOST_ROWS: usize = 200;
/// The longest line between records.
const RECORD_RULE: usize = 40;

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

    /// The table in a box at most `room` cells wide, or as records in `width` columns when the
    /// box doesn't fit. `None` only when there is no room at all.
    pub(super) fn layout(
        &self,
        width: usize,
        room: usize,
        theme: &Theme,
    ) -> Option<Vec<Vec<Span<'static>>>> {
        if width == 0 {
            return None;
        }
        let Some((header, body)) = self.rows.split_first() else {
            return Some(Vec::new());
        };
        let columns = self
            .aligns
            .len()
            .max(self.rows.iter().map(Vec::len).max().unwrap_or(0));
        if columns == 0 {
            return Some(Vec::new());
        }
        let hidden = body.len().saturating_sub(MOST_ROWS);
        let body = &body[..body.len() - hidden];
        let widths = column_widths(header, body, columns, room);
        let frame = Frame::for_theme(theme);
        let wrapped = |row: &[Cell]| -> Vec<Vec<Line<'static>>> {
            widths
                .iter()
                .enumerate()
                .map(|(col, width)| wrap_cell(row.get(col), *width))
                .collect()
        };
        let header_lines = wrapped(header);
        let body_lines: Vec<_> = body.iter().map(|row| wrapped(row)).collect();
        let tallest = std::iter::once(&header_lines)
            .chain(&body_lines)
            .flatten()
            .map(Vec::len)
            .max()
            .unwrap_or(1);
        let box_width = 1 + widths
            .iter()
            .map(|width| width + COLUMN_FRAME)
            .sum::<usize>();
        if tallest > MOST_LINES || box_width > room {
            return Some(self.records(header, body, hidden, width, &frame));
        }
        let mut out = vec![frame.rule(&widths, frame.top)];
        out.extend(self.row_lines(header_lines, &widths, true, &frame));
        out.push(frame.rule(&widths, frame.middle));
        let last = body_lines.len().saturating_sub(1);
        for (at, row) in body_lines.into_iter().enumerate() {
            out.extend(self.row_lines(row, &widths, false, &frame));
            if at < last {
                out.push(frame.rule(&widths, frame.middle));
            }
        }
        out.push(frame.rule(&widths, frame.bottom));
        if hidden > 0 {
            out.push(vec![Span::raw(more_rows(hidden))]);
        }
        Some(out)
    }

    /// The narrow layout, as Claude Code draws it: each row a record of a bold `Header:` line per
    /// cell with its value beside it, the value's later lines from the left edge, and a line
    /// between records.
    fn records(
        &self,
        header: &[Cell],
        body: &[Vec<Cell>],
        hidden: usize,
        width: usize,
        frame: &Frame,
    ) -> Vec<Cell> {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let rule = vec![Span::styled(
            frame
                .across
                .repeat(width.saturating_sub(1).min(RECORD_RULE)),
            frame.style,
        )];
        // A header row alone shows its cells as values.
        let rows: Vec<Vec<Cell>> = if body.is_empty() {
            vec![header.to_vec()]
        } else {
            body.to_vec()
        };
        let labelled = !body.is_empty();
        let mut out: Vec<Cell> = Vec::new();
        for row in &rows {
            let mut record = Vec::new();
            for (col, cell) in row.iter().enumerate() {
                let label: String = labelled
                    .then(|| header.get(col))
                    .flatten()
                    .map(|label| plain(label).trim().to_owned())
                    .unwrap_or_default();
                let value = collapsed(cell);
                if label.is_empty() && spans_width(&value) == 0 {
                    continue;
                }
                let first_room = if label.is_empty() {
                    width.saturating_sub(1)
                } else {
                    width.saturating_sub(str_width(&label) + 3)
                };
                let lines = wrap_spans(&value, first_room.max(10), &[], &[]);
                let (first, later) = lines
                    .split_first()
                    .map_or((Vec::new(), &[][..]), |(first, later)| {
                        (first.spans.clone(), later)
                    });
                let mut line = Vec::new();
                if !label.is_empty() {
                    line.push(Span::styled(format!("{label}:"), bold));
                    line.push(Span::raw(" "));
                }
                line.extend(first);
                record.push(line);
                if !later.is_empty() {
                    let rest: Vec<Span<'static>> = later
                        .iter()
                        .enumerate()
                        .flat_map(|(at, line)| {
                            let gap = (at > 0).then(|| Span::raw(" "));
                            gap.into_iter().chain(line.spans.iter().cloned())
                        })
                        .collect();
                    record.extend(
                        wrap_spans(&rest, width.saturating_sub(3).max(1), &[], &[])
                            .into_iter()
                            .map(|line| line.spans)
                            .filter(|spans| spans_width(spans) > 0),
                    );
                }
            }
            if record.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(rule.clone());
            }
            out.extend(record);
        }
        if hidden > 0 {
            if !out.is_empty() {
                out.push(rule);
            }
            out.push(vec![Span::raw(more_rows(hidden))]);
        }
        out
    }

    /// A row's lines in the box: each cell in the middle of the row, half the lines it lacks
    /// above it (rounded down), a header cell centred and a body cell aligned as its column.
    fn row_lines(
        &self,
        wrapped: Vec<Vec<Line<'static>>>,
        widths: &[usize],
        header: bool,
        frame: &Frame,
    ) -> Vec<Cell> {
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
        let tops: Vec<usize> = wrapped
            .iter()
            .map(|lines| (height - lines.len()) / 2)
            .collect();
        (0..height)
            .map(|line| {
                let mut spans: Cell = vec![frame.edge()];
                for (col, width) in widths.iter().enumerate() {
                    spans.push(Span::raw(" "));
                    let content = line
                        .checked_sub(tops[col])
                        .and_then(|at| wrapped[col].get(at))
                        .map(|l| l.spans.clone())
                        .unwrap_or_default();
                    let room = width.saturating_sub(spans_width(&content));
                    let align = if header {
                        Alignment::Center
                    } else {
                        self.aligns.get(col).copied().unwrap_or(Alignment::None)
                    };
                    let (left, right) = match align {
                        Alignment::Right => (room, 0),
                        Alignment::Center => (room / 2, room - room / 2),
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

/// Claude Code's widths. Each column wants its longest cell and needs its longest word, never
/// under three cells, and the text gets `room` less the borders and padding. When every column's
/// longest cell fits, that is its width. Otherwise each gets its longest word and a share of what
/// is left in proportion to how much more it wants, rounded down. When even the longest words
/// don't fit, they shrink in proportion, and words break.
fn column_widths(header: &[Cell], body: &[Vec<Cell>], columns: usize, room: usize) -> Vec<usize> {
    // A column's widest `of` its cells, the header's included.
    let measure = |col: usize, of: fn(&Cell) -> usize| -> usize {
        std::iter::once(header)
            .chain(body.iter().map(Vec::as_slice))
            .filter_map(|row| row.get(col))
            .map(of)
            .max()
            .unwrap_or(0)
            .max(MIN_COLUMN)
    };
    let wants: Vec<usize> = (0..columns)
        .map(|col| measure(col, |cell| spans_width(cell)))
        .collect();
    let needs: Vec<usize> = (0..columns)
        .map(|col| measure(col, |cell| longest_word(cell)))
        .collect();
    let text_room = room
        .saturating_sub(1 + COLUMN_FRAME * columns)
        .max(MIN_COLUMN * columns);
    let (wanted, needed) = (wants.iter().sum::<usize>(), needs.iter().sum::<usize>());
    if wanted <= text_room {
        return wants;
    }
    if needed <= text_room {
        let spare = (text_room - needed) as f64;
        let more: Vec<usize> = wants
            .iter()
            .zip(&needs)
            .map(|(want, need)| want - need)
            .collect();
        let all_more = more.iter().sum::<usize>() as f64;
        // In Claude Code's order: each column's share of the whole, times what is left.
        return needs
            .iter()
            .zip(&more)
            .map(|(need, more)| need + (*more as f64 / all_more * spare).floor() as usize)
            .collect();
    }
    let scale = text_room as f64 / needed as f64;
    needs
        .iter()
        .map(|need| ((*need as f64 * scale).floor() as usize).max(MIN_COLUMN))
        .collect()
}

/// A cell's lines at `width`. A word wider than its column breaks, which happens only once the
/// longest words were shrunk to fit.
fn wrap_cell(cell: Option<&Cell>, width: usize) -> Vec<Line<'static>> {
    let cell = cell.cloned().unwrap_or_default();
    let lines: Vec<Line<'static>> = wrap_spans(&cell, width.max(1), &[], &[])
        .into_iter()
        .filter(|line| spans_width(&line.spans) > 0)
        .collect();
    if lines.is_empty() {
        vec![Line::default()]
    } else {
        lines
    }
}

/// `… 12 more rows not shown`, as Claude Code counts the rows past its 200.
fn more_rows(count: usize) -> String {
    let rows = if count == 1 { "row" } else { "rows" };
    format!("… {} more {rows} not shown", thousands(count))
}

fn thousands(count: usize) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A cell's text without its styles.
fn plain(cell: &[Span<'_>]) -> String {
    cell.iter().map(|span| span.content.as_ref()).collect()
}

/// A cell on one line, its runs of whitespace one space each, styles kept.
fn collapsed(cell: &[Span<'static>]) -> Cell {
    let mut out: Cell = Vec::new();
    let mut gap = false;
    for span in cell {
        let mut text = String::new();
        for c in span.content.chars() {
            if c.is_whitespace() {
                gap = true;
                continue;
            }
            if gap && (!text.is_empty() || !out.is_empty()) {
                text.push(' ');
            }
            gap = false;
            text.push(c);
        }
        if !text.is_empty() {
            out.push(Span::styled(text, span.style));
        }
    }
    out
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
        // Claude Code writes the box with no colour of its own, so it takes the terminal's text
        // colour, as the words do.
        let style = theme.style(Role::Text);
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
