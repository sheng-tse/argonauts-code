//! The workflow view, which is the run tree: how it opens, moving in it, opening an agent or
//! a finished step, and frames that hold through resizes.

mod pty_support;

use pty_support::*;
use serde_json::{Value, json};

/// Answers every request for the workflow or an agent, up to `times` of each.
fn with_views(script: Script, crew: &[Agent], times: usize) -> Script {
    let mut script = script;
    for _ in 0..times {
        script = script
            .await_command("request")
            .at(0, workflow_view(crew, None));
    }
    script
}

fn workflow_request(value: &Value) -> bool {
    value["view"]["view"] == "workflow"
}

fn agent_request(value: &Value) -> bool {
    value["view"]["view"] == "agent"
}

const VIEW_HEADER: &str = "Goal, plans, build, review, and merge";

/// The left pane's pointer row.
fn selected_phase(screen: &Screen) -> Option<String> {
    let phases = ["Goal", "Plan", "Build", "Merge"];
    screen.visible.iter().find_map(|row| {
        let at = row.find('❯')?;
        let rest = &row[at..];
        phases
            .iter()
            .find(|phase| {
                rest.split('│')
                    .next()
                    .is_some_and(|cell| cell.contains(*phase))
            })
            .map(|phase| (*phase).to_owned())
    })
}

#[test]
fn run_tree_opens_with_slash_workflows_and_ctrl_t() {
    let (_, crew, events) = crew_run(3);
    let script = with_views(Script::idle().all(0, events), &crew, 2);
    let mut harness = Harness::start(script);
    harness.wait_visible("esc to interrupt");
    harness.send("/workflows");
    harness.press(Key::Enter);
    let request = harness.wait_command_where("request", workflow_request);
    assert_eq!(
        request["view"],
        json!({"view": "workflow", "run": RUN, "workflow": null})
    );
    let view = harness.wait_visible(VIEW_HEADER);
    assert!(
        row_of(&view, TOP_RULE).is_none(),
        "the view takes the screen"
    );
    // The header counts the run as its panel row does, not as the view's data does.
    for part in [
        "Phases",
        "Build · 3 agents",
        "\\d+/3 agents · \\d+(h\\d+m|m\\d+s|s)",
        "✓ Goal",
    ] {
        assert!(
            row_of(&view, part).is_some(),
            "{part}:\n{}",
            view.visible_text()
        );
    }
    harness.wait_visible("esc back");
    harness.press(Key::Esc);
    harness.wait_visible(TOP_RULE);
    harness.press(Key::Ctrl('t'));
    harness.wait_command_where("request", workflow_request);
    harness.wait_visible(VIEW_HEADER);
}

#[test]
fn run_tree_navigation() {
    let (_, crew, events) = crew_run(3);
    let script = with_views(Script::idle().all(0, events), &crew, 1);
    let mut harness = Harness::start(script);
    harness.wait_visible("esc to interrupt");
    harness.press(Key::Ctrl('t'));
    let view = harness.wait_visible(VIEW_HEADER);
    assert!(
        selected_phase(&view).is_some(),
        "a phase is selected:\n{}",
        view.visible_text()
    );
    harness.press(Key::Up);
    harness.press(Key::Up);
    harness.press(Key::Up);
    let top = harness.wait_until("the pointer on Goal", |s| {
        selected_phase(s).as_deref() == Some("Goal")
    });
    assert!(
        row_of(&top, "Goal · 3 agents").is_some(),
        "{}",
        top.visible_text()
    );
    harness.press(Key::Down);
    let plan = harness.wait_until("the pointer on Plan", |s| {
        selected_phase(s).as_deref() == Some("Plan")
    });
    assert!(
        row_of(&plan, "Plan · ").is_some(),
        "{}",
        plan.visible_text()
    );
    harness.press(Key::Down);
    harness.wait_until("the pointer on Build", |s| {
        selected_phase(s).as_deref() == Some("Build")
    });
    harness.press(Key::Tab);
    let agents = harness.wait_visible("❯.*owner · card-002");
    harness.press(Key::Down);
    let moved = harness.wait_visible("❯.*owner · card-003");
    assert_ne!(agents.visible_text(), moved.visible_text());
    harness.press(Key::Esc);
    harness.wait_visible(TOP_RULE);
}

#[test]
fn down_past_panel_opens_run_tree() {
    let (_, crew, events) = crew_run(7);
    let script = with_views(Script::idle().all(0, events), &crew, 1);
    let mut harness = Harness::launch(script)
        .size(40, 100)
        .ui("[panel]\nagents = true")
        .start();
    // The run's row comes first, so four of the seven agents show and three wait below.
    harness.wait_visible("^  ↓ 3 more");
    // The first ↓ focuses the top row; four more reach the fifth, the last one shown.
    for _ in 0..5 {
        harness.press(Key::Down);
    }
    harness.assert_no_command("request");
    // ↓ on the last visible row scrolls the rest in, a row at a time.
    harness.press(Key::Down);
    harness.wait_visible("^  ↓ 2 more");
    harness.assert_no_command("request");
    harness.press(Key::Down);
    harness.wait_gone("^  ↓ \\d+ more");
    harness.assert_no_command("request");
    harness.press(Key::Down);
    // The panel ends the screen, so the last agent is on its last row.
    harness.wait_until("the last agent picked", |screen| {
        let rows = screen.visible.len() - blank_rows_at_bottom(screen);
        rows.checked_sub(1)
            .is_some_and(|last| screen.visible[last].starts_with('❯'))
    });
    harness.assert_no_command("request");
    harness.press(Key::Down);
    harness.wait_command_where("request", workflow_request);
    harness.wait_visible(VIEW_HEADER);
}

