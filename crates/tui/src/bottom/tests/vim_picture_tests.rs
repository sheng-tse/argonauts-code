//! The line under the input with vim keys on, drawn from the pictures' own example at their
//! width of 118 columns: no label in NORMAL, `-- INSERT --` in INSERT, as Claude Code draws it,
//! and `j` and `k` walking history and the list under the input as ↓ and ↑ do.

use std::time::Instant;

use ratatui::text::Line;

use super::super::*;
use crate::base::draw::render::Clock;
use crate::base::text::plain_text;
use crate::base::theme::{Glyphs, Theme};
use crate::testing::pictures::picture_check::assert_rows_match;

const WIDTH: u16 = 118;
const PROMPT: &str = "fix the money rounding so the tests pass";

/// The picture's rows, each without the blanks that pad it to the picture's width.
fn trimmed(picture: &str) -> String {
    picture
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

/// The input on autopilot with the run of the pictures in the list under it.
fn state(vim: VimMode, draft: &str) -> BottomState {
    let mut state = BottomState::new();
    state.mode.approval = ApprovalMode::Autopilot;
    state.mode.vim = Some(vim);
    let mut row = WorkflowRow::new(
        "run-01",
        "Fix the money rounding",
        WorkflowRowState::Running,
    );
    row.count = Some((2, 5));
    row.elapsed_ms = 100_000;
    row.tokens = 31_000;
    state.workflows.rows = vec![row];
    // The conversation has begun, so an empty input offers no example.
    state.composer.end_example();
    state.composer.set_text(draft);
    state
}

fn drawn(state: &BottomState) -> Vec<Line<'static>> {
    let theme = theme();
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    state.render(&ctx).lines
}

/// From the input's top rule: the rules, the draft, the line under it, and `then` more rows.
fn rows_from_the_rule(state: &BottomState, then: usize) -> Vec<Line<'static>> {
    let lines = drawn(state);
    let rule = lines
        .iter()
        .position(|line| plain_text(std::slice::from_ref(line)).starts_with('─'))
        .expect("the input's top rule");
    lines[rule..(rule + 4 + then).min(lines.len())].to_vec()
}

/// The picture's second part: no label in NORMAL, then `-- INSERT --` with a draft. Its first
/// part draws the old line, and its captions aren't screen.
#[test]
fn vim_label_nothing_in_normal_and_insert_dim_first() {
    let picture = trimmed(include_str!("../../../tests/golden/bottom/vim_label.txt"));
    let html = include_str!("../../../tests/golden/bottom/vim_label.html");
    for (state, rows) in [
        (state(VimMode::Normal, ""), 7..11),
        (state(VimMode::Insert, "fix the te"), 12..16),
    ] {
        assert_rows_match(
            "vim_label",
            &rows_from_the_rule(&state, 0),
            (&picture, html),
            rows,
            &theme(),
            &[],
        );
    }
}

/// `k k` back through what you typed, `j j` forward again, and one more `j` into the list under
/// the input, each under its caption. The picture's list also holds a background shell's row,
/// which the list doesn't, so its rows and the picture's last part, a step onto that row, stay
/// out of the comparison.
#[test]
fn vim_jk_history_then_the_list_and_back() {
    let picture = trimmed(include_str!("../../../tests/golden/bottom/vim_jk.txt"));
    let html = include_str!("../../../tests/golden/bottom/vim_jk.html");
    let normal = |draft: &str, selected: Option<usize>| {
        let mut state = state(VimMode::Normal, draft);
        state.workflows.selected = selected;
        rows_from_the_rule(&state, 2)
    };
    for (lines, rows) in [
        (normal(PROMPT, None), 1..7),
        (normal("", None), 10..16),
        (normal("", Some(0)), 19..25),
    ] {
        assert_rows_match("vim_jk", &lines, (&picture, html), rows, &theme(), &[]);
    }
}
