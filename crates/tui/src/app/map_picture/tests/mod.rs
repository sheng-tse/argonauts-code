#[cfg(test)]
pub(super) mod motion_tests;
#[cfg(test)]
pub(super) mod world_cost_tests;
use std::time::{Duration, Instant};

use argonauts_core::app::ViewData;
use argonauts_core::behaviors::behavior_map::BehaviorMap;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Rect, Size};

use super::*;
use crate::app::model::ViewModel;
use crate::base::mascot::graphics::pictures_in;
use crate::base::theme::{Glyphs, Theme};
use crate::frame::state::AppState;
use crate::panels::stats::behaviors::Drawings;
use crate::panels::stats::behaviors::sample::three_models;
use crate::panels::{OverviewView, Tab, UsageWatch};
use crate::terminal::InlineTerminal;
use crate::world::{Depth, Pointed, Shape};

const COLUMNS: u16 = 118;
const ROWS: u16 = 44;
const CELL: (u16, u16) = (10, 20);
/// The map's first row under the panel's rule: the tabs, a blank, the sub-tabs, a blank, the
/// line under them and a blank.
const MAP_TOP: u16 = 7;

struct Open {
    ui: Ui<Vec<u8>>,
    at: Instant,
    state: AppState,
    map: BehaviorMap,
}

impl Open {
    /// The panel on Behaviors with the keys on the map, on the full screen or inline, in a
    /// terminal that takes pictures through `graphics`.
    fn new(graphics: Option<Graphics>, fullscreen: bool) -> Self {
        Self::with(graphics, fullscreen, |_| {})
    }

    /// As [`Open::new`], with `change` made to the theme the settings give.
    fn with(graphics: Option<Graphics>, fullscreen: bool, change: impl FnOnce(&mut Theme)) -> Self {
        Self::of(three_models().map, graphics, fullscreen, change)
    }

    /// As [`Open::new`], on the made-up crew `key`'s world, whose makers each have a galaxy.
    fn crew(key: crate::world::map::fields::crews::Key) -> Self {
        let mut map = crate::panels::stats::behaviors::crews::map(key);
        // The tab takes the map it asked for, so the crew's map answers its request.
        map.request = three_models().map.request;
        Self::of(map, Some(Graphics::Kitty), true, |_| {})
    }

    fn of(
        map: BehaviorMap,
        graphics: Option<Graphics>,
        fullscreen: bool,
        change: impl FnOnce(&mut Theme),
    ) -> Self {
        let terminal =
            InlineTerminal::new(Vec::new(), Size::new(COLUMNS, ROWS), Position::new(0, 0));
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        change(&mut theme);
        let at = Instant::now();
        let mut ui = Ui::new(terminal, theme, ViewModel::new("conversation"), at);
        if fullscreen {
            ui.use_fullscreen();
        }
        ui.use_pictures(graphics, graphics.map(|_| CELL));
        ui.use_pixel_mouse(graphics.is_some());
        let mut open = Self {
            ui,
            at,
            state: AppState::default(),
            map,
        };
        open.ui.push_view(Box::new(OverviewView::new(
            Tab::Overview,
            UsageWatch::default(),
            crate::config::panel(),
        )));
        open.keys(&[KeyCode::Down, KeyCode::Right, KeyCode::Right]);
        let map = open.map.clone();
        assert!(
            open.ui
                .offer_view_data(&ViewData::Behaviors { map: Box::new(map) })
        );
        open.keys(&[KeyCode::Down]);
        open
    }

    fn keys(&mut self, codes: &[KeyCode]) {
        for code in codes {
            let key = Event::Key(KeyEvent::new(*code, KeyModifiers::NONE));
            self.ui.handle_event_in(key, self.at, &self.state);
        }
    }

    fn mouse(&mut self, kind: MouseEventKind, (column, row): (u16, u16)) {
        let mouse = MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        self.ui
            .handle_event_in(Event::Mouse(mouse), self.at, &self.state);
    }

    /// Draws a frame and takes what went out.
    fn frame(&mut self) -> String {
        self.at += Duration::from_millis(100);
        self.ui.frame_in(self.at, &self.state).unwrap();
        written(&mut self.ui)
    }

    /// The panel's first row, its rule, on the full screen.
    fn panel_top(&self) -> u16 {
        (0..ROWS)
            .find(|row| row_text(&self.ui, *row, 0) == "─".repeat(usize::from(COLUMNS)))
            .expect("the panel's rule")
    }

    /// The pixel on screen of the first action's dot, where the scene last drawn put it, with
    /// the map's first cell at `cell`.
    /// The map's cells on screen, as the last frame showed them.
    fn map_cells(&self) -> Rect {
        self.ui.map.shown.expect("the map shows")
    }

