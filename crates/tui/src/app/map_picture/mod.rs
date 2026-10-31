//! A docked view's map on screen, such as the Behaviors map: its scene goes over the cells the
//! view leaves for one, where the terminal draws pictures, and while the map shows the mouse
//! points into it. The terminal then reports every move of the mouse, in pixels where it can
//! (Ghostty, Kitty and WezTerm), and the moment the map leaves the screen the mouse goes back to
//! how it was.
//!
//! The kitty protocol keeps the scene's pictures and places each part on its own, so a scene
//! that changes sends only what changed. iTerm2 keeps no pictures, so it gets the scene drawn
//! into one, again only when what it shows changed, and holds still. Either way the pictures
//! are packed off the frame, and the last one stays until the next is ready: the screen draws
//! the frame that sends it when it arrives, and no frame looks for it before.
//!
//! A scene that moves is asked for again at each step of one clock, 8 a second, which wakes the
//! app only while the map's cells were in the last frame and its rules let it run: see
//! [`crate::world::deliver::clock`].

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use super::{Ui, UiAction};
use crate::base::mascot::Graphics;
use crate::base::mascot::graphics;
use crate::frame::view::ViewCtx;
use crate::frame::view::map::{MapPoint, MapView};
use crate::terminal::setup;
use crate::world::deliver::clock::{IDLE_AFTER, Showing, Sources, Steps};
use crate::world::deliver::worker::{self, Cancel, Worker};
use crate::world::deliver::{self, Spot, Stage};
use crate::world::{Depth, Scene};

/// The kitty protocol's first id for a map's pictures, clear of the mascot's.
const FIRST_ID: u32 = 0x4d50_0000;
/// The first id for the pictures beside the map's line, clear of the map's own.
const LINE_FIRST_ID: u32 = 0x4d4c_0000;
/// Bytes of pixels the terminal keeps for the line's pictures: some hundred of them.
const LINE_KEPT_BYTES: usize = 4 << 20;
/// How long the note on selecting text inline stays.
const SHIFT_HINT_FOR: Duration = Duration::from_secs(6);
/// How long the terminal keeps the map's pictures once they leave the screen, so coming
/// straight back sends none of them again.
const KEEP_AFTER_LEAVING: Duration = Duration::from_secs(60);

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
    /// The scene last asked of the map, and what for.
    asked: Option<Asked>,
    /// The pictures the kitty protocol keeps for the map, and where each part shows.
    stage: Stage,
    /// The same for the picture beside the map's line.
    line: Stage,
    /// When the map's pictures last left the screen, while the terminal still keeps them.
    left_at: Option<Instant>,
    /// The whole picture iTerm2 shows.
    whole: Option<Whole>,
    /// The picture iTerm2 shows beside the map's line.
    whole_line: Option<Whole>,
    /// Draws iTerm2's whole pictures off the frame: the escapes that draw one, from its cell.
    whole_packing: Worker<(Position, Vec<u8>)>,
    /// Whole pictures drawn and not yet sent, by their key.
    whole_packed: HashMap<u64, (Position, Vec<u8>)>,
    /// Escapes that go out before the next frame's placements, such as the placements a
    /// resize took off.
    waiting: Vec<u8>,
    /// While the map's mouse is on: whether positions come in pixels.
    pointer: Option<bool>,
    /// The mouse rests over the map.
    hovering: bool,
    /// The terminal said it reports the mouse in pixels when asked.
    pixels: bool,
    /// Inline, the terminal's own selection takes Shift while the map's mouse is on, which is
    /// said once.
    told_of_shift: bool,
    /// Focus, input and power, which say when the map's motion may wake the app.
    sources: Sources,
    /// The motion's steps, from when the map last came on screen.
    steps: Option<Steps>,
    /// The terminal keeps pictures, so a step sends only what changed.
    kept_pictures: bool,
}

/// Said once, inline, the first time the map takes the mouse.
const SHIFT_SELECTS: &str = "The map follows the mouse; hold Shift to select text meanwhile";

