//! What the app sends for views and pickers, taking over a session, and a reattached run's
//! replay.

use argonauts_core::app::AppEvent;
use serde_json::{Value, json};

use super::conversation::{app, spend};
use super::run::{Agent, RUN, RunScript};

pub fn view(data: Value) -> AppEvent {
    app(json!({"type": "view", "data": data}))
}

/// The label a workflow gives the agent, `phase:target`, such as `build:json-writer`.
pub fn label(agent: &Agent) -> String {
    let step = match agent.role.as_str() {
        "worker" => "build",
        "reader" => "read",
        other => other,
    };
    let words: Vec<String> = agent
        .title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|word| !word.is_empty() && !["a", "an", "the"].contains(&word.as_str()))
        .take(2)
        .collect();
    format!("{step}:{}", words.join("-"))
}

/// `status` is `waiting`, `running`, `needs_you`, `done`, or `failed`.
pub fn agent_row(agent: &Agent, status: &str) -> Value {
    json!({"session": agent.session, "label": label(agent),
           "worker": agent.worker, "status": status, "tokens": 381_000,
           "spent": spend(18_100, 0.84), "elapsed_ms": 362_000, "subagents": 0,
           "context": null, "fallback_for": null})
}

pub fn nested_workflow_row(id: &str, name: &str) -> Value {
    json!({"row": "workflow", "id": id, "name": name, "status": "running", "agents_done": 2,
           "agents_total": 4, "workers": ["claude-code/opus", "codex/gpt-5.6"],
           "elapsed_ms": 720_000})
}

fn tagged_agent(agent: &Agent, status: &str) -> Value {
    let mut row = agent_row(agent, status);
    row["row"] = json!("agent");
    row
}

/// The three agents that read the request back, all done.
pub fn goal_agents() -> Vec<Agent> {
    (1..=3)
        .map(|n| Agent {
            session: format!("s-{:03}", 10 + n),
            task: "goal".to_owned(),
            role: "reader".to_owned(),
            ..Agent::numbered(n)
        })
        .collect()
}

/// The run's workflow: Understand (three readers) and Plan done, Build running with `agents` and,
/// when given, a nested workflow row, and Merge waiting.
pub fn workflow_view(agents: &[Agent], nested: Option<Value>) -> AppEvent {
    let mut rows: Vec<Value> = agents
        .iter()
        .enumerate()
        .map(|(i, agent)| tagged_agent(agent, if i == 0 { "done" } else { "running" }))
        .collect();
    rows.extend(nested);
    let total = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    let goal: Vec<Value> = goal_agents()
        .iter()
        .map(|agent| tagged_agent(agent, "done"))
        .collect();
    let phase = |name: &str, standard: &str, status: &str, done: u32, total: u32, rows: Value| {
        let count = (total > 0).then(|| json!({"done": done, "total": total}));
        json!({"name": name, "standard": standard, "status": status, "count": count,
               "rows": rows})
    };
    view(
        json!({"data": "workflow", "view": {"run": RUN, "id": "wf-001",
        "name": "council", "description": "Understand, plans, build, review, and merge",
        "breadcrumb": [], "agents_done": 4, "agents_total": 4 + total,
        "elapsed_ms": 9_840_000, "spent": spend(747_000, 4.10),
        "phases": [
            phase("Understand", "understand", "done", 3, 3, json!(goal)),
            phase("Plan", "plan", "done", 0, 0, json!([])),
            phase("Build", "do", "running", 1, total, json!(rows)),
            phase("Merge", "decide", "waiting", 0, 0, json!([])),
        ]}}),
    )
}

pub fn agent_view(agent: &Agent) -> AppEvent {
    view(
        json!({"data": "agent", "view": {"run": RUN, "row": agent_row(agent, "running"),
        "prompt": format!("You own {}: {}.\nKeep the CSV path.", agent.task, agent.title),
        "activity": [
            {"tool": "Read", "summary": "src/export.rs", "ok": true},
            {"tool": "Edit", "summary": "src/export.rs", "ok": null}],
        "tool_calls": 2,
        "diff": {"files_changed": 4, "insertions": 120, "deletions": 18},
        "subagents": null, "outcome": null}}),
    )
}

/// One row of a settings picker. `layer` is where its value comes from, such as `built-in`
/// or `global`.
pub fn setting_row(key: &str, value: &str, layer: &str, allowed: &[&str]) -> Value {
    json!({"key": key, "value": value, "layer": layer, "default_scope": "global",
           "safety": false, "allowed": allowed, "description": format!("Sets {key}")})
}

/// A settings command's picker, such as `theme`, filled with `rows`.
pub fn settings_view(command: &str, rows: Vec<Value>) -> AppEvent {
    view(json!({"data": "settings", "rows": rows, "command": command, "section": null}))
}

/// A section's rows, such as `ui` for `/theme`, answering `Request { Settings { section } }`.
pub fn section_view(section: &str, rows: Vec<Value>) -> AppEvent {
    view(json!({"data": "settings", "rows": rows, "command": null, "section": section}))
}

pub fn config_view(entries: &[(&str, &str, &str)]) -> AppEvent {
    let entries: Vec<Value> = entries
        .iter()
        .map(|(key, value, layer)| {
            json!({"key": key, "value": value, "layer": layer, "safety": false})
        })
        .collect();
    view(json!({"data": "config", "entries": entries}))
}

