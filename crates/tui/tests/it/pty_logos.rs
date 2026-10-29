//! The logos' pictures: given to a terminal that draws placeholders and answers the graphics
//! query, and to no other, on the screen that draws them. kitty and Ghostty keep one store of
//! pictures for each screen and empty the alternate one each time it is entered, so the logos
//! go out again after every entry: at start on the full screen, after a program you ran, and
//! for a view that opens over the inline conversation. None go out while `ui.logos` is off, and
//! a font size change that calls for another picture size sends them again drawn for the new
//! cells. A model's name drops its maker's word only after a logo that shows. The bottom line
//! and its picker show each CLI's logo at every real size, and a council numbers the logo of a
//! model seated twice.

use std::os::unix::fs::PermissionsExt;

use argonauts_core::app::AppEvent;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::council::{
    CouncilEvent, CouncilEventKind, CouncilMessage, FloorReason, MessageKind, Seat, SeatRole,
    Seating, Speaker, TextPart,
};
use argonauts_core::logos::LogoMark;
use argonauts_core::names::{MessageId, ModelRef, SeatId, WorkflowId};
use argonauts_core::workflow::StepRef;
use argonauts_testkit::screen::{Cell, REAL_SIZES};
use serde_json::json;

use crate::pty_support;
use pty_support::*;

/// Eleven logos, four of them in a second ink for light screens.
const LOGO_PICTURES: usize = 15;

const ENTER_ALTERNATE: &str = "\x1b[?1049h";
const LEAVE_ALTERNATE: &str = "\x1b[?1049l";
/// The end of a synchronized update, which every frame is.
const END_UPDATE: &str = "\x1b[?2026l";

/// The virtual placements in `raw`, one a picture sent.
fn placements(raw: &str) -> usize {
    raw.matches("a=p,U=1,").count()
}

/// What the app wrote since it last entered the alternate screen.
fn since_last_entry(raw: &[u8]) -> String {
    let raw = String::from_utf8_lossy(raw);
    let from = raw
        .rfind(ENTER_ALTERNATE)
        .unwrap_or_else(|| panic!("never entered the alternate screen: {raw:?}"));
    raw[from..].to_owned()
}

fn started(launch: Launch) -> TestDriver {
    let driver = launch.start();
    driver.wait_for(READY_TEXT);
    driver
}

const READY_TEXT: &str = "Ready to go";

fn script() -> Script {
    Script::idle().at(0, notice("info", READY_TEXT))
}

/// The bottom line with a plan meter for each of three CLIs, the one in use at 15%.
fn three_plans() -> Script {
    let plans = status_with(json!({
        "main_agent": "claude-code/opus",
        "plan_chips": [chip("Claude Code", Some(plan_usage("claude-code", 0.15)), "healthy"),
                       chip("Codex", Some(plan_usage("codex", 0.44)), "healthy"),
                       chip("agy", Some(plan_usage("antigravity-cli", 0.0)), "healthy")],
        "line": {"settings": {"plan-usage": {"show": "all"}}}}));
    Script::idle().at(0, plans)
}

/// Each plan's meter after its CLI's logo, in whole cells.
const METERS: &str = r"\S+ 5h ▄▁▁▁▁▁  15%   \S+ 5h ▄▄▄▁▁▁  44%   \S+ 5h ▁▁▁▁▁▁   0%$";

/// Ghostty, as the app sees it from the variables it sets, at `rows` by `cols`.
fn ghostty(script: Script, rows: u16, cols: u16) -> Launch {
    TestDriver::launch(script)
        .env("TERM_PROGRAM", "ghostty")
        .env("COLORTERM", "truecolor")
        .takes_pictures()
        .size(rows, cols)
}

/// The logos the bottom line shows, by their placeholders' cells, once each shows its picture.
fn logos_shown(driver: &TestDriver) -> usize {
    driver.wait_for_pictures();
    let cells = driver.cells();
    let line = cells
        .cell_rows()
        .filter(|row| row.iter().any(|cell| !cell.text.trim().is_empty()))
        .last()
        .unwrap_or_default();
    let pictured = line
        .iter()
        .filter(|cell| cell.picture.is_some_and(|picture| picture.shows))
        .count();
    pictured / 2
}

