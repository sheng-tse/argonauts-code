//! Run events, numbered in order: agents and their tool calls, tasks, approvals, and a whole
//! crew run to start from.

use argonauts_core::app::AppEvent;
use serde_json::{Value, json};

use super::conversation::{app, mode_working, option, spend, status_with};

pub const RUN: &str = "run-01";

/// A compact elapsed time. The run's events carry a fixed timestamp, so on the wall clock it
/// can read anything from `0s` up.
pub const ELAPSED: &str = r"(?:\d+h\d+m|(?:\d+m)?\d+s)";
/// How long an agent has run, as the list under the input says it: `3s`, `29m`, `1h 7m`.
pub const LISTED_FOR: &str = r"(?:\d+h \d+m|\d+m|\d+s)";

/// Each compact time a running row can read once it has counted on from `from_ms`, given to the
/// screen no sooner than `asked`: from `from_ms` itself up to that plus all the time since.
/// Call it once the screen shows the row.
pub fn counted_on(from_ms: u64, asked: std::time::Instant) -> String {
    let waited = u64::try_from(asked.elapsed().as_millis()).unwrap_or(u64::MAX);
    let times: Vec<String> = (from_ms / 1000..=from_ms.saturating_add(waited) / 1000)
        .map(compact)
        .collect();
    format!("(?:{})", times.join("|"))
}

/// The screen's compact time for `secs`: `45s`, `6m02s`, `1h07m`.
fn compact(secs: u64) -> String {
    let (hours, minutes, seconds) = (secs / 3600, secs / 60 % 60, secs % 60);
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}
pub const EVIDENCE: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

/// One agent of a scripted run.
#[derive(Debug, Clone)]
pub struct Agent {
    pub session: String,
    pub task: String,
    pub title: String,
    pub worker: String,
    pub role: String,
}

const WORKERS: [&str; 3] = [
    "claude-code/opus",
    "codex/gpt-5.6",
    "antigravity-cli/gemini-3.6-pro",
];

const TITLES: [&str; 7] = [
    "Add the JSON writer",
    "Wire the --json flag",
    "Export tests",
    "Update the docs",
    "Error messages",
    "Refactor the CSV path",
    "Benchmarks",
];

impl Agent {
    /// How the screen names it, such as `Opus on Claude Code`.
    pub fn name(&self) -> String {
        let worker = self.worker.parse().expect("a model");
        argonauts_core::look::display_names::DisplayNames::default().worker(&worker)
    }

    /// The `n`th agent, from 1: `s-00n` on `task-00(n+1)`, with workers taking turns.
    pub fn numbered(n: usize) -> Self {
        let index = n.saturating_sub(1);
        Self {
            session: format!("s-{n:03}"),
            task: format!("task-{:03}", n + 1),
            title: TITLES[index % TITLES.len()].to_owned(),
            worker: WORKERS[index % WORKERS.len()].to_owned(),
            role: "worker".to_owned(),
        }
    }

    pub fn endpoint(n: usize) -> Self {
        Self {
            worker: "ollama/llama3.2".to_owned(),
            ..Self::numbered(n)
        }
    }

    fn agent_id(&self) -> &str {
        self.worker.split('/').next().unwrap_or(&self.worker)
    }

    fn is_endpoint(&self) -> bool {
        self.agent_id() == "ollama"
    }
}

/// Builds one run's events with rising `seq` numbers.
#[derive(Debug, Clone)]
pub struct RunScript {
    pub run: String,
    seq: u64,
}

impl Default for RunScript {
    fn default() -> Self {
        Self::new(RUN)
    }
}

impl RunScript {
    pub fn new(run: &str) -> Self {
        Self {
            run: run.to_owned(),
            seq: 0,
        }
    }

    pub fn next_seq(&self) -> u64 {
        self.seq
    }

