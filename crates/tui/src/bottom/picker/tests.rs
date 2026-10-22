use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{Picker, PickerEvent, PickerRow, VISIBLE_ROWS};
use crate::render::{Clock, RenderCtx};
use crate::text::plain_text;
use crate::theme::{Glyphs, Theme};
use crate::views::settings::flow::Stage;

fn picker(labels: &[&str]) -> Picker {
    let mut picker = Picker::new("Model", "Pick one");
    picker.rows = labels
        .iter()
        .map(|label| PickerRow::new(*label, ""))
        .collect();
    picker
}

fn press(picker: &mut Picker, code: KeyCode) -> PickerEvent {
    picker.handle_key(KeyEvent::from(code))
}

fn text(picker: &Picker, width: u16) -> String {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    plain_text(&picker.lines(&ctx))
}

#[test]
fn numbers_pick_and_typing_filters() {
    let mut picker = picker(&["haiku", "opus", "gpt-5.6"]);
    assert_eq!(
        press(&mut picker, KeyCode::Char('2')),
        PickerEvent::Picked(1)
    );
    assert_eq!(press(&mut picker, KeyCode::Char('9')), PickerEvent::Moved);

    assert_eq!(press(&mut picker, KeyCode::Char('g')), PickerEvent::Moved);
    // Once a filter starts, digits are part of it.
    press(&mut picker, KeyCode::Char('p'));
    press(&mut picker, KeyCode::Char('t'));
    press(&mut picker, KeyCode::Char('-'));
    press(&mut picker, KeyCode::Char('5'));
    assert_eq!(picker.shown(), [2]);
    assert_eq!(press(&mut picker, KeyCode::Enter), PickerEvent::Picked(2));
    press(&mut picker, KeyCode::Backspace);
    assert_eq!(picker.filter, "gpt-");
    assert_eq!(press(&mut picker, KeyCode::Esc), PickerEvent::Closed);
}

#[test]
fn letters_pick_when_rows_have_them() {
    let mut picker = picker(&["Just this session", "Everywhere"]);
    picker.rows[0].key = Some('s');
    picker.rows[1].key = Some('e');
    assert_eq!(
        press(&mut picker, KeyCode::Char('E')),
        PickerEvent::Picked(1)
    );
    assert_eq!(press(&mut picker, KeyCode::Char('x')), PickerEvent::Moved);
    assert!(picker.filter.is_empty());
}

#[test]
fn the_reset_row_reports_reset() {
    let mut picker = picker(&["dark", "Reset"]);
    picker.rows[1].reset = true;
    assert_eq!(
        press(&mut picker, KeyCode::Char('2')),
        PickerEvent::Reset(1)
    );
}

#[test]
fn a_typed_line_takes_every_character() {
    let mut picker = picker(&[]);
    picker.input = Some(String::new());
    for ch in "45 q".chars() {
        assert_eq!(press(&mut picker, KeyCode::Char(ch)), PickerEvent::Moved);
    }
    press(&mut picker, KeyCode::Backspace);
    assert_eq!(
        press(&mut picker, KeyCode::Enter),
        PickerEvent::Entered("45 ".into())
    );
    let control_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(picker.handle_key(control_c), PickerEvent::Unhandled);
}

#[test]
fn quit_keys_go_on() {
    let mut picker = picker(&["a"]);
    let control_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(picker.handle_key(control_c), PickerEvent::Unhandled);
}

#[test]
fn long_lists_scroll_with_arrows_on_the_edges() {
    let labels: Vec<String> = (1..=12).map(|n| format!("row {n}")).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let mut picker = picker(&labels);
    let first = text(&picker, 60);
    assert!(first.contains("↓ 8. row 8"), "{first}");
    assert!(first.contains("… +4 more"), "{first}");
    assert!(!first.contains("row 9"), "{first}");

    for _ in 0..VISIBLE_ROWS {
        press(&mut picker, KeyCode::Down);
    }
    let scrolled = text(&picker, 60);
    assert!(scrolled.contains("↑ 2. row 2"), "{scrolled}");
    assert!(scrolled.contains("❯ 9. row 9"), "{scrolled}");
    assert!(scrolled.contains("… +3 more"), "{scrolled}");

    press(&mut picker, KeyCode::End);
    let end = text(&picker, 60);
    assert!(end.contains("❯ 12. row 12"), "{end}");
    assert!(!end.contains("more"), "{end}");
}

#[test]
fn no_rows_shows_the_placeholder() {
    let mut picker = picker(&[]);
    picker.placeholder = "Asking which models fit…".into();
    let shown = text(&picker, 40);
    assert!(shown.contains("Asking which models fit…"), "{shown}");
    assert!(shown.contains("Esc to close"), "{shown}");
}

