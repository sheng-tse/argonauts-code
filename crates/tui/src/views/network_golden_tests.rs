//! A council's workflow view while the internet is down and ten seconds after it's back, and
//! the two rows the conversation gets, beside `tests/golden/network_after`. Each is drawn from
//! the picture's own numbers at its width and compared cell for cell, colours included.
//!
//! The picture's last part, the working line of the main agent's own turn while Claude Code
//! retries, is checked beside the working line, in `bottom/retry.rs`.
//!
//! The picture is read as the view draws every picture of it: the box's frame bright where
//! the picture draws it faint, and Gemma named `Gemma4 E2B`, as the model's name is written
//! everywhere else, where the picture writes `E2b`. A phase's count also follows the longest
//! counted name (`Propose    4/4  `), as `paused_after` draws it, where this picture puts it
//! at the column's end (`Propose      4/4`).

use std::time::{Duration, Instant};

use argonauts_core::limits::{NETWORK_BACK, NETWORK_DOWN, waiting_for_the_network};
use argonauts_core::names::{SessionKey, WorkflowId};
use argonauts_core::views::{
    AgentRow, PhaseCount, PhaseRow, RowNote, RowStatus, StatusKind, StatusLine, Tone, WorkflowRow,
    WorkflowState, WorkflowView as WorkflowData,
};
use argonauts_core::workflow::StandardPhase;
use argonauts_core::{RunId, Spend};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::workflow::WorkflowView;
use super::workflow_pictures::{Picture, buffer_of, dark, picture};
use super::{View, ViewCtx};
use crate::cell::{HistoryCell, Notice, Tone as NoticeTone};
use crate::render::{Clock, RenderCtx};
use crate::state::AppState;

/// The frame stops six cells short of the screen's right edge, as Claude Code's does.
const SCREEN: u16 = 124;
/// The conversation's rows take the picture's whole width.
const WIDTH: u16 = 118;

/// The frame's rows: the rule, the name, the description, the status line, a blank row, the
/// box with its six phases, and the key line.
const FRAME_ROWS: u16 = 14;

const HAIKU: &str = "claude-code/claude-haiku-5-5";
const LUNA: &str = "codex/gpt-6-luna";
const FLASH: &str = "antigravity-cli/gemini-3.8-flash";
/// On Ollama through OpenCode, so it keeps working offline.
const GEMMA: &str = "opencode/ollama/gemma4:e2b";

/// How long the internet had been gone in the first frame.
const DOWN_FOR: Duration = Duration::from_secs(130);

fn agent(
    session: u32,
    label: &str,
    model: &str,
    status: RowStatus,
    (tokens, seconds): (u64, u64),
    note: Option<RowNote>,
) -> WorkflowRow {
    WorkflowRow::Agent(AgentRow {
        session: SessionKey::numbered(session),
        label: label.to_owned(),
        worker: model.parse().expect("a model"),
        status,
        tokens,
        spent: Spend::default(),
        elapsed_ms: seconds * 1000,
        subagents: 0,
        context: None,
        fallback_for: None,
        answered: false,
        tries: Vec::new(),
        note,
        inner: None,
        left_out: false,
        not_needed: false,
        fallback_why: None,
    })
}

fn phase(name: &str, status: RowStatus, rows: Vec<WorkflowRow>, done: u32) -> PhaseRow {
    let total = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    PhaseRow {
        name: name.to_owned(),
        detail: String::new(),
        standard: StandardPhase::Do,
        status,
        try_number: None,
        count: PhaseCount::of(done, total),
        rows,
        note: None,
    }
}

