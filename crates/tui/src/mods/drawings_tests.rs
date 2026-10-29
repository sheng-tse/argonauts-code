//! Config › Mods held to its approved drawings: the list of a home with mods of its own, each
//! card as the pane beside the list draws it before the mod has numbers of its own, and the
//! context mod's card on a tab.

use argonauts_core::settings::mods::view::{ModRow, ModRowKind, ModsView};
use argonauts_core::settings::mods::{
    ModFile, ModForm, ModOrigin, ModPart, ModPlace, ModReading, builtin, check,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::draw;
use super::list::ModsScreen;
use crate::approved_screen;
use crate::kit::Kit;
use crate::state::AppState;
use crate::views::{View, ViewCtx};

const SIZES: [(u16, u16); 2] = [(120, 38), (177, 40)];

fn card_row(file: ModFile) -> ModRow {
    ModRow {
        name: file.name.clone(),
        origin: ModOrigin::BuiltIn,
        path: format!("~/.argonauts/mods/{}.toml", file.name),
        kind: ModRowKind::Card {
            built_in: None,
            reading: None,
            file: Box::new(file),
            note: None,
            needs_trust: false,
            asks_for_line: false,
            hidden_by_line: false,
        },
    }
}

fn built_in(name: &str) -> ModFile {
    let built = builtin::BUILTIN
        .iter()
        .find(|built| built.name == name)
        .unwrap_or_else(|| panic!("{name} isn't built in"));
    check::read(built.name, built.text).unwrap()
}

fn starter(name: &str) -> ModFile {
    let (name, text) = builtin::STARTERS
        .iter()
        .find(|(starter, _)| *starter == name)
        .unwrap_or_else(|| panic!("{name} isn't a starter"));
    check::read(name, text).unwrap()
}

/// The mods the drawing shows, none of them with numbers yet.
fn view() -> ModsView {
    let files = [
        built_in("context"),
        built_in("spend"),
        built_in("tokens"),
        starter("checks"),
    ];
    ModsView {
        rows: files.into_iter().map(card_row).collect(),
        yours_dir: "~/.argonauts/mods".to_owned(),
        ..ModsView::default()
    }
}

/// A built-in mod that draws itself, from a file of yours.
fn own_row(name: &str) -> ModRow {
    let built = builtin::BUILTIN
        .iter()
        .find(|built| built.name == name)
        .unwrap_or_else(|| panic!("{name} isn't built in"));
    let builtin::BuiltinKind::Own { about } = built.kind else {
        panic!("{name} is a card");
    };
    ModRow {
        name: name.to_owned(),
        origin: ModOrigin::Yours,
        path: format!("~/.argonauts/mods/{name}.toml"),
        kind: ModRowKind::Own {
            about: about.to_owned(),
            text: built.text.to_owned(),
            built_in: built.text.to_owned(),
        },
    }
}

/// A home whose plan usage shows in three places and has its numbers, with the branch and
/// checks starters and its own stats and bottom line files.
fn home_view() -> ModsView {
    let mut rows = Vec::new();
    for name in [
        "context",
        "plan-usage",
        "spend",
        "tokens",
        "runs",
        "waiting",
    ] {
        let mut file = built_in(name);
        let mut row = card_row(file.clone());
        if name == "plan-usage" {
            file.places = vec![ModPlace::Usage, ModPlace::BottomLine, ModPlace::Status];
            let reading = ModReading {
                parts: vec![
                    ModPart::new("Claude Code 5h", 50.0)
                        .of(100.0)
                        .on_cli("claude-code"),
                ],
                ..ModReading::default()
            };
            row = card_row(file);
            if let ModRowKind::Card { reading: read, .. } = &mut row.kind {
                *read = Some(reading);
            }
        }
        rows.push(row);
    }
    let mut branch = starter("branch");
    branch.places.clear();
    for file in [branch, starter("checks")] {
        rows.push(ModRow {
            origin: ModOrigin::Yours,
            ..card_row(file)
        });
    }
    rows.push(own_row(builtin::STATS));
    rows.push(own_row(builtin::BOTTOM_LINE));
    ModsView {
        rows,
        yours_dir: "~/.argonauts/mods".to_owned(),
        ..ModsView::default()
    }
}

/// The screen with the mod `at` rows down chosen.
fn chosen(at: usize, size: (u16, u16)) -> Buffer {
    drawn(view(), at, size)
}

/// `view` on a screen of its own, the mod `at` rows down chosen.
fn drawn(view: ModsView, at: usize, (cols, rows): (u16, u16)) -> Buffer {
    let theme = approved_screen::theme();
    let state = AppState::default();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: std::time::Instant::now(),
    };
    let mut screen = ModsScreen::new(view);
    for _ in 0..at {
        screen.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &ctx);
    }
    let area = Rect::new(0, 0, cols, rows);
    let mut buf = Buffer::empty(area);
    screen.render(area, &mut buf, &ctx);
    buf
}

/// The card in the pane on the right of `buf`: the first box inside the screen's frame, from
/// the row that opens it down to the row that closes it.
fn pane_card(buf: &Buffer) -> Rect {
    let area = buf.area;
    let symbol = |x: u16, y: u16| buf[(x, y)].symbol().to_owned();
    for y in 0..area.height {
        let opens: Vec<u16> = (0..area.width).filter(|&x| symbol(x, y) == "╭").collect();
        let Some(&left) = opens.iter().find(|&&x| x > 0) else {
            continue;
        };
        let Some(right) = (left + 1..area.width).find(|&x| symbol(x, y) == "╮") else {
            continue;
        };
        if let Some(bottom) = (y + 1..area.height).find(|&row| symbol(left, row) == "╰") {
            return Rect::new(left, y, right - left + 1, bottom - y + 1);
        }
    }
    panic!("no card in the pane");
}

#[test]
fn the_list_draws_as_its_drawing_does() {
    for size in SIZES {
        let (cols, rows) = size;
        let buf = drawn(home_view(), 0, size);
        approved_screen::assert_matches(
            &format!("config-mods/list@{cols}x{rows}.txt"),
            &buf,
            size,
            0,
        );
    }
}

#[test]
fn each_card_draws_beside_the_list_as_its_drawing_does() {
    for size in SIZES {
        let (cols, rows) = size;
        let mut all = Buffer::empty(Rect::new(0, 0, cols, rows));
        let mut top = 0;
        for at in 0..view().rows.len() {
            let buf = chosen(at, size);
            let card = pane_card(&buf);
            for y in 0..card.height {
                for x in 0..card.width {
                    all[(x, top + y)] = buf[(card.x + x, card.y + y)].clone();
                }
            }
            top += card.height + 1;
        }
        let file = format!("config-mods/cards@{cols}x{rows}.txt");
        approved_screen::assert_matches(&file, &all, size, 0);
    }
}

#[test]
fn the_context_mod_on_a_tab_draws_as_its_drawing_does() {
    let theme = approved_screen::theme();
    let kit = Kit::new(&theme);
    let file = ModFile {
        form: ModForm::Full,
        ..built_in("context")
    };
    let reading = ModReading::sample_for(&file);
    for size in SIZES {
        let (cols, rows) = size;
        let drawn = draw::on_tab(&kit, &file, &reading, usize::from(cols));
        let area = Rect::new(0, 0, cols, drawn.height() as u16);
        let mut buf = Buffer::empty(area);
        for (y, line) in drawn.lines.iter().enumerate() {
            buf.set_line(0, y as u16, line, cols);
        }
        let name = format!("context-mod/card@{cols}x{rows}.txt");
        approved_screen::assert_matches(&name, &buf, size, 0);
    }
}
