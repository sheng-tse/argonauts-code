//! Where the sub-tab's blocks go in its room: the progress of Summaries and the levels row
//! above the map, the map with its legend beside it or under it, and under the map the
//! pointer's line, the summary and the list in the rows left. Where the room is wide enough,
//! the list runs beside the map instead, under its legend, from the map's top to the key line.
//!
//! The map's size comes from the room and the terminal's cell, by three rules:
//! - its share: the biggest box of the picture's shape within 64 of the window's 120 columns
//!   and 13 of the 19 rows the map and the list share, as at 120 × 38;
//! - its cap: never wider than the one picture stretched 1.3 times, where it stays soft rather
//!   than blurred;
//! - the list beside it: where that gives a bigger map without fewer rows for the list, the map
//!   takes every row the two share, and the legend and the list go beside it.
//!
//! The planet, the universe and the zooms take the map's box, so nothing moves between levels.

use argonauts_core::look::display_names::DisplayNames;
use ratatui::text::Span;

use super::{At, BehaviorsTab, GroupColours, braille, shortcuts, summary, words};
use crate::base::kit::legend::BETWEEN_WORDS;
use crate::base::kit::{Drawn, Kit};
use crate::world::map::fields;

/// The map starts this many cells in, and the legend this many cells after it.
pub const MAP_INSET: usize = 1;
pub const LEGEND_GAP: usize = 2;
/// The cells left blank at the right of the screen, as under the periods.
pub(super) const RIGHT_EDGE: usize = 2;
/// The legend's narrowest, which a screen too narrow to hold it beside the map shows under it
/// instead.
const LEGEND_CELLS: usize = 24;
/// The narrowest the list beside the map gets: one Most common column.
const LIST_CELLS: usize = 55;
/// Rows the legend keeps beside the map where the list runs under it, as many as it had beside
/// a 13-row map, so the list starts on the same row whatever is compared.
const LEGEND_ROWS: usize = 13;

/// The map's share of the window's columns, and of the rows the map and the list share.
const SHARE_COLUMNS: (usize, usize) = (64, 120);
const SHARE_ROWS: (usize, usize) = (13, 19);
/// The most the map's one picture is stretched across its cells.
const MOST_STRETCH: f64 = 1.3;
/// The tab sits one cell in from the window's edge.
const TAB_INSET: usize = 1;

/// A terminal that doesn't say how big its cells are is taken to have cells 17 × 43 px, the
/// shape the screens in `approved/` are drawn at.
pub const ASSUMED_CELL: (u16, u16) = (17, 43);

/// Rows under the map before the list: the blank after the map's block, the pointer's two
/// rows and the headline. Braille keeps a blank before the headline too.
const UNDER_MAP: usize = 4;

/// The map's size in cells, and where its legend and list go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    pub columns: usize,
    pub rows: usize,
    /// The legend sits beside the map; on a screen too narrow for it, under it.
    pub legend_beside: bool,
    /// Where the list runs beside the map under the legend, the rows the two take from the
    /// map's top to the key line. `None` where the list goes under the map.
    pub beside_rows: Option<usize>,
}

impl Default for Fit {
    fn default() -> Self {
        Self::DRAWN
    }
}

impl Fit {
    /// The size the world's numbers are given for, 64 × 13 cells of 18 × 42 px, which the map
    /// has until its room is measured.
    pub const DRAWN: Fit = Fit {
        columns: 64,
        rows: 13,
        legend_beside: true,
        beside_rows: None,
    };

    /// The map in a tab `width` cells wide whose map and list share `free` rows, with `under`
    /// rows under the map before the list, at `cell` pixels a cell.
    pub fn for_room(width: usize, free: usize, under: usize, cell: (u16, u16)) -> Fit {
        let stacked = Self::list_under(width, free, cell);
        let Some((columns, rows)) = Self::list_beside(width, free, cell) else {
            return stacked;
        };
        let beside_rows = free + under;
        let listed = beside_rows.saturating_sub(LEGEND_ROWS + 1);
        let listed_under = free.saturating_sub(stacked.rows);
        if columns * rows <= stacked.columns * stacked.rows || listed < listed_under {
            return stacked;
        }
        Fit {
            columns,
            rows,
            legend_beside: true,
            beside_rows: Some(beside_rows),
        }
    }