/// `state` is `running`, `waiting`, `crashed`, `merged`, and so on.
pub fn run_summary(run: &str, title: &str, state: &str, remind: bool) -> Value {
    json!({"run": run, "title": title, "mode": "crew", "workflow": "council", "state": state,
           "stopped_at": "merge approval", "updated": "2026-09-30T11:00:00Z",
           "spent": spend(9000, 0.46), "sessions_to_resume": 2, "waiting": [],
           "live": state == "running", "remind": remind})
}

pub fn runs_view(runs: Vec<Value>) -> AppEvent {
    view(json!({"data": "runs", "runs": runs}))
}

pub fn all_repos_view(repos: Vec<(&str, Vec<Value>)>) -> AppEvent {
    let repos: Vec<Value> = repos
        .into_iter()
        .map(|(repo, runs)| json!({"repo": repo, "runs": runs}))
        .collect();
    view(json!({"data": "all_repos", "repos": repos}))
}

/// The agent panel's rows from the controller, for this repo.
pub fn agent_panel_view_data(rows: Vec<Value>) -> AppEvent {
    view(json!({"data": "crew", "panel": {"repo": super::conversation::REPO, "rows": rows}}))
}

pub fn raw_view(agent: &Agent, lines: &[&str]) -> AppEvent {
    view(
        json!({"data": "raw", "raw": {"session": agent.session, "lines": lines,
        "truncated": false, "path": "/state/raw.jsonl", "cli_transcript": null}}),
    )
}

pub fn diff_view(agent: &Agent, text: &str) -> AppEvent {
    view(json!({"data": "diff", "diff": {"task": agent.task,
        "stat": {"files_changed": 1, "insertions": 2, "deletions": 1}, "text": text,
        "path": "/state/tasks/diff"}}))
}

/// Hands the terminal to `argv` in `cwd`, or refuses when `argv` is empty, as for an
/// endpoint with no UI of its own.
pub fn take_over(agent: &Agent, argv: &[&str], cwd: &str) -> AppEvent {
    let command = argv.split_first().map(|(program, args)| {
        json!({"program": program, "args": args, "env_set": [], "env_remove": [],
               "stdin": null, "cwd": cwd, "files": [], "keep_stdin_open": false})
    });
    let agent_id = agent.worker.split('/').next().unwrap_or(&agent.worker);
    app(
        json!({"type": "take_over", "ticket": {"task": agent.task, "command": command,
        "worktree": cwd, "agent_id": agent_id}, "place": "here", "run": RUN}),
    )
}

pub fn attached(run: &str, title: &str) -> AppEvent {
    app(json!({"type": "attached", "summary": run_summary(run, title, "running", false)}))
}

pub fn condensed_replay(
    run: &str,
    turns: &[(&str, &str)],
    milestones: &[&str],
    recap: &str,
) -> AppEvent {
    let turns: Vec<Value> = turns
        .iter()
        .map(|(from, text)| {
            let from = if *from == "you" {
                json!({"party": "user"})
            } else {
                json!({"party": "agent", "session": "s-001", "worker": from})
            };
            json!({"from": from, "text": text})
        })
        .collect();
    app(
        json!({"type": "condensed_replay", "run": run, "turns": turns,
        "milestones": milestones, "recap": recap, "recap_model": null}),
    )
}

pub fn replay_done(run: &str, seq: u64) -> AppEvent {
    app(json!({"type": "replay_done", "run": run, "seq": seq}))
}

pub const REPLAY_MILESTONES: [&str; 3] = [
    "Goal approved",
    "Plan approved · 3 tasks",
    "task-002 merged into the run branch",
];

pub const REPLAY_RECAP: &str = "Two tasks are done and the merge waits for you.";

/// Reattaching to a long run: `tool_calls` calls per agent for three agents, a question answered
/// long ago, then the condensed replay and `ReplayDone`. Returns the script for events after
/// it, and the history's events.
pub fn long_replayed_run(tool_calls: usize) -> (RunScript, Vec<Agent>, Vec<AppEvent>) {
    let mut run = RunScript::default();
    let crew: Vec<Agent> = (1..=3).map(Agent::numbered).collect();
    let mut events = vec![
        attached(RUN, "Add a --json flag to export"),
        run.started("Add a --json flag to export", "crew"),
        run.phase(
            "understand",
            "finished",
            Some("3 agents agreed on the goal"),
        ),
        run.phase("planning", "finished", Some("Plan 2 won")),
        run.plan_approval("approval-001"),
        run.decided(
            "approval-001",
            json!({"action": "approve"}),
            json!({"party": "user"}),
        ),
        run.phase("build", "started", None),
    ];
    for (i, agent) in crew.iter().enumerate() {
        events.push(run.task_created(agent));
        let key = u8::try_from(i + 1).unwrap_or(u8::MAX);
        events.push(run.session_started(agent, key));
    }
    for n in 0..tool_calls {
        for agent in &crew {
            let id = format!("{}-{n}", agent.session);
            let path = format!("src/replayed_{n}.rs");
            events.push(run.tool_started(agent, &id, "Read", json!({"file_path": path})));
            events.push(run.tool_finished(agent, &id, true, "ok"));
        }
    }
    events.push(condensed_replay(
        RUN,
        &[
            ("you", "Add a --json flag to the export command"),
            ("codex/gpt-5.6", "The JSON writer sits beside the CSV one."),
        ],
        &REPLAY_MILESTONES,
        REPLAY_RECAP,
    ));
    let last = run.next_seq().saturating_sub(1);
    events.push(replay_done(RUN, last));
    (run, crew, events)
}
