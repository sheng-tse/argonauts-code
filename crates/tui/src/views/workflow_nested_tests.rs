//! Workflows inside workflows beside their pictures: `Add due dates`, an orchestrator run where
//! the worker build:due-flag on Claude Code runs a workflow of its own, a release run whose own
//! file calls `workflow("lint")`, and a Captain run where Claude Code starts a second workflow
//! for the same task. Each test builds the moment its picture catches, draws it at the
//! picture's width, and compares every cell, its colour included.

use std::sync::Arc;
use std::time::Instant;

use argonauts_core::Spend;
use argonauts_core::api::UsageSummary;
use argonauts_core::app::ViewData;
use argonauts_core::names::{SessionKey, WorkflowId};
use argonauts_core::overview::ModelTokens;
use argonauts_core::views::{
    ActivityLine, AgentRow, AgentView as AgentData, ContextUse, InnerAgent, Moved,
    NestedWorkflowRow, PhaseCount, PhaseRow, RowNote, RowStatus, StatusKind, StatusLine, Tone,
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
pub(super) const SONNET: &str = "claude-code/claude-sonnet-5-5";
pub(super) const HAIKU: &str = "claude-code/claude-haiku-5-5";
pub(super) const GPT: &str = "codex/gpt-5.6";
pub(super) const LUNA: &str = "codex/gpt-6-luna";
pub(super) const FLASH: &str = "antigravity-cli/gemini-3.8-flash";

/// Your models as the other tabs rank them, so each keeps the colour it has there: Sonnet 5.5
/// is Claude Code's second and Haiku 5.5 its third, GPT 5.6 Codex's first and GPT 6 Luna its
/// second, and Gemini 3.8 Flash agy's third.
pub(super) fn state() -> AppState {
    ranked_with("gpt-6-luna")
}

/// The same ranking with `second` as Codex's second model.
fn ranked_with(second: &str) -> AppState {
    let ranked = [
        ("claude-code", "claude-opus-5-5", 900_000),
        ("claude-code", "claude-sonnet-5-5", 500_000),
        ("claude-code", "claude-haiku-5-5", 100_000),
        ("codex", "gpt-5.6", 800_000),
        ("codex", second, 300_000),
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
    drawn_in(view, rows, &state())
}

fn drawn_in(view: &mut WorkflowView, rows: u16, state: &AppState) -> Buffer {
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

pub(super) fn phase(
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

pub(super) fn waiting(name: &str) -> PhaseRow {
    phase(name, RowStatus::Waiting, None, Vec::new())
}

fn noted(mut row: AgentRow, words: &str, tone: Tone) -> AgentRow {
    row.note = Some(RowNote::new(words, tone));
    row
}

/// `row` as one agent of a workflow its host's CLI runs, the `index`th it started.
pub(super) fn inner(row: AgentRow, workflow: &WorkflowId, index: u32) -> WorkflowRow {
    let mut row = row;
    row.inner = Some(InnerAgent {
        workflow: workflow.clone(),
        index,
    });
    WorkflowRow::Agent(row)
}

/// A workflow inside: `parent` when an agent's own CLI runs it, `cli` when it is a CLI's own.
#[allow(clippy::too_many_arguments)]
pub(super) fn inside(
    id: &WorkflowId,
    name: &str,
    status: RowStatus,
    agents: (u32, u32),
    seconds: u64,
    parent: Option<u32>,
    cli: Option<&str>,
    phases: Vec<PhaseRow>,
    rows: Vec<WorkflowRow>,
) -> WorkflowRow {
    WorkflowRow::Workflow(NestedWorkflowRow {
        id: id.clone(),
        name: name.to_owned(),
        status,
        agents_done: agents.0,
        agents_total: agents.1,
        workers: Vec::new(),
        elapsed_ms: seconds * 1000,
        parent: parent.map(SessionKey::numbered),
        worker: cli.map(str::to_owned),
        phases,
        rows,
    })
}

pub(super) fn add_due_flag_id() -> WorkflowId {
    argonauts_core::views::cli_workflow_run_id("wf_due_flag")
}

fn date_cases_id() -> WorkflowId {
    argonauts_core::views::cli_workflow_run_id("wf_due_flag-date-cases")
}

/// Add due dates, at the moments its pictures catch.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Moment {
    /// 2 m 52 s in: add-due-flag's Write is done and Check under way.
    Working,
    /// 3 m 31 s in: add-due-flag has ended.
    Ended,
    /// 3 m 17 s in, with two levels inside: Check started date-cases.
    Deepest,
}

pub(super) fn due_dates(moment: Moment) -> WorkflowData {
    let id = add_due_flag_id();
    let write = phase(
        "Write",
        RowStatus::Done,
        Some((2, 2)),
        vec![
            inner(
                agent(1, "write:add", SONNET, RowStatus::Done, 12_100, 41),
                &id,
                1,
            ),
            inner(
                agent(1, "write:list", SONNET, RowStatus::Done, 9_600, 38),
                &id,
                2,
            ),
        ],
    );
    let check = match moment {
        Moment::Working => phase(
            "Check",
            RowStatus::Running,
            Some((0, 1)),
            vec![inner(
                agent(1, "check:dates", HAIKU, RowStatus::Running, 4_200, 12),
                &id,
                3,
            )],
        ),
        Moment::Ended => phase(
            "Check",
            RowStatus::Done,
            Some((1, 1)),
            vec![inner(
                agent(1, "check:dates", HAIKU, RowStatus::Done, 6_800, 31),
                &id,
                3,
            )],
        ),
        Moment::Deepest => phase(
            "Check",
            RowStatus::Running,
            Some((2, 3)),
            vec![
                inner(
                    agent(1, "check:dates", HAIKU, RowStatus::Done, 6_800, 31),
                    &id,
                    3,
                ),
                inside(
                    &date_cases_id(),
                    "date-cases",
                    RowStatus::Running,
                    (1, 2),
                    22,
                    None,
                    Some("claude-code"),
                    Vec::new(),
                    vec![
                        inner(
                            agent(1, "case:leap-day", HAIKU, RowStatus::Done, 3_900, 14),
                            &id,
                            4,
                        ),
                        inner(
                            agent(1, "case:bad-date", HAIKU, RowStatus::Running, 2_700, 9),
                            &id,
                            5,
                        ),
                    ],
                ),
            ],
        ),
    };
    let (flag, agents, seconds, total, says) = match moment {
        Moment::Working => (
            WorkflowRow::Agent(agent(
                1,
                "build:due-flag",
                SONNET,
                RowStatus::Running,
                22_400,
                160,
            )),
            (2, 3),
            118,
            ((3, 5), 172),
            "build:due-flag runs a workflow of its own, add-due-flag: Write is done, Check is under way",
        ),
        Moment::Ended => (
            WorkflowRow::Agent(noted(
                agent(1, "build:due-flag", SONNET, RowStatus::Done, 27_900, 194),
                "passed",
                Tone::Good,
            )),
            (3, 3),
            151,
            ((5, 5), 211),
            "build: both tasks passed; review starts next",
        ),
        Moment::Deepest => (
            WorkflowRow::Agent(agent(
                1,
                "build:due-flag",
                SONNET,
                RowStatus::Running,
                22_400,
                185,
            )),
            (4, 5),
            143,
            ((5, 7), 197),
            "add-due-flag, build:due-flag's workflow, started date-cases from its Check phase",
        ),
    };
    let status = if moment == Moment::Ended {
        RowStatus::Done
    } else {
        RowStatus::Running
    };
    let workflow = inside(
        &id,
        "add-due-flag",
        status,
        agents,
        seconds,
        Some(1),
        Some("claude-code"),
        vec![write, check],
        Vec::new(),
    );
    let tests = noted(
        agent(2, "build:due-tests", GPT, RowStatus::Done, 30_300, 62),
        "passed",
        Tone::Good,
    );
    let ((done, all), elapsed) = total;
    let build = phase(
        "Build",
        if moment == Moment::Ended {
            RowStatus::Done
        } else {
            RowStatus::Running
        },
        Some((done, all)),
        vec![flag, workflow, WorkflowRow::Agent(tests)],
    );
    WorkflowData {
        run: run(),
        id: WorkflowId::numbered(1),
        name: "Add due dates".to_owned(),
        description: "add --due to the todo CLI, each part reviewed on another CLI".to_owned(),
        style: None,
        status_lines: vec![StatusLine::new(StatusKind::Note, says)],
        agents_done: done,
        agents_total: all,
        elapsed_ms: elapsed * 1000,
        spent: Spend::default(),
        phases: vec![
            build,
            waiting("Review"),
            waiting("Sign-off"),
            waiting("Merge"),
        ],
        notes: vec!["orchestrator workflow".to_owned()],
        required: Vec::new(),
        goals: Vec::new(),
        worker: None,
        state: WorkflowState::Running,
    }
}

/// 2 m 52 s in: the worker's workflow sits right under its row, open while it runs, its
/// phases as headings with their agents under them, and Build counts the agents inside.
#[test]
fn a_workflow_inside_reads_as_its_picture() {
    let picture = picture!("nested_orchestrator");
    let mut view = WorkflowView::new(due_dates(Moment::Working));
    picture.assert_drawn(0..16, &drawn(&mut view, 16), 0);
}

/// ← folds it to one line that keeps its count, and the model column doesn't move; on a CLI's
/// own workflow the key line says where its keys are. Once it ends it folds by itself.
#[test]
fn a_workflow_inside_folds_as_its_picture() {
    let picture = picture!("nested_orchestrator_folded");
    let mut view = WorkflowView::new(due_dates(Moment::Working));
    press(&mut view, KeyCode::Enter);
    press(&mut view, KeyCode::Down);
    press(&mut view, KeyCode::Left);
    picture.assert_drawn(1..8, &drawn(&mut view, 12), 5);

    let mut view = WorkflowView::new(due_dates(Moment::Ended));
    picture.assert_drawn(12..24, &drawn(&mut view, 12), 0);
}

/// Two levels inside the run, each two cells further in, with the models and times still in
/// one column, and the pane saying once that this is the deepest.
#[test]
fn the_deepest_workflow_reads_as_its_picture() {
    let picture = picture!("nested_deepest");
    let mut view = WorkflowView::new(due_dates(Moment::Deepest));
    picture.assert_drawn(0..21, &drawn(&mut view, 21), 0);
}

/// Your own workflow file calls `workflow("lint")` from its Check phase: Argonauts Code's own,
/// so its row says just workflow, and its phases show as headings.
#[test]
fn a_scripts_own_child_reads_as_its_picture() {
    let picture = picture!("nested_script_child");
    let lint = WorkflowId::numbered(2);
    let lint_row = inside(
        &lint,
        "lint",
        RowStatus::Running,
        (1, 3),
        31,
        None,
        None,
        vec![
            phase(
                "Rust",
                RowStatus::Done,
                Some((1, 1)),
                vec![WorkflowRow::Agent(noted(
                    agent(4, "lint:clippy", HAIKU, RowStatus::Done, 5_100, 19),
                    "2 fixed",
                    Tone::Good,
                ))],
            ),
            phase(
                "Docs",
                RowStatus::Running,
                Some((0, 2)),
                vec![
                    WorkflowRow::Agent(agent(5, "lint:readme", LUNA, RowStatus::Running, 3_000, 8)),
                    WorkflowRow::Agent(agent(6, "lint:changelog", HAIKU, RowStatus::Queued, 0, 0)),
                ],
            ),
        ],
        Vec::new(),
    );
    let data = WorkflowData {
        run: run(),
        id: WorkflowId::numbered(1),
        name: "Ship the 0.3 release".to_owned(),
        description: "your file .argonauts/workflows/release.js".to_owned(),
        style: None,
        status_lines: vec![StatusLine::new(
            StatusKind::Note,
            "check: lint, a workflow of its own, runs Rust then Docs",
        )],
        agents_done: 4,
        agents_total: 6,
        elapsed_ms: 130_000,
        spent: Spend::default(),
        phases: vec![
            phase(
                "Build",
                RowStatus::Done,
                Some((2, 2)),
                vec![
                    WorkflowRow::Agent(agent(1, "build:notes", HAIKU, RowStatus::Done, 11_000, 41)),
                    WorkflowRow::Agent(agent(
                        2,
                        "build:version",
                        HAIKU,
                        RowStatus::Done,
                        6_400,
                        22,
                    )),
                ],
            ),
            phase(
                "Check",
                RowStatus::Running,
                Some((2, 4)),
                vec![
                    WorkflowRow::Agent(noted(
                        agent(3, "check:types", HAIKU, RowStatus::Done, 8_200, 22),
                        "passed",
                        Tone::Good,
                    )),
                    lint_row,
                ],
            ),
            waiting("Merge"),
        ],
        notes: vec!["release workflow".to_owned()],
        required: Vec::new(),
        goals: Vec::new(),
        worker: None,
        state: WorkflowState::Running,
    };
    let mut view = WorkflowView::new(data);
    picture.assert_drawn(0..15, &drawn(&mut view, 15), 0);
}

fn answered(mut row: AgentRow, note: RowNote) -> WorkflowRow {
    row.answered = true;
    row.note = Some(note);
    WorkflowRow::Agent(row)
}

/// A Captain run's view, Claude Code's phases then the merge, with `last` as Final's rows.
fn money(agents: (u32, u32), seconds: u64, says: &str, final_phase: PhaseRow) -> WorkflowData {
    let build = phase(
        "Build",
        RowStatus::Done,
        Some((1, 1)),
        vec![WorkflowRow::Agent(agent(
            1,
            "build:fix-money",
            HAIKU,
            RowStatus::Done,
            14_600,
            9,
        ))],
    );
    let review = phase(
        "Review",
        RowStatus::Done,
        Some((2, 2)),
        vec![
            answered(
                agent(2, "review:money", LUNA, RowStatus::Done, 51_800, 7),
                RowNote::new("sent it back · 1 blocking", Tone::Warn),
            ),
            WorkflowRow::Agent(noted(
                agent(3, "fix:review-findings", HAIKU, RowStatus::Done, 15_400, 13),
                "fixed the sign bug",
                Tone::Plain,
            )),
        ],
    );
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
        phases: vec![build, review, final_phase, waiting("Merge")],
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

fn later(n: u32) -> WorkflowId {
    argonauts_core::views::cli_workflow_run_id(&format!("wf_later_{n}"))
}

/// Claude Code's second workflow for the task is a row in Final, the last phase that ran,
/// open while it runs, and Merge waits for it.
#[test]
fn a_later_workflow_is_a_row_of_the_last_phase_that_ran() {
    let picture = picture!("nested_captain_second");
    let second = inside(
        &later(2),
        "final-signoff-money",
        RowStatus::Running,
        (1, 2),
        15,
        None,
        Some("claude-code"),
        vec![
            phase(
                "Review",
                RowStatus::Done,
                Some((1, 1)),
                vec![answered(
                    agent(6, "review:final-2", LUNA, RowStatus::Done, 17_100, 4),
                    RowNote::new("approved", Tone::Good),
                )],
            ),
            phase(
                "Judge",
                RowStatus::Running,
                Some((0, 1)),
                vec![WorkflowRow::Agent(agent(
                    7,
                    "sign-off:money-2",
                    FLASH,
                    RowStatus::Running,
                    9_000,
                    5,
                ))],
            ),
        ],
        Vec::new(),
    );
    let mut final_phase = phase(
        "Final",
        RowStatus::Running,
        Some((3, 4)),
        vec![
            answered(
                agent(4, "review:final", LUNA, RowStatus::Done, 16_800, 4),
                RowNote::new("approved", Tone::Good).then("1 note"),
            ),
            answered(
                agent(5, "sign-off:money", FLASH, RowStatus::Done, 17_700, 6),
                RowNote::new("sent it back", Tone::Warn),
            ),
            second,
        ],
    );
    final_phase.note =
        Some("Merge waits for final-signoff-money, then merges the change".to_owned());
    let data = money(
        (6, 7),
        100,
        "final: Gemini 3.8 Flash sent it back over \"1.234\" · Haiku 5.5 fixed it and asks again in final-signoff-money",
        final_phase,
    );
    let mut view = WorkflowView::new(data);
    picture.assert_drawn(19..36, &drawn(&mut view, 17), 0);
}

/// The same task's later workflows, each a row in Final: the two that ended fold to one line
/// each, and the fourth is open while it runs.
#[test]
fn the_later_workflows_fold_once_they_end() {
    let picture = picture!("list_after");
    let ended = |n: u32, name: &str, seconds: u64| {
        inside(
            &later(n),
            name,
            RowStatus::Done,
            (2, 2),
            seconds,
            None,
            Some("claude-code"),
            vec![phase("Review", RowStatus::Done, Some((2, 2)), Vec::new())],
            Vec::new(),
        )
    };
    let fourth = inside(
        &later(4),
        "final-signoff-money-4",
        RowStatus::Running,
        (0, 1),
        3,
        None,
        Some("claude-code"),
        vec![
            phase(
                "Review",
                RowStatus::Running,
                Some((0, 1)),
                vec![WorkflowRow::Agent(agent(
                    8,
                    "review:final-4",
                    LUNA,
                    RowStatus::Running,
                    0,
                    1,
                ))],
            ),
            waiting("Judge"),
        ],
        Vec::new(),
    );
    let final_phase = phase(
        "Final",
        RowStatus::Running,
        Some((6, 7)),
        vec![
            answered(
                agent(4, "review:final", LUNA, RowStatus::Done, 16_800, 4),
                RowNote::new("sent it back", Tone::Warn).then("1 finding"),
            ),
            answered(
                agent(5, "sign-off:money", LUNA, RowStatus::Done, 17_600, 5),
                RowNote::new("sent it back", Tone::Warn).then("1 finding"),
            ),
            ended(2, "final-signoff-money", 18),
            ended(3, "final-signoff-money-3", 16),
            fourth,
        ],
    );
    let data = money(
        (9, 10),
        159,
        "final: Codex has sent it back 7 times · Haiku 5.5 asks again in final-signoff-money-4",
        final_phase,
    );
    let mut view = WorkflowView::new(data);
    picture.assert_drawn(0..15, &drawn(&mut view, 16), 0);
}

/// What the app answers for an agent view: its row as the view had it, and what it did.
fn agent_view(
    row: AgentRow,
    prompt: &str,
    activity: &[(&str, &str, Option<bool>)],
    calls: u32,
) -> AgentData {
    AgentData {
        run: run(),
        row,
        prompt: prompt.to_owned(),
        activity: activity
            .iter()
            .map(|(tool, summary, ok)| ActivityLine {
                tool: (*tool).to_owned(),
                summary: (*summary).to_owned(),
                ok: *ok,
                note: None,
            })
            .collect(),
        tool_calls: calls,
        diff: None,
        subagents: None,
        outcome: None,
        goal: None,
        plan: None,
        facts: Vec::new(),
        to_fix: None,
        review: None,
        asked_for: None,
        earlier: None,
        moved: None,
        given: None,
        prompt_about: None,
        started_from: None,
    }
}

fn opened(view: &mut WorkflowView, data: AgentData) {
    assert!(view.accept(&ViewData::Agent {
        view: Box::new(data)
    }));
}

/// Enter on the worker opens its agent view, with one new section, Its workflow: the workflow's
/// count and tokens, and its phases on one line. Its left pane is the same outline.
#[test]
fn the_workers_agent_view_shows_its_workflow() {
    // Claude Code leaves a blank line between an open agent's sections.
    let picture = picture!("nested_worker_agent");
    let mut view = WorkflowView::new(due_dates(Moment::Working));
    press(&mut view, KeyCode::Enter);
    press(&mut view, KeyCode::Enter);
    let mut row = agent(1, "build:due-flag", SONNET, RowStatus::Running, 22_400, 160);
    row.context = Some(ContextUse {
        used: 28_000,
        window: Some(200_000),
        cap: Some(200_000),
    });
    let mut prompt =
        vec!["Add --due <date> to todo add, and show the date in todo list.".to_owned()];
    prompt.extend((1..=8).map(|line| format!("Keep the old list output, part {line}.")));
    let mut data = agent_view(
        row,
        &prompt.join("\n"),
        &[
            ("Read", "src/cli.js", Some(true)),
            ("Read", "test/cli.test.js", Some(true)),
            ("Workflow", "add-due-flag", None),
        ],
        11,
    );
    data.facts = vec!["worker".to_owned(), "worktree".to_owned()];
    opened(&mut view, data);
    picture.assert_drawn(0..20, &drawn(&mut view, 25), 5);
}

/// Enter on an agent inside: its first line says whose workflow it's in, its CLI reports a
/// shortened prompt and only its last call, and its keys only move, scroll and expand.
#[test]
fn an_inner_agents_view_says_whose_workflow_it_is_in() {
    // Claude Code leaves a blank line between an open agent's sections.
    let picture = picture!("nested_inner_agent");
    let mut view = WorkflowView::new(due_dates(Moment::Working));
    press(&mut view, KeyCode::Enter);
    for _ in 0..4 {
        press(&mut view, KeyCode::Down);
    }
    press(&mut view, KeyCode::Enter);
    let WorkflowRow::Agent(row) = inner(
        agent(1, "check:dates", HAIKU, RowStatus::Running, 4_200, 12),
        &add_due_flag_id(),
        3,
    ) else {
        unreachable!()
    };
    let data = agent_view(
        row,
        "Check that todo add --due refuses a bad date, and that todo list prints the date.",
        &[("Bash", "npm test -- --grep due", None)],
        3,
    );
    opened(&mut view, data);
    picture.assert_drawn(0..14, &drawn(&mut view, 19), 5);
}

/// The agent view of an agent two levels inside says in words how deep it is.
#[test]
fn the_deepest_agents_view_names_its_levels() {
    // Claude Code leaves a blank line between an open agent's sections.
    let picture = picture!("nested_deepest");
    let mut view = WorkflowView::new(due_dates(Moment::Deepest));
    press(&mut view, KeyCode::Enter);
    for _ in 0..7 {
        press(&mut view, KeyCode::Down);
    }
    press(&mut view, KeyCode::Enter);
    let WorkflowRow::Agent(row) = inner(
        agent(1, "case:bad-date", HAIKU, RowStatus::Running, 2_700, 9),
        &add_due_flag_id(),
        5,
    ) else {
        unreachable!()
    };
    let data = agent_view(
        row,
        "Try todo add with --due 2026-02-30 and --due soon; both must be refused.",
        &[("Bash", "node src/cli.js add x --due 2026-02-30", None)],
        2,
    );
    opened(&mut view, data);
    picture.assert_drawn(29..45, &drawn(&mut view, 21), 5);
}

/// Opened from its row in the bottom list, the run's view opens on Build with add-due-flag
/// open and the cursor on it.
#[test]
fn the_view_opens_on_a_workflow_inside_from_its_row() {
    let mut view = WorkflowView::new(due_dates(Moment::Ended)).on_inside(&add_due_flag_id());
    let buf = drawn(&mut view, 16);
    let text: Vec<String> = (0..16)
        .map(|y| (0..SCREEN).map(|x| buf[(x, y)].symbol()).collect())
        .collect();
    let row = text
        .iter()
        .find(|line| line.contains("add-due-flag   "))
        .expect("its row");
    assert!(row.contains("❯  ▾ add-due-flag"), "{text:#?}");
    assert!(
        text.iter().any(|line| line.contains("✔ Write")),
        "{text:#?}"
    );
}

const OPUS: &str = "claude-code/claude-opus-5-5";
const LUNA_56: &str = "codex/gpt-5.6-luna";

fn fallback_row(mut row: AgentRow, asked: &str, note: RowNote) -> AgentRow {
    row.fallback_for = Some(asked.parse().unwrap());
    row.note = Some(note);
    row
}

fn left_out(mut row: AgentRow, why: &str) -> AgentRow {
    row.left_out = true;
    row.status = RowStatus::Skipped;
    row.tokens = 0;
    row.elapsed_ms = 0;
    row.note = Some(RowNote::new(why, Tone::Plain));
    row
}

/// One row as the agents pane draws it, `width` cells after the three the picture indents it
/// by, its labels as wide as `label`, in a workflow that `own_cli` runs when one does.
fn one_row(row: &WorkflowRow, label: usize, width: usize, own_cli: Option<&str>) -> Buffer {
    use super::workflow_rows::{Around, Cells, RowColumns};
    let theme = dark();
    let state = state();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let mut data = due_dates(Moment::Working);
    data.phases[0].rows = vec![row.clone()];
    let around = Around {
        run: &data.run,
        own_cli,
        ticking: 0,
        ctx: &ctx,
    };
    let cells = Cells::of(row, None, &around);
    let mut line = RowColumns::fit(label, width, &theme).line(&cells, 0, false, &theme);
    line.spans.insert(0, ratatui::text::Span::raw("   "));
    super::workflow_pictures::buffer_of(&[line], u16::try_from(width + 3).unwrap())
}

/// Each row names the model that did the job: a fallback at work is marked as one, and
/// once it answers as its verdict says; a job left out is a grey dash, with no numbers.
#[test]
fn each_moved_row_reads_as_its_picture() {
    let picture = picture!("moved_rows");
    let warn = |words: &str| RowNote::new(words, Tone::Warn);
    let rows = [
        (
            3,
            fallback_row(
                agent(1, "review:due-tests", SONNET, RowStatus::Running, 9_300, 38),
                OPUS,
                warn("overloaded"),
            ),
            None,
        ),
        (
            6,
            fallback_row(
                agent(2, "review:due-flag", FLASH, RowStatus::Running, 9_100, 12),
                LUNA_56,
                warn("crashed"),
            ),
            None,
        ),
        (
            9,
            fallback_row(
                agent(2, "review:due-flag", FLASH, RowStatus::Done, 18_000, 41),
                LUNA_56,
                RowNote::new("approved", Tone::Good),
            ),
            None,
        ),
        (
            15,
            left_out(
                agent(3, "propose:p3", FLASH, RowStatus::Skipped, 0, 0),
                "agy is signed out",
            ),
            None,
        ),
    ];
    for (at, row, own) in rows {
        let drawn = one_row(&WorkflowRow::Agent(row), 16, 93, own);
        picture.assert_drawn(at..at + 1, &drawn, 0);
    }
    let mut handed = agent(4, "review:final", LUNA, RowStatus::Done, 16_800, 4);
    handed.answered = true;
    handed.note = Some(RowNote::new("approved", Tone::Good).then("1 note"));
    let drawn = one_row(&WorkflowRow::Agent(handed), 16, 93, Some("claude-code"));
    picture.assert_drawn(12..13, &drawn, 0);
}

fn reviewing() -> WorkflowData {
    let mut data = due_dates(Moment::Ended);
    data.agents_done = 3;
    data.agents_total = 4;
    data.elapsed_ms = 168_000;
    data.status_lines = vec![StatusLine::new(
        StatusKind::Note,
        "review: Codex crashed twice · Gemini 3.8 Flash on agy reviews build:due-flag in its place",
    )];
    data.phases[0] = phase(
        "Build",
        RowStatus::Done,
        Some((2, 2)),
        vec![
            WorkflowRow::Agent(agent(
                1,
                "build:due-flag",
                SONNET,
                RowStatus::Done,
                27_900,
                194,
            )),
            WorkflowRow::Agent(agent(
                2,
                "build:due-tests",
                GPT,
                RowStatus::Done,
                30_300,
                62,
            )),
        ],
    );
    data.phases[1] = phase(
        "Review",
        RowStatus::Running,
        Some((1, 2)),
        vec![
            WorkflowRow::Agent(noted(
                agent(3, "review:due-tests", OPUS, RowStatus::Done, 18_400, 41),
                "approved",
                Tone::Good,
            )),
            WorkflowRow::Agent(fallback_row(
                agent(4, "review:due-flag", FLASH, RowStatus::Running, 9_100, 12),
                LUNA_56,
                RowNote::new("crashed", Tone::Warn),
            )),
        ],
    );
    data
}

/// The fallback from another CLI in its box, in the orchestrator run.
#[test]
fn a_fallback_reads_as_its_picture_in_its_box() {
    let picture = picture!("moved_rows");
    let mut view = WorkflowView::new(reviewing());
    picture.assert_drawn(19..31, &drawn(&mut view, 12), 0);
}

/// Enter on the fallback: its own numbers first, then whose place it takes, which keeps the reason
/// and says why this model may review the work.
#[test]
fn a_fallbacks_view_says_whose_place_it_takes() {
    let picture = picture!("moved_agent");
    let mut view = WorkflowView::new(reviewing());
    press(&mut view, KeyCode::Enter);
    press(&mut view, KeyCode::Down);
    press(&mut view, KeyCode::Enter);
    let WorkflowRow::Agent(row) = reviewing().phases[1].rows[1].clone() else {
        unreachable!()
    };
    let mut prompt = vec![
        "Review build:due-flag's change: the --due flag on todo add and todo list.".to_owned(),
    ];
    prompt.extend((1..=5).map(|line| format!("Look at part {line} of the change.")));
    let mut data = agent_view(
        row,
        &prompt.join("\n"),
        &[
            ("Read", "src/cli.js", Some(true)),
            ("Read", "test/cli.test.js", Some(true)),
            ("Bash", "npm test -- --grep due", None),
        ],
        3,
    );
    data.facts = vec!["reviewer".to_owned(), "read-only".to_owned()];
    data.moved = Some(Moved::FallbackFor {
        asked: LUNA_56.parse().unwrap(),
        which: "crashed twice as it started".to_owned(),
        lines: vec![
            "Codex: \"Failed to synchronize managed preferences\"".to_owned(),
            "agy isn't build:due-flag's provider, so the review stays independent".to_owned(),
        ],
    });
    opened(&mut view, data);
    // In this run GPT 5.6 Luna is Codex's second model, which has its second colour.
    let state = ranked_with("gpt-5.6-luna");
    picture.assert_drawn(0..21, &drawn_in(&mut view, 26, &state), 5);
}

/// A council goes on without the job no model could take: grey, uncounted, and said once in
/// the pane's title. One label column holds for the whole workflow, every level of it, sized
/// to the labels its rows have.
#[test]
fn a_job_left_out_reads_as_its_picture() {
    let picture = picture!("council_after");
    let propose = phase(
        "Propose",
        RowStatus::Running,
        Some((1, 2)),
        vec![
            WorkflowRow::Agent(noted(
                agent(1, "propose:p1", HAIKU, RowStatus::Done, 14_200, 48),
                "proposal ready",
                Tone::Good,
            )),
            WorkflowRow::Agent(agent(2, "propose:p2", LUNA, RowStatus::Running, 11_000, 52)),
            WorkflowRow::Agent(left_out(
                agent(3, "propose:p3", FLASH, RowStatus::Skipped, 0, 0),
                "signed out",
            )),
        ],
    );
    let mut phases = vec![propose];
    for name in [
        "Challenge",
        "Decide",
        "Build",
        "Cross-review",
        "Sign-off",
        "Merge",
    ] {
        phases.push(waiting(name));
    }
    let data = WorkflowData {
        run: run(),
        id: WorkflowId::numbered(1),
        name: "Per key rate limit".to_owned(),
        description:
            "per-API-key token buckets, refill capped at capacity, a whole-second Retry-After"
                .to_owned(),
        style: None,
        status_lines: vec![StatusLine::new(
            StatusKind::Note,
            "propose:p3 is left out: agy is signed out and nothing else can take it · the council goes on with 2",
        )],
        agents_done: 1,
        agents_total: 2,
        elapsed_ms: 62_000,
        spent: Spend::default(),
        phases,
        notes: vec!["Council".to_owned()],
        required: Vec::new(),
        goals: Vec::new(),
        worker: None,
        state: WorkflowState::Running,
    };
    let mut view = WorkflowView::new(data);
    picture.assert_drawn(0..14, &drawn(&mut view, 15), 0);
}

/// Enter on the job left out: the model it was meant for and what it was, then why it was
/// left out and what that means for the run. Nothing ran, so there are no numbers and no
/// sections under it.
///
/// The picture shows the top of this view below the fallback's, four cells in, and leaves out
/// the blank row an agent view draws under its state, which the fallback's view above keeps.
#[test]
fn a_job_left_outs_view_reads_as_its_picture() {
    let picture = picture!("moved_agent");
    let job = left_out(
        agent(3, "propose:p3", FLASH, RowStatus::Skipped, 0, 0),
        "signed out",
    );
    let propose = phase(
        "Propose",
        RowStatus::Running,
        Some((1, 2)),
        vec![
            WorkflowRow::Agent(agent(1, "propose:p1", HAIKU, RowStatus::Done, 14_200, 48)),
            WorkflowRow::Agent(agent(2, "propose:p2", LUNA, RowStatus::Running, 11_000, 52)),
            WorkflowRow::Agent(job.clone()),
        ],
    );
    let data = WorkflowData {
        run: run(),
        id: WorkflowId::numbered(1),
        name: "Per key rate limit".to_owned(),
        description: String::new(),
        style: None,
        status_lines: Vec::new(),
        agents_done: 1,
        agents_total: 2,
        elapsed_ms: 62_000,
        spent: Spend::default(),
        phases: vec![propose],
        notes: vec!["Council".to_owned()],
        required: Vec::new(),
        goals: Vec::new(),
        worker: None,
        state: WorkflowState::Running,
    };
    let mut view = WorkflowView::new(data);
    press(&mut view, KeyCode::Enter);
    press(&mut view, KeyCode::Down);
    press(&mut view, KeyCode::Down);
    press(&mut view, KeyCode::Enter);
    let mut opened_view = agent_view(job, "", &[], 0);
    opened_view.facts = vec!["proposer".to_owned()];
    opened_view.moved = Some(Moved::LeftOut {
        because: "agy is signed out".to_owned(),
        lines: vec![
            "a council can do without one proposal, so it goes on with two".to_owned(),
            "sign in to agy and the next run asks it again".to_owned(),
        ],
    });
    opened(&mut view, opened_view);
    let buf = drawn(&mut view, 16);

    let text = |y: u16| -> String { (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect() };
    let (left, top) = (0..buf.area.height)
        .find_map(|y| {
            let row = text(y);
            let at = row.find("× Left out")?;
            Some((row[..at].chars().count() as u16, y))
        })
        .expect("the view's state line");
    let right = (left..buf.area.width)
        .rev()
        .find(|x| buf[(*x, top)].symbol() == "│")
        .expect("the pane's edge");
    let pane = |y: u16| -> String {
        (left..right)
            .map(|x| buf[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    };
    assert_eq!(pane(top + 1), "", "the blank row under the state");
    let bottom = (top..buf.area.height)
        .find(|y| text(*y).contains('╰'))
        .expect("the box's bottom");
    for y in top + 5..bottom {
        assert_eq!(pane(y), "", "nothing under why it was left out, row {y}");
    }

    let mut part = Buffer::empty(Rect::new(0, 0, SCREEN, 4));
    for (to, from) in [(0, top), (1, top + 2), (2, top + 3), (3, top + 4)] {
        for x in left..right {
            part[(4 + x - left, to)] = buf[(x, from)].clone();
        }
    }
    picture.assert_drawn(32..36, &part, 0);
}

/// At 80 and 100 columns a row that can't have everything gives up whole things: its stats
/// from the right, never part of `22.4k tok`, and a workflow inside lets go of whose it is
/// before its count of agents. What's left is cut only once nothing more can go.
#[test]
fn a_narrow_pane_drops_whole_stats_and_keeps_a_workflows_count() {
    for width in [80, 100] {
        let theme = dark();
        let state = state();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let mut view = WorkflowView::new(due_dates(Moment::Working));
        let area = Rect::new(0, 0, width, 16);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, &ctx);
        let rows: Vec<String> = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect();
        let nested = rows
            .iter()
            .find(|row| row.contains("▾ add-due-flag"))
            .unwrap_or_else(|| panic!("the workflow inside's row: {rows:#?}"));
        assert!(nested.contains(" 2/3 agents "), "{width}: {nested}");
        for row in &rows {
            // Tokens are whole wherever they show.
            assert!(
                !row.contains("k t") || row.contains("k tok"),
                "{width}: {row}"
            );
            assert!(
                !row.contains("k t…") && !row.contains("k to…"),
                "{width}: {row}"
            );
        }
    }
}