    /// How a picture over the whole map is placed: over its columns and rows.
    fn over_the_map(&self) -> String {
        let cells = self.map_cells();
        format!(",c={},r={},", cells.width, cells.height)
    }

    fn first_dot(&self, (column, row): (u16, u16)) -> (u16, u16) {
        let scene = self
            .ui
            .map
            .asked
            .as_ref()
            .and_then(|asked| asked.scene.clone())
            .expect("a scene on screen");
        let (x, y) = scene
            .hits
            .marks()
            .iter()
            .find_map(|mark| match (mark.pointed, &mark.shape) {
                (Pointed::Action(0), Shape::Point { x, y }) => Some((*x, *y)),
                _ => None,
            })
            .expect("the first dot");
        let cells = self.map_cells();
        let across = f32::from(cells.width * CELL.0) / scene.width() as f32;
        let down = f32::from(cells.height * CELL.1) / scene.height() as f32;
        (
            column * CELL.0 + (x * across).round() as u16,
            row * CELL.1 + (y * down).round() as u16,
        )
    }

    fn screen(&self) -> Vec<String> {
        (0..ROWS).map(|row| row_text(&self.ui, row, 0)).collect()
    }
}

fn written(ui: &mut Ui<Vec<u8>>) -> String {
    String::from_utf8_lossy(&std::mem::take(ui.terminal_mut().writer_mut())).into_owned()
}

/// The kitty ids of every picture the terminal was sent to keep, as their keys, in order.
fn picture_ids(sent: &str) -> Vec<String> {
    sent.match_indices("\x1b_Ga=t")
        .filter_map(|(at, _)| {
            let keys = sent[at..].split(';').next()?;
            let id = keys.split(',').find(|key| key.starts_with("i="))?;
            Some(id.to_owned())
        })
        .collect()
}

/// The kitty id of the last picture the terminal was sent to keep, as its key.
fn picture_id(sent: &str) -> Option<String> {
    let at = sent.rfind("\x1b_Ga=t")?;
    let keys = sent[at..].split(';').next()?;
    let id = keys.split(',').find(|key| key.starts_with("i="))?;
    Some(id.to_owned())
}

/// Frees the picture, from the screen and from the terminal's memory.
fn forgets(sent: &str, id: &str) -> bool {
    sent.contains(&format!("\x1b_Ga=d,d=I,q=2,{id}\x1b\\"))
}

/// Takes a placement of the picture off the screen, keeping the picture.
fn takes_off(sent: &str, id: &str) -> bool {
    sent.contains(&format!("\x1b_Ga=d,d=i,q=2,{id},p="))
}

/// The cell the first placement went out from, by the cursor move before it, 0-based.
fn picture_cell(sent: &str) -> Option<(u16, u16)> {
    let at = sent.find("\x1b_Ga=p")?;
    let start = sent[..at].rfind("\x1b[")?;
    let (row, column) = sent[start + 2..at].strip_suffix('H')?.split_once(';')?;
    Some((
        column.parse::<u16>().ok()? - 1,
        row.parse::<u16>().ok()? - 1,
    ))
}