    /// Any run event, from its `kind` JSON.
    pub fn event(&mut self, task: Option<&str>, session: Option<&str>, kind: Value) -> AppEvent {
        let seq = self.seq;
        self.seq += 1;
        let source = if session.is_some() {
            "agent"
        } else {
            "argonauts-code"
        };
        app(
            json!({"type": "run", "event": {"seq": seq, "ts": "2026-09-30T12:00:00Z",
            "run_id": self.run, "task_id": task, "session": session, "source": source,
            "kind": kind}}),
        )
    }

    fn run_kind(&mut self, data: Value) -> AppEvent {
        self.event(None, None, json!({"type": "run", "data": data}))
    }

    pub fn started(&mut self, title: &str, mode: &str) -> AppEvent {
        self.run_kind(
            json!({"event": "started", "request": title, "title": title, "mode": mode,
            "forced": false, "workflow": "council",
            "estimate": {"task_type": "cli-feature", "summary": "a flag and a writer",
                         "files_likely": ["src/export.rs"], "file_count": 3,
                         "risk_labels": [], "tests_exist": true, "unknowns": []},
            "captain_or_crew": {"mode": mode, "move_to_crew": false, "rule": "crew_labels",
                       "reason": "touches the CLI and the writer"},
            "conversation_turn": null, "continues": null, "repo_root": "/repo", "base_branch": "main",
            "base_commit": "8b41d07", "uncommitted": [], "uncommitted_included": false,
            "settings_sha256": "5e7f", "settings": {"workflow": "council"}, "overrides": [],
            "cli_versions": {}, "version": "0.1.0"}),
        )
    }

    /// `status` is `started`, `finished`, `failed`, or `skipped`.
    pub fn phase(&mut self, phase: &str, status: &str, summary: Option<&str>) -> AppEvent {
        self.event(
            None,
            None,
            json!({"type": "phase", "data": {"phase": phase, "status": status,
                                             "summary": summary}}),
        )
    }

    pub fn task_created(&mut self, agent: &Agent) -> AppEvent {
        self.event(
            Some(&agent.task),
            None,
            json!({"type": "task", "data": {"event": "created", "id": agent.task,
                "title": agent.title, "task": agent.title, "kind": {"kind": "build"},
                "owned_paths": ["src/export.rs"], "read_only_paths": [], "forbidden_paths": [],
                "hidden_paths": [], "depends_on": [], "criteria": [], "worker": agent.worker,
                "reviewers": [],
                "limit": {"limit": null, "max_minutes": null, "max_tool_calls": null,
                           "inactivity_min": null},
                "predictions": null}}),
        )
    }

    pub fn task_state(&mut self, agent: &Agent, from: Option<&str>, to: &str) -> AppEvent {
        self.event(
            Some(&agent.task),
            None,
            json!({"type": "task", "data": {"event": "state", "from": from, "to": to,
                                            "reason": "scripted"}}),
        )
    }

    /// `follow_key` is the agent's number in the panel, 1 to 9.
    pub fn session_started(&mut self, agent: &Agent, follow_key: u8) -> AppEvent {
        let kind = if agent.is_endpoint() {
            "endpoint"
        } else {
            "cli"
        };
        self.event(
            Some(&agent.task),
            Some(&agent.session),
            json!({"type": "session", "data": {"event": "started", "worker": agent.worker,
                "agent_id": agent.agent_id(), "provider": "any", "kind": kind, "role": agent.role,
                "step": "build", "access": "build", "lifetime": "task", "resumed_from": null,
                "follow_key": follow_key,
                // The run's workflow started it, so it's one of the run's agents.
                "workflow_step": {"workflow": "wf-001", "phase": 0, "path": [0]}}}),
        )
    }

    /// What Argonauts Code sent the agent: the launch and the prompt's parts.
    pub fn session_input(&mut self, agent: &Agent, parts: &[(&str, &str)]) -> AppEvent {
        let parts: Vec<Value> = parts
            .iter()
            .map(|(label, text)| {
                json!({"kind": part_kind(label), "label": label, "text": text,
                       "approx_tokens": text.len() / 4, "source": null})
            })
            .collect();
        self.event(
            Some(&agent.task),
            Some(&agent.session),
            json!({"type": "session", "data": {"event": "input",
                "launch": {"program": agent.agent_id(), "args": ["exec", "--json", "<prompt>"],
                    "cwd": format!("/wt/{}", agent.task), "env_set": ["ARGONAUTS_SESSION"],
                    "env_removed": [], "prompt_via": "stdin", "sandbox": "worktree"},
                "parts": parts, "queued": []}}),
        )
    }