    /// The map's share of the room, with the list under it.
    fn list_under(width: usize, free: usize, cell: (u16, u16)) -> Fit {
        let window = width + TAB_INSET;
        let columns = (window * SHARE_COLUMNS.0 / SHARE_COLUMNS.1).min(widest(cell));
        let rows = free * SHARE_ROWS.0 / SHARE_ROWS.1;
        let (columns, rows) = shaped(columns, rows, cell);
        Fit {
            columns,
            rows,
            legend_beside: width >= 1 + MAP_INSET + columns + LEGEND_GAP + LEGEND_CELLS,
            beside_rows: None,
        }
    }

    /// The map in every row it shares with the list, leaving the list its room beside it.
    fn list_beside(width: usize, free: usize, cell: (u16, u16)) -> Option<(usize, usize)> {
        let columns = width
            .checked_sub(MAP_INSET + LEGEND_GAP + RIGHT_EDGE + LIST_CELLS)?
            .min(widest(cell));
        (columns > 0 && free > 0).then(|| shaped(columns, free, cell))
    }

    /// The legend's rows beside the map.
    pub fn legend_rows(&self) -> usize {
        match self.beside_rows {
            Some(_) => LEGEND_ROWS,
            None => self.rows,
        }
    }

    /// The cells beside the map from the legend's column to the screen's right edge, in a tab
    /// `width` cells wide.
    pub fn side_cells(&self, width: usize) -> usize {
        width.saturating_sub(MAP_INSET + self.columns + LEGEND_GAP + RIGHT_EDGE)
    }

    /// The map's size in pixels at `cell` pixels a cell, which the planet and the universe are
    /// drawn at.
    pub fn pixels(&self, cell: (u16, u16)) -> (usize, usize) {
        (
            self.columns * usize::from(cell.0),
            self.rows * usize::from(cell.1),
        )
    }
}

/// The widest map the one picture covers stretched at most [`MOST_STRETCH`] times.
fn widest(cell: (u16, u16)) -> usize {
    (MOST_STRETCH * fields::WIDTH as f64 / f64::from(cell.0.max(1))).floor() as usize
}

/// The biggest box of whole cells within `columns` and `rows` of the picture's shape at `cell`
/// pixels a cell, at least one cell either way.
fn shaped(columns: usize, rows: usize, cell: (u16, u16)) -> (usize, usize) {
    let (columns, rows) = (columns.max(1), rows.max(1));
    let rows_a_column =
        f64::from(cell.0) / f64::from(cell.1.max(1)) * fields::HEIGHT as f64 / fields::WIDTH as f64;
    let whole = |cells: f64| (cells.round_ties_even() as usize).max(1);
    if columns as f64 * rows_a_column <= rows as f64 + 1e-9 {
        (columns, whole(columns as f64 * rows_a_column))
    } else {
        (whole(rows as f64 / rows_a_column), rows)
    }
}

impl BehaviorsTab {
    /// The blocks above the map's: the progress of Summaries while they're made, and the world's
    /// row of levels where the world is drawn. The map's block comes next.
    pub fn map_block(&self, kit: &Kit<'_>) -> usize {
        usize::from(self.deep.above_map(kit).is_some()) + usize::from(self.draws_world())
    }

    /// The map's size and where its legend and list go, as the blocks last laid them out.
    pub fn fit(&self) -> Fit {
        self.fit.get()
    }

    /// How often the world steps at the map's size.
    pub fn rate(&self) -> crate::world::deliver::clock::Rate {
        let fit = self.fit();
        crate::world::deliver::clock::Rate::for_map(fit.columns, fit.rows)
    }