fn row_text(ui: &Ui<Vec<u8>>, row: u16, from: u16) -> String {
    let screen = ui.terminal().screen_buffer().expect("the full screen");
    (from..screen.area.width)
        .map(|x| screen[(x, row)].symbol())
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// On the full screen the map's pictures go over the cells the panel leaves for them, beside
/// the legend, and the mouse reports every move, in pixels: the land and the dots fill the
/// cells, and what stands or moves on the map lies over them. A dot the mouse rests near gets
/// its ring, one small picture over the rest; a click opens what the dot was, which puts the
/// map away, so the pictures leave the screen and the mouse goes back to selecting. A minute
/// later the terminal lets the pictures go.
#[test]
fn the_full_screen_places_the_map_and_the_mouse_points_into_it() {
    let mut open = Open::new(Some(Graphics::Kitty), true);
    let sent = open.frame();
    assert!(sent.contains("\x1b[?1003h\x1b[?1016h"), "{sent:?}");
    let first = picture_ids(&sent);
    assert!(first.len() >= 2, "the land and the dots at least: {sent:?}");
    // The map's share of a 118-column window, of the picture's shape at 10 × 20 px cells.
    assert_eq!(open.over_the_map(), ",c=62,r=15,");
    assert_eq!(sent.matches(&open.over_the_map()).count(), 2, "{sent:?}");
    let (column, row) = picture_cell(&sent).expect("the picture's first cell");
    assert_eq!((column, row), (2, open.panel_top() + MAP_TOP));
    // The map's cells stay blank under it, with the legend two cells after them.
    let legend = usize::from(column + open.map_cells().width) + 2;
    assert_eq!(
        row_text(&open.ui, row, 0),
        format!("{}Every model", " ".repeat(legend))
    );

    // A frame that changes nothing on the map sends nothing more, and the panel keeps its
    // height.
    open.ui.dirty = true;
    assert!(!open.frame().contains("\x1b_G"));
    assert_eq!(open.panel_top() + MAP_TOP, row);

    let dot = open.first_dot((column, row));
    open.mouse(MouseEventKind::Moved, dot);
    let sent = open.frame();
    let ringed = pictures_in(sent.as_bytes());
    assert_eq!(ringed.len(), 1, "only the ring goes out: {sent:?}");
    assert!(
        !sent.contains(&open.over_the_map()),
        "the land stays: {sent:?}"
    );
    let ring = picture_id(&sent).expect("the ring");
    assert!(first.iter().all(|id| !takes_off(&sent, id)), "{sent:?}");

    open.mouse(MouseEventKind::Down(MouseButton::Left), dot);
    let sent = open.frame();
    assert!(takes_off(&sent, &ring), "{sent:?}");
    assert!(takes_off(&sent, &first[0]), "{sent:?}");
    assert!(!forgets(&sent, &first[0]), "{sent:?}");
    assert!(sent.contains("\x1b[?1016l\x1b[?1003l"), "{sent:?}");
    assert!(!sent.contains("\x1b_Ga=t"), "{sent:?}");
    assert!(!sent.contains("\x1b_Ga=p"), "{sent:?}");
    assert!(!sent.contains("\x1b[?1000l"), "{sent:?}");

    open.at += Duration::from_secs(59);
    let sent = open.frame();
    assert!(!forgets(&sent, &ring), "{sent:?}");
    open.at += Duration::from_millis(800);
    let sent = open.frame();
    assert!(forgets(&sent, &ring), "{sent:?}");
    assert!(first.iter().all(|id| forgets(&sent, id)), "{sent:?}");
}

/// Coming back within the minute sends none of the pictures again: they are placed from what
/// the terminal kept.
#[test]
fn coming_straight_back_places_the_pictures_kept() {
    let mut open = Open::new(Some(Graphics::Kitty), true);
    let ids = picture_ids(&open.frame());
    assert!(!ids.is_empty());
    open.keys(&[KeyCode::Up, KeyCode::Left]);
    let left = open.frame();
    assert!(ids.iter().all(|id| takes_off(&left, id)), "{left:?}");
    open.at += Duration::from_secs(30);
    open.keys(&[KeyCode::Right, KeyCode::Down]);
    let back = open.frame();
    assert!(
        ids.iter()
            .all(|id| back.contains(&format!("\x1b_Ga=p,q=2,C=1,{id},p="))),
        "{back:?}"
    );
    assert!(!back.contains("\x1b_Ga=t"), "{back:?}");
}

/// Without pictures the map is braille in the same cells, and the mouse reads the middle of
/// the cell it is over: moving onto a dot names it under the map, and moving off takes the
/// line away.
#[test]
fn without_pictures_the_mouse_points_at_the_braille_cells() {
    let mut open = Open::new(None, true);
    let sent = open.frame();
    assert!(sent.contains("\x1b[?1003h"), "{sent:?}");
    assert!(!sent.contains("1016h"), "{sent:?}");
    assert!(!sent.contains("\x1b_G"), "{sent:?}");
    let before = open.screen();
    let top = open.panel_top() + MAP_TOP;
    let action = &open.map.actions[0];
    let cells = open.map_cells();
    let cell = (
        2 + (action.x * f32::from(cells.width)) as u16,
        top + ((1.0 - action.y) * f32::from(cells.height)) as u16,
    );
    open.mouse(MouseEventKind::Moved, cell);
    open.frame();
    assert_ne!(open.screen(), before, "the dot it rests on is named");
    open.mouse(MouseEventKind::Moved, (cell.0, 0));
    open.frame();
    assert_eq!(open.screen(), before);

    open.ui.pop_view();
    let sent = open.frame();
    assert!(sent.contains("\x1b[?1016l\x1b[?1003l"), "{sent:?}");
}

/// The inline screen reports no mouse of its own, so the map turns it on while it shows and
/// all the way off once it goes.
#[test]
fn inline_the_mouse_is_reported_only_while_the_map_shows() {
    let mut open = Open::new(Some(Graphics::Kitty), false);
    let sent = open.frame();
    assert!(
        sent.contains("\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b[?1016h"),
        "{sent:?}"
    );
    let (column, row) = picture_cell(&sent).expect("the picture's first cell");
    assert_eq!(column, 2);
    assert!(row + 13 <= ROWS);
    let id = picture_id(&sent).expect("its id");

    open.ui.pop_view();
    let sent = open.frame();
    assert!(takes_off(&sent, &id), "{sent:?}");
    assert!(
        sent.contains("\x1b[?1016l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l"),
        "{sent:?}"
    );
}

/// While the map's mouse reports pixels, the rest of the screen still reads cells.
#[test]
fn pixels_read_as_cells_off_the_map() {
    let mut open = Open::new(Some(Graphics::Kitty), true);
    open.frame();
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 205,
        row: 95,
        modifiers: KeyModifiers::NONE,
    };
    let cells = open.ui.mouse_in_cells(mouse);
    assert_eq!((cells.column, cells.row), (20, 4));
}

