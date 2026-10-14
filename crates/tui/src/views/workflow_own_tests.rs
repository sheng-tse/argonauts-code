//! A run that Claude Code's own workflow runs, drawn as one box with the run's name and
//! counts, the phases, and every step with its model and counts.

use std::time::Instant;

use argonauts_core::views::{RowStatus, WorkflowView as WorkflowData};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use super::workflow::WorkflowView;
use super::workflow_tests::agent;
use super::{View, ViewCtx, ViewOutcome};
use crate::state::AppState;
use crate::theme::{Glyphs, Role, Theme};

const PICTURE: &str = include_str!("../../tests/golden/run_after.txt");
const COLOURS: &str = include_str!("../../tests/golden/run_after.html");
const VIEW: &str = include_str!("../../tests/golden/run_after.view.json");
const WIDTH: u16 = 118;

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

/// The picture's run: the build done, one reviewer asked for changes, the sign-off going. It is
/// the view the app builds from that run's events, which the app's own test keeps in step.
fn add_due_dates() -> WorkflowData {
    serde_json::from_str(VIEW).expect("the run's view")
}

fn drawn(view: &mut WorkflowView, theme: &Theme) -> Buffer {
    let state = AppState::default();
    let ctx = ViewCtx {
        theme,
        state: &state,
        now: Instant::now(),
    };
    let rows = view
        .dock_height(ratatui::layout::Size::new(WIDTH, 40), &ctx)
        .expect("the frame says how tall it is");
    let area = Rect::new(0, 0, WIDTH, rows);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    buf
}

fn text(buf: &Buffer) -> Vec<String> {
    let area = buf.area;
    (area.y..area.bottom())
        .map(|y| {
            let mut line = String::new();
            for x in area.x..area.right() {
                line.push_str(buf[(x, y)].symbol());
            }
            line.trim_end().to_owned()
        })
        .collect()
}

fn picture_text() -> Vec<String> {
    PICTURE
        .trim_end()
        .lines()
        .map(|line| line.trim_end().to_owned())
        .collect()
}

#[test]
fn a_run_claude_codes_own_workflow_runs_reads_as_the_picture_draws_it() {
    let theme = theme();
    let mut view = WorkflowView::new(add_due_dates());
    let drawn = text(&drawn(&mut view, &theme));
    assert_eq!(drawn, picture_text(), "\n{}", drawn.join("\n"));
}

/// Each cell's look in the picture: its colour by name, and bold.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Look {
    colour: Option<&'static str>,
    bold: bool,
}

/// The picture's colours, one `Look` per cell of each line, read off its spans.
fn picture_looks() -> Vec<Vec<(char, Look)>> {
    let body = COLOURS
        .split_once("<pre")
        .and_then(|(_, rest)| rest.split_once('>'))
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_once("</pre>"))
        .map(|(body, _)| body)
        .expect("the picture's cells");
    let plain = Look {
        colour: None,
        bold: false,
    };
    let mut lines = vec![Vec::new()];
    let mut rest = body;
    while !rest.is_empty() {
        let (look, text, after) = match rest.strip_prefix("<span style=\"") {
            Some(span) => {
                let (style, after) = span.split_once("\">").expect("a span's style");
                let (text, after) = after.split_once("</span>").expect("a span's end");
                (look_of(style), text, after)
            }
            None => {
                let end = rest.find("<span").unwrap_or(rest.len());
                (plain.clone(), &rest[..end], &rest[end..])
            }
        };
        let text = text
            .replace("&#x27;", "'")
            .replace("&gt;", ">")
            .replace("&lt;", "<")
            .replace("&amp;", "&");
        for c in text.chars() {
            if c == '\n' {
                lines.push(Vec::new());
            } else {
                lines.last_mut().unwrap().push((c, look.clone()));
            }
        }
        rest = after;
    }
    lines
}

/// The picture's colour names, each with the theme role that draws it. Its accent is the
/// periwinkle the theme draws cards and working marks in.
const ROLES: [(&str, Role); 6] = [
    ("acc", Role::Card),
    ("dim", Role::Dim),
    ("faint", Role::Faint),
    ("grn", Role::Success),
    ("red", Role::Error),
    ("amb", Role::Warning),
];

fn look_of(style: &str) -> Look {
    let colour = ROLES
        .into_iter()
        .map(|(name, _)| name)
        .find(|name| style.contains(&format!("color:var(--term-{name})")));
    Look {
        colour,
        bold: style.contains("font-weight:700"),
    }
}

