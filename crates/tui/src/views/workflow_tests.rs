//! The workflow view draws whatever a workflow reports: its own name, its own phases in its own
//! order, and its own agents. These tests give it workflows no list of phases could have
//! foreseen: one phase, twelve, phases that repeat, and one that has reported nothing yet.

use std::time::Instant;

use argonauts_core::events::session::{
    EndReason, SubagentStatus, VendorAgent, VendorAgentState, VendorPhase, VendorWorkflow,
};
use argonauts_core::names::{SessionKey, WorkflowId};
use argonauts_core::views::{
    AgentRow, PhaseCount, PhaseRow, RowStatus, WorkflowRow, WorkflowView as WorkflowData,
};
use argonauts_core::workflow::StandardPhase;
use argonauts_core::{ErrorCategory, RunId, Spend};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::workflow::WorkflowView;
use super::{View, ViewCtx};
use crate::stability::{assert_frames, buffer_text};
use crate::state::AppState;
use crate::theme::{Glyphs, Theme};

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn run() -> RunId {
    RunId::new("run-01").unwrap()
}

pub(super) fn agent(session: &str, label: &str, status: RowStatus) -> AgentRow {
    AgentRow {
        session: SessionKey::parse(session).unwrap(),
        label: label.into(),
        worker: "codex/gpt-5.6".parse().unwrap(),
        status,
        tokens: 12_000,
        spent: Spend::default(),
        elapsed_ms: 90_000,
        subagents: 0,
        context: None,
        standing_in_for: None,
        answered: None,
        tries: Vec::new(),
    }
}

pub(super) fn phase(name: &str, status: RowStatus, agents: Vec<AgentRow>) -> PhaseRow {
    let done = agents
        .iter()
        .filter(|agent| agent.status == RowStatus::Done)
        .count() as u32;
    PhaseRow {
        name: name.into(),
        detail: String::new(),
        standard: StandardPhase::Do,
        status,
        attempt: None,
        count: PhaseCount::of(done, agents.len() as u32),
        rows: agents.into_iter().map(WorkflowRow::Agent).collect(),
    }
}

pub(super) fn workflow(name: &str, phases: Vec<PhaseRow>) -> WorkflowData {
    let agents = || {
        phases
            .iter()
            .flat_map(|phase| &phase.rows)
            .filter_map(|row| match row {
                WorkflowRow::Agent(agent) => Some(agent),
                _ => None,
            })
    };
    WorkflowData {
        run: run(),
        id: WorkflowId::numbered(1),
        name: name.into(),
        description: "Whatever its own script says it does".into(),
        style: None,
        status_lines: Vec::new(),
        breadcrumb: Vec::new(),
        agents_done: agents()
            .filter(|agent| agent.status == RowStatus::Done)
            .count() as u32,
        agents_total: agents().count() as u32,
        elapsed_ms: 840_000,
        spent: Spend::default(),
        phases,
        notes: Vec::new(),
        required: Vec::new(),
        goals: Vec::new(),
    }
}

pub(super) fn draw(view: &mut WorkflowView, width: u16, height: u16) -> String {
    let theme = theme();
    let state = AppState::default();
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    view.render(area, &mut buf, &ctx);
    assert_frames(&buf);
    buffer_text(&buf)
}

pub(super) fn press(view: &mut WorkflowView, code: KeyCode) -> super::ViewOutcome {
    let theme = theme();
    let state = AppState::default();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
}

fn left_pane(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.starts_with(" │"))
        .filter_map(|line| line.split('│').nth(1))
        .map(|cell| cell.trim_end().to_owned())
        .collect()
}

#[test]
fn a_workflow_of_one_phase_shows_that_phase_by_its_own_name() {
    let only = phase(
        "Translate the changelog",
        RowStatus::Running,
        vec![agent("s-001", "translate:fr", RowStatus::Running)],
    );
    let mut view = WorkflowView::new(workflow("changelog-fr", vec![only]));
    let text = draw(&mut view, 100, 14);
    assert!(
        text.lines().nth(1).unwrap().contains("changelog-fr"),
        "{text}"
    );
    assert_eq!(
        left_pane(&text)
            .iter()
            .filter(|row| !row.is_empty())
            .count(),
        1,
        "{text}"
    );
    assert!(text.contains("❯ 1 Translate the changelog 0/1"), "{text}");
    assert!(
        text.contains("┬ Translate the changelog · 1 step ─"),
        "{text}"
    );
    assert!(text.contains("translate:fr"), "{text}");
}