/// Giving the terminal away takes the picture and the mouse with it; coming back puts them
/// back on the next frame.
#[test]
fn the_picture_and_the_mouse_go_while_the_terminal_is_given_away() {
    let mut open = Open::new(Some(Graphics::Kitty), true);
    let id = picture_id(&open.frame()).expect("its id");
    open.ui.suspend().unwrap();
    let sent = written(&mut open.ui);
    assert!(forgets(&sent, &id), "{sent:?}");
    assert!(sent.contains("\x1b[?1016l\x1b[?1003l"), "{sent:?}");
    open.ui.resume().unwrap();
    let sent = open.frame();
    let again = picture_id(&sent).expect("the picture again");
    assert_ne!(again, id);
    assert!(sent.contains("\x1b[?1003h\x1b[?1016h"), "{sent:?}");
}

/// The header's mascot set to blocks or off leaves the map's picture alone: the terminal alone
/// says whether it draws one.
#[test]
fn the_map_is_a_picture_whatever_the_header_mascot_is_set_to() {
    for mascot in [
        argonauts_core::settings::ui::MascotDrawing::Blocks,
        argonauts_core::settings::ui::MascotDrawing::Off,
    ] {
        let mut open = Open::with(Some(Graphics::Kitty), true, |theme| theme.mascot = mascot);
        let sent = open.frame();
        assert!(picture_cell(&sent).is_some(), "{mascot:?}: {sent:?}");
    }
}