    pub fn agent(&mut self, agent: &Agent, event: Value) -> AppEvent {
        self.event(
            Some(&agent.task),
            Some(&agent.session),
            json!({"type": "agent", "data": event}),
        )
    }

    pub fn tool_started(&mut self, agent: &Agent, id: &str, name: &str, input: Value) -> AppEvent {
        self.agent(
            agent,
            json!({"type": "tool_started", "id": id, "name": name, "input": input,
                   "parent": null}),
        )
    }

    pub fn tool_finished(&mut self, agent: &Agent, id: &str, ok: bool, output: &str) -> AppEvent {
        self.agent(
            agent,
            json!({"type": "tool_finished", "id": id, "ok": ok, "output": output,
                   "parent": null}),
        )
    }

    pub fn text_delta(&mut self, agent: &Agent, text: &str) -> AppEvent {
        self.agent(
            agent,
            json!({"type": "text_delta", "text": text, "parent": null}),
        )
    }

    pub fn message(&mut self, agent: &Agent, text: &str) -> AppEvent {
        self.agent(
            agent,
            json!({"type": "message", "text": text, "parent": null}),
        )
    }

    pub fn usage(&mut self, agent: &Agent, output_tokens: u64) -> AppEvent {
        self.agent(
            agent,
            json!({"type": "usage", "input_tokens": 4000, "output_tokens": output_tokens,
                   "cached_tokens": 0, "cost_usd": null, "cumulative": true}),
        )
    }

    pub fn file_diff(&mut self, agent: &Agent, path: &str, hunk: &str) -> AppEvent {
        self.event(
            Some(&agent.task),
            Some(&agent.session),
            json!({"type": "session", "data": {"event": "file_diff", "path": path,
                "added": 2, "removed": 1, "hunk": hunk, "truncated": false}}),
        )
    }

    pub fn queued(&mut self, agent: &Agent, id: &str, text: &str) -> AppEvent {
        self.event(
            Some(&agent.task),
            Some(&agent.session),
            json!({"type": "session", "data": {"event": "queued", "id": id,
                "from": {"party": "user"}, "to": {"target": "task", "value": agent.task},
                "text": text, "queued_at": "2026-09-30T12:00:00Z", "delivery": "next_turn"}}),
        )
    }

    pub fn session_ended(&mut self, agent: &Agent, tool_calls: u32) -> AppEvent {
        self.event(
            Some(&agent.task),
            Some(&agent.session),
            json!({"type": "session", "data": {"event": "ended", "cli_session": null,
                "exit_code": 0, "signal": null, "duration_ms": 252_000,
                "reason": {"why": "completed"}, "spend": spend(18_100, 0.12),
                "tool_calls": tool_calls, "result_text": "Done.", "structured": null,
                "last_message": "Done.", "raw_log": null}}),
        )
    }

    /// A quiet read, then an edit, for one agent.
    pub fn some_work(&mut self, agent: &Agent) -> Vec<AppEvent> {
        let read = format!("{}-read", agent.session);
        let edit = format!("{}-edit", agent.session);
        vec![
            self.tool_started(agent, &read, "Read", json!({"file_path": "src/export.rs"})),
            self.tool_finished(agent, &read, true, "fn write(&self) {}"),
            self.tool_started(
                agent,
                &edit,
                "Edit",
                json!({"file_path": "src/export.rs", "old_string": "self.write_csv(out)",
                       "new_string": "match self.format {"}),
            ),
            self.usage(agent, 18_100),
        ]
    }

