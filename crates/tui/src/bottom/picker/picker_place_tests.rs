//! An open picker takes the input's place, as a dialog does in Claude Code 2.1.303: its rule in
//! the question colour is the only edge, with no input row and no cursor. Closing it brings the
//! labelled input rule back, with the draft as it was.

use std::time::Instant;

use super::super::*;
use crate::base::draw::render::Clock;
use crate::base::text::plain_text;
use crate::base::theme::{Glyphs, Theme};

const WIDTH: u16 = 118;
const DRAFT: &str = "half a sentence typed before /model";

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn text(view: &BottomView) -> Vec<String> {
    view.lines
        .iter()
        .map(|line| plain_text(std::slice::from_ref(line)))
        .collect()
}

fn with_draft() -> BottomState {
    let mut state = BottomState::new();
    state.composer.label = Some("Opus 5.5".to_owned());
    state.composer.end_example();
    state.composer.set_text(DRAFT);
    state
}

fn picker() -> Picker {
    let mut picker = Picker::new("Main agent", "Who answers in conversation");
    picker.rows = vec![
        PickerRow::new("Claude Code", "default, Opus 5.5"),
        PickerRow::new("Codex", "7 models"),
    ];
    picker
}

#[test]
fn an_open_picker_draws_its_own_rule_and_no_input() {
    let theme = theme();
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    let mut state = with_draft();
    state.picker = Some(picker());
    let view = state.render(&ctx);
    let rows = text(&view);

    let rules: Vec<&String> = rows.iter().filter(|row| row.starts_with('─')).collect();
    assert_eq!(
        rules.len(),
        1,
        "one rule, the picker's:\n{}",
        rows.join("\n")
    );
    assert!(
        !rules[0].contains("Opus 5.5"),
        "the input's labelled rule is gone"
    );
    let rule = view
        .lines
        .iter()
        .find(|line| plain_text(std::slice::from_ref(line)).starts_with('─'))
        .expect("the picker's rule");
    assert_eq!(
        rule.spans[0].style.fg,
        theme.style(Role::Question).fg,
        "the edge is in the question colour"
    );
    assert!(
        !rows.iter().any(|row| row.contains(DRAFT)),
        "no input row while the picker is open"
    );
    assert!(
        !rows.iter().any(|row| row.starts_with(theme.glyphs.prompt)),
        "no › row"
    );
    assert_eq!(view.cursor, None, "no cursor");
    assert_eq!(
        rows[0],
        rules[0].as_str(),
        "the picker's rule is the top edge"
    );
}

#[test]
fn closing_the_picker_brings_back_the_labelled_input_and_its_draft() {
    let theme = theme();
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    let mut state = with_draft();
    state.picker = Some(picker());
    let _ = state.render(&ctx);
    state.picker = None;
    let view = state.render(&ctx);
    let rows = text(&view);
    let rule = rows
        .iter()
        .find(|row| row.starts_with('─'))
        .expect("the input's rule");
    assert!(rule.contains("Opus 5.5"), "{rule}");
    assert!(
        rows.iter().any(|row| row.contains(DRAFT)),
        "{}",
        rows.join("\n")
    );
    assert!(view.cursor.is_some());
}

#[test]
fn the_prompt_search_still_stands_in_for_the_picker() {
    let mut state = with_draft();
    state.picker = Some(picker());
    assert!(state.picker_in_place());
    state.history_search = Some(HistorySearch::new(Vec::new(), Vec::new(), 0));
    assert!(!state.picker_in_place(), "the prompt search draws over it");
}
