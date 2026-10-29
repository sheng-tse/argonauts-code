//! The workflow view and an open agent's view as their pictures draw them, each from the
//! picture's own numbers at its width and compared cell for cell, colours included: a council
//! whose OpenCode challenger gave no answer, the same council paused, a local reviewer and the
//! fallback for a change too big for it, and the view's keys while an agent runs, once it's
//! done and once it failed.
//!
//! The pictures draw a box's frame in the faint colour, as the older drawings did; the frame is
//! bright, as every picture since the workflow view's frame was settled draws it, and
//! each test puts the pictures' frames in that colour first.

use std::time::Instant;

use argonauts_core::names::{ModelRef, SessionKey, WorkflowId};
use argonauts_core::views::{
    ActivityLine, AgentRow, AgentView as AgentData, PhaseCount, PhaseRow, ReviewGiven, RowNote,
    RowStatus, StatusKind, StatusLine, Tone, WorkflowRow, WorkflowState,
    WorkflowView as WorkflowData,
};
use argonauts_core::workflow::StandardPhase;
use argonauts_core::{RunId, Spend};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::workflow::WorkflowView;
use super::workflow_pictures::buffer_of;
use super::workflow_pictures::{Picture, dark, picture};
use super::{View, ViewCtx};
use crate::bottom::{
    AgentPanel, WorkflowPanel, WorkflowRow as PanelRow, WorkflowRowState, panel_lines,
};
use crate::render::{Clock, RenderCtx};
use crate::state::AppState;
use crate::theme::Theme;

/// The frame stops six cells short of the screen's right edge, as Claude Code's does.
const SCREEN: u16 = 124;

/// The frame's rows above its box: the rule, the name, the description, the status line, and
/// a blank row.
const ABOVE_BOX: u16 = 5;

const HAIKU: &str = "claude-code/claude-haiku-5-5";
const LUNA: &str = "codex/gpt-6-luna";
const FLASH: &str = "antigravity-cli/gemini-3.8-flash";

/// OpenCode's challenger, on the local Ollama.
fn gemma() -> ModelRef {
    ModelRef::new("opencode", Some("ollama/gemma4:e2b".to_owned()))
}

fn run() -> RunId {
    RunId::new("run-01").expect("a run id")
}

fn row(session: u32, label: &str, worker: &ModelRef, status: RowStatus) -> AgentRow {
    AgentRow {
        session: SessionKey::numbered(session),
        label: label.to_owned(),
        worker: worker.clone(),
        status,
        tokens: 0,
        spent: Spend::default(),
        elapsed_ms: 0,
        subagents: 0,
        context: None,
        fallback_for: None,
        answered: false,
        tries: Vec::new(),
        note: None,
        inner: None,
        left_out: false,
        not_needed: false,
        fallback_why: None,
    }
}

/// An agent's row from the picture's numbers: tokens, seconds and what it says after them.
fn agent(
    session: u32,
    label: &str,
    worker: &ModelRef,
    (status, tokens, seconds): (RowStatus, u64, u64),
    note: Option<(&str, Tone)>,
) -> WorkflowRow {
    let mut agent = row(session, label, worker, status);
    agent.tokens = tokens;
    agent.elapsed_ms = seconds * 1000;
    agent.note = note.map(|(text, tone)| RowNote::new(text, tone));
    WorkflowRow::Agent(agent)
}

fn model(id: &str) -> ModelRef {
    id.parse().expect("a model")
}

fn phase(
    name: &str,
    status: RowStatus,
    count: Option<(u32, u32)>,
    rows: Vec<WorkflowRow>,
) -> PhaseRow {
    PhaseRow {
        name: name.to_owned(),
        detail: String::new(),
        standard: StandardPhase::Do,
        status,
        try_number: None,
        count: count.and_then(|(done, all)| PhaseCount::of(done, all)),
        rows,
        note: None,
    }
}

/// Your models ranked as the other tabs rank them, so each keeps the colour it has there.
fn state() -> AppState {
    super::workflow_nested_tests::state()
}

fn drawn(view: &mut WorkflowView, rows: u16, state: &AppState) -> Buffer {
    let theme = dark();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    let area = Rect::new(0, 0, SCREEN, rows);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    buf
}

fn press(view: &mut WorkflowView, code: KeyCode, state: &AppState) {
    let theme: Theme = dark();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx);
}

/// A challenger's prompt as the council gives it: its first line, then the request, the other
/// plans and the run's status, `lines` in all.
fn challenge_prompt(lines: usize) -> String {
    let mut prompt =
        vec!["Argue against the other proposed plans below. Their authors are hidden.".to_owned()];
    for part in ["The request", "The other plans", "The run's status"] {
        prompt.push(String::new());
        prompt.push(format!("## {part}"));
    }
    let mut line = 0;
    while prompt.len() < lines {
        line += 1;
        prompt.insert(prompt.len() - 4, format!("Its line {line}."));
    }
    prompt.join("\n")
}

fn call(tool: &str, summary: &str) -> ActivityLine {
    ActivityLine {
        tool: tool.to_owned(),
        summary: summary.to_owned(),
        ok: Some(true),
        note: None,
    }
}

