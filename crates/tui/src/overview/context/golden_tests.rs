//! The Context tab against its picture, `context_cc`, frame by frame: the tab as it opens,
//! every list open, one crew agent's own block, and the run that had no session of the
//! conversation. Each frame is the panel drawn from the picture's numbers, 118 cells wide.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};

use super::example::{self, Example};
use crate::overview::golden::{Picture, Region, differences, theme};
use crate::overview::{OverviewView, Tab, UsageWatch};
use crate::state::AppState;
use crate::views::{View, ViewCtx};

const WIDTH: u16 = 118;

/// The panel on `example`, opened with every list open where `every_list`, after `keys`.
fn panel(example: &Example, every_list: bool, keys: &[KeyCode]) -> Buffer {
    let state = example.state();
    let theme = theme();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let mut view = OverviewView::new(Tab::Context, UsageWatch::default())
        .at(example.now)
        .showing_paths_from(example.places.clone())
        .with_context(example.view.clone());
    if every_list {
        view = view.with_every_list();
    }
    let area = |view: &mut OverviewView, state: &AppState| {
        let ctx = ViewCtx {
            theme: &theme,
            state,
            now: Instant::now(),
        };
        let height = view
            .dock_height(Size::new(WIDTH, 120), &ctx)
            .expect("the panel docks");
        Rect::new(0, 0, WIDTH, height)
    };
    // A frame first, as the screen draws one before a key comes.
    let mut buf = Buffer::empty(area(&mut view, &state));
    view.render(buf.area, &mut buf, &ctx);
    for code in keys {
        view.handle_key(KeyEvent::new(*code, KeyModifiers::NONE), &ctx);
        buf = Buffer::empty(area(&mut view, &state));
        view.render(buf.area, &mut buf, &ctx);
    }
    buf
}

/// Fails with every cell of the picture's `rows` the panel drew differently from its row
/// `at` on.
fn assert_rows(picture: &Picture, buf: &Buffer, rows: std::ops::Range<usize>, at: u16) {
    let region = Region {
        rows: rows.start..rows.end,
        columns: 0..usize::from(WIDTH),
        at: (0, at),
    };
    let drawn =
        crate::picture_dump::cut(buf, (0, usize::from(at)), (usize::from(WIDTH), rows.len()));
    let part = format!("{}-rows-{}-{}", picture.name, rows.start, rows.end);
    crate::picture_dump::rows(&part, &drawn);
    let (at, size) = ((0, usize::from(at)), (usize::from(WIDTH), rows.len()));
    crate::picture_dump::buffer_page(&part, buf, at, size, &theme());
    let found = differences(picture, buf, &region, &theme());
    if found.is_empty() {
        return;
    }
    let text = |y: u16| -> String {
        (0..WIDTH)
            .map(|x| {
                buf.cell((x, y))
                    .map_or(" ", |cell| cell.symbol())
                    .to_owned()
            })
            .collect::<String>()
            .trim_end()
            .to_owned()
    };
    let drawn: Vec<String> = (0..buf.area.height).map(text).collect();
    panic!(
        "{} rows {rows:?}: {} cells are different\n{}\n\ndrawn:\n{}\n\npicture:\n{}",
        picture.name,
        found.len(),
        found
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
        drawn.join("\n"),
        picture.text()[rows.start..rows.end].join("\n")
    );
}

/// The last row the panel drew, its key line.
fn key_row(buf: &Buffer) -> u16 {
    buf.area.height - 1
}

/// As the tab opens: the main session's block, then MCP tools, Skills and the crew folded,
/// from the panel's rule to its key line.
#[test]
fn the_context_tab_opens_folded_as_its_picture_draws_it() {
    let picture = Picture::load("context_cc");
    let buf = panel(&example::after_the_run(), false, &[]);
    assert_eq!(buf.area.height, 32, "the panel is as tall as its picture");
    assert_rows(&picture, &buf, 2..34, 0);
}

/// `/context all`: each list a line a server, a group or an agent.
#[test]
fn every_list_opens_as_its_picture_draws_it() {
    let picture = Picture::load("context_cc");
    let buf = panel(&example::after_the_run(), true, &[]);
    // Under the main session's block and its auto-compact window.
    assert_rows(&picture, &buf, 37..55, 20);
}

/// Enter on build:per-api-key: its own block, with its tries and tie-breakers, and the one
/// way back.
#[test]
fn an_agents_own_block_matches_its_picture() {
    let picture = Picture::load("context_cc");
    // Into the list, then down past the main block, the window, the two lists and the
    // crew's title and its first three agents.
    let mut keys = vec![KeyCode::Down; 9];
    keys.push(KeyCode::Enter);
    let buf = panel(&example::after_the_run(), true, &keys);
    assert_rows(&picture, &buf, 58..71, 4);
    assert_rows(&picture, &buf, 72..73, key_row(&buf));
}

/// A request that went straight to a run: no main session's block, and the crew says where
/// the answer came from.
#[test]
fn a_run_with_no_conversation_session_matches_its_picture() {
    let picture = Picture::load("context_cc");
    let buf = panel(&example::straight_to_a_run(), false, &[]);
    assert_rows(&picture, &buf, 76..83, 4);
}