    pub fn approval_opened(
        &mut self,
        id: &str,
        kind: &str,
        task: Option<&str>,
        packet: Value,
        options: Vec<Value>,
        free_text: &str,
    ) -> AppEvent {
        self.event(
            task,
            None,
            json!({"type": "approval", "data": {"event": "opened", "id": id, "kind": kind,
                "title": approval_title(kind, &self.run), "task": task, "packet": packet,
                "evidence_sha256": EVIDENCE, "mode": "supervised", "decide_with": "rules",
                "options": options, "free_text": free_text, "needs_you": true}}),
        )
    }

    /// The merge question, over two tasks.
    pub fn merge_approval(&mut self, id: &str) -> AppEvent {
        let stat = json!({"files_changed": 4, "insertions": 60, "deletions": 9});
        let rows: Vec<Value> = [Agent::numbered(1), Agent::numbered(2)]
            .iter()
            .map(|agent| {
                json!({"task": agent.task, "title": agent.title, "kind": {"kind": "build"},
                       "stat": stat, "worker": agent.worker, "checks_passed": true,
                       "review": "approve"})
            })
            .collect();
        let packet = json!({"kind": "merge", "target": "main",
            "branch": format!("argonauts/{}/merged", self.run), "rows": rows,
            "suite_passed_on_latest_main": true, "checks": [],
            "combined": {"files_changed": 7, "insertions": 120, "deletions": 18},
            "diff_path": "/state/combined.diff", "tree_sha": "t1", "spent": spend(9000, 0.46),
            "limit": {"unit": "usd", "amount": 3.0}, "partial": false});
        let options = vec![
            option('y', "Merge", json!({"action": "merge"}), true),
            option(
                'p',
                "Open a pull request",
                json!({"action": "pull_request"}),
                false,
            ),
            option(
                'b',
                "Request changes on some tasks",
                json!({"action": "request_changes"}),
                false,
            ),
            option(
                'k',
                "Keep the branch",
                json!({"action": "keep_branch"}),
                false,
            ),
            option('x', "Abandon the run", json!({"action": "abandon"}), false),
            option('d', "Show the diff", json!({"action": "show_diff"}), false),
        ];
        self.approval_opened(id, "merge", None, packet, options, "guidance")
    }

    /// A question from an agent, with two answers and your own.
    pub fn question_approval(&mut self, id: &str, agent: &Agent) -> AppEvent {
        let packet = json!({"kind": "question", "from": agent.worker, "task": agent.task,
            "text": "Keep a CSV fallback?", "options": ["Yes, keep it", "No, JSON only"]});
        let options = vec![
            option(
                'a',
                "Yes, keep it",
                json!({"action": "choose", "value": "Yes, keep it"}),
                true,
            ),
            option(
                'b',
                "No, JSON only",
                json!({"action": "choose", "value": "No, JSON only"}),
                false,
            ),
            option(
                'o',
                "Type my own answer",
                json!({"action": "answer"}),
                false,
            ),
        ];
        self.approval_opened(id, "question", Some(&agent.task), packet, options, "answer")
    }

    pub fn plan_approval(&mut self, id: &str) -> AppEvent {
        let packet = json!({"kind": "plan", "summary": "A JSON writer beside the CSV one",
            "judging": null, "near_tie": false, "decisions": [],
            "graph": {"tasks": [], "critical_path": []},
            "acceptance_tests": ["--json prints valid JSON"],
            "limit": {"lines": [], "total": {"unit": "usd", "amount": 3.0},
                       "estimate_low": 0.4, "estimate_high": 0.9},
            "new_dependencies": [], "flags": []});
        let options = vec![
            option('y', "Approve the plan", json!({"action": "approve"}), true),
            option(
                'e',
                "Approve with edits",
                json!({"action": "approve_with_edits"}),
                false,
            ),
            option('b', "Send back", json!({"action": "send_back"}), false),
            option(
                's',
                "Run it as Captain",
                json!({"action": "switch_to_captain"}),
                false,
            ),
            option('x', "Abandon", json!({"action": "stop"}), false),
            option('d', "Show the plan", json!({"action": "show_diff"}), false),
        ];
        self.approval_opened(id, "plan", None, packet, options, "edits")
    }