/// The council at Challenge with its four challengers: three done, and challenge:p3 as `p3`.
fn challenging(p3: AgentRow) -> WorkflowData {
    let proposal = Some(("proposal ready", Tone::Good));
    let done = |seconds| (RowStatus::Done, 0, seconds);
    let challengers = vec![
        agent(
            5,
            "challenge:p0",
            &model(HAIKU),
            (RowStatus::Done, 54_700, 16),
            None,
        ),
        agent(
            6,
            "challenge:p1",
            &model(LUNA),
            (RowStatus::Done, 17_400, 7),
            None,
        ),
        agent(
            7,
            "challenge:p2",
            &model(FLASH),
            (RowStatus::Done, 44_100, 22),
            None,
        ),
        WorkflowRow::Agent(p3),
    ];
    WorkflowData {
        run: run(),
        id: WorkflowId::numbered(1),
        name: "Per key rate limiter".to_owned(),
        description: "Give each X-Api-Key its own token bucket with one shared bucket for requests"
            .to_owned(),
        style: None,
        status_lines: Vec::new(),
        agents_done: 7,
        agents_total: 8,
        elapsed_ms: 204_000,
        spent: Spend::default(),
        phases: vec![
            phase(
                "Propose",
                RowStatus::Done,
                Some((4, 4)),
                vec![
                    agent(1, "propose:p0", &model(HAIKU), done(22), proposal),
                    agent(2, "propose:p1", &model(LUNA), done(6), proposal),
                    agent(3, "propose:p2", &model(FLASH), done(100), proposal),
                    agent(4, "propose:p3", &gemma(), done(26), proposal),
                ],
            ),
            phase("Challenge", RowStatus::Running, Some((3, 4)), challengers),
        ],
        notes: vec!["Council".to_owned()],
        required: Vec::new(),
        goals: Vec::new(),
        worker: None,
        state: WorkflowState::Running,
    }
}

fn opened(row: AgentRow, prompt: usize, activity: Vec<ActivityLine>, calls: u32) -> AgentData {
    AgentData {
        run: run(),
        row,
        prompt: challenge_prompt(prompt),
        activity,
        tool_calls: calls,
        diff: None,
        subagents: None,
        outcome: None,
        goal: None,
        plan: None,
        facts: ["challenger", "read-only"].map(str::to_owned).to_vec(),
        to_fix: None,
        asked_for: None,
        earlier: None,
        review: None,
        moved: None,
        given: None,
        prompt_about: None,
        started_from: None,
    }
}

/// Opens `opened`, the agent `at` of the Challenge phase, in the view of `data`.
fn open_agent(data: WorkflowData, at: usize, opened: AgentData, state: &AppState) -> WorkflowView {
    let mut view = WorkflowView::new(data);
    for _ in 0..2 {
        press(&mut view, KeyCode::Up, state);
    }
    press(&mut view, KeyCode::Down, state);
    press(&mut view, KeyCode::Enter, state);
    for _ in 0..at {
        press(&mut view, KeyCode::Down, state);
    }
    press(&mut view, KeyCode::Enter, state);
    assert!(view.accept(&argonauts_core::app::ViewData::Agent {
        view: Box::new(opened),
    }));
    view
}

/// A picture as every screen draws it: the frame bright; a failed agent's `✘`, Claude
/// Code's own cross, where the pictures draw `✗`; and OpenCode's Gemma named `Gemma4 E2B`, as
/// the model's name is written everywhere else, where the pictures write `E2b`.
fn as_drawn(picture: &mut Picture, gemma: &[usize]) {
    picture.bright_frames();
    picture.respell('✗', '✘');
    for &row in gemma {
        picture.reword(row, "Gemma4 E2b", "Gemma4 E2B");
    }
}

fn the_agent_views() -> Picture {
    let mut picture = picture!("agent_keys_after");
    as_drawn(&mut picture, &[]);
    for row in [5, 45] {
        picture.reword(row, "Gemma4 E2b", "Gemma4 E2B");
    }
    picture
}

/// A view's frame: the rule, the name and the description above its box, then the box and
/// its keys, `rows` in all from the box's top.
fn agent_frame(rows: u16) -> u16 {
    AGENT_ABOVE + rows
}

/// The rows above an open agent's box: the rule, the name, the description and a blank row.
const AGENT_ABOVE: u16 = 4;

/// The view while challenge:p3 runs, 2m 10s in: stop and restart are among its keys.
#[test]
fn a_running_agents_view_reads_as_its_picture() {
    let picture = the_agent_views();
    let state = state();
    let mut p3 = row(8, "challenge:p3", &gemma(), RowStatus::Running);
    p3.tokens = 43_600;
    p3.elapsed_ms = 130_000;
    let activity = vec![
        call("read", "ratelimit/bucket.py"),
        call("read", "tests/test_bucket.py"),
        call("edit", "ratelimit/bucket.py"),
        call("read", "ratelimit/middleware.py"),
        call("edit", "ratelimit/bucket.py"),
    ];
    let opened = opened(p3.clone(), 107, activity, 5);
    let mut view = open_agent(challenging(p3), 3, opened, &state);
    let buf = drawn(&mut view, agent_frame(17), &state);
    picture.assert_drawn(4..21, &buf, AGENT_ABOVE);
}

/// challenge:p3 at its end: it never gave the answer its schema asks for, so it failed, in red.
fn failed_p3() -> AgentRow {
    let mut p3 = row(8, "challenge:p3", &gemma(), RowStatus::Failed);
    p3.tokens = 85_600;
    p3.elapsed_ms = 169_000;
    p3.note = Some(RowNote::new(
        "ended without an answer: it tried to edit files in a read-only session",
        Tone::Bad,
    ));
    p3
}

