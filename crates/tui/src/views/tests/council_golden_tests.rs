//! The workflow view of a council that proposed, challenged and decided, beside rows 40 to 78
//! of `tests/golden/council_rows_after`: the view when it's done on Decide, then on Sign-off
//! and on Challenge, and the run's row in the bottom list. Each is drawn from the picture's own
//! numbers at its width and compared cell for cell, colours included.

use std::time::Instant;

use argonauts_core::app::views::{
    AgentRow, CommandRow, PhaseCount, PhaseRow, RowNote, RowStatus, StatusKind, StatusLine, Tone,
    WorkflowRow, WorkflowState, WorkflowView as WorkflowData,
};
use argonauts_core::base::names::{SessionKey, WorkflowId};
use argonauts_core::workflows::workflow::StandardPhase;
use argonauts_core::{RunId, Spend};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::super::workflow::WorkflowView;
use crate::frame::state::AppState;
use crate::frame::view::{View, ViewCtx};
use crate::testing::pictures::workflow_pictures::{Picture, dark, picture};

/// The frame stops six cells short of the screen's right edge, as Claude Code's does.
const SCREEN: u16 = 124;

/// The frame's rows above its box: the rule, the name, the description, the status line, and
/// a blank row.
const ABOVE_BOX: u16 = 5;

const HAIKU: &str = "claude-code/claude-haiku-5-5";
const LUNA: &str = "codex/gpt-6-luna";
const FLASH: &str = "antigravity-cli/gemini-3.8-flash";
const TASK: &str = "per-key-rate-limit";

/// The run's own name, which the view and its row in the bottom list show.
const RATE: &str = "Per key rate limiter";

fn agent(
    session: u32,
    label: &str,
    model: &str,
    (tokens, seconds): (u64, u64),
    note: Option<(&str, Tone)>,
) -> WorkflowRow {
    WorkflowRow::Agent(AgentRow {
        session: SessionKey::numbered(session),
        label: label.to_owned(),
        worker: model.parse().expect("a model"),
        status: RowStatus::Done,
        tokens,
        spent: Spend::default(),
        elapsed_ms: seconds * 1000,
        subagents: 0,
        context: None,
        fallback_for: None,
        answered: false,
        tries: Vec::new(),
        note: note.map(|(text, tone)| RowNote::new(text, tone)),
        inner: None,
        left_out: false,
        not_needed: false,
        fallback_why: None,
    })
}

fn step(label: &str, command: &str, result: &str, seconds: u64) -> WorkflowRow {
    WorkflowRow::Command(CommandRow {
        label: label.to_owned(),
        command: command.to_owned(),
        result: Some(result.to_owned()),
        tone: Tone::Plain,
        status: RowStatus::Done,
        elapsed_ms: seconds * 1000,
    })
}

fn phase(name: &str, rows: Vec<WorkflowRow>, agents: u32) -> PhaseRow {
    PhaseRow {
        name: name.to_owned(),
        detail: String::new(),
        standard: StandardPhase::Do,
        status: RowStatus::Done,
        try_number: None,
        count: PhaseCount::of(agents, agents),
        rows,
        note: None,
    }
}

