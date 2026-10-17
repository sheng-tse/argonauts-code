//! The workflow view, which is the run tree: how it opens, moving in it, opening an agent or
//! a finished step, and frames that hold through resizes.

mod pty_support;

use argonauts_core::app::AppEvent;
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

/// A crew run as the engine runs one now, its workflow declared once its main agent's brief held,
/// so its row, Ctrl+T, and `/workflows` open the workflow view. A run with agents and no workflow
/// opens its newest agent instead.
fn declared_run(agents: usize) -> (Vec<Agent>, Vec<AppEvent>) {
    let (mut run, crew, mut events) = crew_run(agents);
    events.push(run.event(
        None,
        None,
        json!({"type": "workflow", "data": {"event": "started", "id": "wf-001",
            "from": "council", "version": "1", "parent": null,
            "workflow": {"name": "export-json", "description": "Add a --json flag to export",
                "phases": []}}}),
    ));
    (crew, events)
}

fn workflow_request(value: &Value) -> bool {
    value["view"]["view"] == "workflow"
}

fn agent_request(value: &Value) -> bool {
    value["view"]["view"] == "agent"
}

const VIEW_HEADER: &str = "Understand, plans, build, review, and merge";

/// The view takes about half the screen, as Claude Code's does, so a tall one shows an
/// agent's sections whole.
fn taller(driver: &mut TestDriver) {
    driver.resize(64, COLS);
    driver.wait_until("the prompt at the foot of a taller screen", |screen| {
        row_of(screen, TOP_RULE).is_some_and(|row| row > 40)
    });
}

/// The left pane's pointer row.
fn selected_phase(screen: &Screen) -> Option<String> {
    let phases = ["Understand", "Plan", "Build", "Merge"];
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
    let (crew, events) = declared_run(3);
    let script = with_views(Script::idle().all(0, events), &crew, 2);
    let mut driver = TestDriver::start(script);
    driver.wait_visible(WORKING);
    driver.send("/workflows");
    driver.press(Key::Enter);
    let request = driver.wait_command_where("request", workflow_request);
    assert_eq!(
        request["view"],
        json!({"view": "workflow", "run": RUN, "workflow": null})
    );
    let view = driver.wait_visible(VIEW_HEADER);
    // The view takes the input's place under the conversation, as Claude Code's does.
    assert!(
        row_of(&view, TOP_RULE).is_none(),
        "the input gives way to the view:\n{}",
        view.visible_text()
    );
    for part in [
        "Phases",
        "Build · 3 agents",
        "\\d+/\\d+ agents · \\d+(h\\d+m|m\\d+s|s)",
        "✔ Understand",
    ] {
        assert!(
            row_of(&view, part).is_some(),
            "{part}:\n{}",
            view.visible_text()
        );
    }
    driver.wait_visible("esc back");
    driver.press(Key::Esc);
    driver.wait_visible(TOP_RULE);
    driver.press(Key::Ctrl('t'));
    driver.wait_command_where("request", workflow_request);
    driver.wait_visible(VIEW_HEADER);
}

#[test]
fn run_tree_navigation() {
    let (crew, events) = declared_run(3);
    let script = with_views(Script::idle().all(0, events), &crew, 1);
    let mut driver = TestDriver::start(script);
    driver.wait_visible(WORKING);
    driver.press(Key::Ctrl('t'));
    let view = driver.wait_visible(VIEW_HEADER);
    assert!(
        selected_phase(&view).is_some(),
        "a phase is selected:\n{}",
        view.visible_text()
    );
    driver.press(Key::Up);
    driver.press(Key::Up);
    driver.press(Key::Up);
    let top = driver.wait_until("the pointer on Understand", |s| {
        selected_phase(s).as_deref() == Some("Understand")
    });
    assert!(
        row_of(&top, "Understand · 3 agents").is_some(),
        "{}",
        top.visible_text()
    );
    driver.press(Key::Down);
    let plan = driver.wait_until("the pointer on Plan", |s| {
        selected_phase(s).as_deref() == Some("Plan")
    });
    assert!(
        row_of(&plan, "Plan · ").is_some(),
        "{}",
        plan.visible_text()
    );
    driver.press(Key::Down);
    driver.wait_until("the pointer on Build", |s| {
        selected_phase(s).as_deref() == Some("Build")
    });
    driver.press(Key::Right);
    let agents = driver.wait_visible("❯.*build:add-json");
    driver.press(Key::Down);
    let moved = driver.wait_visible("❯.*build:wire-json");
    assert_ne!(agents.visible_text(), moved.visible_text());
    // Esc goes back a level, as ← does: to the phases, then out of the view.
    driver.press(Key::Esc);
    driver.wait_until("the pointer back on Build", |s| {
        selected_phase(s).as_deref() == Some("Build")
    });
    driver.press(Key::Esc);
    driver.wait_visible(TOP_RULE);
}