/// The view of challenge:p1 once it's done: no stop or restart, and what it argued, its
/// points, under what its row says of them.
#[test]
fn a_done_agents_view_reads_as_its_picture() {
    let picture = the_agent_views();
    let state = state();
    let mut p1 = row(6, "challenge:p1", &model(LUNA), RowStatus::Done);
    p1.tokens = 17_400;
    p1.elapsed_ms = 7_000;
    let answered = ActivityLine {
        tool: "the answer below".to_owned(),
        summary: String::new(),
        ok: Some(true),
        note: Some("no command run: the plans came with the prompt".to_owned()),
    };
    let mut opened = opened(p1, 104, vec![answered], 1);
    opened.review = Some(ReviewGiven {
        verdict: "against p0 · 3 points".to_owned(),
        tone: Tone::Text,
        findings: Vec::new(),
    });
    opened.outcome = Some(
        [
            "Assuming header names are case-sensitive can merge requests incorrectly when \
             clients send X-API-Key and x-api-key.",
            "Calling `retry_after()` separately after a rejected `allow()` relies on bucket \
             state staying put between the two calls.",
            "The proposed `math.ceil((1 - tokens) / rate)` needs a defined behavior for zero or \
             negative rates.",
        ]
        .join("\n"),
    );
    let mut view = open_agent(challenging(failed_p3()), 1, opened, &state);
    let buf = drawn(&mut view, agent_frame(17), &state);
    picture.assert_drawn(24..41, &buf, AGENT_ABOVE);
}

/// The view of challenge:p3 once it failed: Failed and why it failed in red, and no stop or
/// restart.
#[test]
fn a_failed_agents_view_reads_as_its_picture() {
    let picture = the_agent_views();
    let state = state();
    let activity = vec![
        call("read", "ratelimit/bucket.py"),
        call("read", "tests/test_bucket.py"),
        call("edit", "ratelimit/bucket.py"),
        call("read", "ratelimit/middleware.py"),
        call("edit", "ratelimit/bucket.py"),
        call("edit", "ratelimit/bucket.py"),
        call("edit", "ratelimit/bucket.py"),
        call("read", "ratelimit/bucket.py"),
    ];
    let opened = opened(failed_p3(), 107, activity, 8);
    let mut view = open_agent(challenging(failed_p3()), 3, opened, &state);
    let buf = drawn(&mut view, agent_frame(17), &state);
    picture.assert_drawn(44..61, &buf, AGENT_ABOVE);
}

const RATE: &str = "Per key rate limiter";

/// What the council's script says it does, longer than the header has room for.
const DOES: &str = "Give each X-Api-Key its own token bucket with one shared bucket for requests \
                    without a key, and answer 429 with a whole-second Retry-After.";

/// The council's six phases, from the picture's numbers, with Challenge as `challenge`.
fn council_phases(challenge: PhaseRow) -> Vec<PhaseRow> {
    let proposal = Some(("proposal ready", Tone::Good));
    let done = |tokens, seconds| (RowStatus::Done, tokens, seconds);
    let one = |name: &str, row: WorkflowRow| phase(name, RowStatus::Done, Some((1, 1)), vec![row]);
    vec![
        phase(
            "Propose",
            RowStatus::Done,
            Some((4, 4)),
            vec![
                agent(1, "propose:p0", &model(HAIKU), done(58_000, 22), proposal),
                agent(2, "propose:p1", &model(LUNA), done(16_100, 6), proposal),
                agent(3, "propose:p2", &model(FLASH), done(102_100, 100), proposal),
                agent(4, "propose:p3", &gemma(), done(7_100, 26), proposal),
            ],
        ),
        challenge,
        one(
            "Decide",
            agent(
                9,
                "decide:rate-limit-per-key",
                &model(HAIKU),
                done(31_000, 19),
                Some(("chose p0", Tone::Text)),
            ),
        ),
        one(
            "Build",
            agent(
                10,
                "build:rate-limit-per-key",
                &model(HAIKU),
                done(160_000, 120),
                None,
            ),
        ),
        one(
            "Cross-review",
            agent(
                11,
                "review:rate-limit-per-key",
                &model(LUNA),
                done(21_000, 9),
                Some(("approved", Tone::Good)),
            ),
        ),
        one(
            "Sign-off",
            agent(
                12,
                "sign-off:rate-limit-per-key",
                &model(LUNA),
                done(18_100, 7),
                Some(("read the task's change", Tone::Good)),
            ),
        ),
    ]
}

/// The four challengers, three done and challenge:p3 as `p3` says.
fn challengers(p3: WorkflowRow) -> Vec<WorkflowRow> {
    let done = |tokens, seconds| (RowStatus::Done, tokens, seconds);
    vec![
        agent(
            5,
            "challenge:p0",
            &model(HAIKU),
            done(54_700, 16),
            Some(("against p1 and p2 · 6 points", Tone::Plain)),
        ),
        agent(
            6,
            "challenge:p1",
            &model(LUNA),
            done(17_400, 7),
            Some(("against p0 · 3 points", Tone::Plain)),
        ),
        agent(
            7,
            "challenge:p2",
            &model(FLASH),
            done(44_100, 22),
            Some(("against p0 and p1 · 5 points", Tone::Plain)),
        ),
        p3,
    ]
}