fn type_text(picker: &mut Picker, text: &str) {
    for c in text.chars() {
        press(picker, KeyCode::Char(c));
    }
}

fn picker_of_four() -> Picker {
    picker(&["haiku", "opus", "sonnet", "gpt-5.6"])
}

#[test]
fn without_vim_keys_letters_filter_and_hjkl_are_letters() {
    let mut picker = picker_of_four();
    assert!(text(&picker, 80).contains("↑↓  to move · type to filter · Esc to close"));
    assert_eq!(press(&mut picker, KeyCode::Char('j')), PickerEvent::Moved);
    assert_eq!(picker.filter, "j");
    assert!(picker.shown().is_empty());
    press(&mut picker, KeyCode::Backspace);
    type_text(&mut picker, "o");
    assert_eq!(picker.shown(), [1, 2]);
    press(&mut picker, KeyCode::Down);
    assert_eq!(picker.pointed(), Some(2));
    type_text(&mut picker, "l");
    assert_eq!(picker.filter, "ol");
    assert_eq!(press(&mut picker, KeyCode::Esc), PickerEvent::Closed);
}

#[test]
fn vim_keys_move_choose_and_close_without_filtering() {
    let mut picker = picker_of_four();
    picker.vim = true;
    assert_eq!(press(&mut picker, KeyCode::Char('j')), PickerEvent::Moved);
    assert_eq!(press(&mut picker, KeyCode::Char('j')), PickerEvent::Moved);
    assert_eq!(picker.pointed(), Some(2));
    press(&mut picker, KeyCode::Char('k'));
    assert_eq!(picker.pointed(), Some(1));
    press(&mut picker, KeyCode::Char('G'));
    assert_eq!(picker.pointed(), Some(3));
    press(&mut picker, KeyCode::Char('g'));
    assert_eq!(picker.pointed(), Some(0));
    // Other letters do nothing, and never filter.
    assert_eq!(press(&mut picker, KeyCode::Char('o')), PickerEvent::Moved);
    assert_eq!(picker.filter, "");
    assert_eq!(picker.shown().len(), 4);
    // The arrows and numbers work as without vim keys.
    press(&mut picker, KeyCode::Down);
    assert_eq!(
        press(&mut picker, KeyCode::Char('l')),
        PickerEvent::Picked(1)
    );
    assert_eq!(
        press(&mut picker, KeyCode::Char('3')),
        PickerEvent::Picked(2)
    );
    assert_eq!(press(&mut picker, KeyCode::Char('h')), PickerEvent::Closed);
    // A paste waits for `/` too.
    assert_eq!(picker.handle_paste("opus"), PickerEvent::Unhandled);
    assert!(text(&picker, 80).contains("j/k to move · / to filter · Esc to close"));
}

#[test]
fn with_vim_keys_slash_starts_the_filter_and_esc_leaves_it() {
    let mut picker = picker_of_four();
    picker.vim = true;
    assert_eq!(press(&mut picker, KeyCode::Char('/')), PickerEvent::Moved);
    assert!(picker.filtering);
    assert!(text(&picker, 80).contains("Filter:"));
    assert!(text(&picker, 80).contains("Esc to clear the filter"));
    // While filtering, every letter and digit is filter text.
    type_text(&mut picker, "gpt-5");
    assert_eq!(picker.filter, "gpt-5");
    assert_eq!(picker.shown(), vec![3]);
    assert_eq!(press(&mut picker, KeyCode::Esc), PickerEvent::Moved);
    assert!(!picker.filtering);
    assert_eq!(picker.filter, "");
    assert_eq!(picker.shown().len(), 4);
    // Back to moving with letters, and Esc closes now.
    press(&mut picker, KeyCode::Char('j'));
    assert_eq!(picker.pointed(), Some(1));

    // Enter while filtering picks the row the filter left under the pointer.
    press(&mut picker, KeyCode::Char('/'));
    type_text(&mut picker, "son");
    assert_eq!(press(&mut picker, KeyCode::Enter), PickerEvent::Picked(2));
    assert_eq!(picker.handle_paste("x"), PickerEvent::Moved);
    assert_eq!(picker.filter, "sonx");
    assert_eq!(press(&mut picker, KeyCode::Esc), PickerEvent::Moved);
    assert_eq!(press(&mut picker, KeyCode::Esc), PickerEvent::Closed);
}

#[test]
fn with_vim_keys_rows_with_letters_still_pick_by_letter() {
    let mut picker = picker(&["Just this session", "This project, just me", "Everywhere"]);
    for (row, key) in picker.rows.iter_mut().zip(['s', 'm', 'e']) {
        row.key = Some(key);
    }
    picker.vim = true;
    press(&mut picker, KeyCode::Char('j'));
    assert_eq!(picker.pointed(), Some(1));
    assert_eq!(press(&mut picker, KeyCode::Char('/')), PickerEvent::Moved);
    assert!(!picker.filtering);
    assert_eq!(
        press(&mut picker, KeyCode::Char('e')),
        PickerEvent::Picked(2)
    );
    // A new set of rows starts with no filter being typed.
    picker.filtering = true;
    picker.set_rows(vec![PickerRow::new("a", "")]);
    assert!(!picker.filtering);
}