#[test]
fn twelve_phases_are_numbered_in_their_own_order_and_scroll() {
    let names = [
        "Survey", "Outline", "Draft", "Numbers", "Figures", "Critique", "Revise", "Proof",
        "Typeset", "Index", "Package", "Announce",
    ];
    let phases: Vec<PhaseRow> = names
        .iter()
        .enumerate()
        .map(|(i, name)| match i {
            0..=2 => phase(
                name,
                RowStatus::Done,
                vec![agent(&format!("s-{:03}", i + 1), "a", RowStatus::Done)],
            ),
            3 => phase(
                name,
                RowStatus::Running,
                vec![agent("s-004", "numbers:tables", RowStatus::Running)],
            ),
            _ => phase(name, RowStatus::Waiting, Vec::new()),
        })
        .collect();
    let mut view = WorkflowView::new(workflow("write-the-paper", phases));
    // Tall enough for all twelve: each keeps its own number, two digits wide at the end.
    let text = draw(&mut view, 100, 24);
    let shown = left_pane(&text);
    assert!(shown[0].ends_with("✔  Survey   1/1"), "{text}");
    assert!(shown[3].starts_with(" ❯ 4  Numbers  0/1"), "{text}");
    assert!(shown[9].contains("10 Index"), "{text}");
    assert!(shown[11].contains("12 Announce"), "{text}");
    assert!(text.contains("┬ Numbers · 1 step ─"), "{text}");

    // Too short for twelve: the list moves so the selected phase stays on screen.
    for _ in 0..8 {
        press(&mut view, KeyCode::Down);
    }
    let text = draw(&mut view, 100, 14);
    let shown = left_pane(&text);
    assert!(
        shown.iter().any(|row| row.contains("❯ 12 Announce")),
        "{text}"
    );
    assert!(!shown.iter().any(|row| row.contains("Survey")), "{text}");
    assert!(text.contains("Not started yet"), "{text}");
}

#[test]
fn phases_that_repeat_stay_apart_and_keep_their_own_agents() {
    let phases = vec![
        phase(
            "Fix",
            RowStatus::Done,
            vec![agent("s-001", "fix:first-pass", RowStatus::Done)],
        ),
        phase(
            "Review",
            RowStatus::Done,
            vec![agent("s-002", "review:first-pass", RowStatus::Done)],
        ),
        phase(
            "Fix",
            RowStatus::Running,
            vec![agent("s-003", "fix:second-pass", RowStatus::Running)],
        ),
        phase("Review", RowStatus::Waiting, Vec::new()),
    ];
    let mut view = WorkflowView::new(workflow("fix-until-clean", phases.clone()));
    let text = draw(&mut view, 100, 14);
    let shown = left_pane(&text);
    assert!(shown[0].contains("✔ Fix"), "{text}");
    assert!(shown[1].contains("✔ Review"), "{text}");
    assert!(shown[2].contains("❯ 3 Fix"), "{text}");
    assert!(shown[3].contains("4 Review"), "{text}");
    assert!(text.contains("fix:second-pass"), "{text}");
    assert!(!text.contains("fix:first-pass"), "{text}");

    // A refresh keeps the cursor on the second Fix, not the first of that name.
    let mut later = workflow("fix-until-clean", phases);
    later.phases[2].status = RowStatus::Done;
    later.phases[3].status = RowStatus::Running;
    assert!(view.accept(&argonauts_core::app::ViewData::Workflow {
        view: Box::new(later),
    }));
    let text = draw(&mut view, 100, 14);
    assert!(text.contains("┬ Fix · 1 step ─"), "{text}");
    assert!(text.contains("fix:second-pass"), "{text}");

    press(&mut view, KeyCode::Up);
    let text = draw(&mut view, 100, 14);
    assert!(text.contains("┬ Review · 1 step ─"), "{text}");
    assert!(text.contains("review:first-pass"), "{text}");
}

#[test]
fn a_workflow_that_has_reported_nothing_yet_still_opens() {
    let mut view = WorkflowView::new(workflow("warming-up", Vec::new()));
    let text = draw(&mut view, 100, 14);
    assert!(
        text.lines().nth(1).unwrap().contains("warming-up"),
        "{text}"
    );
    assert!(!text.contains("0/0"), "{text}");
    assert!(text.contains("No phases yet"), "{text}");
    // Nothing to select, and no key panics on the empty lists.
    for code in [KeyCode::Down, KeyCode::Up, KeyCode::Right, KeyCode::Enter] {
        press(&mut view, code);
    }
    draw(&mut view, 100, 14);
}