    /// The most rows the panel gives the tab's blocks. The map is sized from them even while
    /// the panel is shorter for want of rows to fill.
    pub fn size_map_for(&mut self, rows: usize) {
        self.most_rows = Some(rows);
    }

    /// How far below the top of the map's block the pointer's line is, where the list beside
    /// the map puts the line in that block rather than in the next.
    pub fn pointer_line_in_block(&self) -> Option<usize> {
        let fit = self.fit();
        fit.beside_rows.map(|_| fit.rows + 1)
    }

    /// The sub-tab's blocks for a screen `width` wide with `room` rows under the tabs: above
    /// the map what [`BehaviorsTab::map_block`] counts, then the map with its legend, then the
    /// pointer's line and the summary.
    pub fn blocks(
        &self,
        kit: &Kit<'_>,
        colours: &GroupColours,
        names: &DisplayNames,
        width: usize,
        room: usize,
    ) -> Vec<Drawn> {
        if let Some(lines) = self.deep.questions(kit, width + 1) {
            return vec![drawn(lines)];
        }
        let Some(map) = &self.map else {
            let mut note = Drawn::new();
            note.push(vec![kit.dim(" Reading what your agents did…")]);
            return vec![note];
        };
        let world = self.draws_world();
        let progress = self.deep.above_map(kit);
        // The levels row and the panel's blank under it. The progress of Summaries, which
        // comes and goes, takes the list's rows rather than the map's.
        let above = 2 * usize::from(world);
        let under = UNDER_MAP + usize::from(!world);
        let free = self.most_rows.unwrap_or(room).saturating_sub(above + under);
        let cell = self.cell.unwrap_or(ASSUMED_CELL);
        let fit = Fit::for_room(width, free, under, cell);
        self.fit.set(fit);
        let look = self.look(kit, map, colours, names, width);
        if let At::Card { row, action } = self.at {
            let at = self.actions_of(row).get(action).copied();
            if let Some(action) = at.and_then(|at| map.actions.get(at)) {
                return vec![look.card(action)];
            }
        }
        let mut blocks = Vec::new();
        // The progress and its gap come out of the list's rows beside the map, not the map's.
        let mut beside = fit.beside_rows;
        if let Some(lines) = progress {
            beside = beside.map(|rows| rows.saturating_sub(lines.len() + 1));
            blocks.push(drawn(lines));
        }
        if world {
            let mut levels = Drawn::new();
            levels.push(look.levels_row());
            blocks.push(levels);
        }
        if let Some(rows) = beside {
            blocks.push(self.beside_block(&look, fit, rows));
            return blocks;
        }
        let lit = self.lit();
        let map_block = match world {
            true => world_block(&look, fit),
            false => braille::block(&look, self.marks(&lit), fit.legend_beside),
        };
        let used: usize =
            blocks.iter().map(|b| b.height() + 1).sum::<usize>() + map_block.height() + 1;
        blocks.push(map_block);
        blocks.push(self.under_map(&look, room.saturating_sub(used)));
        blocks
    }

    /// The rows under the map, in the `room` left for them: the pointer's line, then the
    /// summary's headline and what the keys show under it.
    fn under_map(&self, look: &words::Look<'_>, room: usize) -> Drawn {
        let mut out = self.pointer_and_headline(look);
        for line in self.list_or_keys(look, room.saturating_sub(out.height())) {
            out.push(line);
        }
        out
    }

    /// The pointer's line, then the summary's headline unless the keys show instead.
    fn pointer_and_headline(&self, look: &words::Look<'_>) -> Drawn {
        let mut out = Drawn::new();
        // Two rows for the pointer's line whatever it says, so the summary never moves under
        // the pointer; braille keeps a blank between, as its map always had.
        for line in self.pointer_rows(look) {
            out.push(line);
        }
        if !self.draws_world() {
            out.push(Vec::new());
        }
        if !self.shortcuts {
            out.push(summary::headline(
                look.kit,
                look.map,
                look.width.saturating_sub(1),
            ));
            for line in self.deep.kept_lines(look.kit).unwrap_or_default() {
                out.push(line.spans);
            }
        }
        out
    }