/// A terminal whose variables say kitty but which didn't say it reports pixels, as one started
/// from Ghostty does, is read in cells.
#[test]
fn pixels_are_read_only_where_the_terminal_said_it_reports_them() {
    let mut open = Open::new(Some(Graphics::Kitty), true);
    open.ui.use_pixel_mouse(false);
    let sent = open.frame();
    assert!(sent.contains("\x1b[?1003h"), "{sent:?}");
    assert!(!sent.contains("1016h"), "{sent:?}");
    let mouse = MouseEvent {
        kind: MouseEventKind::Moved,
        column: 40,
        row: 3,
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(
        open.ui.mouse_in_cells(mouse),
        mouse,
        "cells are left as cells"
    );
}

/// Inline, the terminal's own selection takes Shift while the map follows the mouse, which is
/// said once.
#[test]
fn inline_the_map_says_once_that_shift_selects() {
    let mut open = Open::new(Some(Graphics::Kitty), false);
    open.frame();
    let hint = open.ui.model.bottom.hint.take().map(|hint| hint.text);
    assert_eq!(hint.as_deref(), Some(SHIFT_SELECTS));
    open.keys(&[KeyCode::Right]);
    open.frame();
    assert_eq!(open.ui.model.bottom.hint, None, "once");
    let mut full = Open::new(Some(Graphics::Kitty), true);
    full.frame();
    assert_eq!(
        full.ui.model.bottom.hint, None,
        "the full screen keeps selection its own way"
    );
}

/// A picture of the map whose drawing panicked is said on the bottom line, once, with what
/// the drawing said, and the map goes on.
#[test]
fn a_picture_that_couldnt_be_drawn_is_said_once() {
    let mut open = Open::new(Some(Graphics::Kitty), true);
    open.frame();
    open.ui
        .map
        .whole_packing
        .draw(1, |_| -> Option<(Position, Vec<u8>)> {
            panic!("no pixels")
        });
    // The worker's wake, as the screen hears it.
    open.ui.pictures_arrived();
    open.frame();
    let hint = open.ui.model.bottom.hint.take().map(|hint| hint.text);
    assert_eq!(
        hint.as_deref(),
        Some("The map's picture couldn't be drawn: no pixels")
    );
    assert!(open.ui.map.shown.is_some(), "the map goes on");
    open.frame();
    assert_eq!(open.ui.model.bottom.hint, None, "once");
}

/// A scene of the world whose drawing panicked is said on the bottom line, once, with what the
/// drawing said.
#[test]
fn a_scene_that_couldnt_be_drawn_is_said_once() {
    let mut screen = LinedScreen::new(Graphics::Kitty);
    screen.frame();
    screen.drawings.want(1, None, |_, _| panic!("no stars"));
    screen.ui.pictures_arrived();
    screen.frame();
    let hint = screen.ui.model.bottom.hint.take().map(|hint| hint.text);
    assert_eq!(
        hint.as_deref(),
        Some("The map's picture couldn't be drawn: no stars")
    );
    screen.frame();
    assert_eq!(screen.ui.model.bottom.hint, None, "once");
}

/// iTerm2 keeps no pictures, so the map goes out drawn whole over its cells, and again only
/// when what it shows changed: a frame that changes nothing on the map sends nothing.
#[test]
fn iterm2_gets_the_map_whole_and_again_only_when_it_changes() {
    let mut open = Open::new(Some(Graphics::Iterm), true);
    let sent = open.frame();
    let whole = "\x1b]1337;File=inline=1;";
    assert_eq!(sent.matches(whole).count(), 1, "{sent:?}");
    let cells = open.map_cells();
    let whole_size = format!(";width={};height={}:", cells.width, cells.height);
    assert!(sent.contains(&whole_size), "{sent:?}");
    assert!(!sent.contains("\x1b_G"), "{sent:?}");
    let (column, row) = (2, open.panel_top() + MAP_TOP);

    open.ui.dirty = true;
    assert!(!open.frame().contains(whole));

    // iTerm2 reports the mouse in cells. The ring stays out of its whole pictures, so the
    // map isn't drawn again for it.
    let drawn = open.ui.map.whole;
    let (x, y) = open.first_dot((column, row));
    open.mouse(MouseEventKind::Moved, (x / CELL.0, y / CELL.1));
    open.frame();
    assert_eq!(open.ui.map.whole, drawn, "the map stays as it was");
}

/// A resize may leave the pictures where the cells no longer are, so every placement goes and
/// the next frame places them again from the pictures the terminal kept. The map's pictures
/// are drawn once at their own size, which the terminal fits to the cells, so a new cell size,
/// as after a font change, places the same pictures again over the new cells.
#[test]
fn a_resize_places_the_maps_pictures_again_at_any_cell_size() {
    let mut open = Open::new(Some(Graphics::Kitty), true);
    let ids = picture_ids(&open.frame());
    let land = ids.first().expect("the land").clone();
    open.ui.resize(Size::new(COLUMNS, ROWS), None);
    let sent = open.frame();
    assert!(takes_off(&sent, &land), "{sent:?}");
    assert!(
        sent.contains(&format!("\x1b_Ga=p,q=2,C=1,{land},p=")),
        "{sent:?}"
    );
    assert!(sent.contains(&open.over_the_map()), "{sent:?}");
    assert!(!sent.contains("\x1b_Ga=t"), "{sent:?}");

    open.ui.measure_cells_with(|| Some((12, 24)));
    open.ui.resize(Size::new(COLUMNS, ROWS), None);
    let sent = open.frame();
    assert!(takes_off(&sent, &land), "{sent:?}");
    assert!(
        sent.contains(&format!("\x1b_Ga=p,q=2,C=1,{land},p=")),
        "{sent:?}"
    );
    assert!(sent.contains(&open.over_the_map()), "{sent:?}");
    assert!(!sent.contains("\x1b_Ga=t"), "{sent:?}");
}

/// The wheel over the map zooms the world rather than scrolling the panel: a turn toward you
/// goes out to the planet, so the map's pictures leave the screen and the panel stays where
/// it was.
#[test]
fn the_wheel_over_the_map_zooms_the_world() {
    let mut open = Open::new(Some(Graphics::Kitty), true);
    let sent = open.frame();
    let land = picture_ids(&sent)[0].clone();
    let (column, row) = picture_cell(&sent).expect("the map's first cell");
    let screen = open.screen();
    let middle = ((column + 32) * CELL.0, (row + 6) * CELL.1);
    open.mouse(MouseEventKind::ScrollDown, middle);
    let sent = open.frame();
    assert!(takes_off(&sent, &land), "{sent:?}");
    assert_eq!(
        open.screen()[..usize::from(row)],
        screen[..usize::from(row)]
    );
}

/// A zoom between the universe's levels stretches pictures the terminal keeps: each level and
/// its sky goes out once, and the zoom's other steps send only placements.
#[test]
fn a_zoom_into_a_galaxy_sends_each_picture_once_and_then_only_places_them() {
    let mut open = Open::crew(crate::world::map::fields::crews::Key::R);
    open.frame();
    open.keys(&[KeyCode::Char('3')]);
    for _ in 0..10 {
        open.frame();
    }
    // From the groupings' row onto the picture, where → picks the first galaxy.
    open.keys(&[KeyCode::Down, KeyCode::Right]);
    open.frame();
    open.keys(&[KeyCode::Enter]);
    let began = open.at;
    // A frame at each of the zoom's steps before it arrives.
    let sent: Vec<usize> = (0..6u32)
        .map(|step| {
            open.at = began + Duration::from_millis(125) * step + Duration::from_millis(10);
            open.ui.frame_in(open.at, &open.state).unwrap();
            pictures_in(written(&mut open.ui).as_bytes()).len()
        })
        .collect();
    assert_eq!(sent.iter().sum::<usize>(), 4, "{sent:?}");
    assert!(
        sent.iter().filter(|&&pictures| pictures == 0).count() >= 2,
        "{sent:?}"
    );
}

/// A docked map of one blank picture, with a picture beside its line while `beside` holds one,
/// and the world's drawings to ask for scenes on.
#[derive(Debug)]
struct Lined {
    beside: Arc<std::sync::Mutex<Option<crate::world::Picture>>>,
    drawings: Arc<Drawings>,
}

impl crate::frame::view::View for Lined {
    fn render(&mut self, _area: Rect, _buf: &mut ratatui::buffer::Buffer, _ctx: &ViewCtx<'_>) {}

    fn docked(&self) -> bool {
        true
    }

    fn handle_key(
        &mut self,
        _key: KeyEvent,
        _ctx: &ViewCtx<'_>,
    ) -> crate::frame::view::ViewOutcome {
        crate::frame::view::ViewOutcome::Consumed
    }

    fn map(&mut self) -> Option<&mut dyn MapView> {
        Some(self)
    }
}

impl MapView for Lined {
    fn place_pictures(&mut self, _cell: Option<(u16, u16)>) {}

    fn map_cells(&self) -> Option<crate::frame::view::map::MapCells> {
        Some(crate::frame::view::map::MapCells {
            area: Rect::new(2, 1, 20, 4),
            picture: Some(1),
        })
    }

    fn scene(&self, cell: (u16, u16), _ctx: &ViewCtx<'_>) -> Option<Arc<Scene>> {
        let (width, height) = (20 * usize::from(cell.0), 4 * usize::from(cell.1));
        let blank = crate::world::Image::transparent(width, height);
        Some(Arc::new(Scene::single(crate::world::Picture::new(
            1, blank,
        ))))
    }

    fn point(&mut self, _at: Option<MapPoint>) -> bool {
        false
    }

    fn click(&mut self) -> bool {
        false
    }

    fn beside(&self) -> Option<crate::frame::view::map::Beside> {
        let picture = self.beside.lock().unwrap().clone()?;
        Some(crate::frame::view::map::Beside {
            area: Rect::new(3, 7, 2, 1),
            picture,
        })
    }

    fn take_failure(&self) -> Option<String> {
        self.drawings.take_failure()
    }
}

/// A screen of `Lined` through `graphics`, the picture beside its line to change and the
/// drawings its map asks for scenes on.
struct LinedScreen {
    ui: Ui<Vec<u8>>,
    at: Instant,
    beside: Arc<std::sync::Mutex<Option<crate::world::Picture>>>,
    drawings: Arc<Drawings>,
}

impl LinedScreen {
    fn new(graphics: Graphics) -> Self {
        let terminal = InlineTerminal::new(Vec::new(), Size::new(80, 24), Position::new(0, 0));
        let at = Instant::now();
        let mut ui = Ui::new(terminal, Theme::dark(), ViewModel::new("conversation"), at);
        ui.use_fullscreen();
        ui.use_pictures(Some(graphics), Some(CELL));
        let beside = Arc::new(std::sync::Mutex::new(Some(Self::picture(7))));
        let drawings = Arc::new(Drawings::default());
        ui.push_view(Box::new(Lined {
            beside: Arc::clone(&beside),
            drawings: Arc::clone(&drawings),
        }));
        Self {
            ui,
            at,
            beside,
            drawings,
        }
    }

    /// A picture the size the line's pictures are drawn at.
    fn picture(key: u64) -> crate::world::Picture {
        crate::world::Picture::new(key, crate::world::Image::transparent(72, 84))
    }

    fn set(&self, picture: Option<crate::world::Picture>) {
        *self.beside.lock().unwrap() = picture;
    }

    /// Draws a frame and takes what went out.
    fn frame(&mut self) -> String {
        self.at += Duration::from_millis(100);
        self.ui.dirty = true;
        self.ui.frame_in(self.at, &AppState::default()).unwrap();
        written(&mut self.ui)
    }
}

/// The placement of the picture `id` names, as its keys.
fn placement<'a>(sent: &'a str, id: &str) -> Option<&'a str> {
    let at = sent.find(&format!("\x1b_Ga=p,q=2,C=1,{id},"))?;
    sent[at..].split('\x1b').nth(1)
}

