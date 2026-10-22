//! Goals on the real screen: the workflow view's rows and goal panel, the opened agent's Goal
//! tab, and its Input tab with each part under where it came from. The app's answers are
//! scripted: `add-due-dates`, three workers each on a goal
//! that Codex's own goal keeps, and `build:cli` on its third try.

use crate::pty_support;
use argonauts_core::app::AppEvent;
use pty_support::*;
use serde_json::{Value, json};

const WORKER: &str = "codex/gpt-5.6";

/// The agents by session: who planned, and the three that build.
const CREW: [(&str, &str, &str); 4] = [
    ("s-001", "plan:orchestrator", "claude-code/opus"),
    ("s-002", "build:store", WORKER),
    ("s-003", "build:cli", WORKER),
    ("s-004", "build:docs", WORKER),
];

fn session_started(run: &mut RunScript, session: &str, label: &str, worker: &str) -> AppEvent {
    let agent_id = worker.split('/').next().unwrap_or(worker);
    run.event(
        None,
        Some(session),
        json!({"type": "session", "data": {"event": "started", "worker": worker,
            "agent_id": agent_id, "provider": "any", "kind": "cli", "role": "worker",
            "step": "build", "access": "build", "lifetime": "task", "resumed_from": null,
            "follow_key": null, "label": label,
            "workflow_step": {"workflow": "wf-001", "phase": 1, "path": [0]}}}),
    )
}

/// What `build:cli` was asked, each part labelled with where it came from.
fn asked(run: &mut RunScript) -> AppEvent {
    let part = |kind: &str, label: &str, source: Value, text: &str| {
        json!({"kind": kind, "label": label, "text": text, "approx_tokens": text.len() / 4,
               "source": source})
    };
    run.event(
        None,
        Some("s-003"),
        json!({"type": "session", "data": {"event": "input",
            "launch": {"program": "codex",
                "args": ["exec", "--json", "--sandbox", "workspace-write", "<prompt>"],
                "cwd": "/wt/build-cli", "env_set": ["ARGONAUTS_SESSION"], "env_removed": [],
                "prompt_via": "stdin", "sandbox": "worktree"},
            "parts": [
                part("prompt", "frame", Value::Null,
                    "You build one task in your own worktree. Touch only the paths it owns.\n\
                     Report what you changed and how you checked it."),
                part("task", "orchestrator", json!("Opus 5.5"),
                    "Add the overdue command: tasks overdue lists tasks due before today.\n\
                     Dates parse as ISO only. Every test in tests/overdue.rs must pass."),
                part("rules", "your file", json!("roles/worker.md"),
                    "Keep commands small, and write the test before the code."),
                part("message", "your request", Value::Null,
                    "have the workers use codex's own goal for each task"),
            ],
            "queued": []}}),
    )
}

fn agent_row(session: &str, label: &str, worker: &str, status: &str, elapsed_ms: u64) -> Value {
    json!({"row": "agent", "session": session, "label": label, "worker": worker,
           "status": status, "tokens": 283_500, "spent": spend(0, 0.0),
           "elapsed_ms": elapsed_ms, "subagents": 0, "context": null,
           "fallback_for": null})
}

fn goal_line(text: &str, met: bool, by: &str, on_try: Option<u32>, failing: Option<&str>) -> Value {
    json!({"text": text, "met": met, "checked_by": by, "met_on_try": on_try,
           "failing": failing})
}

fn try_number(number: u32, did: &str, diff: (u32, u32, u32), failing: Option<u32>) -> Value {
    json!({"number": number, "did": did,
           "changed": {"insertions": diff.0, "deletions": diff.1, "files_changed": diff.2},
           "failing": failing})
}