    /// What goes where the list goes, in `room` rows: the keys while they show, the line of
    /// Summaries while they're made, else the list.
    fn list_or_keys(&self, look: &words::Look<'_>, room: usize) -> Vec<Vec<Span<'static>>> {
        if self.shortcuts {
            return shortcuts::rows(look.kit, &self.shortcuts(), look.width);
        }
        match self.deep.under_map(look.kit) {
            Some(line) => vec![unindented(line.spans)],
            None => self
                .listed(look, room)
                .lines
                .into_iter()
                .map(|line| line.spans)
                .collect(),
        }
    }

    /// The map with the pointer's line and the headline under it, and beside them the legend, a
    /// blank and the list, in `rows` rows from the map's top.
    fn beside_block(&self, look: &words::Look<'_>, fit: Fit, rows: usize) -> Drawn {
        let map_cells = MAP_INSET + fit.columns;
        let mut left: Vec<Vec<Span<'static>>> = match self.draws_world() {
            true => (0..fit.rows)
                .map(|_| vec![Span::raw(" ".repeat(map_cells))])
                .collect(),
            false => braille::map_rows(look, self.marks(&self.lit())),
        };
        left.push(Vec::new());
        // The words under the map end a column's blanks before the list beside it, which
        // starts only the legend's gap after the map.
        let under_cells = (map_cells + LEGEND_GAP).saturating_sub(BETWEEN_WORDS);
        let under_map = self.look(
            look.kit,
            look.map,
            look.colours,
            look.names,
            under_cells + 1,
        );
        left.extend(
            self.pointer_and_headline(&under_map)
                .lines
                .into_iter()
                .map(|line| line.spans),
        );
        let mut right: Vec<Vec<Span<'static>>> = match self.draws_world() {
            true => look.world_legend(),
            false => braille::legend_beside(look),
        }
        .iter()
        .map(|words| crate::base::text::truncate_spans(words, look.legend_room()))
        .collect();
        right.resize(fit.legend_rows(), Vec::new());
        right.push(Vec::new());
        let side_cells = fit.side_cells(look.width);
        let side = self.look(look.kit, look.map, look.colours, look.names, side_cells + 1);
        let list_rows = rows.saturating_sub(right.len());
        // Every row beside the map keeps to its column, as the legend's rows do.
        right.extend(
            self.list_or_keys(&side, list_rows)
                .into_iter()
                .map(|row| crate::base::text::truncate_spans(&unindented(row), side_cells)),
        );
        let mut out = Drawn::new();
        for row in 0..rows.max(left.len()) {
            let mut line = left.get(row).cloned().unwrap_or_default();
            if let Some(words) = right.get(row).filter(|words| !words.is_empty()) {
                let used = crate::base::text::spans_width(&line);
                line.push(Span::raw(
                    " ".repeat((map_cells + LEGEND_GAP).saturating_sub(used)),
                ));
                line.extend(words.iter().cloned());
            }
            out.push(line);
        }
        out
    }
}

/// The map's rows left blank for the world's picture, with what ← → steps through beside them,
/// or under them on a screen too narrow to hold it beside.
fn world_block(look: &words::Look<'_>, fit: Fit) -> Drawn {
    let legend = look.world_legend();
    let mut out = Drawn::new();
    let blank = || vec![Span::raw(" ".repeat(MAP_INSET + fit.columns))];
    if fit.legend_beside {
        for row in 0..fit.rows {
            let mut line = blank();
            if let Some(words) = legend.get(row).filter(|words| !words.is_empty()) {
                line.push(Span::raw(" ".repeat(LEGEND_GAP)));
                line.extend(crate::base::text::truncate_spans(words, look.legend_room()));
            }
            out.push(line);
        }
        return out;
    }
    for _ in 0..fit.rows {
        out.push(Vec::new());
    }
    out.push(Vec::new());
    for words in legend {
        let mut line = vec![Span::raw(" ")];
        line.extend(words);
        out.push(line);
    }
    out
}