/// The picture beside the map's line goes over the two cells the line leaves for it, a part of
/// its own: the terminal keeps it, it is placed again only when it changes, taken off when the
/// line has none, and placed again from what the terminal kept when it comes back.
#[test]
fn the_picture_beside_the_maps_line_goes_over_its_cells() {
    let mut screen = LinedScreen::new(Graphics::Kitty);
    let first = format!("i={LINE_FIRST_ID}");
    let sent = screen.frame();
    let (column, row) = picture_cell(&sent).expect("the map placed");
    let line = placement(&sent, &first).unwrap_or_else(|| panic!("{sent:?}"));
    assert!(line.contains(",c=2,r=1"), "{line:?}");
    // From the cell the line leaves, as far from the map's first cell as the view drew it.
    let from = format!("\x1b[{};{}H\x1b{line}", row + 6 + 1, column + 1 + 1);
    assert!(sent.contains(&from), "{sent:?}");
    assert_eq!(
        picture_ids(&sent).iter().filter(|id| **id == first).count(),
        1
    );

    assert!(placement(&screen.frame(), &first).is_none(), "unchanged");

    screen.set(None);
    assert!(takes_off(&screen.frame(), &first));

    screen.set(Some(LinedScreen::picture(7)));
    let sent = screen.frame();
    assert!(placement(&sent, &first).is_some(), "{sent:?}");
    assert!(picture_ids(&sent).is_empty(), "kept: {sent:?}");

    screen.set(Some(LinedScreen::picture(8)));
    let sent = screen.frame();
    let second = format!("i={}", LINE_FIRST_ID + 1);
    assert_eq!(picture_ids(&sent), std::slice::from_ref(&second));
    assert!(placement(&sent, &second).is_some(), "{sent:?}");
}

