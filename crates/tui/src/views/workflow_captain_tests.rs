//! A Captain run that Claude Code's own workflow runs, beside its pictures: `Fix the money
//! rounding`, where Claude Code's agents hand the reviews to Codex and the sign-off to
//! Antigravity CLI. Each test builds the moment its picture catches, draws it at the picture's
//! width, and compares every cell, its colour included.

use std::sync::Arc;
use std::time::Instant;

use argonauts_core::Spend;
use argonauts_core::api::UsageSummary;
use argonauts_core::app::ViewData;
use argonauts_core::names::ModelRef;
use argonauts_core::names::WorkflowId;
use argonauts_core::overview::ModelTokens;
use argonauts_core::steps::plan::Severity;
use argonauts_core::views::{
    ActivityLine, AgentRow, AgentView as AgentData, AskedFor, CommandRow, EarlierFindings, Finding,
    PhaseCount, PhaseRow, ReviewGiven, RowNote, RowStatus, StatusKind, StatusLine, Tone,
    WorkflowRow, WorkflowState, WorkflowView as WorkflowData,
};
use argonauts_core::workflow::StandardPhase;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::workflow::WorkflowView;
use super::workflow_examples::{agent, run};
use super::workflow_pictures::{dark, picture};
use super::{View, ViewCtx};
use crate::state::AppState;

const SCREEN: u16 = 124;
const HAIKU: &str = "claude-code/claude-haiku-5-5";
const LUNA: &str = "codex/gpt-6-luna";
const FLASH: &str = "antigravity-cli/gemini-3.8-flash";

/// Your models as the other tabs rank them, the most tokens first, so each keeps the colour it
/// has there: Haiku 5.5 is Claude Code's third, GPT 6 Luna Codex's second, and Gemini 3.8 Flash
/// Antigravity CLI's third.
fn state() -> AppState {
    let ranked = [
        ("claude-code", "claude-opus-5-5", 900_000),
        ("claude-code", "claude-sonnet-5-5", 500_000),
        ("claude-code", "claude-haiku-5-5", 100_000),
        ("codex", "gpt-6-astra", 800_000),
        ("codex", "gpt-6-luna", 300_000),
        ("antigravity-cli", "gemini-3.6-pro", 600_000),
        ("antigravity-cli", "gemini-3.6-flash", 400_000),
        ("antigravity-cli", "gemini-3.8-flash", 100_000),
    ];
    let usage = UsageSummary {
        all_time_models: ranked
            .map(|(agent, model, tokens)| ModelTokens {
                agent: agent.to_owned(),
                model: Some(model.to_owned()),
                tokens,
            })
            .to_vec(),
        ..UsageSummary::default()
    };
    let mut state = AppState::default();
    state.usage = Some(Arc::new(usage));
    state
}

fn drawn(view: &mut WorkflowView, rows: u16) -> Buffer {
    let theme = dark();
    let state = state();
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

fn press(view: &mut WorkflowView, code: KeyCode) {
    let theme = dark();
    let state = state();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx);
}

/// A row Claude Code's agent handed over: the model that answered, with its own tokens and time.
fn answered(mut row: AgentRow, note: RowNote) -> WorkflowRow {
    row.answered = true;
    row.note = Some(note);
    WorkflowRow::Agent(row)
}

fn noted(mut row: AgentRow, note: RowNote) -> WorkflowRow {
    row.note = Some(note);
    WorkflowRow::Agent(row)
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
        count: count.and_then(|(done, total)| PhaseCount::of(done, total)),
        rows,
        note: None,
    }
}

fn waiting(name: &str) -> PhaseRow {
    phase(name, RowStatus::Waiting, None, Vec::new())
}

/// The run's view as the app gives it: Claude Code's phases, then the merge.
fn captain(agents: (u32, u32), seconds: u64, says: &str, phases: Vec<PhaseRow>) -> WorkflowData {
    WorkflowData {
        run: run(),
        id: WorkflowId::numbered(1),
        name: "Fix the money rounding".to_owned(),
        description: "Codex reviews, as you asked".to_owned(),
        style: None,
        status_lines: vec![StatusLine::new(StatusKind::Note, says)],
        agents_done: agents.0,
        agents_total: agents.1,
        elapsed_ms: seconds * 1000,
        spent: Spend::default(),
        phases,
        notes: vec![
            "Captain".to_owned(),
            "Claude Code's workflow fix-money-rounding".to_owned(),
        ],
        required: Vec::new(),
        goals: Vec::new(),
        worker: Some("claude-code".to_owned()),
        state: WorkflowState::Running,
    }
}

fn build_done() -> PhaseRow {
    let build = agent(1, "build:fix-money", HAIKU, RowStatus::Done, 14_600, 10);
    phase(
        "Build",
        RowStatus::Done,
        Some((1, 1)),
        vec![WorkflowRow::Agent(build)],
    )
}