/// What an agent's own CLI reports, or the runner: names in any language and any order, an
/// agent in a phase the script never declared, and phases with no agents.
fn reported(phases: &[&str], agents: &[(&str, Option<u32>, VendorAgentState)]) -> WorkflowData {
    let workflow = VendorWorkflow {
        id: "wf-live-1".into(),
        call: None,
        name: "réécrire-les-docs".into(),
        description: "Rewrites the docs page by page".into(),
        phases: phases
            .iter()
            .enumerate()
            .map(|(i, title)| VendorPhase {
                index: i as u32 + 1,
                title: (*title).to_owned(),
                detail: String::new(),
            })
            .collect(),
        agents: agents
            .iter()
            .enumerate()
            .map(|(i, (label, phase, state))| VendorAgent {
                index: i as u32 + 1,
                label: (*label).to_owned(),
                phase: *phase,
                model: Some("opus".into()),
                state: *state,
                tokens: 4_000,
                tool_calls: 2,
                duration_ms: 30_000,
                started_at_ms: None,
                attempt: 1,
                prompt: None,
                result: None,
            })
            .collect(),
        status: SubagentStatus::Running,
        tokens: 8_000,
        tool_calls: 4,
        duration_ms: 60_000,
        summary: None,
        started_at_ms: None,
    };
    let host = agent("s-001", "main", RowStatus::Running);
    WorkflowData::of_vendor(&run(), &host, &workflow, None)
}

#[test]
fn a_reported_workflow_is_drawn_with_its_own_words() {
    let data = reported(
        &["Lire", "Réécrire", "Relire"],
        &[
            ("lire:guide", Some(1), VendorAgentState::Done),
            ("réécrire:guide", Some(2), VendorAgentState::Running),
            ("réécrire:api", Some(2), VendorAgentState::Queued),
        ],
    );
    let mut view = WorkflowView::new(data);
    let text = draw(&mut view, 100, 14);
    assert!(text.contains("réécrire-les-docs"), "{text}");
    let shown = left_pane(&text);
    assert!(shown[0].contains("✔ Lire     1/1"), "{text}");
    assert!(shown[1].contains("❯ 2 Réécrire 0/2"), "{text}");
    assert!(shown[2].contains("3 Relire"), "{text}");
    assert!(text.contains("┬ Réécrire · 2 steps ─"), "{text}");
    assert!(text.contains("1/3 steps"), "{text}");
}

#[test]
fn a_reported_workflow_with_no_phases_or_agents_yet_is_drawn_empty() {
    let mut view = WorkflowView::new(reported(&[], &[]));
    let text = draw(&mut view, 100, 14);
    assert!(text.contains("réécrire-les-docs"), "{text}");
    assert!(text.contains("No phases yet"), "{text}");
}

#[test]
fn the_kind_of_workflow_follows_its_name_and_the_tokens_its_time() {
    let mut data = workflow(
        "add-due-dates",
        vec![phase(
            "Build",
            RowStatus::Running,
            vec![agent("s-001", "build:store", RowStatus::Running)],
        )],
    );
    data.notes = vec!["orchestrator workflow".into()];
    data.spent.input_tokens = 900_000;
    data.spent.output_tokens = 200_000;
    let mut view = WorkflowView::new(data);
    let text = draw(&mut view, 120, 14);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[1].trim_end(),
        " add-due-dates · orchestrator workflow",
        "{text}"
    );
    assert!(
        lines[2]
            .trim_end()
            .ends_with("0/1 steps · 14m00s · ↓ 1.1M tokens"),
        "{text}"
    );
}

#[test]
fn a_step_you_require_carries_a_mark_and_the_others_keep_their_column() {
    let mut data = workflow(
        "switch-reads",
        vec![
            phase(
                "Plan",
                RowStatus::Done,
                vec![agent("s-001", "plan:orchestrator", RowStatus::Done)],
            ),
            phase(
                "Review",
                RowStatus::Running,
                vec![agent("s-002", "review:switch-reads", RowStatus::Running)],
            ),
            phase("Secrets scan", RowStatus::Waiting, Vec::new()),
            phase("Benchmark", RowStatus::Waiting, Vec::new()),
            phase("Sign off", RowStatus::Waiting, Vec::new()),
        ],
    );
    // Names are matched whatever their case, as a file and a script may each write them.
    data.required = vec!["secrets scan".into(), "Benchmark".into()];
    let mut view = WorkflowView::new(data);
    let text = draw(&mut view, 100, 14);
    let shown = left_pane(&text);
    assert!(shown[0].contains("✔   Plan"), "{text}");
    assert!(shown[1].contains("❯ 2   Review"), "{text}");
    assert!(shown[2].contains("3 ★ Secrets scan"), "{text}");
    assert!(shown[3].contains("4 ★ Benchmark"), "{text}");
    assert!(shown[4].contains("5   Sign off"), "{text}");
    assert!(!text.contains("a step you require"), "{text}");

    press(&mut view, KeyCode::Down);
    let text = draw(&mut view, 100, 14);
    assert!(
        text.contains("┬ Secrets scan · 0 steps · ★ a step you require ─"),
        "{text}"
    );
}

