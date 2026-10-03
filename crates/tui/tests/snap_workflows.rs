mod common;

use argonauts_core::app::{UiCommand, ViewData, ViewRequest};
use argonauts_core::events::session::{SubagentKind, SubagentStatus, SubagentUpdate};
use argonauts_core::handoff::QualityDebt;
use argonauts_core::names::{RoleName, SessionKey, WorkerRef, WorkflowId};
use argonauts_core::settings::ui::{UiSettings, VendorInternals};
use argonauts_core::views::{
    ActivityLine, AgentRow, AgentView as AgentData, ContextRow, ContextUse,
    ContextView as ContextData, NestedWorkflowRow, PhaseRow, RowStatus, SubagentsSection,
    WorkflowRow, WorkflowView as WorkflowData,
};
use argonauts_core::workflow::StandardPhase;
use argonauts_core::{CardId, DiffStat, RunId, Spend};
use argonauts_tui::bottom::{AgentPanel, WorkflowPanel, WorkflowRowState, panel_lines};
use argonauts_tui::bridge::{Cx, Effects, panel};
use argonauts_tui::model::ViewModel;
use argonauts_tui::render::{Clock, RenderCtx};
use argonauts_tui::stability::{WIDTHS, assert_frames, assert_lines, buffer_text, samples};
use argonauts_tui::state::{AppState, CardRow, Doing, RunState, SessionState, SessionStatus};
use argonauts_tui::theme::{Role, Theme};
use argonauts_tui::views::agent::AgentView;
use argonauts_tui::views::context::ContextView;
use argonauts_tui::views::workflow::WorkflowView;
use argonauts_tui::views::{View, ViewCtx, ViewOutcome};
use common::{t0, theme};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Style;
use time::OffsetDateTime;

const SIZES: [(u16, u16); 2] = [(100, 32), (80, 24)];

/// The views whose rows list agents in columns, at the widths those columns change at.
const ROW_SIZES: [(u16, u16); 4] = [(80, 24), (100, 32), (120, 32), (200, 40)];

fn run() -> RunId {
    RunId::new("run-01").unwrap()
}

fn key(s: &str) -> SessionKey {
    SessionKey::parse(s).unwrap()
}

fn worker(s: &str) -> WorkerRef {
    s.parse().unwrap()
}

fn usd(cost: f64) -> Spend {
    Spend {
        cost_usd: cost,
        ..Spend::default()
    }
}

fn agent(
    session: &str,
    label: &str,
    by: &str,
    status: RowStatus,
    tokens: u64,
    cost: f64,
    secs: u64,
) -> AgentRow {
    AgentRow {
        session: key(session),
        label: label.into(),
        worker: worker(by),
        status,
        tokens,
        spent: usd(cost),
        elapsed_ms: secs * 1000,
        subagents: 0,
        context: None,
        standing_in_for: None,
    }
}

fn phase(
    name: &str,
    standard: StandardPhase,
    status: RowStatus,
    done: u32,
    rows: Vec<WorkflowRow>,
) -> PhaseRow {
    PhaseRow {
        name: name.into(),
        standard,
        status,
        done,
        total: rows.len().max(1) as u32,
        rows,
    }
}

fn nested_row() -> NestedWorkflowRow {
    NestedWorkflowRow {
        id: WorkflowId::numbered(2),
        name: "build:c-demo-tests".into(),
        status: RowStatus::Running,
        agents_done: 2,
        agents_total: 4,
        workers: vec![worker("claude-code/opus"), worker("codex/gpt-5.6")],
        elapsed_ms: 720_000,
    }
}

/// A run's workflow a few hours in: two phases done, one building with a nested workflow and a
/// failure, and two waiting.
fn ui_kit() -> WorkflowData {
    WorkflowData {
        run: run(),
        id: WorkflowId::numbered(1),
        name: "argonauts-ui-kit".into(),
        description: "Build a standalone Rust UI kit prototype that matches Claude Code: inline \
                      chat, history cells, the bottom region, and the agent panel"
            .into(),
        breadcrumb: Vec::new(),
        agents_done: 8,
        agents_total: 12,
        elapsed_ms: 9_840_000,
        spent: usd(4.10),
        phases: vec![
            phase(
                "Scaffold",
                StandardPhase::Understand,
                RowStatus::Done,
                1,
                vec![WorkflowRow::Agent(agent(
                    "s-001",
                    "scaffold:crates",
                    "claude-code/opus",
                    RowStatus::Done,
                    212_400,
                    0.31,
                    845,
                ))],
            ),
            phase(
                "Build",
                StandardPhase::Do,
                RowStatus::Running,
                2,
                vec![
                    WorkflowRow::Agent(agent(
                        "s-002",
                        "build:a-terminal-bottom",
                        "claude-code/opus",
                        RowStatus::Done,
                        381_000,
                        0.84,
                        362,
                    )),
                    WorkflowRow::Agent(agent(
                        "s-003",
                        "build:b-history-cells",
                        "codex/gpt-5.6",
                        RowStatus::Done,
                        747_000,
                        1.12,
                        4_860,
                    )),
                    WorkflowRow::Workflow(nested_row()),
                    WorkflowRow::Agent(agent(
                        "s-005",
                        "build:d-plain-mode",
                        "agy/gemini-3.6",
                        RowStatus::Failed,
                        95_200,
                        0.0,
                        1_905,
                    )),
                ],
            ),
            phase(
                "Merge",
                StandardPhase::Check,
                RowStatus::Waiting,
                0,
                vec![WorkflowRow::Agent(agent(
                    "s-006",
                    "merge:owner",
                    "claude-code/opus",
                    RowStatus::Waiting,
                    0,
                    0.0,
                    0,
                ))],
            ),
            phase(
                "Look",
                StandardPhase::Check,
                RowStatus::Waiting,
                0,
                Vec::new(),
            ),
        ],
    }
}

/// The nested workflow behind the `◇` row, with its breadcrumb.
fn demo_tests() -> WorkflowData {
    WorkflowData {
        run: run(),
        id: WorkflowId::numbered(2),
        name: "build:c-demo-tests".into(),
        description: "Demo scenes and snapshot tests for every part of the kit".into(),
        breadcrumb: vec!["argonauts-ui-kit".into()],
        agents_done: 2,
        agents_total: 4,
        elapsed_ms: 720_000,
        spent: usd(0.92),
        phases: vec![
            phase(
                "Draft",
                StandardPhase::Plan,
                RowStatus::Done,
                2,
                vec![
                    WorkflowRow::Agent(agent(
                        "s-011",
                        "draft:scenes",
                        "claude-code/opus",
                        RowStatus::Done,
                        88_000,
                        0.22,
                        241,
                    )),
                    WorkflowRow::Agent(agent(
                        "s-012",
                        "draft:snapshots",
                        "codex/gpt-5.6",
                        RowStatus::Done,
                        120_300,
                        0.31,
                        302,
                    )),
                ],
            ),
            phase(
                "Test",
                StandardPhase::Check,
                RowStatus::Running,
                0,
                vec![
                    WorkflowRow::Agent(agent(
                        "s-013",
                        "test:scenes",
                        "claude-code/opus",
                        RowStatus::Running,
                        41_000,
                        0.12,
                        170,
                    )),
                    WorkflowRow::Agent(agent(
                        "s-014",
                        "test:snapshots",
                        "codex/gpt-5.6",
                        RowStatus::Running,
                        38_200,
                        0.09,
                        150,
                    )),
                ],
            ),
        ],
    }
}

