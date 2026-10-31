//! The setup sheets drawn beside their pictures in `tests/golden/setup_*`, cell for cell with
//! colours and weight, and the keys that move the pointer and send each change.
//!
//! A picture writes a logo's two cells as `▐▌`, or `▐` and the seat's number for a model that
//! sits more than once; those two cells hold whatever draws the logo, so they are checked to
//! hold one, not what it is.

use argonauts_core::app::setup_sheet::{
    AsideRow, FillRow, Part, Pointer, SetupSheet, SheetAction, SheetColumn, SheetLayout, SheetRow,
    Tone, UnderLine, WayRow, Words,
};
use argonauts_core::look::logos::Logo;
use argonauts_testkit::pictures::picture;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Modifier};

use super::*;
use crate::frame::state::AppState;
use crate::testing::pictures::workflow_pictures::dark;

const WIDTH: u16 = 120;

fn cc() -> Logo {
    Logo::dot("claude-code")
}

fn named(logo: Logo, name: &str) -> Part {
    Part::named(logo, name)
}

fn seat(name: &str, number: Option<u8>) -> Part {
    Part::Named {
        logo: cc(),
        name: name.to_owned(),
        number,
    }
}

fn text(words: &str) -> Part {
    Part::text(words)
}

fn ready() -> Option<Words> {
    Some(Words::new("Ready", Tone::Ok))
}

fn seat_row(id: &str, role: &str, who: Part, what: &str) -> SheetRow {
    SheetRow::Fill(FillRow {
        id: id.to_owned(),
        label: "Seat".to_owned(),
        role: Words::plain(role),
        who: vec![who],
        who_tone: Tone::Text,
        what: vec![Words::dim(what)],
        state: ready(),
        columns: vec![SheetColumn::Who, SheetColumn::Role],
        lit: false,
        under: Vec::new(),
    })
}

fn step_row(
    id: &str,
    label: &str,
    role: &str,
    who: Vec<Part>,
    what: Vec<Words>,
    state: &str,
) -> FillRow {
    FillRow {
        id: id.to_owned(),
        label: label.to_owned(),
        role: Words::dim(role),
        who,
        who_tone: Tone::Text,
        what,
        state: Some(Words::new(state, Tone::Ok)),
        columns: vec![SheetColumn::Who],
        lit: false,
        under: Vec::new(),
    }
}

fn ways() -> Vec<SheetRow> {
    let way = |id: &str, name: &str, value: Option<&str>, hint: Option<&str>| {
        SheetRow::Way(WayRow {
            id: id.to_owned(),
            allowed: true,
            name: name.to_owned(),
            value: value.map(str::to_owned),
            hint: hint.map(str::to_owned),
            inner: false,
        })
    };
    vec![
        SheetRow::Heading {
            label: "How tasks are done".to_owned(),
            text: "Picked per task, from what you allow".to_owned(),
        },
        way("ways.own_session", "Own session", None, None),
        way(
            "ways.workflows",
            "Workflows",
            Some("Any"),
            Some("or a list: Maker-checker, Spec-first, …"),
        ),
        way("ways.agents", "Agents", Some("Up to 4"), None),
    ]
}

const COUNCIL_ABOUT: [&str; 2] = [
    "Each seat answers alone first, then they talk until every seat agrees, then they split the work.",
    "Seats see each other only as Seat letters, never a name, a maker or a CLI. You see real names.",
];
const COUNCIL_KEYS: &str = "space adds or drops · tab to the next column · ←  →  to change · ↑  ↓  \
                            to move · enter to switch · esc to leave it";
const WAYS_NOTES: [&str; 2] = [
    "The note keeper picks a way for each task from these and gives its reason in a few words.",
    "A list of workflows means any of them. A way you ask for in your prompt, or pick at the plan, \
     decides that task.",
];

fn council(rows: Vec<SheetRow>, notes: Vec<&str>, pointer: Pointer) -> SetupSheet {
    SetupSheet {
        workflow: "council".to_owned(),
        title: "Set up Council".to_owned(),
        about: COUNCIL_ABOUT.map(str::to_owned).to_vec(),
        status: Vec::new(),
        layout: SheetLayout::Seats,
        rows,
        notes: notes.into_iter().map(str::to_owned).collect(),
        keys: COUNCIL_KEYS.to_owned(),
        pointer: Some(pointer),
        ready: true,
        not_yet: None,
        refresh: false,
    }
}