fn build_cli() -> Value {
    let mut tries = vec![
        try_number(1, "wrote the command and its tests", (142, 0, 3), Some(4)),
        try_number(2, "fixed the date comparison", (9, 6, 1), Some(2)),
        try_number(3, "handling tasks with no due date", (14, 2, 2), Some(1)),
    ];
    tries[0]["elapsed_ms"] = json!(125_000);
    tries[0]["tokens"] = json!(61_000);
    tries[1]["elapsed_ms"] = json!(100_000);
    tries[1]["tokens"] = json!(38_000);
    tries[2]["elapsed_ms"] = json!(137_000);
    tries[2]["running"] = json!(true);
    json!({"session": "s-003", "label": "build:cli", "role": "worker",
        "kept_by": {"by": "cli", "name": "Codex"}, "checked_by": "Argonauts Code",
        "state": "running", "set_ms_ago": 362_000,
        "lines": [
            goal_line("tasks overdue lists tasks due before today", true, "the judge",
                Some(2), None),
            goal_line("tasks add --due rejects a date that isn't ISO", true,
                "cargo test due::parse", Some(1), None),
            goal_line("every test in tests/overdue.rs passes", false, "cargo test overdue",
                None, Some("1 of 12 fails")),
        ],
        "tries": tries, "counting": "failing tests", "work_ms": 300_000, "tokens": 143_000,
        "checks": [
            {"name": "cargo test overdue", "passed": false,
             "result": "1 failed · 11 passed · 4.1s",
             "detail": ["overdue::skips_tasks_without_a_date:", "expected 2 tasks, got 3"]},
            {"name": "cargo test due::parse", "passed": true, "result": "passed · 6 tests"},
            {"name": "the judge", "passed": true, "result": "1 of 1 line met · Astra"},
        ],
        "cli_says": {"cli": "Codex", "state": "active",
            "note": "One test left: tasks without a date are still listed.", "turns": 14}})
}

fn goals() -> Value {
    let kept = json!({"by": "cli", "name": "Codex"});
    let store = json!({"session": "s-002", "label": "build:store", "role": "worker",
    "kept_by": kept, "checked_by": "Argonauts Code", "state": "met",
    "lines": [
        goal_line("tasks keep an optional due date", true, "cargo test store", Some(1), None),
        goal_line("old task files still load", true, "the judge", Some(2), None),
    ],
    "tries": [
        try_number(1, "added the field", (60, 4, 2), Some(1)),
        try_number(2, "read old files", (12, 1, 1), Some(0)),
    ]});
    let docs = json!({"session": "s-004", "label": "build:docs", "role": "worker",
        "kept_by": kept, "checked_by": "Argonauts Code", "state": "running",
        "lines": [
            goal_line("the README names the overdue command", false, "the judge", None, None),
            goal_line("every example runs", false, "sh docs/check.sh", None, None),
        ],
        "tries": [{"number": 1, "did": "drafting the section", "running": true}]});
    json!([store, build_cli(), docs])
}

/// The whole workflow, with `build:cli` in its Build phase.
fn workflow() -> AppEvent {
    let phase = |name: &str, status: &str, count: Value, detail: &str, rows: Value| {
        json!({"name": name, "detail": detail, "standard": "do", "status": status,
               "count": count, "rows": rows})
    };
    view(
        json!({"data": "workflow", "view": {"run": RUN, "id": "wf-001",
        "name": "add-due-dates",
        "description": "Add due dates to tasks, with a command that lists what's overdue",
        "agents_done": 5, "agents_total": 8, "elapsed_ms": 840_000,
        "spent": {"input_tokens": 900_000, "output_tokens": 200_000, "cached_tokens": 0,
                  "cost_usd": 0.0, "cost_partial": false, "cost_estimated": false},
        "notes": ["orchestrator workflow"],
        "goals": goals(),
        "phases": [
            phase("Plan", "done", json!({"done": 1, "total": 1}), "",
                json!([agent_row("s-001", "plan:orchestrator", "claude-code/opus", "done",
                    95_000)])),
            phase("Build", "running", json!({"done": 1, "total": 3}),
                "Opus 5.5 planned · Astra reviews next",
                json!([
                    agent_row("s-002", "build:store", WORKER, "done", 250_000),
                    agent_row("s-003", "build:cli", WORKER, "running", 362_000),
                    agent_row("s-004", "build:docs", WORKER, "running", 90_000),
                ])),
            phase("Review", "waiting", Value::Null, "", json!([])),
            phase("Sign off", "waiting", Value::Null, "", json!([])),
        ]}}),
    )
}

/// The run at work, and the workflow view's answer once the screen asks for it.
fn script() -> Script {
    let mut run = RunScript::default();
    let mut events = vec![
        status_with(json!({"run": RUN, "title": "Add due dates to tasks"})),
        mode_working(RUN),
        run.started("Add due dates to tasks", "crew"),
        run.phase("build", "started", None),
        // Its workflow has started, so Enter on its row opens the workflow view.
        run.event(
            None,
            None,
            json!({"type": "workflow", "data": {"event": "started", "id": "wf-001",
                "from": "orchestrator", "version": "1", "parent": null,
                "workflow": {"name": "add-due-dates",
                    "description": "Add due dates to tasks", "phases": []}}}),
        ),
    ];
    for (session, label, worker) in CREW {
        events.push(session_started(&mut run, session, label, worker));
    }
    events.push(asked(&mut run));
    let cli = Agent {
        session: "s-003".into(),
        task: "task-003".into(),
        title: "The overdue command".into(),
        worker: WORKER.into(),
        role: "worker".into(),
    };
    events.push(run.message(&cli, "Two tests pass now. One is left: tasks with no date."));
    Script::idle()
        .all(0, events)
        .await_command("request")
        .at(0, workflow())
}

