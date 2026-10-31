use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{Picker, PickerEvent, PickerRow, VISIBLE_ROWS};
use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::text::plain_text;
use crate::base::theme::{Glyphs, Theme};
use crate::bottom::picker::flow::Stage;

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

/// Typing a filter leaves every row where it stood: the columns are measured over the whole
/// list, and a row keeps its number.
#[test]
fn a_filter_moves_no_column_and_keeps_each_rows_number() {
    let mut rows: Vec<&str> = vec!["haiku", "opus"];
    let many: Vec<String> = (0..10).map(|n| format!("model-{n}")).collect();
    rows.extend(many.iter().map(String::as_str));
    rows.push("gpt-5.6");
    let mut picker = picker(&rows);
    let row_of = |text: &str, label: &str| -> String {
        text.lines()
            .find(|line| line.contains(label))
            .unwrap_or_else(|| panic!("{label} in {text}"))
            .to_owned()
    };
    for _ in 0..12 {
        press(&mut picker, KeyCode::Down);
    }
    let before = row_of(&text(&picker, 100), "gpt-5.6");
    for key in ['g', 'p', 't'] {
        press(&mut picker, KeyCode::Char(key));
    }
    let after = row_of(&text(&picker, 100), "gpt-5.6");
    assert_eq!(
        after.trim_start_matches(['❯', ' ']),
        before.trim_start_matches(['❯', ' '])
    );
    assert!(after.contains("13. "), "{after}");
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
    assert!(first.contains("↓  8. row 8"), "{first}");
    assert!(first.contains("… +4 more"), "{first}");
    assert!(!first.contains("row 9"), "{first}");

    for _ in 0..VISIBLE_ROWS {
        press(&mut picker, KeyCode::Down);
    }
    let scrolled = text(&picker, 60);
    assert!(scrolled.contains("↑  2. row 2"), "{scrolled}");
    assert!(scrolled.contains("❯  9. row 9"), "{scrolled}");
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
    picker.flow.effort = Some(argonauts_core::app::model_picker::EffortLine::new(
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
        .map(|&level| argonauts_core::app::model_picker::EffortStop {
            level: level.into(),
            description: String::new(),
        })
        .collect();
    let slider = argonauts_core::app::model_picker::EffortSlider::new(
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
    use crate::base::kit::State;
    use crate::base::theme::Role;

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
        assert!(crate::base::text::str_width(line) <= 44, "{line:?}");
    }
}

/// The cells of the gauge's line drawn in the warning colour's hatching, at `now`.
fn hatched(picker: &Picker, theme: &Theme, now: Instant) -> usize {
    let ctx = RenderCtx::new(100, theme, Clock::still(now));
    let lines = picker.lines(&ctx);
    let gauge = lines
        .iter()
        .find(|line| plain_text(std::slice::from_ref(*line)).contains("this pick"))
        .expect("the gauge's line");
    plain_text(std::slice::from_ref(gauge)).matches('╱').count()
}

/// Moving to another row springs the gauge to its new ends within a fifth of a second, asking
/// for frames only while it moves, and with reduced motion it jumps.
#[test]
fn the_gauge_springs_to_the_row_the_pointer_moved_to() {
    use super::Gauge;

    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let before = Gauge {
        label: "this pick".to_owned(),
        fits: 0.2,
        needs: 0.2,
        words: "10 of 50 GB".to_owned(),
        ..Gauge::default()
    };
    let moved_at = Instant::now();
    let after = Gauge {
        fits: 0.2,
        needs: 0.6,
        words: "30 of 50 GB".to_owned(),
        ..before.clone()
    }
    .moving_from(&before, moved_at);
    let mut picker = picker(&["one", "two"]);
    picker.gauge = Some(after);

    let at = |ms: u64| moved_at + std::time::Duration::from_millis(ms);
    let (start, middle, end) = (
        hatched(&picker, &theme, at(0)),
        hatched(&picker, &theme, at(40)),
        hatched(&picker, &theme, at(200)),
    );
    assert_eq!(start, 0);
    assert!(0 < middle && middle < end, "{start} {middle} {end}");
    assert!(picker.next_change(&Clock::still(at(40)), &theme).is_some());
    assert_eq!(picker.next_change(&Clock::still(at(200)), &theme), None);

    theme.reduced_motion = true;
    assert_eq!(hatched(&picker, &theme, at(0)), end);
    assert_eq!(picker.next_change(&Clock::still(at(40)), &theme), None);
}