/// 33 s in: Codex answered review:money, and Claude Code's agent is passing the answer back.
#[test]
fn an_answer_fills_the_row_that_asked_for_it() {
    let picture = picture!("answer_after");
    let review = agent(2, "review:money", LUNA, RowStatus::Running, 51_800, 7);
    let data = captain(
        (1, 2),
        33,
        "review: GPT 6 Luna on Codex sent Haiku 5.5's change back, 1 finding · Claude Code's agent is passing it on",
        vec![
            build_done(),
            phase(
                "Review",
                RowStatus::Running,
                Some((0, 1)),
                vec![answered(
                    review,
                    RowNote::new("sent it back", Tone::Warn).then("passing it on"),
                )],
            ),
            waiting("Final"),
            waiting("Merge"),
        ],
    );
    let mut view = WorkflowView::new(data);
    picture.assert_drawn(0..11, &drawn(&mut view, 12), 0);
}

fn at_one_minute() -> WorkflowData {
    let money = agent(2, "review:money", LUNA, RowStatus::Done, 51_800, 7);
    let fix = agent(3, "fix:review-findings", HAIKU, RowStatus::Done, 15_400, 13);
    let last = agent(4, "review:final", LUNA, RowStatus::Done, 16_800, 4);
    let sign_off = agent(5, "sign-off:money", FLASH, RowStatus::Running, 9_100, 3);
    let mut final_phase = phase(
        "Final",
        RowStatus::Running,
        Some((1, 2)),
        vec![
            answered(last, RowNote::new("approved", Tone::Good).then("1 note")),
            WorkflowRow::Agent(sign_off),
        ],
    );
    final_phase.note = Some(
        "round 2 of the review: Codex sent the first change back in Review, and Haiku 5.5 fixed it"
            .to_owned(),
    );
    captain(
        (4, 5),
        66,
        "final: GPT 6 Luna on Codex approved Haiku 5.5's change, 1 note · Gemini 3.8 Flash on Antigravity CLI signs off",
        vec![
            build_done(),
            phase(
                "Review",
                RowStatus::Done,
                Some((2, 2)),
                vec![
                    answered(money, RowNote::new("sent it back · 1 blocking", Tone::Warn)),
                    noted(fix, RowNote::new("fixed the sign bug", Tone::Plain)),
                ],
            ),
            final_phase,
            waiting("Merge"),
        ],
    )
}

/// 1 m 06 s in, on Final, then one ↑ away on Review: each row names the model that did its
/// job, with " on" its CLI where that isn't Claude Code.
///
/// One label column holds for the whole workflow, every level of it, so the models line up
/// from phase to phase. The picture is wrong to lay Final's rows out by Final's own
/// widest label: the test moves them to the workflow's column, set by Review's
/// `fix:review-findings`, five cells further right.
#[test]
fn a_captain_run_mid_review_reads_as_its_picture() {
    let mut picture = picture!("captain_review_running");
    picture.reword(6, "review:final    ", "review:final         ");
    picture.reword(7, "sign-off:money  ", "sign-off:money       ");
    let mut view = WorkflowView::new(at_one_minute());
    picture.assert_drawn(0..12, &drawn(&mut view, 12), 0);
    press(&mut view, KeyCode::Up);
    picture.assert_drawn(14..20, &drawn(&mut view, 12), 5);
}

/// 15 s in, the first moment anything of the workflow shows: Claude Code's first report listed
/// its phases, and its builder has started. No agent has finished, so the open phase names the
/// phases still ahead of it.
#[test]
fn a_captain_run_shows_once_its_phases_are_known() {
    let picture = picture!("workflow_shape_known");
    let build = agent(1, "build:fix-money", HAIKU, RowStatus::Running, 2_100, 1);
    let mut data = captain(
        (0, 1),
        15,
        "",
        vec![
            phase(
                "Build",
                RowStatus::Running,
                Some((0, 1)),
                vec![WorkflowRow::Agent(build)],
            ),
            waiting("Review"),
            waiting("Final"),
            waiting("Merge"),
        ],
    );
    data.status_lines = Vec::new();
    let mut view = WorkflowView::new(data);
    picture.assert_drawn(16..26, &drawn(&mut view, 11), 0);
}

/// A step Argonauts Code runs itself, done: its label, what ran, what came of it.
fn command(label: &str, ran: &str, result: &str, seconds: u64) -> WorkflowRow {
    WorkflowRow::Command(CommandRow {
        label: label.to_owned(),
        command: ran.to_owned(),
        result: Some(result.to_owned()),
        tone: Tone::Plain,
        status: RowStatus::Done,
        elapsed_ms: seconds * 1000,
    })
}