#[test]
fn a_workflow_with_no_required_step_has_no_column_for_the_mark() {
    let mut view = WorkflowView::new(workflow(
        "plain",
        vec![phase(
            "Build",
            RowStatus::Running,
            vec![agent("s-001", "build:a", RowStatus::Running)],
        )],
    ));
    let text = draw(&mut view, 100, 14);
    assert!(text.contains("❯ 1 Build 0/1 │"), "{text}");
    assert!(!text.contains('★'), "{text}");
}

#[test]
fn who_does_a_phase_sits_at_the_end_of_its_title_when_there_is_room() {
    let mut build = phase(
        "Build",
        RowStatus::Running,
        vec![agent("s-001", "build:store", RowStatus::Running)],
    );
    build.detail = "Opus 5.5 planned · Astra reviews next".into();
    let mut review = phase("Review", RowStatus::Waiting, Vec::new());
    review.detail = "Astra reads each change".into();
    let mut view = WorkflowView::new(workflow("add-due-dates", vec![build, review]));
    let text = draw(&mut view, 120, 14);
    let top = text.lines().find(|line| line.contains("╭ Phases")).unwrap();
    assert!(
        top.trim_end()
            .ends_with("─ Opus 5.5 planned · Astra reviews next ─╮"),
        "{text}"
    );
    // Too narrow for both: the title stays whole and the note goes.
    let text = draw(&mut view, 60, 14);
    let top = text.lines().find(|line| line.contains("╭ Phases")).unwrap();
    assert!(top.contains("Build · 1 step"), "{text}");
    assert!(!top.contains("planned"), "{text}");

    // A phase still to come says who will do it where its agents will be.
    press(&mut view, KeyCode::Down);
    let text = draw(&mut view, 120, 14);
    assert!(
        text.contains("Not started yet · Astra reads each change"),
        "{text}"
    );
}

/// `add-due-dates` in its Build phase, three workers each on a goal that
/// Codex's own goal keeps, with `build:cli` on its third try.
pub(super) fn on_goals() -> WorkflowData {
    use super::goal_panel::fixture::{build_cli, met, starting};
    let timed = |mut agent: AgentRow, ms: u64| {
        agent.elapsed_ms = ms;
        agent
    };
    let mut build = phase(
        "Build",
        RowStatus::Running,
        vec![
            timed(agent("s-002", "build:store", RowStatus::Done), 250_000),
            timed(agent("s-003", "build:cli", RowStatus::Running), 362_000),
            timed(agent("s-004", "build:docs", RowStatus::Running), 90_000),
        ],
    );
    build.detail = "Opus 5.5 planned · Astra reviews next".into();
    let mut data = workflow(
        "add-due-dates",
        vec![
            phase(
                "Plan",
                RowStatus::Done,
                vec![agent("s-001", "plan:orchestrator", RowStatus::Done)],
            ),
            build,
            phase("Review", RowStatus::Waiting, Vec::new()),
            phase("Sign off", RowStatus::Waiting, Vec::new()),
        ],
    );
    data.description = "Add due dates to tasks, with a command that lists what's overdue".into();
    data.notes = vec!["orchestrator workflow".into()];
    data.agents_total = 8;
    data.agents_done = 5;
    data.spent.input_tokens = 1_100_000;
    let mut docs = starting("s-004", "build:docs");
    // On Codex too, as the others are, with nothing checked yet.
    docs.kept_by = build_cli("s-003").kept_by;
    data.goals = vec![met("s-002", "build:store"), build_cli("s-003"), docs];
    data
}

fn row_of<'a>(text: &'a str, label: &str) -> &'a str {
    text.lines()
        .find(|line| line.contains(label))
        .unwrap_or_default()
}