/// The council once it merged, where OpenCode's challenger ended without its answer.
fn council_done() -> WorkflowData {
    let p3 = WorkflowRow::Agent(failed_p3());
    let challenge = phase(
        "Challenge",
        RowStatus::Failed,
        Some((3, 4)),
        challengers(p3),
    );
    WorkflowData {
        run: run(),
        id: WorkflowId::numbered(1),
        name: RATE.to_owned(),
        description: DOES.to_owned(),
        style: None,
        status_lines: vec![
            StatusLine::new(
                StatusKind::Ended,
                "Merged onto main · 7c41d09 · 2 files +30 −4",
            )
            .toned(Tone::Good),
        ],
        agents_done: 11,
        agents_total: 12,
        elapsed_ms: 360_000,
        spent: Spend::default(),
        phases: council_phases(challenge),
        notes: vec!["Council".to_owned()],
        required: Vec::new(),
        goals: Vec::new(),
        worker: None,
        state: WorkflowState::Done,
    }
}

/// Moves the cursor in the Phases list to `phase`, counted from 0.
fn select(view: &mut WorkflowView, phase: usize, state: &AppState) {
    for _ in 0..8 {
        press(view, KeyCode::Up, state);
    }
    for _ in 0..phase {
        press(view, KeyCode::Down, state);
    }
}

fn the_council() -> Picture {
    let mut picture = picture!("council_no_answer");
    as_drawn(&mut picture, &[11, 23]);
    picture
}

/// The council's view once it's done, on Challenge: challenge:p3 failed, so its row is ✘ in red
/// with why, and its phase reads ✘ 3/4.
#[test]
fn a_council_whose_challenger_gave_no_answer_reads_as_its_picture() {
    let picture = the_council();
    let state = state();
    let mut view = WorkflowView::new(council_done());
    select(&mut view, 1, &state);
    let buf = drawn(&mut view, ABOVE_BOX + 9, &state);
    picture.assert_drawn(2..16, &buf, 0);
}

/// The same view on Propose, where propose:p3's fenced plan counts, and on Sign-off.
#[test]
fn the_councils_view_on_propose_and_on_sign_off_reads_as_its_picture() {
    let picture = the_council();
    let state = state();
    for (phase, rows) in [(0, 19..27), (5, 28..36)] {
        let mut view = WorkflowView::new(council_done());
        select(&mut view, phase, &state);
        let buf = drawn(&mut view, ABOVE_BOX + 9, &state);
        picture.assert_drawn(rows, &buf, ABOVE_BOX);
    }
}

/// The run's row in the bottom list, its tokens with one decimal as its view keeps them.
#[test]
fn the_councils_row_in_the_bottom_list_reads_as_its_picture() {
    let picture = the_council();
    let theme = dark();
    let ctx = RenderCtx::new(SCREEN, &theme, Clock::still(Instant::now()));
    let mut row = PanelRow::new("run-01", RATE, WorkflowRowState::Done);
    row.agents_done = 11;
    row.agents_total = 12;
    row.elapsed_ms = 360_000;
    row.tokens = 538_700;
    row.said = Some("merged onto main".to_owned());
    let panel = WorkflowPanel {
        rows: vec![row],
        max_rows: 5,
        ..WorkflowPanel::default()
    };
    let lines = panel_lines(&panel, &AgentPanel::default(), &ctx);
    let mut picture = picture;
    // The bottom list gives every run's name one column, 24 cells, where this picture gives 22.
    picture.widen(37, "limiter  ", 2);
    picture.assert_drawn(37..38, &buffer_of(&lines, SCREEN), 0);
}

/// The council paused at 3m 24s, while OpenCode's challenger worked: its row is paused, the
/// phases after Challenge haven't started, and the header says so.
fn council_paused() -> WorkflowData {
    let mut p3 = row(8, "challenge:p3", &gemma(), RowStatus::Paused);
    p3.tokens = 8_100;
    p3.elapsed_ms = 70_000;
    p3.note = Some(RowNote::new("paused", Tone::Text));
    let challenge = phase(
        "Challenge",
        RowStatus::Paused,
        Some((3, 4)),
        challengers(WorkflowRow::Agent(p3)),
    );
    let mut phases = council_phases(challenge);
    for phase in &mut phases[2..] {
        phase.status = RowStatus::Waiting;
        phase.count = None;
        phase.rows.clear();
    }
    WorkflowData {
        run: run(),
        id: WorkflowId::numbered(1),
        name: RATE.to_owned(),
        description: DOES.to_owned(),
        style: None,
        status_lines: Vec::new(),
        agents_done: 7,
        agents_total: 8,
        elapsed_ms: 204_000,
        spent: Spend::default(),
        phases,
        notes: vec!["Council".to_owned()],
        required: Vec::new(),
        goals: Vec::new(),
        worker: None,
        state: WorkflowState::Paused,
    }
}

/// A paused run: paused in the header, `p resume` in the keys, and paused on the agent that
/// was at work, with the kit's paused glyph.
#[test]
fn a_paused_council_reads_as_its_picture() {
    let mut picture = picture!("paused_after");
    as_drawn(&mut picture, &[10]);
    let mut state = state();
    state.attached = Some(run());
    let mut held = crate::state::RunState::new(run());
    held.waiting = Some(argonauts_core::events::run::WaitReason::Paused);
    state.runs.insert(run(), held);
    let mut view = WorkflowView::new(council_paused());
    select(&mut view, 1, &state);
    let buf = drawn(&mut view, 13, &state);
    picture.assert_drawn(2..15, &buf, 0);
}