impl DockedMap {
    /// A map with nothing on screen yet, its input counted from `started`.
    pub(super) fn new(started: Instant) -> Self {
        Self {
            wanted: None,
            shown: None,
            asked: None,
            stage: stage(FIRST_ID),
            line: stage(LINE_FIRST_ID).with_limit(LINE_KEPT_BYTES),
            left_at: None,
            whole: None,
            whole_line: None,
            whole_packing: Worker::off_the_frame(),
            whole_packed: HashMap::new(),
            waiting: Vec::new(),
            pointer: None,
            hovering: false,
            pixels: false,
            told_of_shift: false,
            sources: Sources::new(started),
            steps: None,
            kept_pictures: false,
        }
    }
}

/// A stage naming its pictures from `first_id`, packing them as the screen's other pictures
/// off the frame are drawn.
fn stage(first_id: u32) -> Stage {
    let stage = Stage::new(first_id);
    if worker::in_line_here() {
        stage.packing_in_line()
    } else {
        stage
    }
}

#[derive(Debug)]
struct Wanted {
    cells: Rect,
    /// What goes over the cells, where they wait for pictures.
    scene: Option<Arc<Scene>>,
    /// The picture beside the map's line, over the cells it covers on screen.
    beside: Option<(Rect, Arc<Scene>)>,
}

#[derive(Debug)]
struct Asked {
    cells: Rect,
    key: u64,
    cell: (u16, u16),
    /// The motion's step it was asked at, while the motion ran.
    step: Option<u128>,
    scene: Option<Arc<Scene>>,
}

/// Where iTerm2 shows the scene drawn into one picture, and what it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Whole {
    cells: Rect,
    cell: (u16, u16),
    look: u64,
}

impl Whole {
    fn key(self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

impl DockedMap {
    /// What the docked view drew of its map, with the dock's first row at `top` on screen: the
    /// cells it shows in and, at `cell` pixels a cell, the scene to put over them. The view is
    /// asked for a new scene when its key, its cells or the cell size changed, and at each step
    /// of its motion while the motion runs.
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
            let scene = drawn.picture.zip(cell).and_then(|(key, cell)| {
                // Reduced motion still steps a change, such as a zoom, whose scenes say they
                // move; the world's own motion is drawn still, and says it doesn't.
                let step = self.step(ctx.now, !ctx.theme.skips_frames);
                self.scene(view, (cells, key, cell), step, ctx)
            });
            let beside = view.beside().and_then(|beside| {
                let area = Rect {
                    y: top.checked_add(beside.area.y)?,
                    ..beside.area
                };
                Some((area, Arc::new(Scene::single(beside.picture))))
            });
            Some(Wanted {
                cells,
                scene,
                beside,
            })
        });
    }

    /// The scene for `cells` at `key` and `cell` pixels a cell, at `step` of the motion: the
    /// last one, while they stay and the motion hasn't stepped.
    fn scene(
        &mut self,
        view: &mut dyn MapView,
        (cells, key, cell): (Rect, u64, (u16, u16)),
        step: Option<u128>,
        ctx: &ViewCtx<'_>,
    ) -> Option<Arc<Scene>> {
        if let Some(asked) = &self.asked
            && (asked.cells, asked.key, asked.cell) == (cells, key, cell)
            && (step.is_none() || asked.step == step)
        {
            return asked.scene.clone();
        }
        let scene = view.scene(cell, ctx);
        self.asked = Some(Asked {
            cells,
            key,
            cell,
            step,
            scene: scene.clone(),
        });
        scene
    }

    /// Which step of the motion `now` falls in, while the motion may run: `motion` says whether
    /// it is allowed. The steps count from the map's first frame on screen.
    fn step(&mut self, now: Instant, motion: bool) -> Option<u128> {
        let still = self.sources.still(self.showing(motion), now);
        let length = self.sources.step(now);
        let steps = self.steps.get_or_insert_with(|| Steps::new(now));
        still.is_none().then(|| steps.index(now, length))
    }

    /// What the clock's rules read of the map as the last frame left it.
    fn showing(&self, motion: bool) -> Showing {
        Showing {
            shown: self.shown.is_some(),
            moves: self
                .asked
                .as_ref()
                .and_then(|asked| asked.scene.as_ref())
                .is_some_and(|scene| scene.moves),
            motion,
            kept_pictures: self.kept_pictures,
        }
    }
}

