use std::time::{Duration, Instant};

use argonauts_core::app::ViewData;
use argonauts_core::behavior_map::BehaviorMap;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Size;

use super::*;
use crate::mascot::graphics::pictures_in;
use crate::model::ViewModel;
use crate::overview::stats::behaviors::sample::three_models;
use crate::overview::{OverviewView, Tab, UsageWatch};
use crate::state::AppState;
use crate::terminal::InlineTerminal;
use crate::theme::{Glyphs, Theme};

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
            map: three_models().map,
        };
        open.ui.push_view(Box::new(OverviewView::new(
            Tab::Overview,
            UsageWatch::default(),
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

    /// The pixel on screen of the first action's dot on the picture, whose first cell is at
    /// `cell`.
    fn first_dot(&self, (column, row): (u16, u16)) -> (u16, u16) {
        let (width, height) = (640.0, 260.0);
        let margin = 12.0 * width / 720.0;
        let action = &self.map.actions[0];
        let x = margin + action.x * (width - 2.0 * margin);
        let y = height - margin - action.y * (height - 2.0 * margin);
        (
            column * CELL.0 + x.round() as u16,
            row * CELL.1 + y.round() as u16,
        )
    }

    fn screen(&self) -> Vec<String> {
        (0..ROWS).map(|row| row_text(&self.ui, row, 0)).collect()
    }
}

fn written(ui: &mut Ui<Vec<u8>>) -> String {
    String::from_utf8_lossy(&std::mem::take(ui.terminal_mut().writer_mut())).into_owned()
}

/// The kitty id of the picture that went out, as its key.
fn picture_id(sent: &str) -> Option<String> {
    let at = sent.rfind("\x1b_Ga=T")?;
    let keys = sent[at..].split(';').next()?;
    let id = keys.split(',').find(|key| key.starts_with("i="))?;
    Some(id.to_owned())
}

fn forgets(sent: &str, id: &str) -> bool {
    sent.contains(&format!("\x1b_Ga=d,d=I,q=2,{id}\x1b\\"))
}

/// The cell the picture went out from, by the cursor move before its first escape, 0-based.
fn picture_cell(sent: &str) -> Option<(u16, u16)> {
    let at = sent.find("\x1b_Ga=T")?;
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

/// On the full screen the map's picture goes over the cells the panel leaves for it, beside
/// the legend, and the mouse reports every move, in pixels. A dot the mouse rests near swaps
/// the picture for one with it ringed; a click opens what the dot was, which puts the map away,
/// so the picture goes and the mouse goes back to selecting.
#[test]
fn the_full_screen_places_the_map_and_the_mouse_points_into_it() {
    let mut open = Open::new(Some(Graphics::Kitty), true);
    let sent = open.frame();
    assert!(sent.contains("\x1b[?1003h\x1b[?1016h"), "{sent:?}");
    assert!(
        sent.contains("\x1b_Ga=T,f=100,q=2,C=1,c=64,r=13,i="),
        "{sent:?}"
    );
    let first_id = picture_id(&sent).expect("its id");
    let (column, row) = picture_cell(&sent).expect("the picture's first cell");
    assert_eq!((column, row), (2, open.panel_top() + MAP_TOP));
    // The map's cells stay blank under it, with the legend beside them.
    assert_eq!(
        row_text(&open.ui, row, 0),
        format!("{}Every model", " ".repeat(68))
    );
    let first = pictures_in(sent.as_bytes());
    assert_eq!(first.len(), 1);

    // A frame that changes nothing on the map sends nothing more, and the panel keeps its
    // height.
    open.ui.dirty = true;
    assert!(!open.frame().contains("\x1b_G"));
    assert_eq!(open.panel_top() + MAP_TOP, row);

    let dot = open.first_dot((column, row));
    open.mouse(MouseEventKind::Moved, dot);
    let sent = open.frame();
    assert!(forgets(&sent, &first_id), "{sent:?}");
    let ringed_id = picture_id(&sent).expect("the ringed picture");
    assert_ne!(ringed_id, first_id);
    assert_eq!(picture_cell(&sent), Some((column, row)));
    let ringed = pictures_in(sent.as_bytes());
    assert_eq!(ringed.len(), 1);
    assert_ne!(ringed, first);

    open.mouse(MouseEventKind::Down(MouseButton::Left), dot);
    let sent = open.frame();
    assert!(forgets(&sent, &ringed_id), "{sent:?}");
    assert!(sent.contains("\x1b[?1016l\x1b[?1003l"), "{sent:?}");
    assert!(!sent.contains("\x1b_Ga=T"), "{sent:?}");
    assert!(!sent.contains("\x1b[?1000l"), "{sent:?}");
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
    let cell = (
        2 + (action.x * 64.0) as u16,
        top + ((1.0 - action.y) * 13.0) as u16,
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
    assert!(forgets(&sent, &id), "{sent:?}");
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