/// Its row in the bottom list, under the cursor: paused by you, and how it goes on.
#[test]
fn a_paused_councils_row_in_the_bottom_list_reads_as_its_picture() {
    let mut picture = picture!("paused_after");
    // The bottom list gives every run's name one column, 24 cells, where this picture gives 22.
    picture.widen(17, "limiter  ", 2);
    let theme = dark();
    let ctx = RenderCtx::new(SCREEN, &theme, Clock::still(Instant::now()));
    let mut row = PanelRow::new("run-01", RATE, WorkflowRowState::Paused);
    row.agents_done = 7;
    row.agents_total = 8;
    row.elapsed_ms = 204_000;
    row.tokens = 328_100;
    row.said = Some("paused by you".to_owned());
    row.carry_on = Some("/resume carries it on".to_owned());
    let panel = WorkflowPanel {
        rows: vec![row],
        max_rows: 5,
        selected: Some(0),
        ..WorkflowPanel::default()
    };
    let lines = panel_lines(&panel, &AgentPanel::default(), &ctx);
    picture.assert_drawn(17..18, &buffer_of(&lines, SCREEN), 0);
}

/// The spec-first run's Review, done: Qwen reviewed the small change with it in its prompt,
/// and GPT 6 Luna took the one too big for Qwen's window.
fn reviewed_locally() -> WorkflowData {
    let qwen = ModelRef::new("ollama", Some("qwen3-coder:30b".to_owned()));
    let approved = Some(("approved", Tone::Good));
    let done = |tokens, seconds| (RowStatus::Done, tokens, seconds);
    let mut fallback = row(4, "review:per-api-key", &model(LUNA), RowStatus::Done);
    fallback.tokens = 71_300;
    fallback.elapsed_ms = 9_000;
    fallback.note = Some(RowNote::new("approved", Tone::Good));
    fallback.fallback_for = Some(qwen.clone());
    fallback.fallback_why = Some("whose 64k window can't hold this change".to_owned());
    WorkflowData {
        run: run(),
        id: WorkflowId::numbered(1),
        name: "Per key limits".to_owned(),
        description: "Spec-first".to_owned(),
        style: None,
        status_lines: Vec::new(),
        agents_done: 5,
        agents_total: 5,
        elapsed_ms: 300_000,
        spent: Spend::default(),
        phases: vec![
            phase(
                "Plan",
                RowStatus::Done,
                Some((1, 1)),
                vec![agent(1, "plan:spec", &model(HAIKU), done(12_000, 30), None)],
            ),
            phase(
                "Build",
                RowStatus::Done,
                Some((2, 2)),
                vec![
                    agent(
                        2,
                        "build:cap-bucket-refills",
                        &model(HAIKU),
                        done(30_000, 60),
                        None,
                    ),
                    agent(
                        3,
                        "build:per-api-key",
                        &model(HAIKU),
                        done(40_000, 90),
                        None,
                    ),
                ],
            ),
            phase(
                "Review",
                RowStatus::Done,
                Some((2, 2)),
                vec![
                    agent(
                        5,
                        "review:cap-bucket-refills",
                        &qwen,
                        done(1_900, 19),
                        approved,
                    ),
                    WorkflowRow::Agent(fallback),
                ],
            ),
        ],
        notes: Vec::new(),
        required: Vec::new(),
        goals: Vec::new(),
        worker: None,
        state: WorkflowState::Running,
    }
}

/// The fallback's row says whose place it took, after its verdict, in the warning colour,
/// cut at the pane; a script's rows name the model alone.
#[test]
fn a_fallback_for_a_local_reviewer_reads_as_its_picture() {
    let mut picture = picture!("standin_after");
    as_drawn(&mut picture, &[]);
    // A model no CLI runs, here Ollama's, is dim as every such model is, where the picture
    // gives it OpenCode's green.
    let dim = dark().style(crate::theme::Role::Dim).fg;
    let at = picture.text(3..4)[0].chars().collect::<String>();
    let column = at
        .find("Qwen3 Coder 30B")
        .map(|byte| at[..byte].chars().count());
    for offset in 0.."Qwen3 Coder 30B".len() {
        picture.recolour(3, column.expect("Qwen's row") + offset, dim);
    }
    let state = state();
    let mut view = WorkflowView::new(reviewed_locally());
    select(&mut view, 2, &state);
    // No status line yet: the box comes after the rule, the name and the description.
    let buf = drawn(&mut view, AGENT_ABOVE + 6, &state);
    picture.assert_drawn(2..7, &buf, AGENT_ABOVE);
}