/// What goes out once a frame drew: the mouse's modes, the parts taken away, and the
/// pictures and placements, each from the first cell of the map or of its line's picture.
#[derive(Debug, Default)]
pub(super) struct MapPlacing {
    escapes: Vec<u8>,
    pictures: Vec<(Position, Vec<u8>)>,
}

/// iTerm2's escapes that draw `scene` into the cells `whole` covers, from its first cell, or
/// `None` once `cancel` says the picture is no longer wanted.
fn drawn_whole(whole: Whole, scene: &Scene, cancel: &Cancel) -> Option<(Position, Vec<u8>)> {
    let image = scene.flatten();
    let png = crate::base::mascot::png::encode_quickly_unless(image.rgba(), || cancel.cancelled())?;
    let cells = whole.cells;
    let size = (usize::from(cells.width), usize::from(cells.height));
    let escapes = graphics::draw_over(Graphics::Iterm, &png, 0, size);
    Some((Position::new(cells.x, cells.y), escapes))
}

impl<W: Write> Ui<W> {
    /// The terminal said, at start, that it reports the mouse in pixels when asked.
    pub fn use_pixel_mouse(&mut self, yes: bool) {
        self.map.pixels = yes;
    }

    /// The cell size a docked map's picture goes over its cells at: wherever the terminal draws
    /// pictures, whatever the header's mascot is set to.
    pub(super) fn places_map_pictures(&self) -> Option<(u16, u16)> {
        self.picture_terminal().map(|(_, cell)| cell)
    }

    /// Focus, input and power read from these, as a test sets them.
    #[cfg(test)]
    pub(super) fn use_world_sources(&mut self, sources: Sources) {
        self.map.sources = sources;
    }

    /// What an event tells the map's clock: the window's focus, and a key or the pointer moving.
    /// Focus coming back, or the first input after the map held still for want of any, draws a
    /// frame so the motion takes up again.
    pub(super) fn notice_for_map(&mut self, event: &Event, now: Instant) {
        let sources = &mut self.map.sources;
        let wakes = match event {
            Event::FocusGained => {
                sources.focus.set_focused(true);
                true
            }
            Event::FocusLost => {
                sources.focus.set_focused(false);
                false
            }
            Event::Key(_) | Event::Mouse(_) | Event::Paste(_) => {
                let idle = now.saturating_duration_since(sources.idle.last_input()) >= IDLE_AFTER;
                sources.idle.input(now);
                idle
            }
            Event::Resize(..) => false,
        };
        if wakes && self.map.shown.is_some() {
            self.dirty = true;
        }
    }

    /// A picture drawn or packed off the frame arrived, so the next frame shows it while the map
    /// is on screen.
    pub fn pictures_arrived(&mut self) {
        if self.map.shown.is_some() {
            self.dirty = true;
        }
    }

    /// When the map next needs a frame: its motion's next step, while it may move, and the
    /// moment the pictures it left behind are freed. A picture drawn or packed off the frame
    /// asks for its own, through [`pictures_arrived`](Self::pictures_arrived).
    pub(super) fn next_map_wake(&self, now: Instant) -> Option<Instant> {
        let map = &self.map;
        let showing = map.showing(!self.theme.skips_frames);
        let step = match map.sources.still(showing, now) {
            Some(_) => None,
            None => map
                .steps
                .map(|steps| steps.next(now, map.sources.step(now))),
        };
        let free = map.left_at.map(|left| left + KEEP_AFTER_LEAVING);
        step.into_iter().chain(free).min()
    }

