//! The Behaviors map on the panel: where its cells are among the body's rows, the cells
//! the pointer's line leaves for its picture, and the mouse over it.

use ratatui::layout::Rect;

use super::{OverviewView, Tab};
use crate::base::kit::Kit;
use crate::frame::view::ViewCtx;
use crate::frame::view::map::{MapCells, MapPoint, MapView};
use crate::panels::stats::behaviors;

impl OverviewView {
    /// The mouse rests at `place` on the Behaviors map, in the map's own units, or has left
    /// it. Whether the dot it is nearest changed.
    pub fn point_at_map(&mut self, place: Option<(f32, f32)>) -> bool {
        self.tab == Tab::Overview
            && self.home.on_behaviors()
            && self.home.behaviors.map_shown()
            && self.home.behaviors.point_at(place)
    }

    /// A click on the Behaviors map opens the card of the dot nearest the mouse.
    pub fn click_map(&mut self) -> bool {
        self.tab == Tab::Overview && self.home.on_behaviors() && self.home.behaviors.click()
    }

    /// Whether Behaviors' map is what the panel shows, so the mouse over it is the map's.
    fn behaviors_map_shown(&self) -> bool {
        self.tab == Tab::Overview && self.home.on_behaviors() && self.home.behaviors.map_shown()
    }

    /// Where the Behaviors map's cells are among the body's rows, when the frame shows them
    /// whole: `heights` are the blocks', of which `shown` show from the scroll on, in `room`
    /// rows `width` cells wide. With them, the cells the pointer's line leaves for its picture,
    /// where the line shows too.
    pub(super) fn map_drawn(
        &self,
        kit: &Kit<'_>,
        ctx: &ViewCtx<'_>,
        heights: &[usize],
        shown: usize,
        (width, room): (usize, usize),
    ) -> Option<(MapCells, Option<Rect>)> {
        let tab = &self.home.behaviors;
        if self.tab != Tab::Overview || !self.home.on_behaviors() || !tab.map_shown() {
            return None;
        }
        let block = tab.map_block(kit);
        if self.scroll > block || self.scroll + shown <= block {
            return None;
        }
        // The blocks are one blank row apart.
        let top: usize = heights[self.scroll..block].iter().map(|h| h + 1).sum();
        let rows = heights.get(block).copied()?;
        let fit = tab.fit();
        if rows < fit.rows || top + fit.rows > room {
            return None;
        }
        // The tab's blocks sit one cell in, and the map one more.
        let left = 1 + behaviors::MAP_INSET;
        if left + fit.columns > width {
            return None;
        }
        let cells = |n: usize| u16::try_from(n).ok();
        let area = Rect::new(
            cells(left)?,
            cells(top)?,
            cells(fit.columns)?,
            cells(fit.rows)?,
        );
        let picture = tab
            .draws_world()
            .then(|| tab.picture_key(&self.home.behavior_colours(kit, &ctx.state.names)));
        // The pointer's line opens the block under the map's, or with the list beside the map
        // it sits under the map in the map's own block.
        let (line_top, line_block) = match tab.pointer_line_in_block() {
            Some(down) => (top + down, block),
            None => (top + rows + 1, block + 1),
        };
        let line = (picture.is_some() && line_block < self.scroll + shown && line_top < room)
            .then(|| {
                let column = 1 + usize::from(behaviors::PICTURE_COLUMN);
                let at = (cells(column)?, cells(line_top)?);
                Some(Rect::new(at.0, at.1, behaviors::PICTURE_CELLS, 1))
            })
            .flatten();
        Some((MapCells { area, picture }, line))
    }

    /// The terminal's cells are `cell` pixels, which the Behaviors map is sized for.
    pub fn with_cell(mut self, cell: (u16, u16)) -> Self {
        self.home.behaviors.cell = Some(cell);
        self
    }

    /// The screen places the Behaviors map's picture over the cells the tab leaves for it.
    pub fn with_map_picture(mut self) -> Self {
        self.home.behaviors.picture_placed = true;
        self
    }
}

impl MapView for OverviewView {
    fn place_pictures(&mut self, cell: Option<(u16, u16)>) {
        self.home.behaviors.picture_placed = cell.is_some();
        self.home.behaviors.cell = cell;
    }

    fn map_cells(&self) -> Option<MapCells> {
        self.map_drawn
    }

    fn beside(&self) -> Option<crate::frame::view::map::Beside> {
        let area = self.line_drawn?;
        let picture = self.home.behaviors.line_picture()?;
        Some(crate::frame::view::map::Beside { area, picture })
    }

    fn scene(
        &self,
        cell: (u16, u16),
        ctx: &ViewCtx<'_>,
    ) -> Option<std::sync::Arc<crate::world::Scene>> {
        self.map_cells()?.picture?;
        let kit = Kit::new(ctx.theme);
        let colours = self.home.behavior_colours(&kit, &ctx.state.names);
        self.home
            .behaviors
            .scene(&colours, ctx.theme, &ctx.state.names, cell, ctx.now)
    }

    fn point(&mut self, at: Option<MapPoint>) -> bool {
        self.behaviors_map_shown() && self.home.behaviors.point(at)
    }

    fn click(&mut self) -> bool {
        self.click_map()
    }

    fn wheel(&mut self, at: MapPoint, up: bool, now: std::time::Instant) -> bool {
        self.behaviors_map_shown() && self.home.behaviors.scroll(at, up, now)
    }

    fn drag(&mut self, to: MapPoint) -> bool {
        self.behaviors_map_shown() && self.home.behaviors.drag(to)
    }
}
