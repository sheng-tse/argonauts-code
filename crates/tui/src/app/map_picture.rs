//! A docked view's map on screen, such as the Behaviors map: its picture goes over the cells
//! the view leaves for one, where the terminal draws pictures, the way the header's mascot
//! goes out, and while the map shows the mouse points into it. The terminal then reports every
//! move of the mouse, in pixels where it can (Ghostty, Kitty and WezTerm), and the moment the
//! map leaves the screen the mouse goes back to how it was.

use std::io::{self, Write};

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use super::{Ui, UiAction};
use crate::mascot::Graphics;
use crate::mascot::graphics;
use crate::terminal::setup;
use crate::views::ViewCtx;
use crate::views::map::{MapPoint, MapView};

/// The kitty protocol's first id for a map's picture, clear of the mascot's.
const FIRST_ID: u32 = 0x4d50_0000;
/// How long the note on selecting text inline stays.
const SHIFT_HINT_FOR: std::time::Duration = std::time::Duration::from_secs(6);

/// Every move of the mouse, with a button down or not.
const MOTION_ON: &str = "\x1b[?1003h";
/// Positions in pixels rather than cells, for the dot nearest the pointer.
const PIXELS_ON: &str = "\x1b[?1016h";
const MOTION_OFF: &str = "\x1b[?1016l\x1b[?1003l";
/// The inline screen reports no mouse of its own, so the map's is all of it.
const INLINE_OFF: &str = "\x1b[?1016l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l";

#[derive(Debug)]
pub(super) struct DockedMap {
    /// What the frame being drawn shows of the map.
    wanted: Option<Wanted>,
    /// Where the last frame showed the map, on screen.
    shown: Option<Rect>,
    /// The picture on screen.
    placed: Option<Placed>,
    next_id: u32,
    /// While the map's mouse is on: whether positions come in pixels.
    pointer: Option<bool>,
    /// The mouse rests over the map.
    hovering: bool,
    /// The terminal may have lost the picture, as after a resize, so it goes out again.
    resend: bool,
    /// The terminal said it reports the mouse in pixels when asked.
    pixels: bool,
    /// Inline, the terminal's own selection takes Shift while the map's mouse is on, which is
    /// said once.
    told_of_shift: bool,
}

/// Said once, inline, the first time the map takes the mouse.
const SHIFT_SELECTS: &str = "The map follows the mouse; hold Shift to select text meanwhile";

impl Default for DockedMap {
    fn default() -> Self {
        Self {
            wanted: None,
            shown: None,
            placed: None,
            next_id: FIRST_ID,
            pointer: None,
            hovering: false,
            resend: false,
            pixels: false,
            told_of_shift: false,
        }
    }
}

