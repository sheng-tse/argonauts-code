//! The screen a drawing is compared with: the one grid of cells, as a terminal in a pty shows it
//! or as a test builds it.

use anyhow::ensure;

use super::drawing::COVERED;
use crate::pty::Pty;
use crate::screen::Grid;

/// What the app drew, cell by cell, its placeholders saying whether their picture shows.
pub type Drawn = Grid;

impl Grid {
    /// The screen `pty` shows now, with its pictures as the terminal holds them. The pty must
    /// be `cols` × `rows`, the size the drawing is drawn for.
    pub fn capture(pty: &Pty, cols: u16, rows: u16) -> anyhow::Result<Grid> {
        let shown = pty.cells();
        ensure!(
            (shown.cols(), shown.rows()) == (cols, rows),
            "the pty is {} × {}, not {cols} × {rows}",
            shown.cols(),
            shown.rows()
        );
        Ok(shown)
    }

    /// Row `row` from column `left` for `width` cells as text, a picture's cells as a drawing
    /// writes them, for a report or a drawing. A placeholder whose picture doesn't show is a
    /// blank, as the terminal paints it.
    pub fn text_of(&self, row: u16, left: u16, width: u16) -> String {
        let right = left.saturating_add(width).min(self.cols());
        let mut text = String::new();
        for col in left..right {
            let cell = self.cell(row, col);
            match cell.picture {
                Some(picture) if picture.shows => text.push(COVERED),
                Some(_) => text.push(' '),
                None if cell.is_covered() && col > left => {}
                None if cell.is_covered() => text.push(' '),
                None => text.push_str(&cell.text),
            }
        }
        text.trim_end().to_owned()
    }
}