#[test]
fn down_past_panel_opens_run_tree() {
    let (crew, events) = declared_run(7);
    let script = with_views(Script::idle().all(0, events), &crew, 1);
    let mut driver = TestDriver::launch(script)
        .size(40, 100)
        .ui("[panel]\nagents = true")
        .start();
    // Your conversation's row and the run's come first, so four of the seven agents show and
    // three wait below.
    driver.wait_visible("^  ↓ 3 more");
    // The first ↓ focuses the run's row; four more reach the fourth agent, the last one shown.
    for _ in 0..5 {
        driver.press(Key::Down);
    }
    driver.assert_no_command("request");
    // ↓ on the last visible row scrolls the rest in, a row at a time.
    driver.press(Key::Down);
    driver.wait_visible("^  ↓ 2 more");
    driver.assert_no_command("request");
    driver.press(Key::Down);
    driver.wait_gone("^  ↓ \\d+ more");
    driver.assert_no_command("request");
    driver.press(Key::Down);
    // The panel ends the screen, so the last agent is on its last row.
    driver.wait_until("the last agent picked", |screen| {
        let rows = screen.visible.len() - blank_rows_at_bottom(screen);
        // The selected agent has the filled dot, as in Claude Code's list.
        rows.checked_sub(1)
            .is_some_and(|last| screen.visible[last].starts_with(&format!("    {BULLET}")))
    });
    driver.assert_no_command("request");
    driver.press(Key::Down);
    driver.wait_command_where("request", workflow_request);
    driver.wait_visible(VIEW_HEADER);
}

#[test]
fn open_collapsed_step_from_tree() {
    let (crew, events) = declared_run(2);
    let goal = goal_agents();
    let script = Script::idle()
        .all(0, events)
        .await_command("request")
        .at(0, workflow_view(&crew, None))
        .await_command("request")
        .at(0, agent_view(&goal[0]));
    let mut driver = TestDriver::start(script);
    driver.wait_visible(WORKING);
    taller(&mut driver);
    driver.press(Key::Ctrl('t'));
    driver.wait_visible(VIEW_HEADER);
    for _ in 0..3 {
        driver.press(Key::Up);
    }
    let step = driver.wait_visible("Understand · 3 agents");
    assert_eq!(count_rows(&step, "read:"), 3, "{}", step.visible_text());
    driver.press(Key::Right);
    driver.press(Key::Enter);
    let request = driver.wait_command_where("request", agent_request);
    assert_eq!(request["view"]["session"], goal[0].session);
    let agent = driver.wait_visible("│ Prompt");
    for part in ["read:add-json", "Activity ·", "Diff · 4 files · \\+120 -18"] {
        assert!(
            row_of(&agent, part).is_some(),
            "{part}:\n{}",
            agent.visible_text()
        );
    }
}