/// The council 2m 15s into Challenge, its sessions alive: the cloud ones held by the internet
/// for `held` when it's gone, and Gemma on this machine working on.
fn council(said: &str, held: Option<Duration>, seconds: u64, gemma_tokens: u64) -> WorkflowData {
    let waiting = || held.map(|held| RowNote::new(waiting_for_the_network(held), Tone::Plain));
    let done = |session, label: &str, model| {
        agent(session, label, model, RowStatus::Done, (9_000, 40), None)
    };
    let running = RowStatus::Running;
    let challenge_for = seconds - 134;
    WorkflowData {
        run: RunId::new("run-01").expect("a run id"),
        id: WorkflowId::numbered(1),
        name: "Per key rate limiter".to_owned(),
        description: "Give each X-Api-Key its own token bucket with one shared bucket for \
                      requests without a key, and answer 429 with a whole-second Retry-After."
            .to_owned(),
        style: None,
        status_lines: vec![StatusLine::new(StatusKind::Note, said)],
        agents_done: 4,
        agents_total: 8,
        elapsed_ms: seconds * 1000,
        spent: Spend::default(),
        phases: vec![
            phase(
                "Propose",
                RowStatus::Done,
                vec![
                    done(2, "propose:p0", HAIKU),
                    done(3, "propose:p1", LUNA),
                    done(4, "propose:p2", FLASH),
                    done(5, "propose:p3", GEMMA),
                ],
                4,
            ),
            phase(
                "Challenge",
                running,
                vec![
                    agent(
                        6,
                        "challenge:p0",
                        HAIKU,
                        running,
                        (11_700, challenge_for),
                        waiting(),
                    ),
                    agent(
                        7,
                        "challenge:p1",
                        LUNA,
                        running,
                        (0, challenge_for),
                        waiting(),
                    ),
                    agent(
                        8,
                        "challenge:p2",
                        FLASH,
                        running,
                        (6_200, challenge_for),
                        waiting(),
                    ),
                    agent(
                        9,
                        "challenge:p3",
                        GEMMA,
                        running,
                        (gemma_tokens, challenge_for),
                        None,
                    ),
                ],
                0,
            ),
            phase("Decide", RowStatus::Waiting, Vec::new(), 0),
            phase("Build", RowStatus::Waiting, Vec::new(), 0),
            phase("Cross-review", RowStatus::Waiting, Vec::new(), 0),
            phase("Sign-off", RowStatus::Waiting, Vec::new(), 0),
        ],
        notes: vec!["Council".to_owned()],
        required: Vec::new(),
        goals: Vec::new(),
        worker: None,
        state: WorkflowState::Running,
    }
}

/// Your models ranked as the other tabs rank them, so each keeps the colour it has there.
fn state() -> AppState {
    super::workflow_nested_tests::state()
}

/// The view on Challenge, drawn at the screen's width.
fn drawn(data: WorkflowData) -> Buffer {
    let theme = dark();
    let state = state();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let mut view = WorkflowView::new(data);
    let press = |view: &mut WorkflowView, code| {
        view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx);
    };
    for _ in 0..6 {
        press(&mut view, KeyCode::Up);
    }
    press(&mut view, KeyCode::Down);
    let area = Rect::new(0, 0, SCREEN, FRAME_ROWS);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    buf
}

fn the_picture() -> Picture {
    picture!("network_after")
}

#[test]
fn the_view_while_the_network_is_down_reads_as_its_picture() {
    let buf = drawn(council(NETWORK_DOWN, Some(DOWN_FOR), 269, 43_600));
    the_picture().assert_drawn(2..16, &buf, 0);
}

#[test]
fn the_view_once_the_network_is_back_reads_as_its_picture() {
    let buf = drawn(council(NETWORK_BACK, None, 279, 53_500));
    the_picture().assert_drawn(19..33, &buf, 0);
}

/// The run's two notes, as the conversation draws a note of its own: one dim row each.
#[test]
fn the_conversations_rows_read_as_their_picture() {
    let picture = the_picture();
    let theme = dark();
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    for (row, said) in [(36, NETWORK_DOWN), (38, NETWORK_BACK)] {
        let lines = Notice::new(NoticeTone::Dim, said).lines(&ctx);
        picture.assert_drawn(row..row + 1, &buffer_of(&lines, WIDTH), 0);
    }
}