/// Enter on each of that Review's rows: the first agent view says how Qwen had its work, and
/// the second whose place GPT 6 Luna took, each in the words the picture quotes under the list.
#[test]
fn a_local_reviewer_and_its_fallback_say_how_each_had_its_work() {
    let picture = picture!("standin_after");
    let caption = picture.text(8..10).join(" ");
    let quoted: Vec<&str> = caption.split('"').skip(1).step_by(2).collect();
    let [given, in_place] = quoted[..] else {
        panic!("the caption quotes two agent views: {caption}");
    };
    let state = state();
    let data = reviewed_locally();
    let rows: Vec<AgentRow> = data.phases[2]
        .rows
        .iter()
        .filter_map(|row| match row {
            WorkflowRow::Agent(row) => Some(row.clone()),
            _ => None,
        })
        .collect();
    let mut qwen = opened(rows[0].clone(), 3, Vec::new(), 0);
    qwen.facts = ["reviewer", "no tools"].map(str::to_owned).to_vec();
    qwen.given = Some(given.to_owned());
    let mut luna = opened(rows[1].clone(), 3, Vec::new(), 0);
    luna.facts = ["reviewer", "read-only"].map(str::to_owned).to_vec();
    luna.moved = Some(argonauts_core::views::Moved::FallbackFor {
        asked: rows[1].fallback_for.clone().expect("whose place it took"),
        which: rows[1].fallback_why.clone().expect("why"),
        lines: Vec::new(),
    });
    for (at, agent, says) in [(0, qwen, given), (1, luna, in_place)] {
        let mut view = WorkflowView::new(data.clone());
        select(&mut view, 2, &state);
        press(&mut view, KeyCode::Enter, &state);
        for _ in 0..at {
            press(&mut view, KeyCode::Down, &state);
        }
        press(&mut view, KeyCode::Enter, &state);
        assert!(view.accept(&argonauts_core::app::ViewData::Agent {
            view: Box::new(agent),
        }));
        let buf = drawn(&mut view, AGENT_ABOVE + 20, &state);
        let text: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        assert!(
            text.iter().any(|line| line.contains(says)),
            "the agent view says {says:?}:\n{}",
            text.join("\n")
        );
    }
}

/// The orchestrator's run as the fork picture draws it: one agent planning, on `worker`.
fn planning(worker: &ModelRef, tokens: u64, seconds: u64) -> (WorkflowData, AgentRow) {
    let mut plan = row(1, "plan:orchestrator", worker, RowStatus::Running);
    plan.tokens = tokens;
    plan.elapsed_ms = seconds * 1000;
    let data = WorkflowData {
        run: run(),
        id: WorkflowId::numbered(1),
        name: "Add due dates".to_owned(),
        description: "Orchestrator".to_owned(),
        style: None,
        status_lines: Vec::new(),
        agents_done: 0,
        agents_total: 1,
        elapsed_ms: seconds * 1000,
        spent: Spend::default(),
        phases: vec![phase(
            "Plan",
            RowStatus::Running,
            None,
            vec![WorkflowRow::Agent(plan.clone())],
        )],
        notes: Vec::new(),
        required: Vec::new(),
        goals: Vec::new(),
        worker: None,
        state: WorkflowState::Running,
    };
    (data, plan)
}

/// What an orchestrator a handover started is given: `first`, the line its own words start
/// on, then lines up to `lines` in all. What the rest holds is what the app reads from a script
/// writer's parts, the workflow file and the request; the picture's words for them are its own.
struct Given {
    lines: usize,
    first: usize,
    opening: &'static str,
}

impl Given {
    fn prompt(&self) -> (String, argonauts_core::views::PromptAbout) {
        let mut prompt: Vec<String> = (0..self.first)
            .map(|line| format!("Frame line {line}."))
            .collect();
        prompt.push(self.opening.to_owned());
        let mut line = 0;
        while prompt.len() < self.lines {
            line += 1;
            prompt.push(format!("Its line {line}."));
        }
        let about = argonauts_core::views::PromptAbout {
            first: self.first,
            holds: ["its input", "your request"].map(str::to_owned).to_vec(),
        };
        (prompt.join("\n"), about)
    }
}

/// The first line a copy of the main session reads.
const FORKED_OPENING: &str = "You are now the orchestrator of the run you just started.";

/// The first line of the script writer's own prompt, which one started from the brief reads
/// after its role's frame.
const WRITES_A_SCRIPT: &str = "You are the orchestrator of a crew of coding agents. Write the \
                               workflow for one request as a script. Do not do the work \
                               yourself, and change no files.";

/// The orchestrator's agent view, opened from its Plan row, with `from` as what it began
/// from, given `given`, with `activity` so far.
fn orchestrator_view(
    worker: &ModelRef,
    (tokens, seconds, calls): (u64, u64, u32),
    (from, given): (argonauts_core::views::StartedFrom, &Given),
    activity: Vec<ActivityLine>,
    state: &AppState,
) -> WorkflowView {
    let (data, mut plan) = planning(worker, tokens, seconds);
    let mut view = WorkflowView::new(data);
    select(&mut view, 0, state);
    press(&mut view, KeyCode::Enter, state);
    press(&mut view, KeyCode::Enter, state);
    if calls > 1 {
        plan.context = Some(argonauts_core::views::ContextUse {
            used: 54_000,
            window: Some(200_000),
            cap: None,
        });
    }
    let mut opened = opened(plan, 0, activity, calls);
    let (prompt, about) = given.prompt();
    opened.prompt = prompt;
    opened.prompt_about = Some(about);
    opened.facts = ["orchestrator", "read-only"].map(str::to_owned).to_vec();
    opened.started_from = Some(from);
    assert!(view.accept(&argonauts_core::app::ViewData::Agent {
        view: Box::new(opened),
    }));
    view
}