/// Done and merged, on Merge: the end line names who built it, who reviewed it and who signed
/// it off, each model in its row's colour.
///
/// One label column holds for the whole workflow, set by its widest label,
/// `fix:review-findings` here. The picture is wrong to keep an earlier run's column in Merge's
/// rows, set by that run's `sign-off:add-due-dates`: the test moves them to this run's column,
/// three cells further left.
#[test]
fn a_merged_captain_run_says_who_did_its_work() {
    let mut picture = picture!("captain_done");
    picture.reword(16, "final check             ", "final check          ");
    picture.reword(17, "commit message          ", "commit message       ");
    picture.reword(18, "merge onto main         ", "merge onto main      ");
    let done = |session, label, model, tokens, seconds| {
        WorkflowRow::Agent(agent(
            session,
            label,
            model,
            RowStatus::Done,
            tokens,
            seconds,
        ))
    };
    let mut data = captain(
        (6, 6),
        91,
        "",
        vec![
            build_done(),
            phase(
                "Review",
                RowStatus::Done,
                Some((2, 2)),
                vec![
                    done(2, "review:money", LUNA, 51_800, 7),
                    done(3, "fix:review-findings", HAIKU, 15_400, 13),
                ],
            ),
            phase(
                "Final",
                RowStatus::Done,
                Some((2, 2)),
                vec![
                    done(4, "review:final", LUNA, 16_800, 4),
                    done(5, "sign-off:money", FLASH, 12_000, 6),
                ],
            ),
            phase(
                "Merge",
                RowStatus::Done,
                Some((1, 1)),
                vec![
                    command(
                        "final check",
                        "python3 -m unittest",
                        "14 of 14 pass, 3 failed at the start",
                        1,
                    ),
                    noted(
                        agent(6, "commit message", HAIKU, RowStatus::Done, 4_100, 8),
                        RowNote::new("given the diff, no tools", Tone::Plain),
                    ),
                    command("merge onto main", "git", "1 task · no conflicts", 1),
                ],
            ),
        ],
    );
    data.state = WorkflowState::Done;
    data.status_lines = vec![
        StatusLine::new(
            StatusKind::Ended,
            "Merged onto main · 3f9c2a1 · 3 files +40 −5 · Haiku 5.5 built it, GPT 6 Luna \
             reviewed, Gemini 3.8 Flash signed off",
        )
        .toned(Tone::Good),
    ];
    let mut view = WorkflowView::new(data);
    picture.assert_drawn(10..22, &drawn(&mut view, 12), 0);
}

/// Enter on review:final: Codex's agent view. Its first lines are Codex's own, with the row's numbers;
/// then who asked for it and passed the verdict back, the prompt and what it holds, the earlier
/// round's finding with its answer, and the verdict with its note.
#[test]
fn a_handed_over_review_opens_as_its_picture() {
    let picture = picture!("captain_review_agent");
    let mut view = WorkflowView::new(at_one_minute());
    press(&mut view, KeyCode::Enter);
    press(&mut view, KeyCode::Enter);
    let row = agent(4, "review:final", LUNA, RowStatus::Done, 16_800, 4);
    let mut prompt = vec![
        "Review build:fix-money's change, as fixed by fix:review-findings, against your request."
            .to_owned(),
    ];
    for part in [
        "Your request",
        "The test run",
        "The change",
        "The earlier finding",
    ] {
        prompt.push(String::new());
        prompt.push(format!("## {part}"));
        prompt.extend((0..36).map(|line| format!("{part}, line {line}")));
    }
    prompt.truncate(152);
    let opened = AgentData {
        run: run(),
        row,
        prompt: prompt.join("\n"),
        activity: vec![ActivityLine {
            tool: "the verdict below".to_owned(),
            summary: String::new(),
            ok: Some(true),
            note: Some("no command run: the test run came with the prompt".to_owned()),
        }],
        tool_calls: 1,
        diff: None,
        subagents: None,
        outcome: None,
        goal: None,
        plan: None,
        facts: ["reviewer", "read-only"].map(str::to_owned).to_vec(),
        to_fix: None,
        asked_for: Some(AskedFor {
            cli: "claude-code".to_owned(),
            label: "review:final".to_owned(),
            worker: ModelRef::new("claude-code", Some("claude-haiku-5-5".to_owned())),
            tokens: 12_300,
            elapsed_ms: 8_000,
        }),
        earlier: Some(EarlierFindings {
            from: "review:money".to_owned(),
            round: 1,
            answered_by: Some("fix:review-findings".to_owned()),
            items: vec![Finding {
                severity: Severity::High,
                place: "invoice/money.py".to_owned(),
                text: "\"-$1.50\" read as +150 cents".to_owned(),
            }],
            answers: vec![
                "\"-$1.50\" was already right; \"$-1.50\" read as -50 cents, now fixed".to_owned(),
            ],
        }),
        review: Some(ReviewGiven {
            verdict: "approved".to_owned(),
            tone: Tone::Good,
            findings: vec![Finding {
                severity: Severity::Low,
                place: "invoice/money.py".to_owned(),
                text: "\"1.234\" becomes 1234 cents, not 123; beyond your request".to_owned(),
            }],
        }),
        moved: None,
    };
    assert!(view.accept(&ViewData::Agent {
        view: Box::new(opened),
    }));
    picture.assert_drawn(0..18, &drawn(&mut view, 23), 5);
}