    /// What the app asks after you took an agent's session over and gave it back.
    pub fn take_over_return(&mut self, id: &str, agent: &Agent) -> AppEvent {
        let packet = json!({"kind": "take_over_return", "task": agent.task,
            "worker": agent.worker,
            "changed": {"files_changed": 1, "insertions": 3, "deletions": 0},
            "commit": "f00d"});
        let options = vec![option(
            'c',
            "Let the worker continue",
            json!({"action": "let_continue"}),
            true,
        )];
        self.approval_opened(
            id,
            "take_over_return",
            Some(&agent.task),
            packet,
            options,
            "none",
        )
    }

    /// `by` is a party such as `{"party": "user"}`.
    pub fn decided(&mut self, id: &str, action: Value, by: Value) -> AppEvent {
        self.event(
            None,
            None,
            json!({"type": "approval", "data": {"event": "decided", "id": id,
                "action": action, "text": null, "by": by, "evidence_sha256": EVIDENCE,
                "auto_preview": null, "tasks": []}}),
        )
    }

    pub fn spot_check_queued(&mut self, id: &str) -> AppEvent {
        self.event(
            None,
            None,
            json!({"type": "approval", "data": {"event": "spot_check_queued", "id": id}}),
        )
    }

    pub fn waiting(&mut self, why: &str) -> AppEvent {
        self.run_kind(json!({"event": "waiting", "reason": {"why": why}, "waiting_on": []}))
    }

    pub fn paused(&mut self, sessions_stopped: u32) -> AppEvent {
        self.run_kind(
            json!({"event": "paused", "sessions_stopped": sessions_stopped, "committed": []}),
        )
    }

    pub fn finished(&mut self, outcome: &str, detail: &str) -> AppEvent {
        self.run_kind(json!({"event": "finished", "outcome": outcome, "detail": detail}))
    }

    pub fn note(&mut self, level: &str, text: &str) -> AppEvent {
        self.event(
            None,
            None,
            json!({"type": "note", "data": {"level": level, "text": text}}),
        )
    }
}

/// A crew run with `agents` agents at work, each with a task, a session, a read, and an edit
/// under way, and the screen in working mode.
pub fn crew_run(agents: usize) -> (RunScript, Vec<Agent>, Vec<AppEvent>) {
    let mut run = RunScript::default();
    let crew: Vec<Agent> = (1..=agents).map(Agent::numbered).collect();
    let mut events = vec![
        status_with(json!({"run": RUN, "title": "Add a --json flag to export"})),
        mode_working(RUN),
        run.started("Add a --json flag to export", "crew"),
        run.phase("build", "started", None),
    ];
    for (i, agent) in crew.iter().enumerate() {
        events.push(run.task_created(agent));
        events.push(run.task_state(agent, None, "running"));
        let key = u8::try_from(i + 1).unwrap_or(u8::MAX);
        events.push(run.session_started(agent, key));
    }
    for agent in &crew {
        events.extend(run.some_work(agent));
    }
    (run, crew, events)
}

/// The title the engine sends with an approval, such as `Merge approval · run-01`.
fn approval_title(kind: &str, run: &str) -> String {
    let kind = kind.replace('_', " ");
    let mut chars = kind.chars();
    let first: String = chars
        .next()
        .into_iter()
        .flat_map(char::to_uppercase)
        .collect();
    format!("{first}{} approval · {run}", chars.as_str())
}

/// A part's kind by its label, as the engine sends them: the task goes as a task.
fn part_kind(label: &str) -> &str {
    match label {
        "goal" | "notes" | "rules" | "status" => label,
        _ => "task",
    }
}

#[test]
fn a_counted_time_reads_each_second_it_may_have_reached() {
    let asked = std::time::Instant::now() - std::time::Duration::from_millis(1500);
    let times = counted_on(59_000, asked);
    assert!(times.starts_with("(?:59s|1m00s"), "{times}");
    let at = regex::Regex::new(&format!("^{times}$")).unwrap();
    assert!(at.is_match("1m00s") && !at.is_match("58s"), "{times}");
}
