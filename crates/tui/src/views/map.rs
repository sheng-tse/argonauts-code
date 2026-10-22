//! A view that draws a map: the screen puts a picture over the cells it leaves for one, where
//! the terminal draws pictures, and while the map shows the mouse points into it.

use ratatui::layout::Rect;

use super::ViewCtx;

/// Where a view's map drew, in the buffer the view drew in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapCells {
    pub area: Rect,
    /// Whether the cells wait for a picture, and a number that changes whenever what it
    /// shows does. `None` when the map is drawn in the cells themselves.
    pub picture: Option<u64>,
}

/// Where the mouse rests over a map, from the top left of its cells: in pixels where the
/// terminal reports them, else at the middle of the cell, in the same units as the map's size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapPoint {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl MapPoint {
    pub fn inside(&self) -> bool {
        (0.0..self.width).contains(&self.x) && (0.0..self.height).contains(&self.y)
    }
}

pub trait MapView {
    /// Whether the screen puts pictures over the cells left for them, so the map lays out for
    /// one rather than drawing in its cells.
    fn place_pictures(&mut self, yes: bool);

    /// Where the map drew last, while it shows.
    fn map_cells(&self) -> Option<MapCells>;

    /// The picture over the map's cells, as a PNG, at `cell` pixels a cell.
    fn picture(&self, cell: (u16, u16), ctx: &ViewCtx<'_>) -> Option<Vec<u8>>;

    /// The mouse rests at `at` over the map, or has left it. Whether what shows changed.
    fn point(&mut self, at: Option<MapPoint>) -> bool;

    /// A click where the mouse rests. Whether it opened something.
    fn click(&mut self) -> bool;
}
