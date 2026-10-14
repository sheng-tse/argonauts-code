//! Adapted from Codex's `custom_terminal.rs` (Apache-2.0), itself derived from ratatui's
//! `Terminal` (MIT, Copyright (c) 2016-2022 Florian Dehau, Copyright (c) 2023-2025 The Ratatui
//! Developers).

use std::io::{self, Write};

use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal::{Clear, ClearType};
use ratatui::buffer::{Buffer, Cell, CellDiffOption, CellWidth};
use ratatui::style::{Color, Modifier};

use super::sgr::Pen;

#[derive(Debug)]
enum Command<'a> {
    Put { x: u16, y: u16, cell: &'a Cell },
    ClearToEnd { x: u16, y: u16, bg: Color },
}

impl Command<'_> {
    fn position(&self) -> (u16, u16) {
        match self {
            Command::Put { x, y, .. } | Command::ClearToEnd { x, y, .. } => (*x, *y),
        }
    }
}

pub fn flush_diff<W: Write>(writer: &mut W, previous: &Buffer, next: &Buffer) -> io::Result<()> {
    let commands = diff(previous, next);
    if commands.is_empty() {
        return Ok(());
    }
    draw(writer, &commands)
}

fn diff<'b>(a: &Buffer, b: &'b Buffer) -> Vec<Command<'b>> {
    let width = usize::from(a.area.width);
    if a.area != b.area || width == 0 {
        return Vec::new();
    }
    let mut commands = Vec::new();
    let mut last_drawn = vec![0usize; usize::from(a.area.height)];
    for (row_index, last) in last_drawn.iter_mut().enumerate() {
        let start = row_index * width;
        let previous_row = &a.content[start..start + width];
        let row = &b.content[start..start + width];
        let bg = row.last().map_or(Color::Reset, |cell| cell.bg);

        // The rightmost column that still matters: a glyph, a background that is different from the
        // row's trailing one, a modifier, or a forced update. Wide glyphs count their whole
        // width. Everything after it is cleared with one erase.
        let mut last_column = 0usize;
        let mut column = 0usize;
        while column < row.len() {
            let cell = &row[column];
            let cell_width = usize::from(cell.cell_width());
            if cell.symbol() != " "
                || cell.bg != bg
                || crate::text::marks::drawn(cell.modifier) != Modifier::empty()
                || cell.diff_option == CellDiffOption::AlwaysUpdate
            {
                last_column = column + cell_width.saturating_sub(1);
            }
            column += cell_width.max(1);
        }
        *last = last_column;

        let clear_start = last_column + 1;
        if clear_start < row.len() {
            // An equal tail needs no erase, unless a wide glyph before the boundary reaches
            // into it: its continuation cells look blank in both buffers.
            let tail_changed = previous_row[clear_start..] != row[clear_start..];
            let wide_overlaps = previous_row[..clear_start]
                .iter()
                .enumerate()
                .rev()
                .find(|(_, cell)| cell.symbol() != " " || cell.cell_width() > 1)
                .is_some_and(|(column, cell)| {
                    column + usize::from(cell.cell_width()) > clear_start
                });
            if tail_changed || wide_overlaps {
                let (x, y) = a.pos_of(start + clear_start);
                commands.push(Command::ClearToEnd { x, y, bg });
            }
        }
    }

    for (x, y, cell) in a.diff_iter(b) {
        let row = usize::from(y - a.area.y);
        if usize::from(x - a.area.x) <= last_drawn[row] {
            commands.push(Command::Put { x, y, cell });
        }
    }
    // Reading order keeps cursor moves short: a row's cells, then its erase right after them.
    commands.sort_by_key(|command| {
        let (x, y) = command.position();
        (y, x)
    });
    commands
}

fn draw<W: Write>(writer: &mut W, commands: &[Command<'_>]) -> io::Result<()> {
    let mut pen = Pen::default();
    // Known after a write, so adjacent cells need no move.
    let mut cursor: Option<(u16, u16)> = None;
    for command in commands {
        let (x, y) = command.position();
        if cursor != Some((x, y)) {
            queue!(writer, MoveTo(x, y))?;
        }
        match command {
            Command::Put { cell, .. } => {
                pen.set(writer, cell.fg, cell.bg, cell.modifier)?;
                queue!(writer, Print(cell.symbol()))?;
                // Terminals don't always agree with our width tables about wide and zero width
                // symbols, so only a one column cell is trusted to leave the cursor after it.
                cursor = (cell.cell_width() == 1).then(|| (x.saturating_add(1), y));
            }
            Command::ClearToEnd { bg, .. } => {
                pen.prepare_erase(writer, *bg)?;
                queue!(writer, Clear(ClearType::UntilNewLine))?;
                cursor = Some((x, y));
            }
        }
    }
    pen.finish(writer)
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    use super::*;

    fn output(previous: &Buffer, next: &Buffer) -> String {
        crossterm::style::force_color_output(true);
        let mut out = Vec::new();
        flush_diff(&mut out, previous, next).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn equal_buffers_write_nothing() {
        let buffer = Buffer::with_lines(["hello", "world"]);
        assert_eq!(output(&buffer, &buffer), "");
    }

    #[test]
    fn one_changed_cell_writes_one_cell() {
        let previous = Buffer::with_lines(["hello"]);
        let next = Buffer::with_lines(["hallo"]);
        assert_eq!(output(&previous, &next), "\x1b[1;2Ha");
    }

    #[test]
    fn adjacent_cells_share_one_move() {
        let previous = Buffer::with_lines(["hello world"]);
        let next = Buffer::with_lines(["help! world"]);
        assert_eq!(output(&previous, &next), "\x1b[1;4Hp!");
    }

    #[test]
    fn blank_tail_is_one_erase() {
        let previous = Buffer::with_lines(["hello world"]);
        let next = Buffer::with_lines(["hi         "]);
        assert_eq!(output(&previous, &next), "\x1b[1;2Hi\x1b[K");
    }

    #[test]
    fn a_filled_tail_erases_in_its_color() {
        let area = Rect::new(0, 0, 6, 1);
        let previous = Buffer::empty(area);
        let mut next = Buffer::empty(area);
        next.set_style(area, Style::new().bg(Color::Blue));
        next.set_string(0, 0, "ab", Style::new());
        let out = output(&previous, &next);
        assert!(out.ends_with("b\x1b[K\x1b[0m"), "{out:?}");
    }

    #[test]
    fn the_cell_after_a_wide_glyph_moves_there_explicitly() {
        let previous = Buffer::with_lines(["      "]);
        let next = Buffer::with_lines(["你好ab"]);
        assert_eq!(
            output(&previous, &next),
            "\x1b[1;1H你\x1b[1;3H好\x1b[1;5Hab"
        );
    }

    #[test]
    fn replacing_a_wide_glyph_at_the_tail_clears_it() {
        let previous = Buffer::with_lines(["a你  "]);
        let next = Buffer::with_lines(["a    "]);
        let out = output(&previous, &next);
        assert!(out.contains("\x1b[K"), "{out:?}");
    }
}
