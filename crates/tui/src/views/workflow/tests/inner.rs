//! Agents of a workflow their CLI runs itself, such as Claude Code's own. They have no session
//! of ours and share the one that runs the workflow, so these tests check that each still
//! opens as itself, and that keys which act on a session say why they can't act on one.

use std::time::Instant;

use argonauts_core::RunId;
use argonauts_core::app::views::{AgentView as AgentData, RowStatus, WorkflowView as WorkflowData};
use argonauts_core::app::{ViewData, ViewRequest};
use argonauts_core::runs::events::session::{
    CliAgent, CliAgentState, CliPhase, CliTool, CliWorkflowRun, SubagentStatus,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::super::WorkflowView;
use super::view::agent as session_row;
use crate::base::kit::logo::golden::with_stand_ins;
use crate::frame::state::AppState;
use crate::frame::view::{View, ViewCtx, ViewOutcome};
use crate::testing::pictures::workflow_pictures::dark;
use crate::views::follow::Shown;

const SCREEN: u16 = 124;

fn run() -> RunId {
    RunId::new("run-01").unwrap()
}

fn cli_agent(index: u32, label: &str, state: CliAgentState) -> CliAgent {
    CliAgent {
        index,
        label: label.to_owned(),
        phase: Some(1),
        model: Some("claude-haiku-4-5".to_owned()),
        state,
        tokens: 0,
        tool_calls: 0,
        duration_ms: 0,
        started_at_ms: None,
        try_number: 1,
        prompt: None,
        result: None,
        ..CliAgent::default()
    }
}

/// Claude Code's own workflow with two builders: the first done on its second try, the second
/// at work.
fn claude_codes_own() -> CliWorkflowRun {
    let mut flag = cli_agent(1, "build:due-flag", CliAgentState::Done);
    (flag.tokens, flag.tool_calls, flag.duration_ms) = (210_000, 14, 69_000);
    flag.try_number = 2;
    flag.prompt = Some("Add --due <date> to todo add, and show the date in todo list.".to_owned());
    flag.result = Some("todo add takes --due, and todo list shows it.".to_owned());
    let mut tests = cli_agent(2, "build:due-tests", CliAgentState::Running);
    (tests.tokens, tests.tool_calls, tests.duration_ms) = (30_300, 9, 62_000);
    tests.prompt = Some("Cover --due with tests.".to_owned());
    CliWorkflowRun {
        id: "wf_due".to_owned(),
        call: None,
        name: "add-due-dates".to_owned(),
        description: "Add due dates".to_owned(),
        phases: vec![CliPhase {
            index: 1,
            title: "Build".to_owned(),
            detail: String::new(),
        }],
        agents: vec![flag, tests],
        status: SubagentStatus::Running,
        tokens: 240_300,
        tool_calls: 23,
        duration_ms: 70_000,
        summary: None,
        started_at_ms: None,
    }
}

/// The workflow's view and each of its agents opened, as the app answers for them.
fn views() -> (WorkflowData, Vec<AgentData>) {
    views_of(&claude_codes_own())
}

fn views_of(workflow: &CliWorkflowRun) -> (WorkflowData, Vec<AgentData>) {
    let mut host = session_row("s-001", "build:add-due-dates", RowStatus::Running);
    host.worker = "claude-code/claude-opus-5-5".parse().unwrap();
    let view = WorkflowData::of_agent(&run(), &host, workflow, None);
    let opened = workflow
        .agents
        .iter()
        .map(|agent| {
            AgentData::of_inner(&run(), &host, workflow, agent.index, None).expect("an agent")
        })
        .collect();
    (view, opened)
}

/// Claude Code's own review where no agent simply finished: one at work in a worktree on the
/// model that took over, one that failed, one that was skipped, and one Claude Code refused
/// to start.
fn every_ending() -> CliWorkflowRun {
    let mut store = cli_agent(1, "review:store", CliAgentState::Running);
    (store.tokens, store.tool_calls, store.duration_ms) = (21_400, 14, 62_000);
    store.model = Some("claude-opus-5-5".to_owned());
    store.fallback_model = Some("claude-sonnet-5-5".to_owned());
    store.agent_type = Some("Explore".to_owned());
    store.isolation = Some("worktree".to_owned());
    store.last_tool = Some(CliTool {
        name: "Read".to_owned(),
        summary: Some("src/store.js".to_owned()),
    });
    let mut cli = cli_agent(2, "review:cli", CliAgentState::Failed);
    (cli.tokens, cli.tool_calls, cli.duration_ms) = (9_300, 3, 180_000);
    cli.last_tool = Some(CliTool {
        name: "TodoWrite".to_owned(),
        summary: None,
    });
    cli.error = Some("stalled — no progress for 180000ms".to_owned());
    let mut list = cli_agent(3, "review:list", CliAgentState::Skipped);
    list.error = Some("skipped by user".to_owned());
    let mut docs = cli_agent(4, "review:docs", CliAgentState::Blocked);
    docs.error =
        Some("[review:docs] blocked by safety classifier: it asks for a secret".to_owned());
    CliWorkflowRun {
        phases: vec![CliPhase {
            index: 1,
            title: "Review".to_owned(),
            detail: String::new(),
        }],
        agents: vec![store, cli, list, docs],
        ..claude_codes_own()
    }
}

fn press(view: &mut WorkflowView, code: KeyCode) -> ViewOutcome {
    let theme = dark();
    let state = AppState::default();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
}

fn rows(view: &mut WorkflowView) -> Vec<String> {
    let theme = dark();
    let state = AppState::default();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    // Tall enough for a view's sections with a blank line between each.
    let area = Rect::new(0, 0, SCREEN, 20);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    let buf = with_stand_ins(&buf);
    (0..area.height)
        .map(|y| {
            let row: String = (0..SCREEN).map(|x| buf[(x, y)].symbol()).collect();
            row.trim_end().to_owned()
        })
        .collect()
}

/// The right pane's text on each row of the box down to its last row with any, without the
/// box's edges.
fn right_pane(rows: &[String]) -> Vec<String> {
    let mut pane: Vec<String> = rows
        .iter()
        .filter_map(|row| {
            let mut panes = row.split('│').skip(2);
            panes.next().map(|pane| pane.trim().to_owned())
        })
        .collect();
    while pane.last().is_some_and(String::is_empty) {
        pane.pop();
    }
    pane
}

fn answer(agent: &AgentData) -> ViewData {
    ViewData::Agent {
        view: Box::new(agent.clone()),
    }
}

/// The index an outcome asks for, when it asks for one of the workflow's own agents.
fn asked_for(outcome: &ViewOutcome) -> Option<u32> {
    match outcome {
        ViewOutcome::Request(ViewRequest::InnerAgent { session, agent, .. })
            if session.as_str() == "s-001" =>
        {
            Some(agent.index)
        }
        _ => None,
    }
}

#[test]
fn each_of_claude_codes_own_agents_opens_as_itself() {
    let (data, opened) = views();
    let mut view = WorkflowView::new(data);
    press(&mut view, KeyCode::Enter);
    let outcome = press(&mut view, KeyCode::Enter);
    assert_eq!(asked_for(&outcome), Some(1), "{outcome:?}");

    // The second agent's answer is none this view asked for yet.
    assert!(!view.accept(&answer(&opened[1])));
    assert!(view.accept(&answer(&opened[0])));
    let drawn = rows(&mut view);
    let pane = right_pane(&drawn);
    assert_eq!(
        pane,
        [
            "✔ Done · Haiku 4.5 on Claude Code · try 2",
            "210.0k tok · 14 tool calls · 1m 9s",
            "",
            "Prompt · shortened by Claude Code · ⏎ expand",
            "Add --due <date> to todo add, and show the date in todo list.",
            "",
            "Activity · 14 tool calls",
            "",
            "Outcome",
            "todo add takes --due, and todo list shows it.",
        ],
        "{drawn:#?}"
    );

    // ↓ asks for the next agent, which shares the first one's session.
    let outcome = press(&mut view, KeyCode::Down);
    assert_eq!(asked_for(&outcome), Some(2), "{outcome:?}");
    assert_eq!(right_pane(&rows(&mut view)).last().unwrap(), "Loading…");
    assert!(view.accept(&answer(&opened[1])));
    let drawn = rows(&mut view);
    let pane = right_pane(&drawn);
    assert_eq!(
        pane[0], "⣾ Running · Haiku 4.5 on Claude Code",
        "{drawn:#?}"
    );
    assert_eq!(pane[4], "Cover --due with tests.", "{drawn:#?}");
    assert_eq!(pane[6], "Activity · 9 tool calls", "{drawn:#?}");
    assert_eq!(pane[9], "Still running…", "{drawn:#?}");

    // Back up asks for the first again: the view holds one agent's answer at a time.
    let outcome = press(&mut view, KeyCode::Up);
    assert_eq!(asked_for(&outcome), Some(1), "{outcome:?}");
}

#[test]
fn keys_that_act_on_a_session_say_why_they_cant_here() {
    const WHY: &str = "build:due-tests runs inside Claude Code's own workflow, so only Claude \
                       Code can message, stop or restart it";
    let (data, opened) = views();
    let mut view = WorkflowView::new(data);
    press(&mut view, KeyCode::Enter);
    press(&mut view, KeyCode::Down);
    // In the list, and then in the opened agent.
    for opened in [None, Some(&opened[1])] {
        if let Some(opened) = opened {
            press(&mut view, KeyCode::Enter);
            assert!(view.accept(&answer(opened)));
        }
        for key in ['m', 'x', 'r'] {
            match press(&mut view, KeyCode::Char(key)) {
                ViewOutcome::Notice(said) => assert_eq!(said, WHY, "{key}"),
                other => panic!("{key}: {other:?}"),
            }
        }
    }
}

#[test]
fn a_refresh_asks_for_the_agent_shown_and_takes_only_its_answer() {
    let (data, opened) = views();
    let first = &opened[0];
    let shown = Shown::agent(run(), &first.row);
    assert!(
        matches!(&shown, Shown::InnerAgent { agent, .. } if agent.index == 1),
        "{shown:?}"
    );
    assert!(shown.answered_by(&answer(first)));
    assert!(!shown.answered_by(&answer(&opened[1])));

    // The session that runs the workflow is asked for as any session is, and an answer for
    // one of its workflow's agents isn't its own.
    let host = session_row("s-001", "build:add-due-dates", RowStatus::Running);
    let shown = Shown::agent(run(), &host);
    assert!(matches!(&shown, Shown::Agent { .. }), "{shown:?}");
    assert!(!shown.answered_by(&answer(first)));
    assert_eq!(data.agents_total, 2);
}

/// The colour under the first cell of the first row that reads `text`, in the open agent.
fn colour_of(view: &mut WorkflowView, text: &str) -> ratatui::style::Color {
    let theme = dark();
    let state = AppState::default();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let area = Rect::new(0, 0, SCREEN, 20);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    let found = (0..area.height).find_map(|y| {
        let row: String = (0..SCREEN).map(|x| buf[(x, y)].symbol()).collect();
        let at = row.find(text)?;
        Some((row[..at].chars().count() as u16, y))
    });
    let (x, y) = found.unwrap_or_else(|| panic!("no row reads {text:?}"));
    buf[(x, y)].fg
}

/// An agent Claude Code skipped or refused to start is no failure of the agent's, and reads
/// as what it is in the list, as it does in Claude Code's own view. One that failed says
/// so, and one whose model was replaced says for which.
#[test]
fn claude_codes_own_agents_say_how_each_ended() {
    let (data, _) = views_of(&every_ending());
    assert_eq!(data.phases[0].status, RowStatus::Running);
    // Once nobody is at work the phase reads as Claude Code marks it: one didn't finish.
    let mut over = every_ending();
    over.agents[0].state = CliAgentState::Done;
    assert_eq!(views_of(&over).0.phases[0].status, RowStatus::Failed);
    over.agents
        .retain(|agent| agent.state != CliAgentState::Failed);
    over.agents
        .retain(|agent| agent.state != CliAgentState::Blocked);
    assert_eq!(views_of(&over).0.phases[0].status, RowStatus::Failed);
    over.agents
        .retain(|agent| agent.state != CliAgentState::Skipped);
    assert_eq!(views_of(&over).0.phases[0].status, RowStatus::Done);

    let mut view = WorkflowView::new(data);
    let drawn = rows(&mut view);
    // Each row's words, and its time at the pane's right edge.
    let pane: Vec<(String, String)> = right_pane(&drawn)
        .iter()
        .map(|row| {
            let (words, time) = row.rsplit_once("  ").expect("a time at the right");
            (words.trim_end().to_owned(), time.to_owned())
        })
        .collect();
    let read = |words: &str, time: &str| (words.to_owned(), time.to_owned());
    assert_eq!(
        pane,
        [
            read(
                &format!("≈ review:store  ◧◨ Sonnet 5.5{}21.4k tok", " ".repeat(46)),
                "1m02s"
            ),
            read(
                &format!("× review:cli    ◧◨ Haiku 4.5{}9.3k tok", " ".repeat(48)),
                "3m00s"
            ),
            read("– review:list   ◧◨ Haiku 4.5", "0s"),
            read("× review:docs   ◧◨ Haiku 4.5", "0s"),
        ],
        "{drawn:#?}"
    );
}

/// Opened, each says what Claude Code reports of it: the kind of agent it is and where it
/// works, the last tool it called, and why it failed or was blocked, in the failure's colour.
#[test]
fn claude_codes_own_agents_open_with_their_last_tool_and_why_they_ended() {
    let theme = dark();
    let failure = theme.style(crate::base::theme::Role::Danger).fg.unwrap();
    let (data, opened) = views_of(&every_ending());
    let mut view = WorkflowView::new(data);
    press(&mut view, KeyCode::Enter);
    press(&mut view, KeyCode::Enter);
    assert!(view.accept(&answer(&opened[0])));
    let drawn = rows(&mut view);
    assert_eq!(
        right_pane(&drawn),
        [
            "≈ Running · Sonnet 5.5 on Claude Code · Explore · worktree · in place of Opus 5.5",
            "21.4k tok · 14 tool calls · 1m 2s",
            "",
            "Prompt",
            "",
            "Activity · 14 tool calls · Claude Code names only the last",
            "⏺ Read(src/store.js) · running",
            "",
            "Outcome",
            "Still running…",
        ],
        "{drawn:#?}"
    );

    press(&mut view, KeyCode::Down);
    assert!(view.accept(&answer(&opened[1])));
    let drawn = rows(&mut view);
    let pane = right_pane(&drawn);
    assert_eq!(pane[0], "× Failed · Haiku 4.5 on Claude Code", "{drawn:#?}");
    assert_eq!(
        pane[4..],
        [
            "",
            "Activity · 3 tool calls · Claude Code names only the last",
            "⏺ TodoWrite",
            "",
            "Outcome",
            "stalled — no progress for 180000ms",
        ],
        "{drawn:#?}"
    );
    assert_eq!(colour_of(&mut view, "stalled — no progress"), failure);

    press(&mut view, KeyCode::Down);
    assert!(view.accept(&answer(&opened[2])));
    let drawn = rows(&mut view);
    let pane = right_pane(&drawn);
    assert_eq!(
        pane[0], "– Skipped · Haiku 4.5 on Claude Code",
        "{drawn:#?}"
    );
    assert_eq!(
        pane[4..],
        ["", "Activity", "No tool calls.", "", "Outcome", "Skipped."],
        "{drawn:#?}"
    );

    press(&mut view, KeyCode::Down);
    assert!(view.accept(&answer(&opened[3])));
    let drawn = rows(&mut view);
    let pane = right_pane(&drawn);
    assert_eq!(
        pane[0], "× Failed · Haiku 4.5 on Claude Code · blocked",
        "{drawn:#?}"
    );
    assert_eq!(
        pane.last().unwrap(),
        "[review:docs] blocked by safety classifier: it asks for a secret",
        "{drawn:#?}"
    );
    assert_eq!(colour_of(&mut view, "[review:docs] blocked"), failure);
}

/// A workflow that is over leaves no agent at work: one still going when it ended reads as
/// stopped, with the time it had, as in Claude Code's own view. Its phase didn't finish.
#[test]
fn an_agent_still_going_when_its_workflow_ended_reads_as_stopped() {
    let mut killed = claude_codes_own();
    killed.status = SubagentStatus::Failed;
    killed.started_at_ms = Some(1_000);
    let mut host = session_row("s-001", "build:add-due-dates", RowStatus::Done);
    host.worker = "claude-code/claude-opus-5-5".parse().unwrap();
    // Long after it ended, the agent's time is still the one it last reported.
    let later = Some(3_600_000);
    let data = WorkflowData::of_agent(&run(), &host, &killed, later);
    assert_eq!(data.phases[0].status, RowStatus::Failed);
    let opened = AgentData::of_inner(&run(), &host, &killed, 2, later).expect("an agent");
    assert_eq!(opened.row.elapsed_ms, 62_000);
    // One that reported no time of its own ran from when it began until the workflow's end.
    let mut untimed = killed.clone();
    untimed.agents[0].started_at_ms = Some(1_000);
    untimed.agents[1].started_at_ms = Some(9_000);
    untimed.agents[1].duration_ms = 0;
    let row = AgentData::of_inner(&run(), &host, &untimed, 2, later).expect("an agent");
    assert_eq!(row.row.elapsed_ms, 70_000 - 8_000);

    let mut view = WorkflowView::new(data);
    let drawn = rows(&mut view);
    let pane = right_pane(&drawn);
    assert!(
        pane[1].starts_with("– build:due-tests  ◧◨ Haiku 4.5 ") && pane[1].contains(" 30.3k tok "),
        "{drawn:#?}"
    );
    assert!(pane[1].ends_with("1m02s"), "{drawn:#?}");

    press(&mut view, KeyCode::Enter);
    press(&mut view, KeyCode::Down);
    press(&mut view, KeyCode::Enter);
    assert!(view.accept(&answer(&opened)));
    let drawn = rows(&mut view);
    let pane = right_pane(&drawn);
    assert_eq!(
        pane[0], "– Skipped · Haiku 4.5 on Claude Code",
        "{drawn:#?}"
    );
    assert_eq!(
        pane.last().unwrap(),
        "The workflow stopped before this agent finished.",
        "{drawn:#?}"
    );
}