#[test]
fn enter_on_workflow_row_opens_agent_view() {
    let (crew, events) = declared_run(3);
    let script = Script::idle()
        .all(0, events)
        .await_command("request")
        .at(
            0,
            workflow_view(&crew, Some(nested_workflow_row("wf-002", "review loop"))),
        )
        .await_command("request")
        .at(0, agent_view(&crew[0]));
    let mut driver = TestDriver::start(script);
    driver.wait_visible(WORKING);
    taller(&mut driver);
    driver.press(Key::Ctrl('t'));
    let view = driver.wait_visible(VIEW_HEADER);
    assert!(
        row_of(&view, "▸ review loop").is_some() || row_of(&view, "▾ review loop").is_some(),
        "a nested workflow is a row among the agents:\n{}",
        view.visible_text()
    );
    driver.press(Key::Right);
    driver.press(Key::Enter);
    let request = driver.wait_command_where("request", agent_request);
    assert_eq!(
        request["view"],
        json!({"view": "agent", "run": RUN, "session": "s-001"})
    );
    let agent = driver.wait_visible("│ Prompt");
    assert_order(&agent, &["│ Prompt", "│ Activity", "Diff · 4 files"]);
    driver.wait_visible("⏺ Running|✔ Done");
    driver.press(Key::Esc);
    driver.wait_visible(VIEW_HEADER);
}

#[test]
fn panel_row_enter_opens_workflow_view() {
    let (crew, events) = declared_run(3);
    let script = with_views(Script::idle().all(0, events), &crew, 1);
    let mut driver = TestDriver::start(script);
    let screen = driver.wait_visible("↓ to manage");
    let mode = expect_row(&screen, "↓ to manage");
    assert!(
        screen.visible[mode + 1..]
            .iter()
            .any(|row| row.contains("Add a --json flag") || row.contains("council")),
        "one row per run:\n{}",
        screen.visible_text()
    );
    driver.press(Key::Down);
    driver.press(Key::Enter);
    driver.wait_command_where("request", workflow_request);
    driver.wait_visible(VIEW_HEADER);
}

#[test]
fn frame_checker_on_resize() {
    let (crew, events) = declared_run(3);
    let script = with_views(Script::idle().all(0, events), &crew, 1);
    let mut driver = TestDriver::start(script);
    driver.wait_visible(WORKING);
    assert_frames(&driver.screen(), COLS);
    driver.press(Key::Ctrl('t'));
    let view = driver.wait_visible(VIEW_HEADER);
    assert_frames(&view, COLS);
    for (rows, cols) in [(24, 60), (24, 40), (40, 140), (30, 80), (32, 200)] {
        driver.resize(rows, cols);
        let screen = driver.wait_until(&format!("a {cols}-column frame"), |screen| {
            screen
                .visible
                .iter()
                .any(|row| width(row) > usize::from(cols) - 10 && row.contains('─'))
        });
        assert_frames(&screen, cols);
        driver.shot(&format!("{cols}"));
    }
    driver.press(Key::Esc);
    let main = driver.wait_visible(TOP_RULE);
    assert_frames(&main, 200);
}

#[test]
fn resize_leaves_no_stale_rows() {
    let script = Script::idle()
        .await_command("submit")
        .timed(conversation_turn(
            "t-001",
            "hi",
            "Hello from the main agent.",
            5,
        ));
    let mut driver = TestDriver::start(script);
    driver.wait_visible(MODE_IDLE);
    driver.submit("hi");
    driver.wait_visible(TURN_END);
    for (rows, cols) in [(32, 80), (32, 120), (20, 100), (32, 60), (32, 100)] {
        driver.resize(rows, cols);
        let screen = driver.wait_until(&format!("the live region at {cols} columns"), |screen| {
            row_of(screen, TOP_RULE)
                .is_some_and(|rule| width(&screen.visible[rule]) == usize::from(cols))
        });
        assert_eq!(
            count_rows(&screen, " Council · Haiku ─$"),
            1,
            "at {cols}:\n{}",
            screen.text()
        );
        assert_eq!(
            count_rows(&screen, "ǁ supervised"),
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
            count_rows(&screen, "Hello from the main agent"),
            1,
            "{}",
            screen.text()
        );
        assert_frames(&screen, cols);
    }
}