    /// Before the frame drawn at `now`: what puts the map's scene where the frame shows it, or
    /// takes it off the screen, and the mouse's modes to match. Pictures taken off stay kept
    /// for a minute, then go. iTerm2 draws a picture into the cells, so where one was is
    /// written again by the frame about to draw.
    pub(super) fn place_map(&mut self, now: Instant) -> MapPlacing {
        let wanted = self.map.wanted.take();
        let mut out = MapPlacing {
            escapes: std::mem::take(&mut self.map.waiting),
            pictures: Vec::new(),
        };
        self.map.shown = wanted.as_ref().map(|wanted| wanted.cells);
        if self.map.shown.is_none() {
            self.map.hovering = false;
            self.map.steps = None;
        }
        self.mouse_for_map(&mut out.escapes);
        let (scene, beside) = match wanted {
            Some(wanted) => (
                wanted.scene.map(|scene| (wanted.cells, scene)),
                wanted.beside,
            ),
            None => (None, None),
        };
        let terminal = self.picture_terminal();
        self.map.kept_pictures = terminal.is_some_and(|(graphics, _)| graphics == Graphics::Kitty);
        match (scene, terminal) {
            (Some((cells, scene)), Some((Graphics::Kitty, cell))) => {
                let escapes = self.map.stage.show(&scene, Spot { cells, cell });
                out.pictures
                    .push((Position::new(cells.x, cells.y), escapes));
                let line = &mut self.map.line;
                match beside {
                    Some((cells, scene)) => {
                        let escapes = line.show(&scene, Spot { cells, cell });
                        out.pictures
                            .push((Position::new(cells.x, cells.y), escapes));
                    }
                    None if line.is_showing() || line.is_packing() => {
                        out.escapes.extend(line.hide());
                    }
                    None => {}
                }
                self.map.left_at = None;
            }
            (Some((cells, scene)), Some((Graphics::Iterm, cell))) => {
                // The whole picture is drawn again for each change, so the pointer's ring stays
                // out of it: the line under the map names what the pointer is on.
                let scene = Arc::new(scene.without(Depth::Pointer));
                let whole = |cells: Rect, scene: &Scene| Whole {
                    cells,
                    cell,
                    look: deliver::look(scene),
                };
                let map = whole(cells, &scene);
                let line = beside.map(|(cells, scene)| (whole(cells, &scene), scene));
                let line_whole = line.as_ref().map(|(line, _)| *line);
                // A picture is drawn off the frame, and the one it replaces stays until it's
                // ready, unless its cells moved.
                let drawn_map =
                    (self.map.whole != Some(map)).then(|| self.packed_whole(map, &scene));
                let drawn_line = line
                    .as_ref()
                    .filter(|(line, _)| self.map.whole_line != Some(*line))
                    .map(|(line, scene)| self.packed_whole(*line, scene));
                self.map.whole_packed.retain(|key, _| {
                    *key == map.key() || line_whole.is_some_and(|line| *key == line.key())
                });
                // What changed comes off first, which inline may take every picture with it.
                let moved = |was: Option<Whole>, now: Option<Rect>| {
                    was.is_some_and(|was| Some(was.cells) != now)
                };
                if matches!(drawn_map, Some(Some(_))) || moved(self.map.whole, Some(cells)) {
                    self.take_whole_picture_off(Some(cells));
                }
                if line_whole.is_none()
                    || matches!(drawn_line, Some(Some(_)))
                    || moved(self.map.whole_line, line_whole.map(|line| line.cells))
                {
                    self.take_line_picture_off(line_whole.map(|line| line.cells));
                }
                if let Some(Some(drawn)) = drawn_map {
                    out.pictures.push(drawn);
                    self.map.whole = Some(map);
                }
                if let (Some(Some(drawn)), Some(line)) = (drawn_line, line_whole) {
                    out.pictures.push(drawn);
                    self.map.whole_line = Some(line);
                }
            }
            _ => {
                let line = &mut self.map.line;
                if line.is_showing() || line.is_packing() {
                    out.escapes.extend(line.hide());
                }
                let stage = &mut self.map.stage;
                if stage.is_showing() || stage.is_packing() {
                    out.escapes.extend(stage.hide());
                }
                if stage.keeps_pictures() && self.map.left_at.is_none() {
                    self.map.left_at = Some(now);
                }
                if self
                    .map
                    .left_at
                    .is_some_and(|left| now.saturating_duration_since(left) >= KEEP_AFTER_LEAVING)
                {
                    out.escapes.extend(self.free_map_pictures());
                }
                self.take_whole_picture_off(None);
                self.take_line_picture_off(None);
                self.map.whole_packing.stop();
                self.map.whole_packed.clear();
            }
        }
        out
    }