#[test]
fn an_agent_on_a_goal_carries_its_squares_its_try_and_the_lines_met() {
    let mut view = WorkflowView::new(on_goals());
    press(&mut view, KeyCode::Right);
    press(&mut view, KeyCode::Down);
    let text = draw(&mut view, 120, 30);
    let store = row_of(&text, "build:store");
    assert!(
        store.contains("✔ build:store GPT 5.6 on Codex   ■■ goal met in 2 tries"),
        "{text}"
    );
    assert!(store.trim_end().ends_with("4m10s │"), "{text}");
    let cli = row_of(&text, "build:cli");
    assert!(
        cli.contains("❯◐ build:cli   GPT 5.6 on Codex   ■■■ try 3 · 2 of 3 lines met"),
        "{text}"
    );
    assert!(cli.trim_end().ends_with("6m02s │"), "{text}");
    assert!(
        row_of(&text, "build:docs").contains("■■ try 1 · 0 of 2 lines met"),
        "{text}"
    );
    // Where a goal stands takes the place of the tokens.
    assert!(!cli.contains("tok"), "{text}");
}

/// On a pane too narrow for the words, a goal's row keeps its squares and says the rest
/// shortly, before it would say nothing of its goal.
#[test]
fn a_narrow_pane_keeps_the_squares_and_shortens_the_goals_words() {
    let mut view = WorkflowView::new(on_goals());
    press(&mut view, KeyCode::Right);
    let text = draw(&mut view, 80, 30);
    assert!(
        row_of(&text, "build:store").contains("build:store GPT 5.6   ■■ met"),
        "{text}"
    );
    assert!(
        row_of(&text, "build:cli").contains("build:cli   GPT 5.6   ■■■ try 3 · 2/3"),
        "{text}"
    );
    assert!(
        row_of(&text, "build:docs").contains("build:docs  GPT 5.6   ■■ try 1 · 0/2"),
        "{text}"
    );
}