/// The council once it merged: three proposals, three challenges, Decide's choice, the build,
/// its cross-review and sign-off, and the merge.
fn council() -> WorkflowData {
    let good = Tone::Good;
    let plain = Tone::Plain;
    let proposal = Some(("proposal ready", good));
    let mut data = WorkflowData {
        run: RunId::new("run-01").expect("a run id"),
        id: WorkflowId::numbered(1),
        name: RATE.to_owned(),
        description: "Give each X-Api-Key its own token bucket in the council rate limiter, cap \
                      refills at capacity, and answer 429 with a whole-second Retry-After."
            .to_owned(),
        style: None,
        status_lines: vec![
            StatusLine::new(
                StatusKind::Ended,
                "Merged onto main · 7c41d09 · 2 files +17 −4",
            )
            .toned(good),
        ],
        agents_done: 11,
        agents_total: 11,
        elapsed_ms: 155_000,
        spent: Spend::default(),
        phases: vec![
            phase(
                "Propose",
                vec![
                    agent(2, "propose:p1", HAIKU, (14_200, 41), proposal),
                    agent(3, "propose:p2", LUNA, (11_000, 52), proposal),
                    agent(4, "propose:p3", FLASH, (9_400, 38), proposal),
                ],
                3,
            ),
            phase(
                "Challenge",
                vec![
                    agent(
                        5,
                        "challenge:p1",
                        HAIKU,
                        (8_100, 24),
                        Some(("against p2 and p3 · 3 points", plain)),
                    ),
                    agent(
                        6,
                        "challenge:p2",
                        LUNA,
                        (7_600, 29),
                        Some(("against p1 and p3 · 2 points", plain)),
                    ),
                    agent(
                        7,
                        "challenge:p3",
                        FLASH,
                        (6_900, 21),
                        Some(("against p1 and p2 · 2 points", plain)),
                    ),
                ],
                3,
            ),
            phase(
                "Decide",
                vec![agent(
                    8,
                    "decide:plan",
                    HAIKU,
                    (43_800, 11),
                    Some(("chose p2", Tone::Text)),
                )],
                1,
            ),
            phase(
                "Build",
                vec![agent(
                    9,
                    &format!("build:{TASK}"),
                    HAIKU,
                    (118_800, 18),
                    Some(("passed", good)),
                )],
                1,
            ),
            phase(
                "Cross-review",
                vec![agent(
                    10,
                    &format!("review:{TASK}"),
                    LUNA,
                    (17_200, 4),
                    Some(("approved", good)),
                )],
                1,
            ),
            phase(
                "Sign-off",
                vec![agent(
                    11,
                    &format!("signoff:{TASK}"),
                    HAIKU,
                    (41_600, 10),
                    Some(("read the task's change", good)),
                )],
                1,
            ),
            phase(
                "Merge",
                vec![
                    step("final check", "python3 -m unittest -q", "5 passed", 1),
                    agent(
                        12,
                        "commit message",
                        HAIKU,
                        (6_200, 9),
                        Some(("given the diff, no tools", plain)),
                    ),
                    step("merge onto main", "git", "1 task · no conflicts", 1),
                ],
                1,
            ),
        ],
        notes: vec!["Council".to_owned()],
        required: Vec::new(),
        goals: Vec::new(),
        worker: None,
        state: WorkflowState::Done,
    };
    data.spent = Spend::default();
    data
}

/// Your models ranked as the other tabs rank them, so each keeps the colour it has there:
/// Haiku 5.5 is Claude Code's third, GPT 6 Luna Codex's second, and Gemini 3.8 Flash
/// agy's third.
fn ctx_parts() -> (crate::base::theme::Theme, AppState) {
    (dark(), super::super::workflow::tests::nested::state())
}

fn drawn(view: &mut WorkflowView, rows: u16) -> Buffer {
    let (theme, state) = ctx_parts();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let area = Rect::new(0, 0, SCREEN, rows);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    buf
}

/// Moves the view's cursor in the Phases list to `phase`, counted from 0, from wherever it
/// opened.
fn select(view: &mut WorkflowView, phase: usize) {
    let (theme, state) = ctx_parts();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let press = |view: &mut WorkflowView, code| {
        view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx);
    };
    for _ in 0..council().phases.len() {
        press(view, KeyCode::Up);
    }
    for _ in 0..phase {
        press(view, KeyCode::Down);
    }
}

/// The picture, with the bottom list's name column 24 cells on every row, as the done run's
/// picture draws it, where this one draws 22.
fn the_picture() -> Picture {
    let mut picture = picture!("council_rows_after");
    // Every other picture of the bottom list gives a run's name 22 cells and two more before
    // the bar, so the bar starts in one column whatever the name; this one gives 22 in all.
    picture.reword(78, "limiter  ", "limiter    ");
    picture
}

#[test]
fn the_done_view_on_decide_reads_as_its_picture() {
    // The run ended, so nothing is left to stop or pause.
    let picture = the_picture();
    let mut view = WorkflowView::new(council());
    select(&mut view, 2);
    picture.assert_drawn(40..55, &drawn(&mut view, 15), 0);
}

#[test]
fn the_same_view_on_sign_off_and_on_challenge_reads_as_its_picture() {
    let picture = the_picture();
    for (phase, rows) in [(5, 58..67), (1, 68..77)] {
        let mut view = WorkflowView::new(council());
        select(&mut view, phase);
        // The box and the key line under it, so the list has all its rows.
        let buf = drawn(&mut view, ABOVE_BOX + 10);
        picture.assert_drawn(rows, &buf, ABOVE_BOX);
    }
}
