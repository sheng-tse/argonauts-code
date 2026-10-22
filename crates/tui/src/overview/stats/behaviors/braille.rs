//! The map in braille, for a terminal that draws no pictures: two dots across and four down in
//! each cell, each cell in the colour of the group with the most dots in it, and behind them the
//! comparison, red where the compared group acts more and blue where less.

use argonauts_core::behavior_map::{BehaviorMap, GRID};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use super::words::Look;
use super::{MAP_COLUMNS, MAP_INSET, MAP_ROWS};
use crate::kit::{Drawn, Kit};
use crate::theme::{ColorLevel, Role, nearest_ansi256};

/// Each dot's bit in its cell's character, by its row and column in the cell.
const BITS: [(usize, usize, u32); 8] = [
    (0, 0, 0x01),
    (1, 0, 0x02),
    (2, 0, 0x04),
    (0, 1, 0x08),
    (1, 1, 0x10),
    (2, 1, 0x20),
    (3, 0, 0x40),
    (3, 1, 0x80),
];
/// A cell is shaded where the difference is more than this share of its largest.
const SHADED: f32 = 0.25;
const MORE: (u8, u8, u8) = (0x4a, 0x2a, 0x33);
const LESS: (u8, u8, u8) = (0x2a, 0x35, 0x50);

fn fill(kit: &Kit<'_>, (r, g, b): (u8, u8, u8)) -> Style {
    match kit.theme.level {
        ColorLevel::TrueColor => Style::new().bg(Color::Rgb(r, g, b)),
        ColorLevel::Ansi256 => Style::new().bg(Color::Indexed(nearest_ansi256((r, g, b)))),
        _ => Style::new(),
    }
}

/// Behind a cell where the compared group acts more.
pub(super) fn more_style(kit: &Kit<'_>) -> Style {
    fill(kit, MORE)
}

/// One braille cell: its dots, the group whose colour it takes, and whether it holds something lit or
/// ringed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Cell {
    pub dots: u32,
    pub group: Option<usize>,
    pub lit: bool,
    pub ringed: bool,
    /// `Some(true)` where the compared group acts more, `Some(false)` where less.
    pub shade: Option<bool>,
}

impl Cell {
    pub fn glyph(&self) -> char {
        match self.dots {
            0 => ' ',
            dots => char::from_u32(0x2800 + dots).unwrap_or(' '),
        }
    }
}

/// The map's cells, row by row from the top.
pub(super) fn cells(
    map: &BehaviorMap,
    overlay: Option<&[f32]>,
    lit: &[usize],
    ringed: Option<usize>,
) -> Vec<Vec<Cell>> {
    let (columns, rows) = (MAP_COLUMNS, MAP_ROWS);
    let mut cells = vec![vec![Cell::default(); columns]; rows];
    // Each cell's groups by their dots, in the order met, so a tie goes to the first.
    let mut counts: Vec<Vec<Vec<(usize, usize)>>> = vec![vec![Vec::new(); columns]; rows];
    for (at, action) in map.actions.iter().enumerate() {
        let column = ((action.x * (columns * 2) as f32) as usize).min(columns * 2 - 1);
        let row = (((1.0 - action.y) * (rows * 4) as f32) as usize).min(rows * 4 - 1);
        let cell = &mut cells[row / 4][column / 2];
        let bit = BITS
            .iter()
            .find(|(r, c, _)| *r == row % 4 && *c == column % 2)
            .map_or(0, |(_, _, bit)| *bit);
        cell.dots |= bit;
        cell.lit |= lit.contains(&at);
        cell.ringed |= ringed == Some(at);
        let seen = &mut counts[row / 4][column / 2];
        match seen.iter_mut().find(|(group, _)| *group == action.group) {
            Some((_, count)) => *count += 1,
            None => seen.push((action.group, 1)),
        }
    }
    for (row, line) in cells.iter_mut().enumerate() {
        for (column, cell) in line.iter_mut().enumerate() {
            let seen = &counts[row][column];
            let most = seen.iter().map(|(_, count)| *count).max().unwrap_or(0);
            cell.group = seen
                .iter()
                .find(|(_, count)| *count == most)
                .map(|(g, _)| *g);
            if let Some(grid) = overlay {
                let x = ((column as f32 + 0.5) / columns as f32 * GRID as f32) as usize;
                let y = ((1.0 - (row as f32 + 0.5) / rows as f32) * GRID as f32) as usize;
                let value = grid
                    .get(y.min(GRID - 1) * GRID + x.min(GRID - 1))
                    .copied()
                    .unwrap_or(0.0);
                cell.shade = (value.abs() > SHADED).then_some(value > 0.0);
            }
        }
    }
    cells
}

/// The braille rows, then the groups' legend under them.
pub(super) fn block(look: &Look<'_>, lit: &[usize], ringed: Option<usize>) -> Drawn {
    let kit = look.kit;
    let overlay = look
        .compared
        .and_then(|group| look.map.comparisons.get(group))
        .and_then(|c| c.overlay.as_deref());
    let mut out = Drawn::new();
    for line in cells(look.map, overlay, lit, ringed) {
        let mut spans = vec![Span::raw(" ".repeat(MAP_INSET))];
        for cell in line {
            let mut style = match cell.shade {
                Some(true) => more_style(kit),
                Some(false) => fill(kit, LESS),
                None => Style::new(),
            };
            if cell.dots != 0 {
                let dim = !lit.is_empty() && !cell.lit && !cell.ringed;
                style = match (cell.ringed, dim, cell.group) {
                    (true, _, _) => style
                        .patch(kit.fg(Role::Bright))
                        .add_modifier(Modifier::BOLD),
                    (_, true, _) => style.patch(kit.fg(Role::Faint)),
                    (_, _, Some(group)) => style.patch(look.colours.style(group)),
                    _ => style,
                };
            }
            spans.push(Span::styled(cell.glyph().to_string(), style));
        }
        out.push(merge(spans));
    }
    out.push(Vec::new());
    for line in look.braille_legend() {
        out.push(line);
    }
    out
}

/// Neighbouring cells of one style as one span.
fn merge(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    for span in spans {
        match out.last_mut() {
            Some(last) if last.style == span.style => {
                last.content = format!("{}{}", last.content, span.content).into();
            }
            _ => out.push(span),
        }
    }
    out
}