/// What an orchestrator began from is the first section of its agent view: a fork of the main
/// session with the conversation's size, or the brief with who wrote it and why the
/// orchestrator has only that, each as the fork picture draws it, with the prompt that brief
/// is and the calls under it.
///
/// Where the view and the picture aren't the same:
/// - The view keeps its sections a line apart, as every agent view does.
/// - The prompt's words are the picture's own: the copy's first line is the build's, and what
///   the rest holds, and the first line of one started from the brief, read as the build
///   writes them.
/// - The second agent view stops after its prompt, where the view goes on to its calls and says so
///   on its frame.
#[test]
fn an_orchestrators_view_says_what_it_began_from() {
    use argonauts_core::events::run::BriefFrom;
    use argonauts_core::views::StartedFrom;

    let mut picture = picture!("fork_start");
    picture.bright_frames();
    picture.reword(
        19,
        "you no longer talk to the user, the brief, and the crew's tools",
        "its input and your request",
    );
    picture.reword(
        46,
        "You are the orchestrator of a run. The request, as the main agent wrote it:",
        "You are the orchestrator of a crew of coding agents. Write the workflow for one reque…",
    );
    picture.reword(
        47,
        "the request, what was decided in the conversation, the crew's tools",
        "its input and your request",
    );
    // The second view first, since spacing the first moves the rows under it.
    picture.space_sections(39, &["Prompt"]);
    picture.space_sections(11, &["Prompt", "Activity", "Outcome"]);
    let state = state();
    let opus = model("claude-code/claude-opus-5-5");
    let forked = Given {
        lines: 14,
        first: 0,
        opening: FORKED_OPENING,
    };
    let activity = vec![
        call("Read", "src/cli.js"),
        ActivityLine {
            ok: None,
            ..call("Grep", "--due")
        },
    ];
    let mut view = orchestrator_view(
        &opus,
        (54_600, 21, 2),
        (
            StartedFrom::Fork {
                tokens: Some(48_200),
            },
            &forked,
        ),
        activity,
        &state,
    );
    let buf = drawn(&mut view, agent_frame(19), &state);
    picture.assert_drawn(11..30, &buf, AGENT_ABOVE);

    let gemini = model("antigravity-cli/gemini-3.6-pro");
    let brief = StartedFrom::Brief(BriefFrom {
        written_by: opus,
        why: Some("agy can't carry a Claude Code conversation".to_owned()),
    });
    let briefed = Given {
        lines: 31,
        first: 12,
        opening: WRITES_A_SCRIPT,
    };
    let read = vec![call("Read", "src/cli.js")];
    let mut view = orchestrator_view(&gemini, (8_900, 9, 1), (brief, &briefed), read, &state);
    let buf = drawn(&mut view, agent_frame(12), &state);
    let bottom = 52;
    let corner = picture.text(bottom..bottom + 1).remove(0).chars().count() - 1;
    let more = "    1–9 of 15 ↓ ";
    let dim = dark().style(crate::theme::Role::Dim).fg;
    picture.write(bottom, corner - 1 - more.chars().count(), more, dim);
    picture.assert_drawn(42..54, &buf, AGENT_ABOVE);
}

/// One of the Captain run's views as the app built it from the run's own events, kept beside
/// the picture by the app's test of that run.
fn captain_view(name: &str) -> String {
    let path = format!(
        "{}/tests/golden/captain_after.{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{path}: {err}"))
}

/// The ranking the other tabs give your models in the Captain run, whose judge ran Gemini
/// 3.8 Flash at its low effort, third among agy's models as the plain one is elsewhere.
fn captain_state() -> AppState {
    let ranked = [
        ("claude-code", "claude-opus-5-5", 900_000),
        ("claude-code", "claude-sonnet-5-5", 500_000),
        ("claude-code", "claude-haiku-5-5", 100_000),
        ("codex", "gpt-5.6", 800_000),
        ("codex", "gpt-6-luna", 300_000),
        ("antigravity-cli", "gemini-3.6-pro", 600_000),
        ("antigravity-cli", "gemini-3.6-flash", 400_000),
        ("antigravity-cli", "gemini-3.8-flash-low", 100_000),
    ];
    let usage = argonauts_core::api::UsageSummary {
        all_time_models: ranked
            .map(
                |(agent, model, tokens)| argonauts_core::overview::ModelTokens {
                    agent: agent.to_owned(),
                    model: Some(model.to_owned()),
                    tokens,
                },
            )
            .to_vec(),
        ..argonauts_core::api::UsageSummary::default()
    };
    let mut state = AppState::default();
    state.usage = Some(std::sync::Arc::new(usage));
    state
}

fn the_captain_run() -> Picture {
    let mut picture = picture!("captain_after");
    as_drawn(&mut picture, &[]);
    picture
}