/// The main agent starts a design of the workflow, as the engine reports it: no task and no
/// workflow step, since there's no workflow yet.
fn orchestrator_designs(run: &mut RunScript, session: &str, label: &str) -> AppEvent {
    run.event(
        None,
        Some(session),
        json!({"type": "session", "data": {"event": "started", "worker": "claude-code/haiku",
            "agent_id": "claude-code", "provider": "anthropic", "kind": "cli",
            "role": "tie-breaker", "step": "design_workflow", "access": "read_only",
            "lifetime": "task", "resumed_from": null, "follow_key": null, "label": label}}),
    )
}

/// A crew run whose main agent is designing its workflow, with what it says so far.
fn designing(run: &mut RunScript) -> Vec<AppEvent> {
    vec![
        status_with(json!({"run": RUN, "title": "Make the rate limiter per API key"})),
        mode_working(RUN),
        run.started("Make the rate limiter per API key", "crew"),
        run.event(
            None,
            None,
            json!({"type": "run", "data": {"event": "named", "name": "add-per-api-key",
                "description": "Add per-API-key rate limiting with Retry-After headers",
                "style": "workflow"}}),
        ),
        orchestrator_designs(run, "s-001", "main:design"),
        run.event(
            None,
            Some("s-001"),
            json!({"type": "agent", "data": {"type": "message",
                "text": "Reading the rate limiter and its failing test.", "parent": null}}),
        ),
    ]
}

/// The session view's line under its header, which follows the agent as it works.
const SESSION_VIEW: &str = r" Output +Input +Raw ";

/// The transcript's items: rows that start with a bullet.
fn items(screen: &Screen) -> usize {
    count_rows(screen, &format!("^{BULLET} "))
}

/// While the main agent designs the workflow, Enter on the run's row opens the main agent's session
/// as it works, and once the workflow is declared, the workflow view. Pressing it again and again
/// leaves the transcript as it was.
#[test]
fn nothing_opens_while_the_main_agent_designs_then_the_workflow_does() {
    let mut run = RunScript::default();
    let events = designing(&mut run);
    let declared = vec![
        run.event(
            None,
            None,
            json!({"type": "workflow", "data": {"event": "started", "id": "wf-001",
                "from": "council", "version": "1", "parent": null,
                "workflow": {"name": "add-per-api-key",
                    "description": "Add per-API-key rate limiting", "phases": []}}}),
        ),
        notice(
            "info",
            "The main agent's plan holds, so the workflow starts.",
        ),
    ];
    let crew = [Agent::numbered(1)];
    let script = Script::idle()
        .all(0, events)
        .await_signal("declare")
        .all(0, declared)
        .await_command("request")
        .at(0, workflow_view(&crew, None));
    // Held still, so the main agent's running bullet doesn't blink between the two counts.
    let mut driver = TestDriver::launch(script)
        .ui("motion = \"reduced\"")
        .start();
    taller(&mut driver);
    let before = driver.wait_visible("~/code/api");
    let shown = items(&before);

    // While the main agent designs it, Ctrl+T says the workflow is being written, however
    // often it's pressed, and opens nothing in its place.
    for _ in 0..10 {
        driver.press(Key::Ctrl('t'));
        driver.wait_visible("The workflow is being written; it opens when its phases are known");
    }
    let after = driver.wait_visible("The workflow is being written");
    assert!(row_of(&after, SESSION_VIEW).is_none(), "{}", after.text());
    driver.assert_no_command("request");
    assert_eq!(
        items(&after),
        shown,
        "Enter put nothing in the transcript:\n{}",
        after.text()
    );
    assert!(
        row_of(&after, "hasn't started a workflow").is_none(),
        "{}",
        after.text()
    );

    driver.signal("declare");
    driver.wait_visible("The main agent's plan holds");
    driver.press(Key::Ctrl('t'));
    driver.wait_command_where("request", workflow_request);
    driver.wait_visible(VIEW_HEADER);
}