/// Rows the flow of Summaries drew, as a block of the tab's. They carry the tab's whole
/// margin, as they'd stand alone, and the panel's inset is one of its cells.
fn drawn(lines: Vec<ratatui::text::Line<'static>>) -> Drawn {
    let mut out = Drawn::new();
    for line in lines {
        out.push(unindented(line.spans));
    }
    out
}

/// A row without the one blank it starts with.
fn unindented(mut spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    if let Some(first) = spans.first_mut()
        && let Some(rest) = first.content.strip_prefix(' ')
    {
        first.content = rest.to_owned().into();
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    const YOURS: (u16, u16) = (17, 43);
    const DRAWN_CELL: (u16, u16) = (18, 42);

    /// The map at a window `columns` × `rows`: the panel keeps six rows for the conversation,
    /// five for its head and two for its key line, and the world's levels row and a blank sit
    /// above the map.
    fn at(columns: usize, rows: usize, cell: (u16, u16)) -> Fit {
        Fit::for_room(columns - 1, free(rows), UNDER_MAP, cell)
    }

    fn free(rows: usize) -> usize {
        rows - 6 - 5 - 2 - 2 - UNDER_MAP
    }

    fn size(fit: Fit) -> (usize, usize, bool) {
        (fit.columns, fit.rows, fit.beside_rows.is_some())
    }

    #[test]
    fn the_map_grows_with_the_window_up_to_its_cap() {
        assert_eq!(size(at(80, 24, YOURS)), (16, 3, false));
        assert_eq!(size(at(100, 30, YOURS)), (37, 7, false));
        assert_eq!(size(at(120, 38, YOURS)), (64, 12, false));
        assert_eq!(size(at(140, 40, YOURS)), (79, 15, true));
        assert_eq!(size(at(177, 40, YOURS)), (88, 16, true));
        assert_eq!(size(at(200, 50, YOURS)), (88, 16, false));
        assert_eq!(size(at(240, 60, YOURS)), (88, 16, false));
        assert_eq!(size(at(265, 60, YOURS)), (88, 16, false));
    }

    #[test]
    fn on_the_cells_the_picture_is_drawn_for_the_map_keeps_its_first_size() {
        assert_eq!(size(at(120, 38, DRAWN_CELL)), (64, 13, false));
        assert_eq!(free(38) - at(120, 38, DRAWN_CELL).rows, 6);
        assert_eq!(size(at(177, 40, DRAWN_CELL)), (83, 17, true));
    }

    #[test]
    fn beside_the_map_the_legend_and_the_list_run_to_the_key_line() {
        let fit = at(177, 40, YOURS);
        assert_eq!(fit.beside_rows, Some(25));
        assert_eq!(fit.legend_rows(), 13);
        assert_eq!(fit.side_cells(176), 83);
        let under = at(120, 38, YOURS);
        assert_eq!(under.legend_rows(), 12);
        assert!(under.legend_beside);
        assert_eq!(free(38) - under.rows, 7);
    }

    #[test]
    fn the_cap_holds_the_picture_to_a_soft_stretch() {
        assert_eq!(widest(YOURS), 88);
        assert_eq!(widest(DRAWN_CELL), 83);
        let stretch = at(265, 60, YOURS).pixels(YOURS).0 as f64 / fields::WIDTH as f64;
        assert!(stretch <= MOST_STRETCH, "{stretch}");
    }

    #[test]
    fn a_room_with_no_rows_still_gets_a_map_one_row_tall() {
        let fit = Fit::for_room(60, 0, UNDER_MAP, YOURS);
        assert_eq!(fit.rows, 1);
        assert!(fit.columns >= 1);
    }
}