fn text_of(screen: &Screen) -> String {
    screen.visible_text()
}

#[test]
fn the_workflow_view_shows_each_goal_and_g_opens_the_agent_on_it() {
    let mut driver = TestDriver::start(script());
    driver.resize(64, 120);
    driver.wait_visible("↓  to manage");
    // The run's row, then its workflow.
    driver.press(Key::Down);
    driver.press(Key::Enter);
    let view = driver.wait_visible("Phases");
    let text = text_of(&view);
    assert!(
        text.contains("orchestrator workflow · workers on Codex's own goal · "),
        "{text}"
    );
    // The run's own clock times it, so only the counts are checked here.
    expect_row(&view, "5/8 agents · \\S+\\s*$");
    assert!(
        text.contains("Build · 3 agents · each task worked as a goal"),
        "{text}"
    );
    // From the phases, the panel previews the goal still running.
    expect_row(
        &view,
        "◆ Goal of build:cli .* kept by Codex's own goal · checked by Argonauts Code",
    );

    driver.press(Key::Right);
    driver.press(Key::Down);
    let view = driver.wait_visible("❯. build:cli");
    expect_row(
        &view,
        "build:store +GPT 5\\.6 · ▪▪ goal met in 2 tries +4m10s",
    );
    expect_row(
        &view,
        "build:cli +GPT 5\\.6 · ▪▪▪ try 3 · 2 of 3 lines met +6m02s",
    );
    expect_row(
        &view,
        "build:docs +GPT 5\\.6 · ▪▪ try 1 · 0 of 2 lines met +1m30s",
    );
    expect_row(
        &view,
        "Until all of these hold +Tries · failing tests after each +Last check",
    );
    expect_row(&view, "1 failed · 11 passed · 4\\.1s");
    expect_row(
        &view,
        "↑↓  select · ⏎ open · g its goal · m message · x stop",
    );

    // The same agent opened, on its Goal tab: the same four parts at full size.
    driver.press(Key::Char('g'));
    let goal = driver.wait_visible("◆ the goal");
    expect_row(
        &goal,
        "^. build:cli · GPT 5\\.6 on Codex +Output +Input +Goal +Raw +try 3 · ",
    );
    expect_row(
        &goal,
        "◆ the goal +kept by Codex's own goal · set 6m ago · 2 of 3 lines met",
    );
    expect_row(
        &goal,
        "✗ +every test in tests/overdue\\.rs passes +cargo test overdue · 1 of 12 fails",
    );
    expect_row(
        &goal,
        "◆ tries +3 so far · no limit set · 5m of work · 143k tokens",
    );
    expect_row(
        &goal,
        "3 +handling tasks with no due date +\\+14 -2 in 2 files .* 1 failing +2m17s · running",
    );
    expect_row(
        &goal,
        "◆ checks +run by Argonauts Code after each try .* ◆ Codex says +its own account",
    );
    expect_row(&goal, "turns +14 in this goal");

    // Shift+Tab goes back a tab, to what it was asked, each part under where it came from.
    driver.press(Key::ShiftTab);
    let input = driver.wait_visible("^Frame · built in");
    for heading in [
        "^Frame · built in$",
        "^From the orchestrator · Opus 5\\.5$",
        "^Your file · roles/worker\\.md$",
        "^From your request$",
        "^Launched as codex exec --json --sandbox workspace-write '<prompt>' · →  expands$",
    ] {
        expect_row(&input, heading);
    }
    expect_row(
        &input,
        "^  have the workers use codex's own goal for each task$",
    );

    // Esc goes back to the workflow, where it was.
    driver.press(Key::Esc);
    driver.wait_visible("◆ Goal of build:cli");
}

#[test]
fn the_row_at_the_bottom_counts_the_goals_still_running() {
    let mut driver = TestDriver::start(script());
    driver.resize(64, 120);
    driver.wait_visible("↓  to manage");
    driver.press(Key::Down);
    driver.press(Key::Enter);
    driver.wait_visible("◆ Goal of build:cli");
    // Back at the input, the row knows the goals the view was just told of.
    driver.press(Key::Esc);
    let screen = driver.wait_visible("2 goals running");
    expect_row(&screen, "0/4 · .* · 2 goals running");
}