fn checks(who: Part, what: &str, state: Words) -> SheetRow {
    SheetRow::Fill(FillRow {
        id: "checks".to_owned(),
        label: "Checks".to_owned(),
        role: Words::plain(""),
        who: vec![who],
        who_tone: Tone::Text,
        what: vec![Words::dim(what)],
        state: Some(state),
        columns: vec![SheetColumn::Who],
        lit: true,
        under: Vec::new(),
    })
}

/// Opus 5.5 in two seats, a role each, and Codex, agy and OpenCode set up too.
fn council_twice() -> SetupSheet {
    let mut rows = vec![
        seat_row(
            "seat-1",
            "Note keeper",
            seat("Opus 5.5 · 1", Some(1)),
            "Proposes, lists the points, writes the plan",
        ),
        seat_row(
            "seat-2",
            "Skeptic",
            seat("Opus 5.5 · 2", Some(2)),
            "Argues against where most seats lean",
        ),
        seat_row(
            "seat-3",
            "Proposer",
            named(Logo::dot("codex"), "5.6 Luna"),
            "Says what it thinks is right",
        ),
        seat_row(
            "seat-4",
            "Checker",
            named(Logo::dot("antigravity-cli"), "3.8 Flash"),
            "Checks each task in a fresh session",
        ),
        SheetRow::Aside(AsideRow {
            id: Some("add".to_owned()),
            logo: None,
            name: "Add one".to_owned(),
            parts: vec![
                text("Up to 6, the same model again too · next ready: "),
                named(Logo::dot("ollama"), "Qwen3 Coder 30B"),
            ],
        }),
        checks(
            named(Logo::dot("antigravity-cli"), "3.8 Flash, the checker seat"),
            "Another maker for every task, fresh each time",
            Words::new("Ready", Tone::Ok),
        ),
        SheetRow::Blank,
    ];
    rows.extend(ways());
    let notes = vec![
        "Opus 5.5 sits twice: two sessions that share nothing, not even memory. You see them as \
         Opus 5.5 · 1 and · 2.",
        "The skeptic answers alone first, like every seat. In the talk it argues the other side; \
         its own stance counts.",
        "The seats are told which seat is the skeptic, and never which model runs any seat.",
        "Each check is a fresh session: the plan and the change, never the talk, never who made \
         it. Another maker first.",
        WAYS_NOTES[0],
        WAYS_NOTES[1],
    ];
    council(
        rows,
        notes,
        Pointer {
            row: "seat-2".to_owned(),
            column: SheetColumn::Role,
        },
    )
}

/// Only Claude Code is set up: three Opus 5.5 seats, and each check falls to a fresh Opus 5.5
/// session, marked Same maker.
fn council_one_maker() -> SetupSheet {
    let not_yet = |agent: &str, name: &str, why: &str| {
        SheetRow::Aside(AsideRow {
            id: None,
            logo: Some(Logo::dot(agent)),
            name: name.to_owned(),
            parts: vec![text(why)],
        })
    };
    let mut rows = vec![
        seat_row(
            "seat-1",
            "Note keeper",
            seat("Opus 5.5 · 1", Some(1)),
            "Proposes, lists the points, writes the plan",
        ),
        seat_row(
            "seat-2",
            "Skeptic",
            seat("Opus 5.5 · 2", Some(2)),
            "Argues against where most seats lean",
        ),
        seat_row(
            "seat-3",
            "Proposer",
            seat("Opus 5.5 · 3", Some(3)),
            "Says what it thinks is right",
        ),
        SheetRow::Aside(AsideRow {
            id: Some("add".to_owned()),
            logo: None,
            name: "Add one".to_owned(),
            parts: vec![text("Up to 6, the same model again too")],
        }),
        not_yet(
            "codex",
            "Codex",
            "Logged out · run codex login, and it shows here",
        ),
        not_yet("antigravity-cli", "agy", "Not installed · /agents add agy"),
        not_yet(
            "opencode",
            "OpenCode",
            "Not installed · /agents add opencode",
        ),
        checks(
            named(cc(), "A fresh Opus 5.5 session"),
            "Blind to who made it; no other maker set up",
            Words::new("Same maker", Tone::Warn),
        ),
        SheetRow::Blank,
    ];
    rows.extend(ways());
    let notes = vec![
        "Only Claude Code is set up, so a fresh Opus 5.5 session checks each task. It never sees \
         the talk or who made",
        "the change, but it may still favour its own style, and each check is marked as such. \
         ←  →  offers Sonnet 5:",
        "another model, but the same maker. Three Opus 5.5 seats tend to make the same mistakes; \
         the skeptic helps.",
        WAYS_NOTES[0],
        WAYS_NOTES[1],
    ];
    council(
        rows,
        notes,
        Pointer {
            row: "checks".to_owned(),
            column: SheetColumn::Who,
        },
    )
}