/// iTerm2 keeps no pictures, so the picture beside the map's line is drawn into its two cells,
/// again only when it changes.
#[test]
fn iterm2_draws_the_picture_beside_the_maps_line_when_it_changes() {
    let mut screen = LinedScreen::new(Graphics::Iterm);
    let drawn = "\x1b]1337;File=inline=1;";
    let sent = screen.frame();
    assert_eq!(sent.matches(drawn).count(), 2, "the map and its line");
    assert_eq!(sent.matches(";width=2;height=1:").count(), 1, "{sent:?}");

    assert!(!screen.frame().contains(drawn));

    screen.set(Some(LinedScreen::picture(8)));
    let sent = screen.frame();
    assert_eq!(sent.matches(drawn).count(), 1, "{sent:?}");
    assert!(sent.contains(";width=2;height=1:"), "{sent:?}");

    screen.set(None);
    assert!(!screen.frame().contains(drawn));
    screen.set(Some(LinedScreen::picture(8)));
    assert_eq!(screen.frame().matches(drawn).count(), 1, "drawn again");
}

/// Keys that leave Behaviors in one burst, before any frame draws: whichever tab they reach,
/// the next frame takes the world's pictures off, and nothing but freeing them wakes the app.
#[test]
fn a_burst_of_keys_to_another_tab_takes_the_world_off_in_its_frame() {
    for (rights, tab) in [(2, "Config"), (4, "Context")] {
        let mut open = Open::new(Some(Graphics::Kitty), true);
        assert!(open.frame().contains("\x1b_Ga=p"), "the world shows first");
        open.keys(&[KeyCode::Up, KeyCode::Up, KeyCode::Up]);
        open.frame();
        open.keys(&vec![KeyCode::Right; rights]);
        let left = open.frame();
        assert!(!left.contains("\x1b_Ga=p"), "{tab}: no picture is placed");
        assert_eq!(open.ui.map.shown, None, "{tab}");
        let wake = open
            .ui
            .next_map_wake(open.at)
            .map(|at| at.saturating_duration_since(open.at));
        assert!(
            wake.is_none_or(|wake| wake >= KEEP_AFTER_LEAVING - Duration::from_secs(1)),
            "{tab}: {wake:?}"
        );
    }
}

/// A panel too small for its own head draws no map, so its pictures go too.
#[test]
fn a_panel_too_small_for_its_head_takes_the_world_off() {
    let mut open = Open::new(Some(Graphics::Kitty), true);
    assert!(open.frame().contains("\x1b_Ga=p"));
    open.ui
        .handle_event_in(Event::Resize(COLUMNS, 4), open.at, &open.state);
    let small = open.frame();
    assert!(!small.contains("\x1b_Ga=p"), "{small:?}");
    assert_eq!(open.ui.map.shown, None);
}

