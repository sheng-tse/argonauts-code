//! The line under the input with vim keys on, drawn from the pictures' own example at their
//! width of 118 columns: no label in NORMAL, `-- INSERT --` in INSERT, as Claude Code draws it,
//! and `j` and `k` walking history and the list under the input as ↓ and ↑ do.

use std::time::Instant;

use ratatui::text::Line;

use super::*;
use crate::bridge::transcript::picture_check::{Unlike, assert_matches};
use crate::render::Clock;
use crate::text::plain_text;
use crate::theme::{Glyphs, Theme};

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
    row.agents_done = 2;
    row.agents_total = 5;
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

/// `⏵⏵ autopilot on` from `col`, in autopilot's own colour, where the pictures paint it amber.
fn autopilot(row: usize, col: usize) -> Unlike {
    Unlike {
        row,
        cols: (col, col + 14),
        why: "autopilot keeps its own colour, where the picture paints it amber",
    }
}

fn caption(row: usize) -> Unlike {
    Unlike {
        row,
        cols: (0, usize::from(WIDTH) - 1),
        why: "the picture's own caption, which the screen doesn't draw",
    }
}

fn rule(row: usize) -> Unlike {
    Unlike {
        row,
        cols: (0, usize::from(WIDTH) - 1),
        why: "the input's rules keep their own grey, a step brighter than the picture's",
    }
}

fn prompt(row: usize) -> Unlike {
    Unlike {
        row,
        cols: (0, 0),
        why: "the prompt is in the text color while the input has the keys",
    }
}

#[test]
fn vim_label_nothing_in_normal_and_insert_dim_first() {
    let today = |row| Unlike {
        row,
        cols: (0, usize::from(WIDTH) - 1),
        why: "the picture's first part is the screen before this change",
    };
    let mut lines = vec![Line::default()];
    lines.extend(std::iter::repeat_n(Line::default(), 4));
    lines.push(Line::default());
    lines.push(Line::default());
    lines.extend(rows_from_the_rule(&state(VimMode::Normal, ""), 0));
    lines.push(Line::default());
    lines.extend(rows_from_the_rule(&state(VimMode::Insert, "fix the te"), 0));
    assert_matches(
        "vim_label",
        &lines,
        &trimmed(include_str!("pictures/vim_label.txt")),
        include_str!("pictures/vim_label.html"),
        &theme(),
        &[
            caption(0),
            today(1),
            today(2),
            today(3),
            today(4),
            caption(6),
            rule(7),
            prompt(8),
            rule(9),
            autopilot(10, 2),
            rule(12),
            prompt(13),
            rule(14),
            autopilot(15, 15),
            // A difference from the picture, which Claude Code makes too: it turns its footer
            // hint off while the vim label shows.
            Unlike {
                row: 15,
                cols: (54, usize::from(WIDTH) - 1),
                why: "while -- INSERT -- shows, the line leaves out ? for shortcuts, as Claude \
                      Code hides its hint while its vim label shows",
            },
        ],
    );
}

#[test]
fn vim_jk_history_then_the_list_and_back() {
    let shell = |row| Unlike {
        row,
        cols: (0, usize::from(WIDTH) - 1),
        why: "a background shell's row, which the list under the input doesn't hold",
    };
    let tokens = |row| Unlike {
        row,
        cols: (66, usize::from(WIDTH) - 1),
        why: "tokens in the one format every screen uses, 31k, where the picture writes 31.0k",
    };
    let normal = |draft: &str, selected: Option<usize>| {
        let mut state = state(VimMode::Normal, draft);
        state.workflows.selected = selected;
        rows_from_the_rule(&state, 2)
    };
    let block = |lines: &mut Vec<Line<'static>>, rows: Vec<Line<'static>>| {
        lines.push(Line::default());
        lines.extend(rows);
        lines.push(Line::default());
        lines.push(Line::default());
    };
    let mut lines = Vec::new();
    block(&mut lines, normal(PROMPT, None));
    block(&mut lines, normal("", None));
    block(&mut lines, normal("", Some(0)));
    block(&mut lines, normal("", None));
    lines.pop();
    let unlike: Vec<Unlike> = [0, 9, 18, 27]
        .into_iter()
        .flat_map(|top| [caption(top), rule(top + 1), prompt(top + 2), rule(top + 3)])
        .chain([shell(7), shell(16), shell(25), shell(34)])
        .chain([tokens(6), tokens(15), tokens(24), tokens(33)])
        .chain([autopilot(4, 2), autopilot(13, 2), autopilot(31, 2)])
        .chain([6, 15, 24, 33].map(|row| Unlike {
            row,
            cols: (28, 35),
            why: "the done part of a run's bar is in the text's own colour, which the picture \
                  names as its foreground",
        }))
        .chain([Unlike {
            row: 24,
            cols: (0, 27),
            why: "the selected row's mark and name keep the list's own colours",
        }])
        .chain([Unlike {
            row: 2,
            cols: (2, usize::from(WIDTH) - 1),
            why: "the draft is in the terminal's own colour, which the picture names as its \
                  foreground",
        }])
        .chain([Unlike {
            row: 22,
            cols: (0, usize::from(WIDTH) - 1),
            why: "the list with the keys puts its own keys on the line, as Claude Code's does",
        }])
        .collect();
    assert_matches(
        "vim_jk",
        &lines,
        &trimmed(include_str!("pictures/vim_jk.txt")),
        include_str!("pictures/vim_jk.html"),
        &theme(),
        &unlike,
    );
}