const PICKS: &str = "The orchestrator picks";
const ORCHESTRATOR_KEYS: &str = "←  →  who · ↑  ↓  step · enter to switch to Orchestrator · \
                                 ctrl+s for this session · esc to leave it";

fn orchestrator(state: &str) -> SetupSheet {
    let luna = || named(Logo::dot("codex"), "5.6 Luna");
    let opus = || named(cc(), "Opus 5.5");
    let qwen = || named(Logo::dot("ollama"), "Qwen3 Coder 30B");
    let status = match state {
        "build" => vec![
            Words::plain("The orchestrator now "),
            Words::new("picks", Tone::Ok),
            Words::plain(
                " who builds each task. Enter saves it to ~/.argonauts/config.toml and switches.",
            ),
        ],
        "review" => vec![
            Words::plain("The orchestrator now "),
            Words::new("picks", Tone::Ok),
            Words::plain(" who builds and who reviews each task. Enter saves both and switches."),
        ],
        _ => vec![
            Words::plain("Every way is "),
            Words::new("allowed", Tone::Ok),
            Words::plain(
                ", as by default. Space keeps one out of every plan; Enter saves and switches.",
            ),
        ],
    };
    let plan = step_row(
        "plan",
        "Plan",
        "Orchestrator",
        vec![luna()],
        vec![Words::plain("Plans and hands out the tasks")],
        "Picked",
    );
    let mut build = step_row(
        "build",
        "Build",
        "Workers",
        vec![text(PICKS)],
        vec![Words::plain("A ready model for each task")],
        "Ready",
    );
    build.under = vec![
        UnderLine {
            label: "Can pick".to_owned(),
            parts: vec![opus(), text(" · "), luna(), text(" · "), qwen()],
        },
        UnderLine {
            label: "Not now".to_owned(),
            parts: vec![
                named(Logo::dot("antigravity-cli"), "3.8 Flash"),
                text(", out of plan until 3:40 PM"),
            ],
        },
        UnderLine {
            label: "Picks by".to_owned(),
            parts: vec![text(
                "Past results on this kind of task, plan left, then cost",
            )],
        },
    ];
    let named_reviewers = vec![opus(), text(", "), qwen()];
    let review_who = match state {
        "build" => named_reviewers.clone(),
        _ => vec![text(PICKS)],
    };
    let mut review = step_row(
        "review",
        "Review",
        "Reviewers",
        review_who,
        vec![Words::plain("2 per task, from another provider")],
        "Ready",
    );
    review.under = vec![UnderLine {
        label: String::new(),
        parts: vec![text("Never the task's worker, and never from its provider")],
    }];
    let mut sign_off = step_row(
        "sign-off",
        "Sign-off",
        "Judge",
        match state {
            "build" => named_reviewers,
            _ => vec![text(PICKS)],
        },
        vec![Words::dim("Follows"), Words::plain(" the reviewers")],
        "Ready",
    );
    if state != "build" {
        sign_off.who_tone = Tone::Dim;
    }
    let mut rows = vec![
        SheetRow::Fill(plan),
        SheetRow::Fill(build),
        SheetRow::Fill(review),
        SheetRow::Fill(sign_off),
        SheetRow::Blank,
    ];
    rows.extend(ways());
    let (pointer, keys) = match state {
        "build" => ("build", ORCHESTRATOR_KEYS.to_owned()),
        "review" => ("review", ORCHESTRATOR_KEYS.to_owned()),
        _ => (
            "ways.workflows",
            "space allows it or not · ←  →  any or a list · ↑  ↓  step · enter to switch to \
             Orchestrator · esc to leave it"
                .to_owned(),
        ),
    };
    SetupSheet {
        workflow: "orchestrator".to_owned(),
        title: "Set up Orchestrator".to_owned(),
        about: vec![
            "One orchestrator plans and hands out the tasks, workers build them, and two \
             reviewers check each one."
                .to_owned(),
        ],
        status,
        layout: SheetLayout::Steps,
        rows,
        notes: vec![
            "The orchestrator picks a way for each task from these and gives its reason in a few \
             words."
                .to_owned(),
            WAYS_NOTES[1].to_owned(),
        ],
        keys,
        pointer: Some(Pointer {
            row: pointer.to_owned(),
            column: SheetColumn::Who,
        }),
        ready: true,
        not_yet: None,
        refresh: false,
    }
}

/// The sheet drawn on its own, as tall as it asks to be.
fn drawn(view: &mut SetupSheetView) -> Buffer {
    drawn_at(view, WIDTH)
}