#[derive(Debug)]
struct Wanted {
    cells: Rect,
    /// The picture's key, and its PNG when it has to go out again.
    picture: Option<(u64, Option<Vec<u8>>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placed {
    cells: Rect,
    key: u64,
    cell: (u16, u16),
    graphics: Graphics,
    id: u32,
}

impl DockedMap {
    /// What the docked view drew of its map, with the dock's first row at `top` on screen: the
    /// cells it shows in and, at `cell` pixels a cell, the picture to put over them when it
    /// isn't there already.
    pub(super) fn saw(
        &mut self,
        view: Option<&mut dyn MapView>,
        top: u16,
        cell: Option<(u16, u16)>,
        ctx: &ViewCtx<'_>,
    ) {
        self.wanted = view.and_then(|view| {
            let drawn = view.map_cells()?;
            let cells = Rect {
                y: top.checked_add(drawn.area.y)?,
                ..drawn.area
            };
            let picture = drawn.picture.zip(cell).map(|(key, cell)| {
                let there = self.placed.is_some_and(|placed| {
                    (placed.cells, placed.key, placed.cell) == (cells, key, cell)
                });
                let png = (self.resend || !there)
                    .then(|| view.picture(cell, ctx))
                    .flatten();
                (key, png)
            });
            Some(Wanted { cells, picture })
        });
    }
}

/// What goes out once a frame drew: the mouse's modes, the old picture taken away, and the
/// new one from the map's first cell.
#[derive(Debug, Default)]
pub(super) struct MapPlacing {
    escapes: Vec<u8>,
    picture: Option<(Position, Vec<u8>)>,
}

impl<W: Write> Ui<W> {
    /// The terminal said, at start, that it reports the mouse in pixels when asked.
    pub fn use_pixel_mouse(&mut self, yes: bool) {
        self.map.pixels = yes;
    }

    /// Whether a docked map's picture goes over its cells: wherever the terminal draws
    /// pictures, whatever the header's mascot is set to.
    pub(super) fn places_map_pictures(&self) -> bool {
        self.picture_terminal().is_some()
    }

    /// Before the frame draws: what puts the map's picture where the frame shows it, or takes
    /// it away, and the mouse's modes to match. iTerm2 draws a picture into the cells, so where
    /// one was is written again by the frame about to draw.
    pub(super) fn place_map(&mut self) -> MapPlacing {
        let wanted = self.map.wanted.take();
        let mut out = MapPlacing::default();
        self.map.shown = wanted.as_ref().map(|wanted| wanted.cells);
        if self.map.shown.is_none() {
            self.map.hovering = false;
        }
        self.mouse_for_map(&mut out.escapes);
        let picture = wanted.and_then(|wanted| Some((wanted.cells, wanted.picture?)));
        let terminal = self.picture_terminal();
        match (picture, terminal) {
            (Some((cells, (key, Some(png)))), Some((graphics, cell))) => {
                self.take_map_picture_off(&mut out.escapes, Some(cells));
                let id = self.map.next_id;
                self.map.next_id = self.map.next_id.wrapping_add(1).max(FIRST_ID);
                let escapes = graphics::draw_over(
                    graphics,
                    &png,
                    id,
                    (usize::from(cells.width), usize::from(cells.height)),
                );
                out.picture = Some((Position::new(cells.x, cells.y), escapes));
                self.map.placed = Some(Placed {
                    cells,
                    key,
                    cell,
                    graphics,
                    id,
                });
                self.map.resend = false;
            }
            // The picture there already shows this.
            (Some((_, (_, None))), Some(_)) => {}
            _ => self.take_map_picture_off(&mut out.escapes, None),
        }
        out
    }

    /// Takes the picture on screen away, unless the next one goes over the same `cells`.
    fn take_map_picture_off(&mut self, escapes: &mut Vec<u8>, next: Option<Rect>) {
        let Some(placed) = self.map.placed.take() else {
            return;
        };
        match placed.graphics {
            Graphics::Kitty => escapes.extend(graphics::forget([placed.id])),
            Graphics::Iterm if Some(placed.cells) != next => {
                if self.scrollback.is_some() {
                    self.terminal.redraw_cells(placed.cells);
                } else {
                    self.terminal.invalidate();
                }
            }
            Graphics::Iterm => {}
        }
    }

    /// The mouse reports every move while the map shows, and goes back once it doesn't.
    fn mouse_for_map(&mut self, escapes: &mut Vec<u8>) {
        let inline = self.scrollback.is_none();
        match (self.map.shown.is_some(), self.map.pointer) {
            (true, None) => {
                let pixels = self.pixel_mouse().is_some();
                if inline {
                    escapes.extend(setup::MOUSE_ON.as_bytes());
                    setup::note_mouse(true);
                    if !std::mem::replace(&mut self.map.told_of_shift, true) {
                        self.model.bottom.hint = Some(crate::bottom::Hint {
                            text: SHIFT_SELECTS.to_owned(),
                            until: std::time::Instant::now() + SHIFT_HINT_FOR,
                        });
                    }
                }
                escapes.extend(MOTION_ON.as_bytes());
                if pixels {
                    escapes.extend(PIXELS_ON.as_bytes());
                }
                self.map.pointer = Some(pixels);
            }
            (false, Some(_)) => {
                escapes.extend(if inline { INLINE_OFF } else { MOTION_OFF }.as_bytes());
                if inline {
                    setup::note_mouse(false);
                }
                self.map.pointer = None;
            }
            _ => {}
        }
    }

    /// The cell size to read pixel positions by, where the terminal said it reports them.
    fn pixel_mouse(&self) -> Option<(u16, u16)> {
        match self.picture_terminal()? {
            (Graphics::Kitty, cell) if self.map.pixels => Some(cell),
            _ => None,
        }
    }

    /// Sends what [`Self::place_map`] made, after the frame drew.
    pub(super) fn send_map_placing(&mut self, placing: MapPlacing) -> io::Result<()> {
        self.terminal.send_escapes(None, &placing.escapes)?;
        match placing.picture {
            Some((at, escapes)) => self.terminal.send_escapes(Some(at), &escapes),
            None => Ok(()),
        }
    }

    /// Before the terminal is given away or the app ends: the picture goes and the mouse goes
    /// back. The next frame after it comes back places it again.
    pub(super) fn let_go_of_map(&mut self) -> io::Result<()> {
        let placing = self.place_map();
        self.send_map_placing(placing)
    }

    /// After a resize or the terminal came back, the picture may be gone, so it goes out again.
    pub(super) fn resend_map(&mut self) {
        self.map.resend = true;
    }

    /// A mouse event the map takes: a move over it rings the dot nearest, a move off it lets
    /// go, and a click on it opens what the dot is. `None` leaves the event to the rest.
    pub(super) fn mouse_on_map(&mut self, mouse: MouseEvent) -> Option<Vec<UiAction>> {
        let pixels = self.map.pointer?;
        let cells = self.map.shown?;
        let (cw, ch) = match pixels.then(|| self.pixel_mouse()).flatten() {
            Some((w, h)) => (f32::from(w), f32::from(h)),
            None => self
                .picture_terminal()
                .map_or((1.0, 1.0), |(_, (w, h))| (f32::from(w), f32::from(h))),
        };
        let (x, y) = if pixels {
            (f32::from(mouse.column), f32::from(mouse.row))
        } else {
            (
                (f32::from(mouse.column) + 0.5) * cw,
                (f32::from(mouse.row) + 0.5) * ch,
            )
        };
        let point = MapPoint {
            x: x - f32::from(cells.x) * cw,
            y: y - f32::from(cells.y) * ch,
            width: f32::from(cells.width) * cw,
            height: f32::from(cells.height) * ch,
        };
        let view = self.views.last_mut()?.map()?;
        match mouse.kind {
            MouseEventKind::Moved => {
                let at = point.inside().then_some(point);
                if at.is_some() || self.map.hovering {
                    self.map.hovering = at.is_some();
                    self.dirty |= view.point(at);
                }
                Some(Vec::new())
            }
            MouseEventKind::Down(MouseButton::Left) if point.inside() && view.click() => {
                self.map.hovering = false;
                self.dirty = true;
                Some(Vec::new())
            }
            _ => None,
        }
    }

    /// A mouse event in cells, as the rest of the screen reads them, while the map's mouse
    /// reports pixels.
    pub(super) fn mouse_in_cells(&self, mut mouse: MouseEvent) -> MouseEvent {
        if self.map.pointer == Some(true)
            && let Some((width, height)) = self.pixel_mouse()
        {
            let screen = self.terminal.screen_size();
            mouse.column = (mouse.column / width.max(1)).min(screen.width.saturating_sub(1));
            mouse.row = (mouse.row / height.max(1)).min(screen.height.saturating_sub(1));
        }
        mouse
    }
}

#[cfg(test)]
#[path = "map_picture_tests.rs"]
mod tests;