/// Once the world is off the screen, no picture still drawing wakes the panel every few
/// milliseconds.
#[test]
fn the_panel_waits_on_no_drawing_once_the_world_is_off() {
    let mut open = Open::new(Some(Graphics::Kitty), true);
    open.frame();
    open.keys(&[
        KeyCode::Up,
        KeyCode::Up,
        KeyCode::Up,
        KeyCode::Right,
        KeyCode::Right,
    ]);
    open.frame();
    let wake = open
        .ui
        .next_deadline(open.at)
        .map(|at| at.saturating_duration_since(open.at));
    assert!(
        wake.is_none_or(|wake| wake > Duration::from_millis(100)),
        "{wake:?}"
    );
}

/// However you leave the world, by ← to another sub-tab, Esc or the tab row, its pictures go in
/// that frame, and nothing but freeing them wakes the app for the next ten seconds.
#[test]
fn leaving_the_world_any_way_costs_nothing_after() {
    let ways: [(&str, &[KeyCode]); 3] = [
        ("left", &[KeyCode::Up, KeyCode::Up, KeyCode::Left]),
        ("esc", &[KeyCode::Esc]),
        (
            "tab row",
            &[
                KeyCode::Up,
                KeyCode::Up,
                KeyCode::Up,
                KeyCode::Right,
                KeyCode::Right,
            ],
        ),
    ];
    for (way, keys) in ways {
        let mut open = Open::new(Some(Graphics::Kitty), true);
        assert!(
            open.frame().contains("\x1b_Ga=p"),
            "{way}: the world shows first"
        );
        open.keys(keys);
        let left = open.frame();
        assert!(
            !left.contains("\x1b_Ga=p"),
            "{way}: placements go in that frame"
        );
        assert_eq!(open.ui.map.shown, None, "{way}");
        let free = open.at + KEEP_AFTER_LEAVING - Duration::from_millis(200);
        let until = open.at + Duration::from_secs(10);
        if let Some(wake) = open.ui.next_deadline(open.at) {
            assert!(
                wake >= free || wake > until,
                "{way}: woken {:?} later",
                wake - open.at
            );
        }
    }
}

/// iTerm2 takes only whole pictures, so the pointer's ring stays out of them: moving the
/// pointer over a still world sends no new picture, and the line under the map says what it
/// is on.
#[test]
fn pointer_moves_send_iterm_no_new_picture() {
    let mut open = Open::with(Some(Graphics::Iterm), true, |theme| {
        theme.reduced_motion = true;
    });
    assert!(open.frame().contains("\x1b]1337;File="), "the world shows");
    let top = open.ui.map.shown.expect("the map shows");
    let drawn = open.ui.map.whole;
    assert!(drawn.is_some());
    // On the first action's dot, then off it and back, in cells as iTerm2 reports them.
    let (x, y) = open.first_dot((top.x, top.y));
    let dot = (x / CELL.0, y / CELL.1);
    let mut rings = 0;
    for step in 0..20u16 {
        let at = if step % 2 == 0 {
            dot
        } else {
            (dot.0 + 20, dot.1)
        };
        open.mouse(MouseEventKind::Moved, at);
        open.frame();
        let asked = open
            .ui
            .map
            .asked
            .as_ref()
            .and_then(|asked| asked.scene.clone());
        rings += usize::from(asked.is_some_and(|scene| {
            scene
                .sprites()
                .iter()
                .any(|sprite| sprite.depth == Depth::Pointer)
        }));
        // Only the line under the map may change, as it names what the pointer is on.
        assert_eq!(open.ui.map.whole, drawn, "move {step}");
    }
    assert!(rings > 0, "the pointer found a dot, and its scene rings it");
}

/// iTerm2's picture is drawn off the frame: a frame doesn't wait for it, and none comes back to
/// look for it while it's drawn; its arrival asks for the frame that sends it.
#[test]
fn an_iterm_picture_is_drawn_off_the_frame() {
    const PICTURE: &str = "\x1b]1337;File=";
    let mut open = Open::with(Some(Graphics::Iterm), true, |theme| {
        theme.reduced_motion = true;
    });
    let mut arrivals = worker::arrivals();
    open.ui.map.whole_packing = Worker::threaded();
    let mut sent = open.frame().contains(PICTURE);
    if !sent {
        assert_eq!(
            open.ui.next_map_wake(open.at),
            None,
            "no frame looks for it"
        );
    }
    let until = argonauts_testkit::pty::deadline::after(Duration::from_secs(10));
    while !sent && Instant::now() < until {
        if !arrivals.has_changed().unwrap_or(false) {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        arrivals.borrow_and_update();
        open.ui.pictures_arrived();
        assert!(open.ui.dirty, "a picture arrived while the map shows");
        sent = open.frame().contains(PICTURE);
    }
    assert!(
        sent,
        "the picture goes out on the frame its arrival asks for"
    );
    assert!(open.ui.map.whole.is_some());
}