#[test]
fn the_goal_panel_under_the_agents_shows_the_selected_agents_goal() {
    let mut view = WorkflowView::new(on_goals());
    press(&mut view, KeyCode::Right);
    press(&mut view, KeyCode::Down);
    let text = draw(&mut view, 120, 30);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[1].trim_end(),
        " add-due-dates · orchestrator workflow · workers on Codex's own goal",
        "{text}"
    );
    assert!(
        lines[2]
            .trim_end()
            .ends_with("5/8 steps · 14m00s · ↓ 1.1M tokens"),
        "{text}"
    );
    let top = lines.iter().find(|line| line.contains("╭ Phases")).unwrap();
    assert!(
        top.contains("┬ Build · 3 steps · each task worked as a goal ─"),
        "{text}"
    );
    assert!(
        top.trim_end()
            .ends_with("─ Opus 5.5 planned · Astra reviews next ─╮"),
        "{text}"
    );
    let panel = lines
        .iter()
        .position(|line| line.contains("◆ Goal of build:cli"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(
        lines[panel].starts_with(" ╭ ◆ Goal of build:cli ─"),
        "{text}"
    );
    assert!(
        lines[panel]
            .trim_end()
            .ends_with("─ kept by Codex's own goal · checked by Argonauts ─╮"),
        "{text}"
    );
    assert!(
        lines[panel + 1].contains("Until all of these hold"),
        "{text}"
    );
    assert!(
        lines[panel + 1].contains("Tries · failing tests after each"),
        "{text}"
    );
    assert!(
        lines[panel + 1].contains("Last check · cargo test overdue"),
        "{text}"
    );
    assert!(text.contains("1 failed · 11 passed · 4.1s"), "{text}");
    assert!(text.contains("Codex says"), "{text}");
    let footer = lines.last().unwrap().trim_end();
    assert!(
        footer.starts_with(" ↑↓ agent · enter open it · g its goal · "),
        "{footer}"
    );

    // The next agent's goal takes the panel, and the frame keeps its rows.
    press(&mut view, KeyCode::Down);
    let next = draw(&mut view, 120, 30);
    assert!(next.contains("◆ Goal of build:docs"), "{next}");
    assert!(!next.contains("Goal of build:cli"), "{next}");
    assert!(next.contains("Not checked yet"), "{next}");
}

#[test]
fn from_the_phases_the_panel_previews_a_goal_still_running() {
    let mut view = WorkflowView::new(on_goals());
    let text = draw(&mut view, 120, 30);
    assert!(text.contains("◆ Goal of build:cli"), "{text}");
    // A phase with no goals says so, in the same box, so nothing jumps.
    press(&mut view, KeyCode::Up);
    let text = draw(&mut view, 120, 30);
    assert!(text.contains("╭ Goal ─"), "{text}");
    assert!(
        text.contains("No agent in this phase works toward a goal"),
        "{text}"
    );
    press(&mut view, KeyCode::Right);
    let text = draw(&mut view, 120, 30);
    assert!(
        text.contains("plan:orchestrator works without a goal"),
        "{text}"
    );
    assert!(
        !text.lines().last().unwrap().contains("g its goal"),
        "{text}"
    );
}

/// The rows the screen gives a docked `view` of a screen `rows` tall: what the view asks for,
/// else the usual, and never the two rows kept above it.
fn dock_rows(view: &mut WorkflowView, rows: u16) -> u16 {
    let theme = theme();
    let state = AppState::default();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let screen = ratatui::layout::Size::new(120, rows);
    view.dock_height(screen, &ctx)
        .unwrap_or_else(|| super::dock::height(rows))
        .min(rows.saturating_sub(2))
}

#[test]
fn a_workflow_with_goals_takes_more_rows_and_one_without_takes_the_usual() {
    let plain = WorkflowView::new(workflow(
        "plain",
        vec![phase(
            "Build",
            RowStatus::Running,
            vec![agent("s-001", "build:a", RowStatus::Running)],
        )],
    ));
    let mut plain = plain;
    let mut goals = WorkflowView::new(on_goals());
    for rows in [24, 40] {
        let usual = super::dock::height(rows);
        assert_eq!(dock_rows(&mut plain, rows), usual, "{rows}");
        let taller = dock_rows(&mut goals, rows);
        assert!(taller > usual && taller <= rows - 2, "{rows}: {taller}");
    }
    // A tall screen's usual frame already holds both boxes, so it stays as it is.
    assert_eq!(dock_rows(&mut goals, 64), super::dock::height(64));
    // A screen too short for both boxes keeps the agents and leaves the panel out.
    let mut view = WorkflowView::new(on_goals());
    let text = draw(&mut view, 120, 12);
    assert!(text.contains("build:cli"), "{text}");
    assert!(!text.contains("Goal of"), "{text}");
}

/// On an agent whose goal is one task's, the footer offers the goal's keys:
/// `p` holds the goal rather than the run, and `e` opens the agent on its Goal tab with the
/// goal's lines ready to change.
#[test]
fn p_holds_the_selected_agents_goal_and_e_opens_its_lines() {
    use argonauts_core::app::UiCommand;
    use argonauts_core::events::goal::GoalAction;

    let theme = theme();
    let mut state = crate::mentions::fixture::working_state();
    let mut data = on_goals();
    data.run = state.attached.clone().unwrap();
    let task = argonauts_core::CardId::new("task-002").unwrap();
    data.goals[1].task = Some(task.clone());
    state.goals.remember(&data.run, &data.goals);
    let mut view = WorkflowView::new(data);
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    view.handle_key(key(KeyCode::Right), &ctx);

    // build:store's goal is met, so nothing on it is offered, and `e` is no key there.
    let area = Rect::new(0, 0, 140, 30);
    let footer = |view: &mut WorkflowView| {
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, &ctx);
        let text = buffer_text(&buf);
        text.lines().last().unwrap().trim().to_owned()
    };
    assert!(!footer(&mut view).contains("e edit its lines"));
    assert!(matches!(
        view.handle_key(key(KeyCode::Char('e')), &ctx),
        super::ViewOutcome::Ignored
    ));

    view.handle_key(key(KeyCode::Down), &ctx);
    let hints = footer(&mut view);
    assert!(
        hints.starts_with(
            "↑↓ agent · enter open it · g its goal · p pause the goal · e edit its lines · \
             m message it · x stop it"
        ),
        "{hints}"
    );
    // At 120 columns the docked view is one cell short of every key, and stopping stays.
    let narrow = Rect::new(0, 0, 120, 30);
    let mut buf = Buffer::empty(narrow);
    view.render(narrow, &mut buf, &ctx);
    let hints = buffer_text(&buf).lines().last().unwrap().trim().to_owned();
    assert_eq!(
        hints,
        "↑↓ agent · enter open it · g its goal · p pause the goal · e edit its lines · \
         x stop it · esc back"
    );
    let held = view.handle_key(key(KeyCode::Char('p')), &ctx);
    assert!(
        matches!(
            &held,
            super::ViewOutcome::Send(UiCommand::Goal {
                action: GoalAction::Pause { task: Some(held) },
                ..
            }) if *held == task
        ),
        "{held:?}"
    );

    let opened = view.handle_key(key(KeyCode::Char('e')), &ctx);
    let super::ViewOutcome::Open(mut agent) = opened else {
        panic!("{opened:?}");
    };
    let mut buf = Buffer::empty(area);
    agent.render(area, &mut buf, &ctx);
    let text = buffer_text(&buf);
    assert!(text.contains("◆ the goal"), "{text}");
    assert!(
        text.lines()
            .last()
            .unwrap()
            .starts_with("its lines, with ; between ❯ "),
        "{text}"
    );
}