#[test]
fn the_arrows_report_left_and_right_and_vim_keys_follow_them_only_where_they_go_somewhere() {
    let mut picker = picker_of_four();
    assert_eq!(press(&mut picker, KeyCode::Left), PickerEvent::Left);
    assert_eq!(press(&mut picker, KeyCode::Right), PickerEvent::Right);
    picker.vim = true;
    assert_eq!(press(&mut picker, KeyCode::Char('h')), PickerEvent::Closed);
    assert_eq!(
        press(&mut picker, KeyCode::Char('l')),
        PickerEvent::Picked(0)
    );
    // A level down, h goes back as ← does.
    picker.flow.stage = Stage::ModelsOf {
        role: None,
        cli: "codex".into(),
    };
    assert_eq!(press(&mut picker, KeyCode::Char('h')), PickerEvent::Left);
    // With a value on the arrows, h and l move it.
    picker.flow.effort = Some(argonauts_core::model_picker::EffortLine::new(
        &["low".to_owned(), "high".to_owned()],
        None,
        None,
    ));
    assert_eq!(press(&mut picker, KeyCode::Char('l')), PickerEvent::Right);
    assert_eq!(press(&mut picker, KeyCode::Char('h')), PickerEvent::Left);
}

#[test]
fn the_slider_takes_the_arrows_enter_and_esc_and_keeps_typing_from_the_composer() {
    let stops = ["low", "high"]
        .iter()
        .map(|&level| argonauts_core::model_picker::EffortStop {
            level: level.into(),
            description: String::new(),
        })
        .collect();
    let slider = argonauts_core::model_picker::EffortSlider::new(
        None,
        "codex".parse().unwrap(),
        "GPT 6",
        stops,
        None,
        None,
    );
    let mut picker = Picker::new("Effort", "");
    picker.flow.slider = slider;
    assert_eq!(press(&mut picker, KeyCode::Left), PickerEvent::Left);
    assert_eq!(press(&mut picker, KeyCode::Right), PickerEvent::Right);
    assert_eq!(press(&mut picker, KeyCode::Enter), PickerEvent::Confirmed);
    assert_eq!(press(&mut picker, KeyCode::Esc), PickerEvent::Closed);
    assert_eq!(press(&mut picker, KeyCode::Char('l')), PickerEvent::Moved);
    assert_eq!(press(&mut picker, KeyCode::Up), PickerEvent::Moved);
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(picker.handle_key(ctrl_c), PickerEvent::Unhandled);
    picker.vim = true;
    assert_eq!(press(&mut picker, KeyCode::Char('l')), PickerEvent::Right);
    assert_eq!(press(&mut picker, KeyCode::Char('h')), PickerEvent::Left);
}

/// A row's state comes before its description as the state's glyph, in the state's color, and the
/// descriptions still start in one column. A narrow list cuts the words, never the mark alone.
#[test]
fn a_rows_state_comes_before_its_description_in_the_states_color() {
    use crate::kit::State;
    use crate::theme::Role;

    let mut picker = Picker::new("Agents", "The agents Argonauts Code can use");
    picker.rows = vec![
        PickerRow {
            state: Some(State::Done),
            ..PickerRow::new("codex", "on · gpt-5.6")
        },
        PickerRow {
            state: Some(State::Failed),
            ..PickerRow::new("opencode", "not installed · npm i -g opencode-ai@latest")
        },
        PickerRow::new("Add an endpoint", "by its base URL"),
    ];
    let drawn = text(&picker, 80);
    for row in [
        "1. codex             ✔ on · gpt-5.6",
        "2. opencode          ✗ not installed · npm i -g opencode-ai@latest",
        "3. Add an endpoint   by its base URL",
    ] {
        assert!(drawn.contains(row), "{row:?} in\n{drawn}");
    }

    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
    let lines = picker.lines(&ctx);
    let mark = |glyph: &str| {
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.trim() == glyph)
            .unwrap_or_else(|| panic!("{glyph} is drawn"))
            .style
    };
    assert_eq!(mark("✔").fg, theme.style(Role::StateDone).fg);
    assert_eq!(mark("✗").fg, theme.style(Role::StateFailed).fg);

    let narrow = text(&picker, 44);
    assert!(narrow.contains("✗ not installed"), "{narrow}");
    for line in narrow.lines() {
        assert!(crate::text::str_width(line) <= 44, "{line:?}");
    }
}