fn prompt() -> String {
    let mut lines = vec![
        "Build the terminal and the whole bottom region of the UI kit.".to_owned(),
        "Read the design notes first, then the reference screenshots.".to_owned(),
    ];
    lines.extend(
        (1..=12).map(|i| format!("Step {i}: build one part, test it, and report what changed.")),
    );
    lines.join("\n")
}

fn activity() -> Vec<ActivityLine> {
    let call = |tool: &str, summary: &str, ok: Option<bool>| ActivityLine {
        tool: tool.into(),
        summary: summary.into(),
        ok,
    };
    vec![
        call("Read", "crates/tui/src/bottom/mod.rs", Some(true)),
        call(
            "Edit",
            "crates/tui/src/bottom/composer/layout.rs",
            Some(true),
        ),
        call("Bash", "cargo test -p argonauts-tui composer", Some(false)),
        call(
            "Bash",
            "cargo clippy -p argonauts-tui --all-targets -- -D warnings",
            Some(true),
        ),
        call("Bash", "cargo test -p argonauts-tui", None),
    ]
}

fn outcome() -> String {
    "**Report:** the terminal and the bottom region are done, with no stubs left.\n\n\
     ## What I built\n\
     - **Terminal:** history above the live region, redrawing only changed cells.\n\
     - **Bottom:** the working line, the composer, the mode line, and the panel.\n\
     - **Tests:** snapshots at 80 and 120 columns.\n\n\
     ## Open\n\
     - Nothing.\n"
        .to_owned()
}

fn subagents() -> SubagentsSection {
    let child =
        |id: &str, label: &str, status: SubagentStatus, message: Option<&str>| SubagentUpdate {
            id: id.into(),
            parent: None,
            kind: SubagentKind::Subagent,
            label: Some(label.into()),
            prompt: None,
            status,
            last_message: message.map(str::to_owned),
            log_uri: None,
            steps_visible: true,
        };
    SubagentsSection {
        title: "Subagents".into(),
        note: "structure not exposed".into(),
        children: vec![
            child(
                "toolu_01",
                "explorer",
                SubagentStatus::Done,
                Some("Found 14 call sites"),
            ),
            child("toolu_02", "planner", SubagentStatus::Running, None),
            child(
                "toolu_03",
                "tester",
                SubagentStatus::Failed,
                Some("2 tests failed"),
            ),
            child("toolu_04", "reviewer", SubagentStatus::Unknown, None),
        ],
    }
}

fn agent_data(row: AgentRow) -> AgentData {
    AgentData {
        run: run(),
        row,
        prompt: prompt(),
        activity: activity(),
        tool_calls: 195,
        diff: Some(DiffStat {
            files_changed: 4,
            insertions: 120,
            deletions: 18,
        }),
        subagents: Some(subagents()),
        outcome: Some(outcome()),
    }
}

fn row_of(data: &WorkflowData, session: &str) -> AgentRow {
    data.phases
        .iter()
        .flat_map(|phase| &phase.rows)
        .find_map(|row| match row {
            WorkflowRow::Agent(agent) if agent.session.as_str() == session => Some(agent.clone()),
            _ => None,
        })
        .unwrap()
}

fn ctx<'a>(theme: &'a Theme, state: &'a AppState) -> ViewCtx<'a> {
    ViewCtx {
        theme,
        state,
        now: t0(),
    }
}

fn press(view: &mut dyn View, code: KeyCode, state: &AppState) -> ViewOutcome {
    let theme = theme();
    view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx(&theme, state))
}

