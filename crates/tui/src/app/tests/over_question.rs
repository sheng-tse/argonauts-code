//! A docked view that stays up while a question holds the keys: the question draws under it,
//! where its keys were, and takes every key until it is answered or passed on.

use super::*;
use crate::frame::state::AppState;
use crate::frame::view::{View, ViewCtx, ViewOutcome};

/// A docked view that stays over a question. It names each of its rows, its keys' row last,
/// and closes on any key it is given, so a key that reaches it shows.
#[derive(Debug)]
struct StaysProbe;

impl View for StaysProbe {
    fn docked(&self) -> bool {
        true
    }

    fn stays_over_question(&self) -> bool {
        true
    }

    fn render(
        &mut self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
        _ctx: &ViewCtx<'_>,
    ) {
        for row in 0..area.height {
            let text = if row + 1 == area.height {
                "probe keys".to_owned()
            } else {
                format!("probe row {row}")
            };
            buf.set_string(area.x, area.y + row, text, ratatui::style::Style::default());
        }
    }

    fn handle_key(&mut self, _key: KeyEvent, _ctx: &ViewCtx<'_>) -> ViewOutcome {
        ViewOutcome::Close
    }
}

fn ui_of(rows: u16) -> (Ui<vt100::Parser>, Instant) {
    crossterm::style::force_color_output(true);
    let terminal = InlineTerminal::new(
        vt100::Parser::new(rows, COLUMNS, 0),
        Size::new(COLUMNS, rows),
        Position::new(0, 2),
    );
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let t0 = Instant::now();
    (
        Ui::new(terminal, theme, ViewModel::new("conversation"), t0),
        t0,
    )
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn the_question_draws_under_the_view_and_takes_its_keys() {
    let (mut ui, t0) = ui_of(24);
    let state = AppState::default();
    ui.push_view(Box::new(StaysProbe));
    ui.frame_in(t0, &state).unwrap();
    let rows = screen(&ui);
    assert!(rows.iter().any(|row| row == "probe keys"), "{rows:#?}");

    ui.model_mut().bottom.question = Some(needs_you(9));
    let shown = t0 + Duration::from_millis(10);
    ui.frame_in(shown, &state).unwrap();
    assert_eq!(ui.views_open(), 1);
    let rows = screen(&ui);
    let top = rows
        .iter()
        .position(|row| row == "probe row 0")
        .unwrap_or_else(|| panic!("{rows:#?}"));
    // Its rows stand as they were, and the question's rule takes the row its keys had.
    assert_eq!(rows[top + 10], "probe row 10", "{rows:#?}");
    assert!(rows[top + 11].starts_with("──"), "{rows:#?}");
    // Under the view the question draws as it does alone.
    assert_eq!(rows[top + 12], "◆ Merge task-002 into main?", "{rows:#?}");
    assert!(!rows.iter().any(|row| row == "probe keys"), "{rows:#?}");
    assert!(
        rows.iter().any(|row| row == "  ❯ 1. Merge"),
        "the question has the pointer: {rows:#?}"
    );

    // A key goes to the question, and the view neither gets it nor closes.
    assert_eq!(
        ui.handle_event_in(key(KeyCode::Char('2')), shown + QUESTION_GUARD, &state),
        answer(9, 1)
    );
    assert_eq!(ui.views_open(), 1);

    // Once the question is gone the view has its keys' row and the keys again.
    ui.model_mut().bottom.question = None;
    let after = shown + Duration::from_secs(1);
    ui.frame_in(after, &state).unwrap();
    let rows = screen(&ui);
    assert!(rows.iter().any(|row| row == "probe keys"), "{rows:#?}");
    ui.handle_event_in(key(KeyCode::Char('2')), after, &state);
    assert_eq!(ui.views_open(), 0);
}

#[test]
fn a_question_passed_on_gives_the_view_its_keys_back() {
    let (mut ui, t0) = ui_of(24);
    let state = AppState::default();
    ui.push_view(Box::new(StaysProbe));
    ui.model_mut().bottom.question = Some(needs_you(9));
    ui.frame_in(t0, &state).unwrap();
    assert!(ui.model().bottom.focused_question().is_some());

    ui.handle_event_in(key(KeyCode::Esc), t0 + QUESTION_GUARD, &state);
    assert!(ui.model().bottom.focused_question().is_none());
    assert_eq!(ui.views_open(), 1, "Esc went to the question");

    let later = t0 + QUESTION_GUARD + Duration::from_millis(50);
    ui.frame_in(later, &state).unwrap();
    let rows = screen(&ui);
    assert!(rows.iter().any(|row| row == "probe keys"), "{rows:#?}");
    ui.handle_event_in(key(KeyCode::Char('j')), later, &state);
    assert_eq!(ui.views_open(), 0, "the view has the keys again");
}

/// Sixteen rows hold the question or the view, never both: the question shows alone, as it
/// does with no view up, and the view is back once it is answered.
#[test]
fn on_a_short_screen_the_question_shows_alone_and_the_view_comes_back() {
    let (mut ui, t0) = ui();
    let state = AppState::default();
    ui.push_view(Box::new(StaysProbe));
    ui.model_mut().bottom.question = Some(needs_you(9));
    ui.frame_in(t0, &state).unwrap();
    assert_eq!(ui.views_open(), 1);
    let rows = screen(&ui);
    assert!(
        !rows.iter().any(|row| row.starts_with("probe")),
        "{rows:#?}"
    );
    assert!(
        rows.iter().any(|row| row == "◆ Merge task-002 into main?"),
        "{rows:#?}"
    );
    assert_eq!(
        ui.handle_event_in(key(KeyCode::Char('2')), t0 + QUESTION_GUARD, &state),
        answer(9, 1)
    );

    ui.model_mut().bottom.question = None;
    let after = t0 + Duration::from_secs(1);
    ui.frame_in(after, &state).unwrap();
    let rows = screen(&ui);
    assert!(rows.iter().any(|row| row == "probe row 0"), "{rows:#?}");
}

/// A view that closes under any question.
#[derive(Debug)]
struct GoesProbe;

impl View for GoesProbe {
    fn render(
        &mut self,
        _area: ratatui::layout::Rect,
        _buf: &mut ratatui::buffer::Buffer,
        _ctx: &ViewCtx<'_>,
    ) {
    }

    fn handle_key(&mut self, _key: KeyEvent, _ctx: &ViewCtx<'_>) -> ViewOutcome {
        ViewOutcome::Consumed
    }
}

/// The question a setting change asks closes the views over the one that stays up for it, and
/// that one stays.
#[test]
fn a_settings_question_makes_way_down_to_the_view_that_stays() {
    let (mut ui, _) = ui_of(24);
    ui.push_view(Box::new(StaysProbe));
    ui.push_view(Box::new(GoesProbe));
    ui.make_way_for_question();
    assert_eq!(ui.views_open(), 1);
    ui.make_way_for_question();
    assert_eq!(ui.views_open(), 1);
}
