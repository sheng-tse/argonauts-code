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

/// A crew run as the engine runs one now, its workflow declared once its lead's brief held, so
/// its row, Ctrl+T, and `/workflows` open the workflow view. A run with agents and no workflow
/// opens its newest agent instead.
fn declared_run(agents: usize) -> (Vec<Agent>, Vec<AppEvent>) {
    let (mut run, crew, mut events) = crew_run(agents);
    events.push(run.event(
        None,
        None,
        json!({"type": "workflow", "data": {"event": "started", "id": "wf-001",
            "from": "peer-crew", "version": "1", "parent": null,
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

const VIEW_HEADER: &str = "Goal, plans, build, review, and merge";

/// The view takes about half the screen, as Claude Code's does, so a tall one shows an
/// agent's sections whole.
fn taller(harness: &mut Harness) {
    harness.resize(64, COLS);
    harness.wait_until("the prompt at the foot of a taller screen", |screen| {
        row_of(screen, TOP_RULE).is_some_and(|row| row > 40)
    });
}

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
    let (crew, events) = declared_run(3);
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
        "✔ Goal",
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
    let (crew, events) = declared_run(3);
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
    harness.press(Key::Right);
    let agents = harness.wait_visible("❯.*build:add-json");
    harness.press(Key::Down);
    let moved = harness.wait_visible("❯.*build:wire-json");
    assert_ne!(agents.visible_text(), moved.visible_text());
    // Esc goes back a level, as ← does: to the phases, then out of the view.
    harness.press(Key::Esc);
    harness.wait_until("the pointer back on Build", |s| {
        selected_phase(s).as_deref() == Some("Build")
    });
    harness.press(Key::Esc);
    harness.wait_visible(TOP_RULE);
}

#[test]
fn down_past_panel_opens_run_tree() {
    let (crew, events) = declared_run(7);
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
    let (crew, events) = declared_run(2);
    let goal = goal_agents();
    let script = Script::idle()
        .all(0, events)
        .await_command("request")
        .at(0, workflow_view(&crew, None))
        .await_command("request")
        .at(0, agent_view(&goal[0]));
    let mut harness = Harness::start(script);
    harness.wait_visible("esc to interrupt");
    taller(&mut harness);
    harness.press(Key::Ctrl('t'));
    harness.wait_visible(VIEW_HEADER);
    for _ in 0..3 {
        harness.press(Key::Up);
    }
    let step = harness.wait_visible("Goal · 3 agents");
    assert_eq!(count_rows(&step, "read:"), 3, "{}", step.visible_text());
    harness.press(Key::Right);
    harness.press(Key::Enter);
    let request = harness.wait_command_where("request", agent_request);
    assert_eq!(request["view"]["session"], goal[0].session);
    let agent = harness.wait_visible("Prompt ·");
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
    let mut harness = Harness::start(script);
    harness.wait_visible("esc to interrupt");
    taller(&mut harness);
    harness.press(Key::Ctrl('t'));
    let view = harness.wait_visible(VIEW_HEADER);
    assert!(
        row_of(&view, "◇ review loop").is_some(),
        "a nested workflow is a row among the agents:\n{}",
        view.visible_text()
    );
    harness.press(Key::Right);
    harness.press(Key::Enter);
    let request = harness.wait_command_where("request", agent_request);
    assert_eq!(
        request["view"],
        json!({"view": "agent", "run": RUN, "session": "s-001"})
    );
    let agent = harness.wait_visible("Prompt ·");
    assert_order(&agent, &["Prompt ·", "Activity ·", "Diff · 4 files"]);
    harness.wait_visible("● Running|✔ Completed");
    harness.press(Key::Esc);
    harness.wait_visible(VIEW_HEADER);
}

#[test]
fn panel_row_enter_opens_workflow_view() {
    let (crew, events) = declared_run(3);
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
    let (crew, events) = declared_run(3);
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

/// The lead starts a design of the workflow, as the engine reports it: no card and no
/// workflow step, since there's no workflow yet.
fn lead_designs(run: &mut RunScript, session: &str, label: &str) -> AppEvent {
    run.event(
        None,
        Some(session),
        json!({"type": "session", "data": {"event": "started", "worker": "claude-code/haiku",
            "vendor": "claude-code", "family": "claude", "kind": "harness",
            "role": "phase-lead", "step": "design_workflow", "access": "read_only",
            "lifetime": "card", "resumed_from": null, "follow_key": null, "label": label}}),
    )
}

/// A crew run whose lead is designing its workflow, with what it says so far.
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
        lead_designs(run, "s-001", "lead:design"),
        run.event(
            None,
            Some("s-001"),
            json!({"type": "agent", "data": {"type": "message",
                "text": "Reading the rate limiter and its failing test.", "parent": null}}),
        ),
    ]
}

/// The session view's line under its header, which follows the agent as it works.
const SESSION_VIEW: &str = r"\[ Output \]   Input .*following$";

/// The transcript's items: rows that start with a bullet.
fn items(screen: &Screen) -> usize {
    count_rows(screen, &format!("^{BULLET} "))
}

/// While the lead designs the workflow, Enter on the run's row opens the lead's session as it
/// works, and once the workflow is declared, the workflow view. Pressing it
/// again and again leaves the transcript as it was.
#[test]
fn enter_opens_the_lead_while_it_designs_then_the_workflow() {
    let mut run = RunScript::default();
    let events = designing(&mut run);
    let declared = vec![
        run.event(
            None,
            None,
            json!({"type": "workflow", "data": {"event": "started", "id": "wf-001",
                "from": "peer-crew", "version": "1", "parent": null,
                "workflow": {"name": "add-per-api-key",
                    "description": "Add per-API-key rate limiting", "phases": []}}}),
        ),
        notice("info", "The lead's plan holds, so the workflow starts."),
    ];
    let crew = [Agent::numbered(1)];
    let script = Script::idle()
        .all(0, events)
        .await_signal("declare")
        .all(0, declared)
        .await_command("request")
        .at(0, workflow_view(&crew, None));
    // Held still, so the lead's running bullet doesn't blink between the two counts.
    let mut harness = Harness::launch(script).ui("motion = \"reduced\"").start();
    taller(&mut harness);
    let before = harness.wait_visible("↓ to manage");
    let shown = items(&before);

    harness.press(Key::Down);
    harness.press(Key::Enter);
    let lead = harness.wait_visible(SESSION_VIEW);
    expect_row(&lead, "^. lead:design · Haiku on Claude Code · ");
    expect_row(&lead, "Reading the rate limiter");
    for _ in 0..10 {
        harness.press(Key::Esc);
        harness.wait_gone(SESSION_VIEW);
        harness.press(Key::Enter);
        harness.wait_visible(SESSION_VIEW);
    }
    harness.press(Key::Esc);
    let after = harness.wait_gone(SESSION_VIEW);
    harness.assert_no_command("request");
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

    harness.signal("declare");
    harness.wait_visible("The lead's plan holds");
    harness.press(Key::Enter);
    harness.wait_command_where("request", workflow_request);
    harness.wait_visible(VIEW_HEADER);
}

/// A key that can't do anything now says why on the notice line, replacing the last, and the
/// transcript gains nothing however often it's pressed. Esc can't pause a run
/// that crashed.
#[test]
fn a_hint_from_a_key_stays_on_the_notice_line() {
    let (_, _, events) = crew_run(2);
    let why = format!("{RUN} crashed. /resume {RUN} recovers it.");
    let hint = app(json!({"type": "hint", "text": why}));
    let mut script = Script::idle().all(0, events);
    for _ in 0..10 {
        script = script.await_command("pause").at(0, hint.clone());
    }
    // Held still, so a running call's bullet doesn't blink between the two counts.
    let mut harness = Harness::launch(script).ui("motion = \"reduced\"").start();
    let before = harness.wait_visible("↓ to manage");
    let shown = items(&before);
    for _ in 0..10 {
        harness.press(Key::Esc);
        harness.wait_command("pause");
    }
    let after = harness.wait_visible("crashed. /resume");
    assert_eq!(items(&after), shown, "{}", after.text());
    assert_eq!(
        count_rows(&after, "crashed. /resume"),
        1,
        "one notice, on the mode line:\n{}",
        after.text()
    );
    let mode = expect_row(&after, "↓ to manage|for shortcuts");
    let notice = expect_row(&after, "crashed. /resume");
    assert!(notice >= mode, "{}", after.text());
}

/// What Enter opens before any agent starts follows the run: the view of what it waits
/// for becomes the lead's session once the lead starts, and that becomes the workflow view
/// once the workflow is declared, with no second Enter.
#[test]
fn the_view_from_a_run_row_follows_the_run() {
    let mut run = RunScript::default();
    let mut events = designing(&mut run);
    let lead = events.split_off(4);
    // The lead's design ends, then code declares the workflow its brief holds.
    let declared = vec![
        run.event(
            None,
            Some("s-001"),
            json!({"type": "session", "data": {"event": "ended", "vendor_session": "v-1",
                "exit_code": 0, "signal": null, "duration_ms": 82_000,
                "reason": {"why": "completed"}, "spend": spend(8_100, 0.03),
                "tool_calls": 6, "result_text": null, "structured": null,
                "last_message": null, "raw_log": null}}),
        ),
        run.event(
            None,
            None,
            json!({"type": "workflow", "data": {"event": "started", "id": "wf-001",
                "from": "peer-crew", "version": "1", "parent": null,
                "workflow": {"name": "add-per-api-key",
                    "description": "Add per-API-key rate limiting", "phases": []}}}),
        ),
        notice("info", "The lead's plan holds, so the workflow starts."),
    ];
    let crew = [Agent::numbered(1)];
    let script = Script::idle()
        .all(0, events)
        .await_signal("lead")
        .all(0, lead)
        .await_signal("declare")
        .all(0, declared)
        .await_command("request")
        .at(0, workflow_view(&crew, None));
    let mut harness = Harness::start(script);
    taller(&mut harness);
    harness.wait_visible("↓ to manage");
    harness.press(Key::Down);
    harness.press(Key::Enter);
    harness.wait_visible("Waiting for the lead to start designing the workflow");
    harness.signal("lead");
    let lead = harness.wait_visible(SESSION_VIEW);
    expect_row(&lead, "^. lead:design · Haiku on Claude Code · ");
    harness.signal("declare");
    harness.wait_command_where("request", workflow_request);
    harness.wait_visible(VIEW_HEADER);
}