/// Draws the view through ratatui's test backend.
fn draw(view: &mut dyn View, width: u16, height: u16, state: &AppState) -> Buffer {
    let theme = theme();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            view.render(area, frame.buffer_mut(), &ctx(&theme, state));
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

/// The screen as text, trailing spaces trimmed, after the frame check.
fn screen(view: &mut dyn View, width: u16, height: u16, state: &AppState) -> String {
    let buf = draw(view, width, height, state);
    assert_frames(&buf);
    trimmed(&buf)
}

fn trimmed(buf: &Buffer) -> String {
    buffer_text(buf)
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The style of the cell where `needle` starts, on the first row that has it.
fn style_at(buf: &Buffer, needle: &str) -> Option<Style> {
    let area = buf.area;
    for y in area.top()..area.bottom() {
        let mut row = String::new();
        let mut starts = Vec::new();
        for x in area.left()..area.right() {
            let symbol = buf[(x, y)].symbol();
            if symbol.is_empty() {
                continue;
            }
            starts.push((row.len(), x));
            row.push_str(symbol);
        }
        if let Some(at) = row.find(needle) {
            let x = starts.iter().find(|(byte, _)| *byte == at)?.1;
            return Some(buf[(x, y)].style());
        }
    }
    None
}

fn fg(theme: &Theme, role: Role) -> Option<ratatui::style::Color> {
    theme.style(role).fg
}

/// Opens `session`'s row with Enter and hands the view its answer.
fn open_agent(view: &mut WorkflowView, data: &WorkflowData, session: &str, state: &AppState) {
    let phase =
        data.phases
            .iter()
            .position(|phase| {
                phase.rows.iter().any(
                    |row| matches!(row, WorkflowRow::Agent(a) if a.session.as_str() == session),
                )
            })
            .unwrap();
    // Up to the first phase, then down to this one, then over to its rows.
    for _ in 0..data.phases.len() {
        press(view, KeyCode::Up, state);
    }
    for _ in 0..phase {
        press(view, KeyCode::Down, state);
    }
    press(view, KeyCode::Right, state);
    let rows = &data.phases[phase].rows;
    let index = rows
        .iter()
        .position(|row| matches!(row, WorkflowRow::Agent(a) if a.session.as_str() == session))
        .unwrap();
    for _ in 0..index {
        press(view, KeyCode::Down, state);
    }
    let outcome = press(view, KeyCode::Enter, state);
    assert!(
        matches!(&outcome, ViewOutcome::Request(ViewRequest::Agent { session: s, .. }) if s.as_str() == session),
        "{outcome:?}"
    );
    let answer = ViewData::Agent {
        view: Box::new(agent_data(row_of(data, session))),
    };
    assert!(view.accept(&answer));
}

#[test]
fn workflow_view() {
    let state = AppState::default();
    for (width, height) in ROW_SIZES {
        let mut view = WorkflowView::new(ui_kit());
        insta::assert_snapshot!(
            format!("workflow_view_{width}"),
            screen(&mut view, width, height, &state)
        );
    }
    let mut view = WorkflowView::new(ui_kit());
    let text = screen(&mut view, 100, 32, &state);
    assert!(text.contains("8/12 agents · 2h44m · $4.10"), "{text}");
    assert!(text.contains("❯ 2 Build"), "{text}");
    assert!(text.contains("Build · 7 agents"), "{text}");
    assert!(text.contains(
        "↑↓ select · tab switch pane · → open · x stop · p pause · l library · esc back"
    ));
    assert!(
        text.lines().next().unwrap().ends_with("[ Run ]  Library"),
        "{text}"
    );

    // Tab moves to the rows, x asks, and the second x stops that agent.
    press(&mut view, KeyCode::Tab, &state);
    press(&mut view, KeyCode::Down, &state);
    assert!(matches!(
        press(&mut view, KeyCode::Char('x'), &state),
        ViewOutcome::Consumed
    ));
    assert!(
        screen(&mut view, 100, 32, &state)
            .contains("x again to stop build:b-history-cells · esc to keep")
    );
    let stop = press(&mut view, KeyCode::Char('x'), &state);
    assert!(
        matches!(&stop, ViewOutcome::Send(UiCommand::StopAgent { session, .. }) if session.as_str() == "s-003"),
        "{stop:?}"
    );
    // p pauses only the run on screen.
    assert!(matches!(
        press(&mut view, KeyCode::Char('p'), &state),
        ViewOutcome::Notice(_)
    ));
    let mut attached = AppState::default();
    attached.attached = Some(run());
    assert!(matches!(
        press(&mut view, KeyCode::Char('p'), &attached),
        ViewOutcome::Send(UiCommand::Pause { now: false })
    ));
}

/// The review's probe at 80 columns: one long open-weight model name used to drop the model
/// from every row. Now names and labels are cut with `…` first, so each row still says whose
/// model ran.
#[test]
fn long_model_names_stay_visible_at_80_columns() {
    let state = AppState::default();
    let long = "opencode/openrouter/qwen3-coder-480b-a35b-instruct-turbo";
    let data = |label: &str| WorkflowData {
        run: run(),
        id: WorkflowId::numbered(3),
        name: "solo-reviewed".into(),
        description: "One small change, reviewed by another family".into(),
        breadcrumb: Vec::new(),
        agents_done: 0,
        agents_total: 2,
        elapsed_ms: 90_000,
        spent: usd(0.12),
        phases: vec![phase(
            "Build",
            StandardPhase::Do,
            RowStatus::Running,
            0,
            vec![
                WorkflowRow::Agent(agent(
                    "s-021",
                    label,
                    long,
                    RowStatus::Running,
                    41_000,
                    0.08,
                    61,
                )),
                WorkflowRow::Agent(agent(
                    "s-022",
                    "reviewer · card-001",
                    "codex/gpt-5.6",
                    RowStatus::Waiting,
                    0,
                    0.0,
                    0,
                )),
            ],
        )],
    };
    for (name, label) in [
        ("long_model_80", "owner · card-001"),
        (
            "long_model_and_label_80",
            "build:card-017-rewrite-the-entire-slugify-module",
        ),
    ] {
        let mut view = WorkflowView::new(data(label));
        let text = screen(&mut view, 80, 12, &state);
        insta::assert_snapshot!(name, text);
        let row = |needle: &str| {
            text.lines()
                .find(|row| row.contains(needle))
                .unwrap_or_else(|| panic!("no {needle:?} row:\n{text}"))
                .to_owned()
        };
        assert!(row("reviewer").contains("GPT 5.6 on"), "{text}");
        assert!(row("1m01s").contains("Qwen3 Coder"), "{text}");
    }
}

#[test]
fn agent_view() {
    let state = AppState::default();
    for (width, height) in ROW_SIZES {
        let data = ui_kit();
        let mut view = WorkflowView::new(data.clone());
        open_agent(&mut view, &data, "s-002", &state);
        insta::assert_snapshot!(
            format!("agent_view_{width}"),
            screen(&mut view, width, height, &state)
        );
    }

    // ↑↓ switch agents in the same frame, asking for each one's answer.
    let data = ui_kit();
    let mut view = WorkflowView::new(data.clone());
    open_agent(&mut view, &data, "s-002", &state);
    let outcome = press(&mut view, KeyCode::Down, &state);
    assert!(
        matches!(&outcome, ViewOutcome::Request(ViewRequest::Agent { session, .. }) if session.as_str() == "s-003"),
        "{outcome:?}"
    );
    let text = screen(&mut view, 100, 32, &state);
    assert!(text.contains("Loading…"), "{text}");
    let answer = ViewData::Agent {
        view: Box::new(agent_data(row_of(&data, "s-003"))),
    };
    assert!(view.accept(&answer));
    let text = screen(&mut view, 100, 32, &state);
    assert!(text.contains("build:b-history-cells ─"), "{text}");
    assert!(
        text.contains("GPT 5.6 on Codex · 747k tok · $1.12 · 1h21m"),
        "{text}"
    );

    // s steers, t asks to take over the agent's card, and Esc goes back to the workflow.
    assert!(matches!(
        press(&mut view, KeyCode::Char('s'), &state),
        ViewOutcome::Consumed
    ));
    for c in "use the new layout".chars() {
        press(&mut view, KeyCode::Char(c), &state);
    }
    assert!(screen(&mut view, 100, 32, &state).contains("steer ❯ use the new layout"));
    let steer = press(&mut view, KeyCode::Enter, &state);
    assert!(
        matches!(&steer, ViewOutcome::Send(UiCommand::Steer { text, session, .. }) if text == "use the new layout" && session.as_str() == "s-003"),
        "{steer:?}"
    );
    assert!(matches!(
        press(&mut view, KeyCode::Char('t'), &state),
        ViewOutcome::Notice(_)
    ));
    assert!(matches!(
        press(&mut view, KeyCode::Esc, &state),
        ViewOutcome::Consumed
    ));
    assert!(view.agent().is_none());
    assert!(screen(&mut view, 100, 32, &state).contains("Phases"));
}

#[test]
fn agent_view_names_the_agent_and_its_run_and_tells_alike_steps_apart() {
    let state = AppState::default();
    let mut data = crew_run();
    for row in &mut data.phases[0].rows {
        if let WorkflowRow::Agent(agent) = row {
            agent.label = "restater".into();
        }
    }
    for width in [80, 100, 120, 200] {
        let mut view = WorkflowView::new(data.clone());
        open_agent(&mut view, &data, "s-101", &state);
        let text = screen(&mut view, width, 32, &state);
        insta::assert_snapshot!(format!("agent_view_identity_{width}"), &text);
        if width >= 120 {
            // Under the title, the agent's name with its id and the run, as a detail view has it.
            assert!(
                text.contains("Opus on Claude Code (claude-code/opus) · run-01"),
                "{text}"
            );
        }
        if width == 200 {
            // The agents' names sit in a column of their own, whole on a wide screen.
            assert!(text.contains("restater  Opus on Claude Code"), "{text}");
            assert!(text.contains("restater  GPT 5.6 on Codex"), "{text}");
        }
    }
}

#[test]
fn agent_view_sections() {
    let state = AppState::default();
    let mut view = AgentView::new(agent_data(row_of(&ui_kit(), "s-002")));
    let text = screen(&mut view, 100, 32, &state);
    insta::assert_snapshot!("agent_view_sections_collapsed", text);
    let order = [
        "Prompt · 14 lines",
        "Activity · last 3 of 195 tool calls",
        "Diff · 4 files · +120 -18",
        "Subagents · 4 · structure not exposed",
        // Counted as drawn at this width.
        "Outcome · 11 lines",
    ];
    let at: Vec<usize> = order
        .iter()
        .map(|title| {
            text.find(title)
                .unwrap_or_else(|| panic!("{title}\n{text}"))
        })
        .collect();
    assert!(at.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
    assert!(text.contains("…12 more lines"), "{text}");
    assert!(
        text.contains("● Bash(cargo test -p argonauts-tui composer)"),
        "{text}"
    );
    assert!(!text.contains("crates/tui/src/bottom/mod.rs"), "{text}");
    assert!(text.contains("Report: the terminal"), "{text}");
    assert!(text.contains("↵expand"), "{text}");

    // Enter expands the section at the top: the Prompt, all of it.
    press(&mut view, KeyCode::Enter, &state);
    let text = screen(&mut view, 100, 32, &state);
    assert!(text.contains("Prompt · 14 lines · ↵collapse"), "{text}");
    assert!(text.contains("Step 12: build one part"), "{text}");
    assert!(
        text.contains(" of ") && text.contains(" ↓"),
        "a position mark: {text}"
    );
    insta::assert_snapshot!("agent_view_sections_prompt_open", text);

    // Tab moves on to the Activity, whose Enter shows every call.
    press(&mut view, KeyCode::Enter, &state);
    press(&mut view, KeyCode::Tab, &state);
    press(&mut view, KeyCode::Enter, &state);
    let text = screen(&mut view, 100, 40, &state);
    assert!(
        text.contains("Activity · last 5 of 195 tool calls · ↵collapse"),
        "{text}"
    );
    assert!(
        text.contains("Read(crates/tui/src/bottom/mod.rs)"),
        "{text}"
    );

    // The colors: a failed call is red, a finished one green.
    let buf = draw(&mut view, 100, 40, &state);
    let theme = theme();
    let failed = style_at(&buf, "● Bash(cargo test -p argonauts-tui composer)");
    assert_eq!(failed.and_then(|s| s.fg), fg(&theme, Role::Error));
    let passed = style_at(&buf, "● Bash(cargo clippy");
    assert_eq!(passed.and_then(|s| s.fg), fg(&theme, Role::Success));

    // `ui.panel.vendor_internals` sets how much the Subagents section says.
    let mut state = AppState::default();
    let mut ui = UiSettings::default();
    ui.panel.vendor_internals = VendorInternals::Summary;
    state.ui = Some(ui.clone());
    let mut view = AgentView::new(agent_data(row_of(&ui_kit(), "s-002")));
    let text = screen(&mut view, 100, 40, &state);
    assert!(
        text.contains("Subagents · 4 · structure not exposed · 1 done, 1 running, 1 failed"),
        "{text}"
    );
    assert!(!text.contains("explorer"), "{text}");
    ui.panel.vendor_internals = VendorInternals::Hidden;
    state.ui = Some(ui);
    let text = screen(&mut view, 100, 40, &state);
    assert!(!text.contains("Subagents"), "{text}");
}

#[test]
fn workflow_view_nested_breadcrumb() {
    let state = AppState::default();
    for (width, height) in SIZES {
        let mut view = WorkflowView::new(demo_tests());
        insta::assert_snapshot!(
            format!("workflow_view_nested_breadcrumb_{width}"),
            screen(&mut view, width, height, &state)
        );
    }
}

#[test]
fn nested_workflow_breadcrumb() {
    let state = AppState::default();
    let mut view = WorkflowView::new(ui_kit());
    let text = screen(&mut view, 100, 32, &state);
    // The parent phase counts the nested workflow as one row.
    assert!(text.contains("❯ 2 Build    2/4"), "{text}");
    assert!(
        text.contains(
            "◇ build:c-demo-tests       workflow · 2/4 agents · Opus on Claude …  12m00s"
        ),
        "{text}"
    );

    press(&mut view, KeyCode::Right, &state);
    press(&mut view, KeyCode::Down, &state);
    press(&mut view, KeyCode::Down, &state);
    let outcome = press(&mut view, KeyCode::Enter, &state);
    assert!(
        matches!(&outcome, ViewOutcome::Request(ViewRequest::Workflow { workflow: Some(id), run: r }) if *id == WorkflowId::numbered(2) && *r == run()),
        "{outcome:?}"
    );
    assert!(view.accept(&ViewData::Workflow {
        view: Box::new(demo_tests())
    }));
    assert_eq!(view.depth(), 2);
    let text = screen(&mut view, 100, 32, &state);
    assert!(
        text.starts_with("argonauts-ui-kit › build:c-demo-tests"),
        "{text}"
    );
    assert!(text.contains("Demo scenes and snapshot tests"), "{text}");
    assert!(text.contains("2/4 agents · 12m0s · $0.92"), "{text}");
    let buf = draw(&mut view, 100, 32, &state);
    let theme = theme();
    assert_eq!(
        style_at(&buf, "argonauts-ui-kit ›").and_then(|s| s.fg),
        fg(&theme, Role::Dim)
    );
    assert_eq!(
        style_at(&buf, "build:c-demo-tests").and_then(|s| s.fg),
        fg(&theme, Role::Label)
    );

    // A refresh of a level already open updates it in place.
    let mut fresher = demo_tests();
    fresher.agents_done = 3;
    assert!(view.accept(&ViewData::Workflow {
        view: Box::new(fresher)
    }));
    assert_eq!(view.depth(), 2);
    assert!(screen(&mut view, 100, 32, &state).contains("3/4 agents"));

    // An agent opened from a nested level shows in the same frame.
    press(&mut view, KeyCode::Right, &state);
    let outcome = press(&mut view, KeyCode::Enter, &state);
    assert!(
        matches!(outcome, ViewOutcome::Request(ViewRequest::Agent { .. })),
        "{outcome:?}"
    );

    assert!(matches!(
        press(&mut view, KeyCode::Esc, &state),
        ViewOutcome::Consumed
    ));
    assert_eq!(view.depth(), 1);
    let top = screen(&mut view, 100, 32, &state);
    let name_row = top.lines().next().unwrap_or_default();
    assert!(name_row.starts_with("argonauts-ui-kit "), "{top}");
    assert!(!name_row.contains('›'), "{top}");
}

/// A peer crew's run: restating, drafting, owners on cards, and reviewers waiting.
fn crew_run() -> WorkflowData {
    let mut codex = agent(
        "s-105",
        "owner · card-002",
        "codex/gpt-5.6",
        RowStatus::Running,
        181_000,
        0.64,
        612,
    );
    codex.standing_in_for = Some(worker("claude-code/opus"));
    codex.subagents = 2;
    let mut claude = agent(
        "s-104",
        "owner · card-001",
        "claude-code/opus",
        RowStatus::Done,
        402_000,
        1.31,
        1_540,
    );
    claude.subagents = 3;
    let mut agy = agent(
        "s-106",
        "owner · card-003",
        "agy/gemini-3.6",
        RowStatus::Running,
        77_300,
        0.0,
        431,
    );
    agy.subagents = 1;
    WorkflowData {
        run: run(),
        id: WorkflowId::numbered(1),
        name: "peer-crew".into(),
        description: "Fix the race in the export test and add a regression test".into(),
        breadcrumb: Vec::new(),
        agents_done: 6,
        agents_total: 11,
        elapsed_ms: 2_760_000,
        spent: usd(3.02),
        phases: vec![
            phase(
                "Understand",
                StandardPhase::Understand,
                RowStatus::Done,
                2,
                vec![
                    WorkflowRow::Agent(agent(
                        "s-101",
                        "restater · claude-code",
                        "claude-code/opus",
                        RowStatus::Done,
                        31_000,
                        0.09,
                        58,
                    )),
                    WorkflowRow::Agent(agent(
                        "s-102",
                        "restater · codex",
                        "codex/gpt-5.6",
                        RowStatus::Done,
                        27_400,
                        0.05,
                        61,
                    )),
                ],
            ),
            phase(
                "Plan",
                StandardPhase::Plan,
                RowStatus::Done,
                1,
                vec![WorkflowRow::Agent(agent(
                    "s-103",
                    "drafter · plan",
                    "claude-code/opus",
                    RowStatus::Done,
                    64_000,
                    0.21,
                    190,
                ))],
            ),
            phase(
                "Do",
                StandardPhase::Do,
                RowStatus::Running,
                1,
                vec![
                    WorkflowRow::Agent(claude),
                    WorkflowRow::Agent(codex),
                    WorkflowRow::Agent(agy),
                ],
            ),
            phase(
                "Check",
                StandardPhase::Check,
                RowStatus::Waiting,
                0,
                vec![
                    WorkflowRow::Agent(agent(
                        "s-107",
                        "reviewer · card-001",
                        "codex/gpt-5.6",
                        RowStatus::Waiting,
                        0,
                        0.0,
                        0,
                    )),
                    WorkflowRow::Agent(agent(
                        "s-108",
                        "reviewer · card-002",
                        "claude-code/opus",
                        RowStatus::Waiting,
                        0,
                        0.0,
                        0,
                    )),
                ],
            ),
        ],
    }
}

#[test]
fn run_tree() {
    let state = AppState::default();
    for (width, height) in ROW_SIZES {
        let mut view = WorkflowView::new(crew_run());
        insta::assert_snapshot!(
            format!("run_tree_{width}"),
            screen(&mut view, width, height, &state)
        );
    }
}

#[test]
fn run_tree_subagents_and_threads() {
    let state = AppState::default();
    let data = crew_run();
    let mut view = WorkflowView::new(data.clone());
    let text = screen(&mut view, 140, 32, &state);
    assert!(text.contains("+3 subagents"), "{text}");
    assert!(text.contains("+2 subagents"), "{text}");
    assert!(text.contains("+1 subagent "), "{text}");
    assert!(text.contains("↺ for Opus on Claude Code"), "{text}");

    let child = |id: &str,
                 kind: SubagentKind,
                 label: Option<&str>,
                 status: SubagentStatus,
                 visible: bool| SubagentUpdate {
        id: id.into(),
        parent: None,
        kind,
        label: label.map(str::to_owned),
        prompt: None,
        status,
        last_message: None,
        log_uri: None,
        steps_visible: visible,
    };
    let sections = [
        (
            "s-104",
            SubagentsSection {
                title: "Subagents".into(),
                note: "structure not exposed".into(),
                children: vec![
                    child(
                        "toolu_01",
                        SubagentKind::Subagent,
                        Some("explorer"),
                        SubagentStatus::Done,
                        true,
                    ),
                    child(
                        "toolu_02",
                        SubagentKind::Subagent,
                        Some("planner"),
                        SubagentStatus::Done,
                        true,
                    ),
                    child(
                        "toolu_03",
                        SubagentKind::Subagent,
                        Some("tester"),
                        SubagentStatus::Done,
                        true,
                    ),
                ],
            },
            "Subagents · 3 · structure not exposed",
        ),
        (
            "s-105",
            SubagentsSection {
                title: "Threads".into(),
                note: "steps not streamed".into(),
                children: vec![
                    child(
                        "thr_1",
                        SubagentKind::Thread,
                        None,
                        SubagentStatus::Running,
                        false,
                    ),
                    child(
                        "thr_2",
                        SubagentKind::Thread,
                        None,
                        SubagentStatus::Done,
                        false,
                    ),
                ],
            },
            "Threads · 2 · steps not streamed",
        ),
        (
            "s-106",
            SubagentsSection {
                title: "Subagents".into(),
                note: "structure not exposed".into(),
                children: vec![child(
                    "conv-7",
                    SubagentKind::Subagent,
                    Some("research"),
                    SubagentStatus::Running,
                    true,
                )],
            },
            "Subagents · 1 · structure not exposed",
        ),
    ];
    for (session, section, title) in sections {
        let mut agent = agent_data(row_of(&data, session));
        agent.subagents = Some(section);
        let mut view = AgentView::new(agent);
        // Past the prompt and the activity, to the section.
        let text = screen(&mut view, 100, 40, &state);
        assert!(text.contains(title), "{text}");
        insta::assert_snapshot!(format!("run_tree_subagents_{session}"), text);
    }
}

fn session(
    key_: &str,
    by: &str,
    role: RoleName,
    card: Option<CardId>,
    status: SessionStatus,
    started: OffsetDateTime,
) -> SessionState {
    let worker = worker(by);
    SessionState {
        key: key(key_),
        follow: None,
        vendor: worker.worker.clone(),
        worker,
        model_assumed: false,
        role,
        step: None,
        card,
        workflow_step: None,
        status,
        started,
        ended: None,
        last_event: started,
        tokens_in: 90_000,
        tokens_out: 18_100,
        cost_usd: None,
        context: None,
        context_window: None,
        rotations: 0,
        todos: Vec::new(),
        tools: Vec::new(),
        messages: Vec::new(),
        streaming: String::new(),
        diffs: Vec::new(),
        subagents: Vec::new(),
        inputs: Vec::new(),
        queued: Vec::new(),
        retry: None,
        standing_in_for: None,
        end: None,
        unknown: Vec::new(),
        doing: Doing::Nothing,
        stalled: false,
        vendor_session: None,
    }
}

fn card(n: u32) -> CardRow {
    CardRow {
        id: CardId::numbered(n),
        title: format!("card {n}"),
        state: None,
        owner: None,
        attempt: 1,
        diff: None,
        standing_in_for: None,
        debt: Vec::new(),
        checkpoints: 0,
        rotations: 0,
        last_gate: None,
        reviews: 0,
        waiting_on_vendor: None,
    }
}

/// A run with a Claude Code owner near its context cap and a Codex replacement whose card still
/// owes a review.
fn crew_state(wall: OffsetDateTime) -> AppState {
    let started = wall - time::Duration::seconds(252);
    let mut run_state = RunState::new(run());
    run_state.title = "fix the export race".into();
    run_state.front_end = Some("desktop-client".into());
    run_state.started = Some(started);
    let mut claude = session(
        "s-104",
        "claude-code/opus",
        RoleName::Owner,
        Some(CardId::numbered(1)),
        SessionStatus::Working,
        started,
    );
    claude.context = Some(ContextUse {
        used: 120_000,
        cap: 200_000,
    });
    claude.subagents = vec![
        SubagentUpdate {
            id: "toolu_01".into(),
            parent: None,
            kind: SubagentKind::Subagent,
            label: Some("explorer".into()),
            prompt: None,
            status: SubagentStatus::Running,
            last_message: None,
            log_uri: None,
            steps_visible: true,
        };
        3
    ];
    let mut codex = session(
        "s-105",
        "codex/gpt-5.6",
        RoleName::Owner,
        Some(CardId::numbered(2)),
        SessionStatus::Working,
        started,
    );
    codex.standing_in_for = Some(worker("claude-code"));
    let done = session(
        "s-103",
        "claude-code/opus",
        RoleName::Drafter,
        None,
        SessionStatus::Done,
        started,
    );
    let mut debt_card = card(2);
    debt_card.debt = vec![QualityDebt {
        since: 1,
        by: worker("codex/gpt-5.6"),
        needs: worker("claude-code/opus"),
    }];
    for session in [done, claude, codex] {
        run_state.session_order.push(session.key.clone());
        run_state.sessions.insert(session.key.clone(), session);
    }
    run_state.cards.insert(CardId::numbered(1), card(1));
    run_state.cards.insert(CardId::numbered(2), debt_card);
    let mut state = AppState::default();
    state.runs.insert(run(), run_state);
    state
}

/// Runs `panel::sync` the way the bridge does after every event.
fn synced(state: &AppState, agents: bool) -> ViewModel {
    let mut model = ViewModel::default();
    let mut settings = UiSettings::default();
    settings.panel.agents = agents;
    let mut out = Effects::default();
    let mut cx = Cx {
        model: &mut model,
        state,
        settings: &settings,
        now: t0(),
        out: &mut out,
    };
    panel::sync(&mut cx);
    model
}

fn panel_text(workflows: &WorkflowPanel, agents: &AgentPanel, width: u16) -> String {
    let theme = theme();
    let ctx = RenderCtx::new(width, &theme, Clock::still(t0()));
    let lines = panel_lines(workflows, agents, &ctx);
    assert_lines(&lines, width);
    common::text(&lines)
}

#[test]
fn agent_panel_row_ctx() {
    let state = crew_state(OffsetDateTime::now_utc());
    let model = synced(&state, true);
    let bottom = &model.bottom;
    assert_eq!(bottom.workflows.rows.len(), 1);
    // The finished drafter isn't listed; the two owners are, under their run.
    assert_eq!(bottom.agents.rows.len(), 2);
    let text = panel_text(&bottom.workflows, &bottom.agents, 160);
    let rows: Vec<&str> = text.lines().collect();
    assert!(rows[0].starts_with("  ○ fix the export race"), "{text}");
    // Under its run, an agent's row is its circle, name, card, bar, and stats, with no task.
    assert!(
        rows[1].starts_with("    ◐ Opus on Claude Code        card-001  "),
        "{text}"
    );
    assert!(rows[1].contains("ctx 120k/200k"), "{text}");
    assert!(rows[1].contains("+3 subagents"), "{text}");

    // Near the cap, the ctx turns to the warning color; below it, it stays dim.
    let theme = theme();
    let ctx = RenderCtx::new(160, &theme, Clock::still(t0()));
    let lines = panel_lines(&bottom.workflows, &bottom.agents, &ctx);
    let ctx_style = |line: &ratatui::text::Line<'_>| {
        line.spans
            .iter()
            .find(|span| span.content.starts_with("ctx "))
            .map(|span| span.style)
    };
    assert_eq!(ctx_style(&lines[1]), Some(theme.style(Role::Dim)));
    let mut near = state.clone();
    if let Some(run) = near.runs.get_mut(&run())
        && let Some(claude) = run.sessions.get_mut(&key("s-104"))
    {
        claude.context = Some(ContextUse {
            used: 190_000,
            cap: 200_000,
        });
    }
    let model = synced(&near, true);
    let lines = panel_lines(&model.bottom.workflows, &model.bottom.agents, &ctx);
    assert_eq!(ctx_style(&lines[1]), Some(theme.style(Role::Warning)));

    // Without `ui.panel.agents`, a run is one row.
    let model = synced(&state, false);
    assert!(model.bottom.agents.rows.is_empty());
    assert_eq!(
        panel_text(&model.bottom.workflows, &model.bottom.agents, 160)
            .lines()
            .count(),
        1
    );
}

/// The agents under a run line up in columns at every width: steps, tokens, and spend
/// right-aligned, the markers after them, and the times ending at the same cell.
#[test]
fn agent_panel_columns() {
    let mut state = crew_state(OffsetDateTime::now_utc());
    if let Some(run) = state.runs.get_mut(&run()) {
        if let Some(claude) = run.sessions.get_mut(&key("s-104")) {
            claude.cost_usd = Some(0.84);
        }
        if let Some(codex) = run.sessions.get_mut(&key("s-105")) {
            codex.tokens_out = 2_400;
        }
    }
    let model = synced(&state, true);
    let bottom = &model.bottom;
    for width in [80u16, 100, 120, 200] {
        let text = panel_text(&bottom.workflows, &bottom.agents, width);
        insta::assert_snapshot!(format!("agent_panel_columns_{width}"), text);
        let rows: Vec<&str> = text.lines().skip(1).collect();
        assert_eq!(rows.len(), 2, "{text}");
        let cells = |row: &str| row.trim_end().chars().count();
        // The times end one cell short of the edge, as the mode line's parts do.
        assert!(
            rows.iter().all(|row| cells(row) == usize::from(width) - 1),
            "{text}"
        );
        let ends_at = |row: &str, needle: &str| {
            row.find(needle)
                .map(|at| row[..at].chars().count() + needle.chars().count())
        };
        if let (Some(long), Some(short)) = (
            ends_at(rows[0], "18.1k tokens"),
            ends_at(rows[1], "2.4k tokens"),
        ) {
            assert_eq!(long, short, "tokens right-aligned:\n{text}");
        }
    }
    let wide = panel_text(&bottom.workflows, &bottom.agents, 200);
    assert!(wide.contains("$0.84"), "{wide}");
}

/// The review's probe: under a run at 80 columns, names too long for their model drop it, and
/// the card stays beside the harness instead of past the cells the models would have filled.
#[test]
fn harness_names_keep_their_card_beside_them() {
    let mut state = crew_state(OffsetDateTime::now_utc());
    if let Some(run) = state.runs.get_mut(&run())
        && let Some(owner) = run.sessions.get_mut(&key("s-104"))
    {
        owner.worker = worker("opencode/openrouter/qwen3-coder-480b-a35b-instruct-turbo");
        owner.vendor = owner.worker.worker.clone();
    }
    let model = synced(&state, true);
    let text = panel_text(&model.bottom.workflows, &model.bottom.agents, 80);
    insta::assert_snapshot!("agent_panel_harness_names_80", text);
    // The cards line up two cells after the widest harness, OpenCode.
    let card_at = |row: &str| {
        row.find("card-")
            .map(|at| row[..at].chars().count())
            .unwrap_or_default()
    };
    let rows: Vec<&str> = text.lines().skip(1).collect();
    assert!(rows[0].contains("OpenCode  card-001"), "{text}");
    assert!(
        rows.iter().all(|row| card_at(row) == card_at(rows[0])),
        "{text}"
    );
}

#[test]
fn debt_markers_on_rows() {
    let state = crew_state(OffsetDateTime::now_utc());
    let model = synced(&state, true);
    let text = panel_text(&model.bottom.workflows, &model.bottom.agents, 160);
    let codex = text
        .lines()
        .find(|row| row.contains("GPT 5.6 on Codex"))
        .unwrap();
    assert!(codex.contains("↺ for Claude Code"), "{text}");
    assert!(codex.contains("◌ needs review"), "{text}");
    let claude = text
        .lines()
        .find(|row| row.contains("Opus on Claude Code"))
        .unwrap();
    assert!(!claude.contains("needs review"), "{text}");

    // On a narrow row the markers outlast the stats.
    let narrow = panel_text(&model.bottom.workflows, &model.bottom.agents, 72);
    let codex = narrow.lines().find(|row| row.contains("Codex")).unwrap();
    assert!(codex.contains("◌ needs review"), "{narrow}");
    assert!(!codex.contains("tokens"), "{narrow}");

    // The workflow view marks the same agent's row from the folded state.
    let mut view = WorkflowView::new(crew_run());
    let buf = draw(&mut view, 150, 32, &state);
    assert_frames(&buf);
    let text = trimmed(&buf);
    let row = text
        .lines()
        .find(|row| row.contains("owner · card-002"))
        .unwrap();
    assert!(row.contains("↺ for Opus on Claude Code"), "{text}");
    assert!(row.contains("◌ needs review"), "{text}");
    let theme = theme();
    assert_eq!(
        style_at(&buf, "◌ needs review").and_then(|s| s.fg),
        fg(&theme, Role::Warning)
    );
}

#[test]
fn run_row_shows_front_end() {
    let wall = OffsetDateTime::now_utc();
    let state = crew_state(wall);
    let model = synced(&state, false);
    let row = &model.bottom.workflows.rows[0];
    assert_eq!(row.front_end.as_deref(), Some("via desktop-client"));
    assert_eq!(row.key, "run-01");
    assert_eq!((row.agents_done, row.agents_total), (1, 3));

    // With a fixed clock the whole row reads the same every time.
    let rows = panel::run_rows(&state, wall, t0());
    let panel = WorkflowPanel {
        rows,
        max_rows: 5,
        ..WorkflowPanel::default()
    };
    let text = panel_text(&panel, &AgentPanel::default(), 120);
    insta::assert_snapshot!("run_row_shows_front_end", text);
    assert!(
        text.contains("1/3 agents · 4m12s · ↓ 54.3k tokens · via desktop-client"),
        "{text}"
    );
    // At 60 columns the bar goes before the front end does.
    let narrow = panel_text(&panel, &AgentPanel::default(), 60);
    assert!(narrow.contains("via desktop-client"), "{narrow}");
}

#[test]
fn state_glyph_word_color() {
    let theme = theme();
    let state = AppState::default();
    let data = ui_kit();
    for (session, words, role) in [
        ("s-002", "✓ Completed", Role::Success),
        ("s-005", "✗ Failed", Role::Error),
        ("s-006", "○ Waiting", Role::Dim),
    ] {
        let mut view = AgentView::new(agent_data(row_of(&data, session)));
        let buf = draw(&mut view, 100, 32, &state);
        let text = trimmed(&buf);
        assert!(text.contains(words), "{session}: {text}");
        assert_eq!(
            style_at(&buf, words).and_then(|s| s.fg),
            fg(&theme, role),
            "{session}"
        );
    }
    let mut running = row_of(&data, "s-002");
    running.status = RowStatus::Running;
    let mut view = AgentView::new(agent_data(running));
    let buf = draw(&mut view, 100, 32, &state);
    assert_eq!(
        style_at(&buf, "● Running").and_then(|s| s.fg),
        fg(&theme, Role::Dim)
    );

    // The workflow view's rows: green ✓, grey ●, red ✗, and the nested ◇.
    let mut view = WorkflowView::new(data);
    let buf = draw(&mut view, 100, 32, &state);
    assert_eq!(
        style_at(&buf, "✓ build:a").and_then(|s| s.fg),
        fg(&theme, Role::Success)
    );
    assert_eq!(
        style_at(&buf, "✗ build:d").and_then(|s| s.fg),
        fg(&theme, Role::Error)
    );
    assert!(style_at(&buf, "◇ build:c").is_some());

    // Run rows say their state in words too, in the state's color.
    let mut panel = WorkflowPanel {
        max_rows: 5,
        ..WorkflowPanel::default()
    };
    for (i, state) in [
        WorkflowRowState::NeedsYou,
        WorkflowRowState::Done,
        WorkflowRowState::Failed,
        WorkflowRowState::Paused,
    ]
    .into_iter()
    .enumerate()
    {
        let mut row = argonauts_tui::bottom::WorkflowRow::new(
            format!("run-{i}"),
            format!("workflow-{i}"),
            state,
        );
        row.agents_done = 1;
        row.agents_total = 2;
        panel.rows.push(row);
    }
    let ctx = RenderCtx::new(120, &theme, Clock::still(t0()));
    let lines = panel_lines(&panel, &AgentPanel::default(), &ctx);
    let text = common::text(&lines);
    insta::assert_snapshot!("state_glyph_word_color_rows", text);
    let last_style = |i: usize| {
        lines[i]
            .spans
            .last()
            .map(|span| (span.content.to_string(), span.style))
    };
    assert_eq!(
        last_style(0),
        Some(("◆ needs you".into(), theme.style(Role::Card)))
    );
    assert_eq!(
        last_style(1),
        Some(("done".into(), theme.style(Role::Success)))
    );
    assert_eq!(
        last_style(2),
        Some(("failed".into(), theme.style(Role::Error)))
    );
    assert_eq!(
        last_style(3),
        Some(("paused".into(), theme.style(Role::Dim)))
    );
    assert_eq!(lines[0].spans[1].content, "◆");
    assert_eq!(lines[1].spans[1].content, "✓");
    assert_eq!(lines[2].spans[1].content, "✗");
    assert_eq!(lines[3].spans[1].content, "■");
}

fn context_data() -> ContextData {
    let row = |run_: Option<RunId>,
               session: &str,
               label: &str,
               by: &str,
               used: u64,
               window: Option<u64>,
               cap: Option<u64>,
               calls: u32,
               cost: f64,
               rotations: u32| ContextRow {
        run: run_,
        session: key(session),
        label: label.into(),
        worker: worker(by),
        used,
        window,
        cap,
        tool_calls: calls,
        spent: usd(cost),
        rotations,
    };
    ContextData {
        rows: vec![
            row(
                None,
                "s-001",
                "lead",
                "claude-code/opus",
                48_200,
                Some(1_000_000),
                Some(200_000),
                12,
                0.21,
                0,
            ),
            row(
                Some(run()),
                "s-105",
                "owner · card-002",
                "codex/gpt-5.6",
                184_000,
                Some(400_000),
                Some(200_000),
                95,
                1.12,
                1,
            ),
            row(
                Some(run()),
                "s-106",
                "reviewer · card-001",
                "agy/gemini-3.6",
                20_400,
                Some(1_000_000),
                None,
                7,
                0.0,
                0,
            ),
        ],
    }
}

#[test]
fn context_view() {
    let state = AppState::default();
    for (width, height) in SIZES {
        let mut view = ContextView::new(context_data());
        insta::assert_snapshot!(
            format!("context_view_{width}"),
            screen(&mut view, width, height, &state)
        );
    }
    let mut view = ContextView::new(context_data());
    let buf = draw(&mut view, 100, 32, &state);
    let theme = theme();
    assert_eq!(
        style_at(&buf, "184k/200k").and_then(|s| s.fg),
        fg(&theme, Role::Warning)
    );
    assert_ne!(
        style_at(&buf, "48.2k/200k").and_then(|s| s.fg),
        fg(&theme, Role::Warning)
    );

    // Enter opens the agent's view; the lead's chat has none.
    assert!(matches!(
        press(&mut view, KeyCode::Enter, &state),
        ViewOutcome::Notice(_)
    ));
    press(&mut view, KeyCode::Down, &state);
    let outcome = press(&mut view, KeyCode::Enter, &state);
    assert!(
        matches!(&outcome, ViewOutcome::Request(ViewRequest::Agent { session, .. }) if session.as_str() == "s-105"),
        "{outcome:?}"
    );
    assert!(matches!(
        press(&mut view, KeyCode::Esc, &state),
        ViewOutcome::Close
    ));
}

/// Every text field set to one sample, so no view can dodge it.
fn with_sample(sample: &str) -> (WorkflowData, AgentData, ContextData) {
    let mut data = ui_kit();
    data.name = sample.into();
    data.description = sample.into();
    for phase in &mut data.phases {
        phase.name = format!("{sample}{}", phase.name);
        for row in &mut phase.rows {
            match row {
                WorkflowRow::Agent(agent) => agent.label = format!("{}{sample}", agent.label),
                WorkflowRow::Workflow(nested) => nested.name = sample.into(),
            }
        }
    }
    let mut agent = agent_data(row_of(&ui_kit(), "s-002"));
    agent.row.label = sample.into();
    agent.prompt = format!("{sample}\n{sample}\n{sample}");
    for call in &mut agent.activity {
        call.summary = sample.into();
    }
    agent.outcome = Some(format!("{sample}\n\n- {sample}"));
    let mut context = context_data();
    for row in &mut context.rows {
        row.label = sample.into();
    }
    (data, agent, context)
}

#[test]
fn workflow_frames_at_every_width() {
    let state = crew_state(OffsetDateTime::now_utc());
    let long_word = samples::long_word();
    let texts = [
        samples::CJK,
        samples::EMOJI,
        samples::LONG_PATH,
        samples::EMPTY,
        long_word.as_str(),
    ];
    for width in WIDTHS {
        for height in [12, 24] {
            for sample in texts {
                let (data, agent, context) = with_sample(sample);
                let mut view = WorkflowView::new(data.clone());
                assert_frames(&draw(&mut view, width, height, &state));
                press(&mut view, KeyCode::Tab, &state);
                press(&mut view, KeyCode::Down, &state);
                assert_frames(&draw(&mut view, width, height, &state));

                let mut nested = WorkflowView::new(demo_tests());
                assert_frames(&draw(&mut nested, width, height, &state));

                let rows = vec![WorkflowRow::Agent(agent.row.clone())];
                let phase = phase(sample, StandardPhase::Do, RowStatus::Running, 0, rows);
                let mut view = AgentView::in_phase(agent.clone(), &phase);
                assert_frames(&draw(&mut view, width, height, &state));
                press(&mut view, KeyCode::Enter, &state);
                press(&mut view, KeyCode::Char('G'), &state);
                assert_frames(&draw(&mut view, width, height, &state));

                let mut view = ContextView::new(context);
                assert_frames(&draw(&mut view, width, height, &state));
            }
        }
        let model = synced(&state, true);
        panel_text(&model.bottom.workflows, &model.bottom.agents, width);
    }
    // Columns hold while the selection moves between phases.
    let mut view = WorkflowView::new(crew_run());
    let before = screen(&mut view, 100, 24, &state);
    press(&mut view, KeyCode::Up, &state);
    let after = screen(&mut view, 100, 24, &state);
    let column = |text: &str, needle: &str| text.lines().find_map(|row| row.find(needle));
    assert_eq!(column(&before, "Phases"), column(&after, "Phases"));
}

fn library_files() -> Vec<argonauts_core::views::WorkflowFile> {
    use argonauts_core::views::{WorkflowFile, WorkflowOrigin};
    let file = |name: &str, origin, source: &str, edited, in_use| WorkflowFile {
        name: name.to_owned(),
        origin,
        source: source.to_owned(),
        description: format!("How the {name} workflow runs"),
        text: format!("name: {name}\nphases: []\n"),
        edited,
        in_use,
    };
    vec![
        file("solo", WorkflowOrigin::BuiltIn, "built in", None, false),
        file(
            "coordinated-crew",
            WorkflowOrigin::Yours,
            "~/.argonauts/workflows/coordinated-crew.yaml",
            Some(true),
            true,
        ),
        file(
            "release",
            WorkflowOrigin::Repo,
            ".argonauts/workflows/release.yaml",
            None,
            false,
        ),
    ]
}

#[test]
fn library_tab() {
    let state = AppState::default();
    let mut view = WorkflowView::new(ui_kit());
    // The tab asks for the files as it opens.
    let outcome = press(&mut view, KeyCode::Char('l'), &state);
    assert!(
        matches!(outcome, ViewOutcome::Request(ViewRequest::WorkflowLibrary)),
        "{outcome:?}"
    );
    let loading = screen(&mut view, 100, 32, &state);
    assert!(loading.contains("Looking for workflow files"), "{loading}");
    assert!(view.accept(&ViewData::WorkflowLibrary {
        workflows: library_files(),
        refresh: false,
    }));
    for (width, height) in SIZES {
        insta::assert_snapshot!(
            format!("library_tab_{width}"),
            screen(&mut view, width, height, &state)
        );
    }
    let text = screen(&mut view, 100, 32, &state);
    assert!(
        text.lines().next().unwrap().ends_with("  Run  [ Library ]"),
        "{text}"
    );
    assert!(text.contains("❯ coordinated-crew"), "{text}");

    let outcome = press(&mut view, KeyCode::Char('c'), &state);
    assert!(
        matches!(
            &outcome,
            ViewOutcome::Send(UiCommand::WorkflowFile { name, action })
                if name == "coordinated-crew"
                    && *action == argonauts_core::input::WorkflowAction::Copy
        ),
        "{outcome:?}"
    );
    // Back on the Run tab; the next visit asks again, since a file may have changed, and
    // shows the last list until the answer comes.
    assert!(matches!(
        press(&mut view, KeyCode::Char('l'), &state),
        ViewOutcome::Consumed
    ));
    assert!(screen(&mut view, 100, 32, &state).contains("Build · 7 agents"));
    assert!(matches!(
        press(&mut view, KeyCode::Char('l'), &state),
        ViewOutcome::Request(ViewRequest::WorkflowLibrary)
    ));
    assert!(screen(&mut view, 100, 32, &state).contains("coordinated-crew"));
}

#[test]
fn library_alone_with_no_run() {
    use argonauts_tui::views::library::LibraryView;
    let state = AppState::default();
    let mut view = LibraryView::new(library_files());
    for (width, height) in SIZES {
        let text = screen(&mut view, width, height, &state);
        assert!(text.contains("Workflow library"), "{text}");
        assert!(text.contains("release"), "{text}");
    }
    press(&mut view, KeyCode::Up, &state);
    press(&mut view, KeyCode::Char('d'), &state);
    let text = screen(&mut view, 100, 32, &state);
    assert!(
        text.contains("solo is the built-in, so there's nothing to compare"),
        "{text}"
    );
}