#[test]
fn ghostty_answering_the_query_gets_every_logo_once() {
    let driver = started(
        TestDriver::launch(script())
            .env("TERM_PROGRAM", "ghostty")
            .env("COLORTERM", "truecolor")
            .takes_pictures(),
    );
    let raw = String::from_utf8_lossy(&driver.raw()).into_owned();
    assert_eq!(placements(&raw), LOGO_PICTURES);
}

/// A placeholder names its picture by its true colour, so without it no row draws a logo.
#[test]
fn without_true_colour_no_logo_goes_out() {
    let driver = started(
        TestDriver::launch(script())
            .env("TERM_PROGRAM", "ghostty")
            .takes_pictures(),
    );
    assert_eq!(placements(&String::from_utf8_lossy(&driver.raw())), 0);
}

#[test]
fn a_terminal_that_leaves_the_query_unanswered_gets_none() {
    let driver = started(TestDriver::launch(script()).env("TERM_PROGRAM", "ghostty"));
    assert_eq!(placements(&String::from_utf8_lossy(&driver.raw())), 0);
}

#[test]
fn wezterm_takes_pictures_but_gets_no_logos() {
    let driver = started(
        TestDriver::launch(script())
            .env("TERM_PROGRAM", "WezTerm")
            .takes_pictures(),
    );
    assert_eq!(placements(&String::from_utf8_lossy(&driver.raw())), 0);
}

#[test]
fn the_full_screen_shows_the_logos_from_its_first_frame() {
    for size in REAL_SIZES {
        let (rows, cols) = (size.rows, size.cols);
        let driver = ghostty(three_plans(), rows, cols).start();
        driver.wait_visible(METERS);
        assert_eq!(logos_shown(&driver), 3, "at {cols} x {rows}");
        // The main screen draws nothing, so it gets none.
        let raw = String::from_utf8_lossy(&driver.raw()).into_owned();
        let entry = raw.find(ENTER_ALTERNATE).expect("the full screen");
        assert_eq!(placements(&raw[..entry]), 0, "at {cols} x {rows}");
        assert_eq!(
            placements(&raw[entry..]),
            LOGO_PICTURES,
            "at {cols} x {rows}"
        );
    }
}