#[test]
fn g_opens_the_selected_agent_on_its_goal_tab_when_the_screen_holds_it() {
    let mut view = WorkflowView::new(on_goals());
    press(&mut view, KeyCode::Right);
    press(&mut view, KeyCode::Down);
    // This screen holds no session of that run, so it says why nothing opens.
    let why = press(&mut view, KeyCode::Char('g'));
    assert!(
        matches!(&why, super::ViewOutcome::Notice(text) if text.contains("build:cli")),
        "{why:?}"
    );

    let theme = theme();
    let mut state = crate::mentions::fixture::working_state();
    let run = state.attached.clone().unwrap();
    let mut data = on_goals();
    data.run = run;
    // The fixture's sessions are s-001 to s-004; build:cli is s-003 there too.
    state.goals.remember(&data.run, &data.goals);
    let mut view = WorkflowView::new(data);
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    view.handle_key(key(KeyCode::Right), &ctx);
    view.handle_key(key(KeyCode::Down), &ctx);
    let opened = view.handle_key(key(KeyCode::Char('g')), &ctx);
    let super::ViewOutcome::Open(mut agent) = opened else {
        panic!("{opened:?}");
    };
    let area = Rect::new(0, 0, 120, 30);
    let mut buf = Buffer::empty(area);
    agent.render(area, &mut buf, &ctx);
    let text = buffer_text(&buf);
    let header = text.lines().next().unwrap();
    assert!(header.contains(" Output  Input  Goal  Raw "), "{text}");
    assert!(text.contains("◆ the goal"), "{text}");
    assert!(text.contains("◆ tries"), "{text}");
}

#[test]
fn a_goal_is_matched_to_its_own_agent_when_several_share_a_session() {
    // A CLI's own workflow runs its agents in one session, so the label tells them apart.
    let mut data = on_goals();
    for row in &mut data.phases[1].rows {
        if let WorkflowRow::Agent(agent) = row {
            agent.session = SessionKey::parse("s-009").unwrap();
        }
    }
    for goal in &mut data.goals {
        goal.session = SessionKey::parse("s-009").unwrap();
    }
    let mut view = WorkflowView::new(data);
    press(&mut view, KeyCode::Right);
    let text = draw(&mut view, 120, 30);
    assert!(
        row_of(&text, "build:store").contains("goal met in 2 tries"),
        "{text}"
    );
    assert!(
        row_of(&text, "build:docs").contains("try 1 · 0 of 2 lines met"),
        "{text}"
    );
    assert!(text.contains("◆ Goal of build:store"), "{text}");
}

#[test]
fn m_opens_a_running_agent_ready_to_take_a_message() {
    let theme = theme();
    let state = crate::mentions::fixture::working_state();
    let mut data = on_goals();
    data.run = state.attached.clone().unwrap();
    let mut view = WorkflowView::new(data);
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    let footer = |view: &mut WorkflowView| {
        let area = Rect::new(0, 0, 120, 30);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, &ctx);
        buffer_text(&buf).lines().last().unwrap().to_owned()
    };
    // From the phases no agent is selected, so the key isn't offered and does nothing.
    assert!(!footer(&mut view).contains("m message it"));
    assert!(matches!(
        view.handle_key(key(KeyCode::Char('m')), &ctx),
        super::ViewOutcome::Ignored
    ));

    view.handle_key(key(KeyCode::Right), &ctx);
    view.handle_key(key(KeyCode::Down), &ctx);
    assert!(
        footer(&mut view).contains("m message it"),
        "{}",
        footer(&mut view)
    );
    let opened = view.handle_key(key(KeyCode::Char('m')), &ctx);
    let super::ViewOutcome::Open(mut agent) = opened else {
        panic!("{opened:?}");
    };
    let area = Rect::new(0, 0, 120, 30);
    let mut buf = Buffer::empty(area);
    agent.render(area, &mut buf, &ctx);
    let text = buffer_text(&buf);
    assert!(
        text.lines().last().unwrap().starts_with("message ❯"),
        "{text}"
    );

    // An agent this screen doesn't hold says why, and the key isn't offered for it.
    let mut elsewhere = WorkflowView::new(on_goals());
    elsewhere.handle_key(key(KeyCode::Right), &ctx);
    assert!(!footer(&mut elsewhere).contains("m message it"));
    let why = elsewhere.handle_key(key(KeyCode::Char('m')), &ctx);
    assert!(
        matches!(&why, super::ViewOutcome::Notice(text) if text.contains("isn't on screen")),
        "{why:?}"
    );
}