#[test]
fn open_collapsed_step_from_tree() {
    let (_, crew, events) = crew_run(2);
    let goal = goal_agents();
    let script = Script::idle()
        .all(0, events)
        .await_command("request")
        .at(0, workflow_view(&crew, None))
        .await_command("request")
        .at(0, agent_view(&goal[0]));
    let mut harness = Harness::start(script);
    harness.wait_visible("esc to interrupt");
    harness.press(Key::Ctrl('t'));
    harness.wait_visible(VIEW_HEADER);
    for _ in 0..3 {
        harness.press(Key::Up);
    }
    let step = harness.wait_visible("Goal · 3 agents");
    assert_eq!(
        count_rows(&step, "restater · goal"),
        3,
        "{}",
        step.visible_text()
    );
    harness.press(Key::Tab);
    harness.press(Key::Enter);
    let request = harness.wait_command_where("request", agent_request);
    assert_eq!(request["view"]["session"], goal[0].session);
    let agent = harness.wait_visible("Prompt ·");
    for part in [
        "restater · goal",
        "Activity ·",
        "Diff · 4 files · \\+120 -18",
    ] {
        assert!(
            row_of(&agent, part).is_some(),
            "{part}:\n{}",
            agent.visible_text()
        );
    }
}

#[test]
fn enter_on_workflow_row_opens_agent_view() {
    let (_, crew, events) = crew_run(3);
    let script = Script::idle()
        .all(0, events)
        .await_command("request")
        .at(
            0,
            workflow_view(&crew, Some(nested_workflow_row("wf-002", "review loop"))),
        )
        .await_command("request")
        .at(0, agent_view(&crew[0]));
    let mut harness = Harness::start(script);
    harness.wait_visible("esc to interrupt");
    harness.press(Key::Ctrl('t'));
    let view = harness.wait_visible(VIEW_HEADER);
    assert!(
        row_of(&view, "◇ review loop").is_some(),
        "a nested workflow is a row among the agents:\n{}",
        view.visible_text()
    );
    harness.press(Key::Tab);
    harness.press(Key::Enter);
    let request = harness.wait_command_where("request", agent_request);
    assert_eq!(
        request["view"],
        json!({"view": "agent", "run": RUN, "session": "s-001"})
    );
    let agent = harness.wait_visible("Prompt ·");
    assert_order(&agent, &["Prompt ·", "Activity ·", "Diff · 4 files"]);
    harness.wait_visible("● Running|✓ Completed");
    harness.press(Key::Esc);
    harness.wait_visible(VIEW_HEADER);
}

#[test]
fn panel_row_enter_opens_workflow_view() {
    let (_, crew, events) = crew_run(3);
    let script = with_views(Script::idle().all(0, events), &crew, 1);
    let mut harness = Harness::start(script);
    let screen = harness.wait_visible("↓ to manage");
    let mode = expect_row(&screen, "↓ to manage");
    assert!(
        screen.visible[mode + 1..]
            .iter()
            .any(|row| row.contains("Add a --json flag") || row.contains("peer-crew")),
        "one row per run:\n{}",
        screen.visible_text()
    );
    harness.press(Key::Down);
    harness.press(Key::Enter);
    harness.wait_command_where("request", workflow_request);
    harness.wait_visible(VIEW_HEADER);
}

#[test]
fn frame_checker_on_resize() {
    let (_, crew, events) = crew_run(3);
    let script = with_views(Script::idle().all(0, events), &crew, 1);
    let mut harness = Harness::start(script);
    harness.wait_visible("esc to interrupt");
    assert_frames(&harness.screen(), COLS);
    harness.press(Key::Ctrl('t'));
    let view = harness.wait_visible(VIEW_HEADER);
    assert_frames(&view, COLS);
    for (rows, cols) in [(24, 60), (24, 40), (40, 140), (30, 80), (32, 200)] {
        harness.resize(rows, cols);
        let screen = harness.wait_until(&format!("a {cols}-column frame"), |screen| {
            screen
                .visible
                .iter()
                .any(|row| width(row) > usize::from(cols) - 10 && row.contains('─'))
        });
        assert_frames(&screen, cols);
        harness.shot(&format!("{cols}"));
    }
    harness.press(Key::Esc);
    let main = harness.wait_visible(TOP_RULE);
    assert_frames(&main, 200);
}

#[test]
fn resize_leaves_no_stale_rows() {
    let script = Script::idle().await_command("submit").timed(chat_turn(
        "t-001",
        "hi",
        "Hello from the default agent.",
        5,
    ));
    let mut harness = Harness::start(script);
    harness.wait_visible(MODE_IDLE);
    harness.submit("hi");
    harness.wait_visible(TURN_END);
    for (rows, cols) in [(32, 80), (32, 120), (20, 100), (32, 60), (32, 100)] {
        harness.resize(rows, cols);
        let screen = harness.wait_until(&format!("the live region at {cols} columns"), |screen| {
            row_of(screen, TOP_RULE)
                .is_some_and(|rule| width(&screen.visible[rule]) == usize::from(cols))
        });
        assert_eq!(
            count_rows(&screen, " peer-crew ─$"),
            1,
            "at {cols}:\n{}",
            screen.text()
        );
        assert_eq!(
            count_rows(&screen, "supervised mode on"),
            1,
            "at {cols}:\n{}",
            screen.text()
        );
        assert_eq!(
            count_rows(&screen, "^❯ hi$"),
            1,
            "at {cols}:\n{}",
            screen.text()
        );
        assert_eq!(
            count_rows(&screen, "Hello from the default agent"),
            1,
            "{}",
            screen.text()
        );
        assert_frames(&screen, cols);
    }
}
