//! Screens as cells, and the laws every drawn screen keeps at the sizes people really use.
//!
//! A [`Grid`] is one screen, from a ratatui buffer turned cell by cell, the screen a terminal in
//! a pty shows, the bytes a program wrote, or plain rows of text. The approved drawings are
//! compared with it, and [`laws`] checks it: the selected row is drawn, columns keep their
//! room, no two models look alike, nothing is cut while there's room to show it whole, and a
//! list that hides rows says so.

mod colour;
mod grid;
pub mod laws;
mod tokens;

#[cfg(test)]
mod tests;

use std::fmt;

pub use colour::{Colour, Rgb};
pub use grid::{Area, Cell, Grid, PLACEHOLDER, Placeholder};
pub use laws::{Breach, Law, Look};

/// A terminal's size in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

impl Size {
    pub const fn new(cols: u16, rows: u16) -> Size {
        Size { cols, rows }
    }
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} × {}", self.cols, self.rows)
    }
}

/// The sizes every panel is drawn at: a laptop's terminal, a wide window, the classic
/// terminal, and a large monitor.
pub const REAL_SIZES: [Size; 4] = [
    Size::new(120, 38),
    Size::new(177, 40),
    Size::new(80, 24),
    Size::new(265, 60),
];