    /// The escapes that free every picture the terminal keeps for the map and its line.
    fn free_map_pictures(&mut self) -> Vec<u8> {
        self.map.left_at = None;
        let mut out = self.map.stage.free();
        out.extend(self.map.line.free());
        out
    }

    /// Takes iTerm2's picture of the map away, unless the next one goes over the same `cells`.
    /// iTerm2's escapes that draw `scene` over `whole`, once drawn off the frame: `None` while
    /// it's drawn, which a later frame looks for again.
    fn packed_whole(&mut self, whole: Whole, scene: &Arc<Scene>) -> Option<(Position, Vec<u8>)> {
        let key = whole.key();
        let map = &mut self.map;
        map.whole_packed.extend(map.whole_packing.take());
        if let Some(drawn) = map.whole_packed.remove(&key) {
            return Some(drawn);
        }
        let scene = scene.clone();
        map.whole_packing
            .draw(key, move |cancel| drawn_whole(whole, &scene, cancel));
        // Drawn in line, it's ready at once.
        map.whole_packed.extend(map.whole_packing.take());
        map.whole_packed.remove(&key)
    }

    fn take_whole_picture_off(&mut self, next: Option<Rect>) {
        let was = self.map.whole.take();
        self.write_over(was, next);
    }

    /// Takes iTerm2's picture beside the map's line away, unless the next one goes over the
    /// same `cells`.
    fn take_line_picture_off(&mut self, next: Option<Rect>) {
        let was = self.map.whole_line.take();
        self.write_over(was, next);
    }

    /// The frame about to draw writes the cells iTerm2 drew the picture `was` into again,
    /// unless the next picture goes over the same cells. Inline it writes every cell again,
    /// which takes every picture with it, so each is drawn again after.
    fn write_over(&mut self, was: Option<Whole>, next: Option<Rect>) {
        let Some(was) = was else {
            return;
        };
        if Some(was.cells) == next {
            return;
        }
        if self.scrollback.is_some() {
            self.terminal.redraw_cells(was.cells);
        } else {
            self.terminal.invalidate();
            self.map.whole = None;
            self.map.whole_line = None;
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
        for (at, escapes) in placing.pictures {
            self.terminal.send_escapes(Some(at), &escapes)?;
        }
        Ok(())
    }

    /// Before the terminal is given away or the app ends: the pictures go, freed at once, and
    /// the mouse goes back. The next frame after it comes back sends them again.
    pub(super) fn let_go_of_map(&mut self) -> io::Result<()> {
        let mut placing = self.place_map(Instant::now());
        placing.escapes.extend(self.free_map_pictures());
        self.send_map_placing(placing)
    }

    /// After a resize the terminal may show the map's pictures where the cells no longer are,
    /// so every placement goes, and the next frame places each part again.
    pub(super) fn resend_map(&mut self) {
        let hidden = self.map.stage.hide();
        self.map.waiting.extend(hidden);
        let hidden = self.map.line.hide();
        self.map.waiting.extend(hidden);
        for whole in [self.map.whole.take(), self.map.whole_line.take()] {
            if let Some(whole) = whole
                && self.scrollback.is_some()
            {
                self.terminal.redraw_cells(whole.cells);
            }
        }
    }

    /// A mouse event the map takes: a move over it rings the dot nearest, a move off it lets
    /// go, a click on it opens what the dot is, the wheel zooms and a drag turns the planet.
    /// `None` leaves the event to the rest.
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
            // The wheel over the map zooms it rather than scrolling the panel.
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if point.inside() => {
                let up = mouse.kind == MouseEventKind::ScrollUp;
                self.dirty |= view.wheel(point, up, Instant::now());
                Some(Vec::new())
            }
            MouseEventKind::Drag(MouseButton::Left) if point.inside() => {
                self.dirty |= view.drag(point);
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
mod tests;