fn drawn_at(view: &mut SetupSheetView, width: u16) -> Buffer {
    let theme = dark();
    let state = AppState::default();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: std::time::Instant::now(),
    };
    let height = view
        .dock_height(Size::new(width, 38), &ctx)
        .expect("a sheet's height");
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    buf
}

fn colour_of(hex: &str) -> Color {
    let rgb = u32::from_str_radix(hex.trim_start_matches('#'), 16).expect("a colour");
    let [_, r, g, b] = rgb.to_be_bytes();
    Color::Rgb(r, g, b)
}

/// Fails unless `buf` draws the picture `name`: every cell's character, and under each one that
/// isn't blank its colour and weight.
fn assert_draws(name: &str, buf: &Buffer, text: &str, colours: &str) {
    let grid = picture::grid(colours);
    let rows: Vec<&str> = text.trim_end().lines().collect();
    let drawn: Vec<String> = (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect();
    crate::testing::pictures::picture_dump::rows(name, &drawn);
    assert_eq!(
        drawn.len(),
        rows.len(),
        "{name}: rows\ndrawn:\n{}\npicture:\n{}",
        drawn.join("\n"),
        rows.join("\n")
    );
    assert_eq!(
        grid.len(),
        rows.len(),
        "{name}: its two files disagree on rows"
    );
    let plain = [Role::Text, Role::Bright].map(|role| dark().style(role).fg);
    for (y, row) in grid.iter().enumerate() {
        assert_eq!(
            picture::text(row).trim_end(),
            rows[y].trim_end(),
            "{name}: its two files disagree on row {y}"
        );
        let mut x = 0;
        while x < row.len() {
            let want = &row[x];
            let cell = &buf[(x as u16, y as u16)];
            if want.ch == '▐' {
                assert_ne!(cell.symbol(), " ", "{name}: no logo at row {y}, column {x}");
                x += 2;
                continue;
            }
            assert_eq!(
                cell.symbol(),
                want.ch.to_string(),
                "{name}: row {y}, column {x}\ndrawn:   {}\npicture: {}",
                drawn[y],
                rows[y]
            );
            if !want.ch.is_whitespace() {
                let got = match cell.fg {
                    Color::Reset => None,
                    fg if plain.contains(&Some(fg)) => None,
                    fg => Some(fg),
                };
                let wanted = want.color.as_deref().map(colour_of);
                assert_eq!(
                    got, wanted,
                    "{name}: colour at row {y}, column {x} ({})",
                    want.ch
                );
                assert_eq!(
                    cell.modifier.contains(Modifier::BOLD),
                    want.bold,
                    "{name}: weight at row {y}, column {x} ({})",
                    want.ch
                );
            }
            x += 1;
        }
        for x in row.len()..usize::from(buf.area.width) {
            assert_eq!(
                buf[(x as u16, y as u16)].symbol(),
                " ",
                "{name}: row {y} runs on"
            );
        }
    }
}

macro_rules! golden {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/golden/",
                $name,
                ".txt"
            )),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/golden/",
                $name,
                ".html"
            )),
        )
    };
}

#[test]
fn set_up_council_with_one_model_in_two_seats_draws_as_its_picture() {
    let (name, text, colours) = golden!("setup_council_twice");
    let mut view = SetupSheetView::new(council_twice());
    assert_draws(name, &drawn(&mut view), text, colours);
}

#[test]
fn set_up_council_with_one_maker_draws_as_its_picture() {
    let (name, text, colours) = golden!("setup_council_one_maker");
    let mut view = SetupSheetView::new(council_one_maker());
    assert_draws(name, &drawn(&mut view), text, colours);
}

#[test]
fn set_up_orchestrator_draws_as_its_pictures() {
    for (name, text, colours) in [
        golden!("setup_orchestrator_build"),
        golden!("setup_orchestrator_review"),
        golden!("setup_orchestrator_ways"),
    ] {
        let state = name.trim_start_matches("setup_orchestrator_");
        let mut view = SetupSheetView::new(orchestrator(state));
        assert_draws(name, &drawn(&mut view), text, colours);
    }
}