/// The picture's name for a drawn colour; `None` for the terminal's own foreground.
fn colour_name(theme: &Theme, colour: Option<Color>) -> Option<&'static str> {
    let plain = [Role::Text, Role::Bright].map(|role| theme.style(role).fg);
    if colour.is_none() || colour == Some(Color::Reset) || plain.contains(&colour) {
        return None;
    }
    let named = ROLES
        .into_iter()
        .find(|(_, role)| theme.style(*role).fg == colour);
    named.map(|(name, _)| name).or(Some("another colour"))
}

#[test]
fn its_colours_are_the_picture_s() {
    let theme = theme();
    let mut view = WorkflowView::new(add_due_dates());
    let buf = drawn(&mut view, &theme);
    crate::picture_dump::rows("run_after", &text(&buf));
    let picture = picture_looks();
    assert_eq!(usize::from(buf.area.height), picture.len());
    for (y, want) in picture.iter().enumerate() {
        for (x, (want_c, want)) in want.iter().enumerate() {
            let cell = &buf[(x as u16, y as u16)];
            let c = cell.symbol().chars().next().unwrap_or(' ');
            assert_eq!(c, *want_c, "row {y} column {x}");
            if c.is_whitespace() {
                continue;
            }
            let got = Look {
                colour: colour_name(&theme, cell.fg.into()),
                bold: cell.modifier.contains(Modifier::BOLD),
            };
            assert_eq!(&got, want, "row {y} column {x} {c:?}");
        }
    }
}

fn press(view: &mut WorkflowView, code: KeyCode) -> ViewOutcome {
    let theme = theme();
    let state = AppState::default();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
}

#[test]
fn arrows_pick_steps_across_phases_and_name_the_picked_steps_phase() {
    let theme = theme();
    let mut view = WorkflowView::new(add_due_dates());
    press(&mut view, KeyCode::Down);
    press(&mut view, KeyCode::Down);
    let lines = text(&drawn(&mut view, &theme));
    assert!(lines[3].contains("Review · 3 steps"), "{}", lines[3]);
    assert!(lines[5].contains("❯ review:due-dates"), "{}", lines[5]);
    press(&mut view, KeyCode::Down);
    press(&mut view, KeyCode::Down);
    let lines = text(&drawn(&mut view, &theme));
    assert!(lines[6].contains("❯ final:due-dates"), "{}", lines[6]);
}

#[test]
fn enter_opens_the_picked_step_and_the_first_before_any_is_picked() {
    let mut view = WorkflowView::new(add_due_dates());
    match press(&mut view, KeyCode::Enter) {
        ViewOutcome::Request(request) => {
            let words = format!("{request:?}");
            assert!(words.contains("s-001"), "{words}");
        }
        other => panic!("Enter opened nothing: {other:?}"),
    }
    let mut view = WorkflowView::new(add_due_dates());
    press(&mut view, KeyCode::Up);
    match press(&mut view, KeyCode::Enter) {
        ViewOutcome::Request(request) => {
            let words = format!("{request:?}");
            assert!(words.contains("s-003"), "{words}");
        }
        other => panic!("Enter opened nothing: {other:?}"),
    }
}

#[test]
fn x_asks_to_stop_the_whole_workflow_whichever_step_is_picked() {
    let theme = theme();
    let mut view = WorkflowView::new(add_due_dates());
    press(&mut view, KeyCode::Down);
    press(&mut view, KeyCode::Char('x'));
    let lines = text(&drawn(&mut view, &theme));
    let keys = lines.last().unwrap();
    assert!(keys.contains("x again to stop the workflow"), "{keys}");
}

#[test]
fn a_long_run_keeps_the_picked_step_in_the_box() {
    let theme = theme();
    let mut data = add_due_dates();
    for at in 0..30 {
        let label = format!("review:part-{at}");
        let mut more = agent(&format!("s{}", at + 10), &label, RowStatus::Waiting);
        more.tokens = 0;
        data.phases[1]
            .rows
            .push(argonauts_core::views::WorkflowRow::Agent(more));
    }
    let mut view = WorkflowView::new(data);
    press(&mut view, KeyCode::End);
    let lines = text(&drawn(&mut view, &theme));
    assert!(
        lines.iter().any(|line| line.contains("❯ review:part-29")),
        "{}",
        lines.join("\n")
    );
    assert!(lines.last().unwrap().contains("esc back"));
}