/// The Captain run's picture where the run's own record says otherwise: its judge ran Gemini
/// 3.8 Flash at its low effort, which the model's name says, in its row and in the line under
/// the header.
///
/// Times are whole seconds counted down, as a running clock counts them: Codex's review took
/// 6984 ms, 6s, and verify:tests 4834 ms, 4s, where the picture rounds them up. And the build
/// doesn't read `8 tests pass` out of verify:tests' own result.
fn the_captain_record(picture: &mut Picture) {
    let low = |picture: &mut Picture, row: usize, from: &str, to: &str| {
        picture.reword(row, from, to);
    };
    low(picture, 8, "Gemini 3.8 Flash ", "Gemini 3.8 Flash Low ");
    low(
        picture,
        71,
        "Gemini 3.8 Flash signed off",
        "Gemini 3.8 Flash Low signed…",
    );
    let ended = picture.text(71..72).remove(0);
    let cut = ended
        .find(" signed…")
        .map(|byte| ended[..byte].chars().count());
    let dim = dark().style(crate::theme::Role::Dim).fg;
    picture.write(71, cut.expect("the cut end line"), " signed…", dim);
    low(picture, 74, "12.7k tok · 8 tests pass", "12.7k tok");
    let dim = dark().style(crate::theme::Role::Dim).fg;
    let review = picture.text(17..18).remove(0);
    let took = review
        .find("7s │")
        .map(|byte| review[..byte].chars().count());
    picture.write(17, took.expect("the review's time"), "6s", dim);
    let verify = picture.text(74..75).remove(0);
    let took = verify
        .find("5s │")
        .map(|byte| verify[..byte].chars().count());
    let dim = dark().style(crate::theme::Role::Dim).fg;
    picture.write(74, took.expect("verify's time"), "4s", dim);
    low(picture, 5, "Gemini 3.8 Flash", "Gemini 3.8 Flash Low");
}

/// 57 s into the Captain run, while Gemini judges: Claude Code's four phases, Review and Judge
/// filled by who answered, and Verify not started; then one ↑ away, on Review.
#[test]
fn the_captain_run_while_it_judges_reads_as_its_picture() {
    let mut picture = the_captain_run();
    the_captain_record(&mut picture);
    let state = captain_state();
    let data: WorkflowData = serde_json::from_str(&captain_view("judging")).unwrap();
    let mut view = WorkflowView::new(data);
    picture.assert_drawn(2..14, &drawn(&mut view, 12, &state), 0);
    press(&mut view, KeyCode::Up, &state);
    picture.assert_drawn(16..22, &drawn(&mut view, 12, &state), ABOVE_BOX);
}

/// The run once merged: the merge in the header's third line, and no Merge phase.
#[test]
fn the_captain_run_once_merged_reads_as_its_picture() {
    let mut picture = the_captain_run();
    the_captain_record(&mut picture);
    let state = captain_state();
    let data: WorkflowData = serde_json::from_str(&captain_view("done")).unwrap();
    let mut view = WorkflowView::new(data);
    picture.assert_drawn(68..80, &drawn(&mut view, 12, &state), 0);
}

/// Its row in the bottom list, while it judges and once merged.
#[test]
fn the_captain_runs_row_in_the_bottom_list_reads_as_its_picture() {
    let picture = the_captain_run();
    let theme = dark();
    let ctx = RenderCtx::new(SCREEN, &theme, Clock::still(Instant::now()));
    let name = "Fix the money rounding";
    for (at, state, (done, all), seconds, tokens, said) in [
        (24, WorkflowRowState::Running, (2, 3), 57, 69_600, None),
        (
            82,
            WorkflowRowState::Done,
            (4, 4),
            83,
            90_400,
            Some("merged onto main"),
        ),
    ] {
        let mut row = PanelRow::new("run-01", name, state);
        row.agents_done = done;
        row.agents_total = all;
        row.elapsed_ms = seconds * 1000;
        row.tokens = tokens;
        row.said = said.map(str::to_owned);
        let panel = WorkflowPanel {
            rows: vec![row],
            max_rows: 5,
            ..WorkflowPanel::default()
        };
        let lines = panel_lines(&panel, &AgentPanel::default(), &ctx);
        picture.assert_drawn(at..at + 1, &buffer_of(&lines, SCREEN), 0);
    }
}

/// Enter on the Judge row at the same moment: Gemini's view, with Haiku's agent that asked for
/// it, at work until the verdict comes, its prompt from past the frame, and the review it was
/// given, which approved and found nothing.
///
/// What the run's record says otherwise: the judge ran Gemini 3.8 Flash at its low effort; agy
/// reported its context fill, which the counts carry; Claude Code reports an agent's tokens
/// only once it ends, so the agent that asked has its time alone; and the prompt as recorded is
/// 70 lines. Its calls read by Claude Code's names for them, as every CLI's do.
#[test]
fn the_judges_view_reads_as_its_picture() {
    let mut picture = the_captain_run();
    picture.reword(30, "Gemini 3.8 Flash ", "Gemini 3.8 Flash Low ");
    picture.reword(
        31,
        "2 tool calls · 15s",
        "2 tool calls · 15s · context 6.5k",
    );
    picture.reword(34, " · 12.2k tok · 17s", " · 17s");
    picture.reword(36, "73 lines", "70 lines");
    picture.reword(38, "72 more lines", "69 more lines");
    picture.reword(45, "view_file", "Read");
    picture.reword(46, "view_file", "Read");
    let state = captain_state();
    let data: WorkflowData = serde_json::from_str(&captain_view("judging")).unwrap();
    let judge: AgentData = serde_json::from_str(&captain_view("judge")).unwrap();
    let mut view = WorkflowView::new(data);
    press(&mut view, KeyCode::Enter, &state);
    press(&mut view, KeyCode::Enter, &state);
    assert!(view.accept(&argonauts_core::app::ViewData::Agent {
        view: Box::new(judge),
    }));
    // The run's note under the header: the rule, the name, the description, the note and a
    // blank row above the view's box.
    let buf = drawn(&mut view, ABOVE_BOX + 23, &state);
    picture.assert_drawn(29..52, &buf, ABOVE_BOX);
}