#[test]
fn on_a_narrower_screen_what_a_step_does_is_cut_short_of_its_state() {
    let mut view = SetupSheetView::new(orchestrator("build"));
    let buf = drawn_at(&mut view, 100);
    let rows: Vec<String> = (0..buf.area.height)
        .map(|y| (0..100).map(|x| buf[(x, y)].symbol()).collect())
        .collect();
    let steps: Vec<&String> = rows
        .iter()
        .filter(|row| row.trim_end().ends_with("Ready"))
        .collect();
    assert!(!steps.is_empty(), "{rows:#?}");
    for row in steps {
        let before = row.trim_end().strip_suffix("Ready").unwrap();
        assert!(before.ends_with("  "), "{row:?}");
        assert!(before.trim_end().ends_with('…'), "{row:?}");
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn press(view: &mut SetupSheetView, code: KeyEvent) -> ViewOutcome {
    let theme = dark();
    let state = AppState::default();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: std::time::Instant::now(),
    };
    view.handle_key(code, &ctx)
}

fn sent(outcome: ViewOutcome) -> Option<SheetAction> {
    match outcome {
        ViewOutcome::Send(UiCommand::SetupSheet { action })
        | ViewOutcome::SendToConversation(UiCommand::SetupSheet { action }) => Some(action),
        _ => None,
    }
}

#[test]
fn the_pointer_moves_over_what_it_can_change_and_each_key_says_what_to_change() {
    let mut view = SetupSheetView::new(council_twice());
    assert_eq!(view.at.as_deref(), Some("seat-2"));
    assert_eq!(view.column, SheetColumn::Role);
    assert_eq!(
        sent(press(&mut view, key(KeyCode::Right))),
        Some(SheetAction::Change {
            row: "seat-2".into(),
            column: SheetColumn::Role,
            forward: true,
        })
    );
    press(&mut view, key(KeyCode::Tab));
    assert_eq!(
        sent(press(&mut view, key(KeyCode::Left))),
        Some(SheetAction::Change {
            row: "seat-2".into(),
            column: SheetColumn::Who,
            forward: false,
        })
    );
    // Down past the seats reaches Add one, then Checks; the heading and the blank are passed.
    for _ in 0..3 {
        press(&mut view, key(KeyCode::Down));
    }
    assert_eq!(view.at.as_deref(), Some("add"));
    assert_eq!(
        sent(press(&mut view, key(KeyCode::Char(' ')))),
        Some(SheetAction::Toggle { row: "add".into() })
    );
    for _ in 0..2 {
        press(&mut view, key(KeyCode::Down));
    }
    assert_eq!(view.at.as_deref(), Some("ways.own_session"));
    for _ in 0..9 {
        press(&mut view, key(KeyCode::Down));
    }
    assert_eq!(
        view.at.as_deref(),
        Some("ways.agents"),
        "it stops at the last"
    );
}

#[test]
fn enter_saves_only_once_it_can_and_esc_leaves_with_nothing_saved() {
    let mut sheet = council_one_maker();
    sheet.ready = false;
    sheet.not_yet = Some("Council needs two seats that can answer".into());
    let mut view = SetupSheetView::new(sheet);
    assert!(matches!(
        press(&mut view, key(KeyCode::Enter)),
        ViewOutcome::Notice(why) if why == "Council needs two seats that can answer"
    ));
    let mut ready = council_one_maker();
    ready.refresh = true;
    assert!(view.accept(&ViewData::SetupSheet {
        sheet: Box::new(ready)
    }));
    assert_eq!(
        sent(press(&mut view, key(KeyCode::Enter))),
        Some(SheetAction::Save { session: false })
    );
    assert_eq!(
        sent(press(
            &mut view,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)
        )),
        Some(SheetAction::Save { session: true })
    );
    assert!(matches!(
        press(&mut view, key(KeyCode::Esc)),
        ViewOutcome::SendToConversation(UiCommand::SetupSheet {
            action: SheetAction::Close
        })
    ));
}

#[test]
fn a_sheet_sent_again_keeps_the_pointer_where_it_was() {
    let mut view = SetupSheetView::new(council_twice());
    press(&mut view, key(KeyCode::Down));
    assert_eq!(view.at.as_deref(), Some("seat-3"));
    let mut again = council_twice();
    again.pointer = None;
    again.refresh = true;
    assert!(view.accept(&ViewData::SetupSheet {
        sheet: Box::new(again)
    }));
    assert_eq!(view.at.as_deref(), Some("seat-3"));
    let other = orchestrator("build");
    assert!(
        !view.accept(&ViewData::SetupSheet {
            sheet: Box::new(other)
        }),
        "another workflow's sheet isn't this one's"
    );
}

#[test]
fn a_screen_reader_reads_each_row() {
    let lines = reader_lines(&council_one_maker());
    assert_eq!(lines[0], "Set up Council");
    assert!(lines.contains(
        &"Seat: Skeptic, Opus 5.5 · 2, Argues against where most seats lean, Ready".to_owned()
    ));
    assert!(lines.contains(&"Codex: Logged out · run codex login, and it shows here".to_owned()));
    assert!(lines.contains(&"Workflows: allowed Any".to_owned()));
}