/// A key that can't do anything now says why on the notice line, replacing the last, and the
/// transcript gains nothing however often it's pressed. Esc can't stop a
/// pausing run now once its run host is gone.
#[test]
fn a_hint_from_a_key_stays_on_the_notice_line() {
    let (_, _, events) = crew_run(2);
    let why = format!("{RUN} stopped when its run host exited. /resume {RUN} carries it on.");
    let hint = app(json!({"type": "hint", "text": why}));
    let mut script = Script::idle().all(0, events).at(0, mode_pausing(RUN));
    for _ in 0..10 {
        script = script.await_command("pause").at(0, hint.clone());
    }
    // Held still, so a running call's bullet doesn't blink between the two counts.
    let mut driver = TestDriver::launch(script)
        .ui("motion = \"reduced\"")
        .start();
    let before = driver.wait_visible("↓ to manage");
    let shown = items(&before);
    for _ in 0..10 {
        driver.press(Key::Esc);
        driver.wait_command("pause");
    }
    let after = driver.wait_visible("its run host exited. /resume");
    assert_eq!(items(&after), shown, "{}", after.text());
    assert_eq!(
        count_rows(&after, "its run host exited. /resume"),
        1,
        "one notice, on the bottom line:\n{}",
        after.text()
    );
    let mode = expect_row(&after, "↓ to manage|for shortcuts");
    let notice = expect_row(&after, "its run host exited. /resume");
    assert!(notice >= mode, "{}", after.text());
}

/// What Ctrl+T opens before any agent starts follows the run: the view of what it waits for
/// says the workflow is being written once the main agent designs it, never the main agent's
/// page, and becomes the workflow view once the workflow is declared, with no second key. The
/// run has no row under the input until then.
#[test]
fn the_view_opened_before_any_agent_starts_follows_the_run() {
    let mut run = RunScript::default();
    let mut events = designing(&mut run);
    let main_agent = events.split_off(4);
    // The main agent's design ends, then code declares the workflow its brief holds.
    let declared = vec![
        run.event(
            None,
            Some("s-001"),
            json!({"type": "session", "data": {"event": "ended", "cli_session": "v-1",
                "exit_code": 0, "signal": null, "duration_ms": 82_000,
                "reason": {"why": "completed"}, "spend": spend(8_100, 0.03),
                "tool_calls": 6, "result_text": null, "structured": null,
                "last_message": null, "raw_log": null}}),
        ),
        run.event(
            None,
            None,
            json!({"type": "workflow", "data": {"event": "started", "id": "wf-001",
                "from": "council", "version": "1", "parent": null,
                "workflow": {"name": "add-per-api-key",
                    "description": "Add per-API-key rate limiting", "phases": []}}}),
        ),
        notice(
            "info",
            "The main agent's plan holds, so the workflow starts.",
        ),
    ];
    let crew = [Agent::numbered(1)];
    let script = Script::idle()
        .all(0, events)
        .await_signal("main_agent")
        .all(0, main_agent)
        .await_signal("declare")
        .all(0, declared)
        .await_command("request")
        .at(0, workflow_view(&crew, None));
    let mut driver = TestDriver::start(script);
    taller(&mut driver);
    driver.wait_visible("~/code/api");
    driver.press(Key::Ctrl('t'));
    let waiting = driver.wait_visible("Waiting for the main agent to start designing the workflow");
    assert!(
        row_of(&waiting, "↓ to manage").is_none(),
        "{}",
        waiting.text()
    );
    // The main agent's design is the workflow being written, so the view stays and opens
    // nothing in its place: never the main agent's page.
    driver.signal("main_agent");
    let writing = driver.wait_visible("Its workflow is being written.");
    assert!(
        row_of(&writing, SESSION_VIEW).is_none(),
        "{}",
        writing.text()
    );
    driver.signal("declare");
    driver.wait_command_where("request", workflow_request);
    driver.wait_visible(VIEW_HEADER);
}