#[test]
fn the_logos_show_again_after_a_program_you_ran_gives_the_screen_back() {
    let tools = tempfile::tempdir().unwrap();
    let editor = tools.path().join("editor.sh");
    std::fs::write(
        &editor,
        "#!/bin/sh\nprintf 'rewritten in the editor' > \"$1\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut driver = ghostty(three_plans(), 40, 177)
        .env("EDITOR", editor.as_os_str())
        .start();
    driver.wait_visible(METERS);
    assert_eq!(logos_shown(&driver), 3);

    driver.press(Key::Ctrl('g'));
    driver.wait_visible("❯ rewritten in the editor");
    driver.wait_visible(METERS);
    let raw = driver.raw();
    assert_eq!(
        String::from_utf8_lossy(&raw)
            .matches(ENTER_ALTERNATE)
            .count(),
        2,
        "the full screen entered again"
    );
    assert_eq!(placements(&since_last_entry(&raw)), LOGO_PICTURES);
    assert_eq!(logos_shown(&driver), 3);
}

#[test]
fn inline_the_logos_go_to_the_main_screen_and_to_each_view_over_it() {
    let mut driver = ghostty(three_plans(), 38, 120).inline().start();
    driver.wait_visible(METERS);
    assert_eq!(logos_shown(&driver), 3);
    let raw = String::from_utf8_lossy(&driver.raw()).into_owned();
    assert!(!raw.contains(ENTER_ALTERNATE));
    assert_eq!(placements(&raw), LOGO_PICTURES);

    driver.press(Key::Ctrl('o'));
    driver.wait_visible("Showing detailed transcript");
    assert_eq!(placements(&since_last_entry(&driver.raw())), LOGO_PICTURES);

    driver.press(Key::Esc);
    driver.wait_visible(METERS);
    // The main screen still holds them.
    let raw = String::from_utf8_lossy(&driver.raw()).into_owned();
    let left = raw.rfind(LEAVE_ALTERNATE).expect("back on the main screen");
    assert_eq!(placements(&raw[left..]), 0);
    assert_eq!(logos_shown(&driver), 3);
}

/// A crew run whose workflow the view shows, with one agent on each of Claude Code's and
/// Codex's models.
fn crew_with_a_workflow() -> Script {
    let (mut run, crew, mut events) = crew_run(2);
    events.push(run.event(
        None,
        None,
        json!({"type": "workflow", "data": {"event": "started", "id": "wf-001",
            "from": "council", "version": "1", "parent": null,
            "workflow": {"name": "export-json", "description": "Add a --json flag to export",
                "phases": []}}}),
    ));
    Script::idle()
        .all(0, events)
        .await_command("request")
        .at(0, workflow_view(&crew, None))
}

/// Where `ui.logos` turns off and on again, once the test says.
fn logos_off_then_on() -> Script {
    three_plans()
        .await_signal("off")
        .at(0, ui_settings(json!({"logos": "off"})))
        .at(0, notice("info", "Logos are off"))
        .await_signal("on")
        .at(0, ui_settings(json!({})))
        .at(0, notice("info", "Logos are on"))
}

/// The cells of the first row that reads `text`, as the screen shows them.
fn row_reading(driver: &TestDriver, text: &str) -> Vec<Cell> {
    driver
        .cells()
        .cell_rows()
        .find(|row| {
            row.iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .contains(text)
        })
        .map(<[_]>::to_vec)
        .unwrap_or_else(|| panic!("no row reads {text}:\n{}", driver.text()))
}

#[test]
fn a_models_name_drops_its_makers_word_only_after_a_logo_that_shows() {
    for (pictures, name) in [(true, "5.6"), (false, "GPT 5.6")] {
        let launch = TestDriver::launch(crew_with_a_workflow())
            .env("TERM_PROGRAM", "ghostty")
            .env("COLORTERM", "truecolor")
            .size(38, 120);
        let launch = if pictures {
            launch.takes_pictures()
        } else {
            launch
        };
        let mut driver = launch.start();
        driver.wait_visible(WORKING);
        driver.press(Key::Ctrl('t'));
        driver.wait_visible(&format!(r"build:wire-json +\S+ +{name} +381\.0k tok"));
        if pictures {
            driver.wait_for_pictures();
        }
        let shown = row_reading(&driver, "build:wire-json")
            .iter()
            .filter(|cell| cell.picture.is_some_and(|picture| picture.shows))
            .count();
        assert_eq!(shown, if pictures { 2 } else { 0 }, "{name}");
    }
}

#[test]
fn logos_turned_off_go_to_no_screen_until_they_are_on_again() {
    let mut driver = ghostty(logos_off_then_on(), 38, 120).inline().start();
    driver.wait_visible(METERS);
    assert_eq!(logos_shown(&driver), 3);

    driver.signal("off");
    driver.wait_visible("Logos are off");
    driver.press(Key::Ctrl('o'));
    driver.wait_visible("Showing detailed transcript");
    assert_eq!(placements(&since_last_entry(&driver.raw())), 0);
    driver.press(Key::Esc);
    driver.wait_gone("Showing detailed transcript");

    driver.signal("on");
    driver.wait_visible("Logos are on");
    driver.wait_visible(METERS);
    assert_eq!(logos_shown(&driver), 3);
    // The main screen kept its own while they were off.
    assert_eq!(
        placements(&String::from_utf8_lossy(&driver.raw())),
        LOGO_PICTURES
    );
    driver.press(Key::Ctrl('o'));
    driver.wait_visible("Showing detailed transcript");
    assert_eq!(placements(&since_last_entry(&driver.raw())), LOGO_PICTURES);
}

#[test]
fn a_font_size_change_sends_the_logos_again_drawn_for_the_new_cells() {
    let mut driver = ghostty(three_plans(), 40, 177).start();
    driver.wait_visible(METERS);
    assert_eq!(logos_shown(&driver), 3);
    let raw = String::from_utf8_lossy(&driver.raw()).into_owned();
    let drawn_for = |cell| {
        String::from_utf8(argonauts_tui::kit::logo::send_pictures(Some(cell))).expect("escapes")
    };
    // The test terminal's cells are 10 x 20 pixels at start.
    assert!(raw.contains(&drawn_for((10, 20))));

    // A slightly larger font keeps the pictures' size, so a whole frame drawn after it, at a
    // new size so that it draws, sends none of them again.
    let from = driver.raw().len();
    driver.resize_with_cell(38, 120, (12, 24));
    driver.wait_visible(METERS);
    driver.wait_until("a frame drawn for the new size", |_| {
        String::from_utf8_lossy(&driver.raw()[from..]).contains(END_UPDATE)
    });
    assert_eq!(
        placements(&String::from_utf8_lossy(&driver.raw()[from..])),
        0
    );

    // A larger font: the same rows and columns, each cell twice as many pixels.
    let from = driver.raw().len();
    driver.resize_with_cell(38, 120, (20, 40));
    let larger = drawn_for((20, 40));
    assert_ne!(larger, drawn_for((10, 20)));
    driver.wait_until("the logos drawn for the larger cells", |_| {
        String::from_utf8_lossy(&driver.raw()[from..]).contains(&larger)
    });
    driver.wait_visible(METERS);
    assert_eq!(logos_shown(&driver), 3);

    // Rows and columns alone change no cell, so nothing goes again.
    let from = driver.raw().len();
    driver.resize(40, 177);
    driver.wait_visible(METERS);
    let after = String::from_utf8_lossy(&driver.raw()[from..]).into_owned();
    assert_eq!(placements(&after), 0);
    assert_eq!(logos_shown(&driver), 3);
}

#[test]
fn logos_off_at_start_go_out_once_they_turn_on() {
    let script = three_plans()
        .await_signal("on")
        .at(0, ui_settings(json!({})))
        .at(0, notice("info", "Logos are on"));
    let driver = ghostty(script, 40, 177).ui("logos = \"off\"").start();
    driver.wait_visible("15%");
    assert_eq!(placements(&String::from_utf8_lossy(&driver.raw())), 0);

    driver.signal("on");
    driver.wait_visible("Logos are on");
    driver.wait_visible(METERS);
    assert_eq!(logos_shown(&driver), 3);
    assert_eq!(
        placements(&String::from_utf8_lossy(&driver.raw())),
        LOGO_PICTURES
    );
}

/// Each row of the screen shown that draws a logo, as text, with how many of its logos show
/// their picture, once every placeholder shows its own.
fn logos_by_row(driver: &TestDriver) -> Vec<(String, usize)> {
    driver.wait_for_pictures();
    driver
        .cells()
        .cell_rows()
        .filter_map(|row| {
            let shown = row
                .iter()
                .filter(|cell| cell.picture.is_some_and(|picture| picture.shows))
                .count();
            let text: String = row
                .iter()
                .filter(|cell| cell.picture.is_none())
                .map(|cell| cell.text.as_str())
                .collect();
            (shown > 0).then(|| (text.trim().to_owned(), shown / 2))
        })
        .collect()
}

#[test]
fn the_bottom_line_picker_shows_each_clis_logo_and_the_line_it_will_draw() {
    for size in REAL_SIZES {
        let mut driver = ghostty(three_plans(), size.rows, size.cols).start();
        driver.wait_visible(METERS);
        driver.send("/bottom-line");
        driver.press(Key::Enter);
        driver.wait_visible("The line, as it will look");
        let rows = logos_by_row(&driver);
        for cli in ["Claude Code", "Codex", "agy"] {
            let row = rows
                .iter()
                .find(|(text, _)| text.contains(&format!("☒  {cli} ")))
                .unwrap_or_else(|| panic!("no row for {cli} at {size}: {rows:#?}"));
            assert_eq!(row.1, 1, "{cli} at {size}: {rows:#?}");
        }
        let line = rows
            .iter()
            .find(|(text, _)| text.contains("supervised mode on"))
            .unwrap_or_else(|| panic!("no line at {size}: {rows:#?}"));
        assert_eq!(line.1, 3, "the line at {size}: {rows:#?}");
    }
}

/// A council event of the run's talk step, as the engine writes it.
fn council(run: &mut RunScript, kind: CouncilEventKind) -> AppEvent {
    let talk = StepRef {
        workflow: WorkflowId::numbered(1),
        phase: 1,
        path: vec![0],
    };
    let kind = RunEventKind::Council(CouncilEvent { talk, kind });
    run.event(
        None,
        None,
        serde_json::to_value(kind).expect("a council event"),
    )
}

fn seat(n: u32, agent: &str, model: &str, number: Option<u8>) -> Seat {
    Seat {
        id: SeatId::numbered(n),
        model: ModelRef::new(agent, Some(model.to_owned())),
        role: SeatRole::Proposer,
        number,
    }
}

fn said(id: &str, n: u32, words: &str) -> CouncilEventKind {
    CouncilEventKind::Message(Box::new(CouncilMessage {
        id: MessageId::parse(id).expect("a message id"),
        from: Speaker::Seat(SeatId::numbered(n)),
        kind: MessageKind::Talk,
        text: vec![TextPart::Words(words.to_owned())],
        reply_to: None,
        mentions: Vec::new(),
        stances: Vec::new(),
        hidden: 0,
        hidden_names: Vec::new(),
        guesses_hidden: 0,
        rewritten: false,
        declined: false,
        started_before: None,
    }))
}

/// A council with Opus 5.5 seated twice beside 5.6 Luna: each seat says something, and the
/// second Opus writes again.
fn council_with_a_model_seated_twice() -> Script {
    let mut run = RunScript::default();
    let mut events = vec![
        status_with(json!({"run": RUN, "title": "Rate limit the API"})),
        mode_working(RUN),
        run.started("Rate limit the API", "crew"),
    ];
    let seated = CouncilEventKind::Seated(Seating {
        seats: vec![
            seat(1, "claude-code", "claude-opus-5-5", Some(1)),
            seat(2, "claude-code", "claude-opus-5-5", Some(2)),
            seat(3, "codex", "gpt-5.6-luna", None),
        ],
        notes: Some(SeatId::numbered(1)),
    });
    events.push(council(&mut run, seated));
    events.push(council(
        &mut run,
        said("m-1", 1, "A token bucket per key, in Redis."),
    ));
    events.push(council(
        &mut run,
        said("m-2", 2, "A window counter is simpler."),
    ));
    events.push(council(
        &mut run,
        said("m-3", 3, "Keep /health open with no key."),
    ));
    events.push(council(
        &mut run,
        CouncilEventKind::Writing {
            seat: SeatId::numbered(2),
            why: FloorReason::RepliedTo {
                by: SeatId::numbered(3),
            },
        },
    ));
    Script::idle().all(0, events)
}

/// A logo as its picture draws it: its mark, and its number when its model sits twice.
type Pictured = (LogoMark, Option<u8>);

/// Each row of the screen shown that draws a logo, as text, with the logos it shows.
fn pictures_by_row(driver: &TestDriver) -> Vec<(String, Vec<Pictured>)> {
    driver.wait_for_pictures();
    driver
        .cells()
        .cell_rows()
        .filter_map(|row| {
            let mut ids: Vec<u32> = row
                .iter()
                .filter_map(|cell| cell.picture.filter(|picture| picture.shows))
                .map(|picture| picture.id)
                .collect();
            // A logo's two halves are one picture.
            ids.dedup();
            let text: String = row
                .iter()
                .filter(|cell| cell.picture.is_none())
                .map(|cell| cell.text.as_str())
                .collect();
            let logos = ids
                .into_iter()
                .map(|id| LogoMark::of_picture(id).unwrap_or_else(|| panic!("picture {id}")))
                .collect::<Vec<_>>();
            (!logos.is_empty()).then(|| (text.trim().to_owned(), logos))
        })
        .collect()
}

#[test]
fn a_council_numbers_the_logo_of_a_model_seated_twice() {
    use LogoMark::{ClaudeCode, Codex};
    for size in REAL_SIZES {
        let driver = ghostty(council_with_a_model_seated_twice(), size.rows, size.cols).start();
        driver.wait_visible("Opus 5.5 · 2 is writing");
        let rows = pictures_by_row(&driver);
        let logos_of = |text: &str| {
            rows.iter()
                .find(|(row, _)| row.contains(text))
                .map(|(_, logos)| logos.clone())
                .unwrap_or_else(|| panic!("no row reads {text} at {size}: {rows:#?}"))
        };
        assert_eq!(
            logos_of("A token bucket per key"),
            [(ClaudeCode, Some(1))],
            "{size}"
        );
        assert_eq!(
            logos_of("A window counter is simpler"),
            [(ClaudeCode, Some(2))],
            "{size}"
        );
        assert_eq!(logos_of("Keep /health open"), [(Codex, None)], "{size}");
        assert_eq!(
            logos_of("Opus 5.5 · 2 is writing"),
            [(ClaudeCode, Some(2))],
            "{size}"
        );
        // A short screen has scrolled the council's name and its seats away.
        let named = driver.visible().lines().any(|row| row.trim() == "Council");
        let seats = rows
            .iter()
            .find(|(row, _)| row.is_empty())
            .map(|(_, logos)| logos.clone());
        assert_eq!(
            seats,
            named.then(|| vec![(ClaudeCode, Some(1)), (ClaudeCode, Some(2)), (Codex, None)]),
            "the seats over the council's name at {size}: {rows:#?}"
        );
    }
}