#[test]
fn with_vim_keys_g_goes_to_the_top_so_its_goal_is_not_offered_there() {
    let theme = theme();
    let state = super::list::keys_state(true);
    let mut view = WorkflowView::new(on_goals());
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    view.handle_key(key(KeyCode::Right), &ctx);
    view.handle_key(key(KeyCode::Down), &ctx);
    let area = Rect::new(0, 0, 120, 30);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    let text = buffer_text(&buf);
    assert!(
        !text.lines().last().unwrap().contains("g its goal"),
        "{text}"
    );
    // `g` moved the cursor to the first agent, and the panel follows it.
    view.handle_key(key(KeyCode::Char('g')), &ctx);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    assert!(buffer_text(&buf).contains("◆ Goal of build:store"));
}

fn tried(session: &str, worker: &str) -> argonauts_core::views::StepTry {
    argonauts_core::views::StepTry {
        session: SessionKey::parse(session).unwrap(),
        worker: worker.parse().unwrap(),
        status: RowStatus::Failed,
        elapsed_ms: 2_000,
        reason: Some(EndReason::VendorError {
            category: ErrorCategory::InvalidRequest,
            message: r#"{"error":{"message":"Invalid schema for response_format 'output': 'required' is required"}}"#
                .into(),
        }),
    }
}

/// A try that ended for a reason the CLI gave no text for still says why, as the agent's own
/// failed row does.
#[test]
fn every_failed_try_says_why_it_ended() {
    let mut review = agent("s-002", "review:due-dates", RowStatus::Running);
    review.worker = "claude-code/claude-haiku-4-5".parse().unwrap();
    let mut limited = tried("s-002", "codex/gpt-5.6-luna");
    limited.reason = Some(EndReason::RateLimited);
    let mut signed_out = tried("s-002", "codex/gpt-5.6-luna");
    signed_out.reason = Some(EndReason::Auth {
        message: "Not logged in. Please run /login".into(),
    });
    review.tries = vec![limited, signed_out];
    let work = phase(
        "Work",
        RowStatus::Running,
        vec![
            agent("s-001", "build:add-due-dates", RowStatus::Done),
            review,
        ],
    );
    let mut view = WorkflowView::new(workflow("add-due-dates", vec![work]));
    press(&mut view, KeyCode::Right);
    press(&mut view, KeyCode::Down);
    press(&mut view, KeyCode::Right);
    let text = draw(&mut view, 120, 16);
    assert!(
        text.contains("✗ GPT 5.6 Luna on Codex · 2s · rate limited"),
        "{text}"
    );
    assert!(
        text.contains("✗ GPT 5.6 Luna on Codex · 2s · Codex isn't signed in"),
        "{text}"
    );
}

#[test]
fn a_step_tried_again_is_one_row_with_its_tries_folded_beneath() {
    let mut review = agent("s-004", "review:due-dates", RowStatus::Done);
    review.worker = "claude-code/claude-haiku-4-5".parse().unwrap();
    review.tries = vec![
        tried("s-002", "codex/gpt-5.6-luna"),
        tried("s-003", "codex/gpt-5.6-luna"),
    ];
    let work = phase(
        "Work",
        RowStatus::Running,
        vec![
            agent("s-001", "build:add-due-dates", RowStatus::Done),
            review,
            agent("s-005", "judge:due-dates", RowStatus::Running),
        ],
    );
    let mut view = WorkflowView::new(workflow("add-due-dates", vec![work]));
    let text = draw(&mut view, 120, 16);
    assert!(text.contains("┬ Work · 3 steps ─"), "{text}");
    assert!(
        text.contains("⎿ took over after GPT 5.6 Luna on Codex failed twice"),
        "{text}"
    );
    assert!(!text.contains("→ shows the tries"), "{text}");

    press(&mut view, KeyCode::Right);
    press(&mut view, KeyCode::Down);
    let text = draw(&mut view, 120, 16);
    assert!(
        text.contains("failed twice · → shows the tries"),
        "the selected step says how to see its tries: {text}"
    );
    assert!(!text.contains("refused the answer format"), "{text}");

    press(&mut view, KeyCode::Right);
    let text = draw(&mut view, 120, 16);
    let tries: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("Codex refused the answer format Argonauts asked for"))
        .collect();
    assert_eq!(tries.len(), 2, "a line per try, in one plain line: {text}");
    assert!(tries[0].contains("✗ GPT 5.6 Luna on Codex · 2s"), "{text}");
    assert!(text.contains("· ← folds them"), "{text}");

    press(&mut view, KeyCode::Left);
    let text = draw(&mut view, 120, 16);
    assert!(!text.contains("refused the answer format"), "{text}");
    assert_eq!(view.depth(), 1, "← folded the tries, and stayed");
}
