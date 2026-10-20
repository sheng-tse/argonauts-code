//! The frozen contracts, checked from outside: every event and message survives a JSON round
//! trip unchanged, settings defaults match the frozen file, and the schemas generate.

use argonauts_core::api::{ApprovalAnswer, RunState, RunSummary, StartRun, StatusSummary};
use argonauts_core::app::{AppEvent, TraceLine, UiCommand, ViewData};
use argonauts_core::control::{ControlReply, ControlRequest, Frame};
use argonauts_core::events::approval::{AnswerAction, ApprovalDecided};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode};
use argonauts_core::events::{ApprovalEvent, RunEvent, RunEventKind};
use argonauts_core::input::{MentionTarget, ParsedInput, SlashCommand, WorkflowVerb};
use argonauts_core::names::{
    AgentId, ApprovalId, ChoiceId, ConversationId, DecisionId, DraftId, MessageId, ModelRef,
    RoleName, SessionKey, TurnId,
};
use argonauts_core::policy::{
    EvidenceDocument, EvidenceFacts, EvidencePacket, EvidenceSubject, PromotionFacts, SpendFacts,
    StuckFacts,
};
use argonauts_core::registry::RegistryFile;
use argonauts_core::settings::fallback::FallbackMode;
use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
use argonauts_core::settings::ui::UiSettings;
use argonauts_core::settings::{SAFETY_KEYS, SettingMeta, SettingScope, Settings, Span};
use argonauts_core::{AgentEvent, Note, NoteLevel, RunId, Spend};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use time::OffsetDateTime;

/// Parses `value` as `T`, writes it back, and checks nothing was lost or added.
fn round_trip<T: Serialize + DeserializeOwned + std::fmt::Debug + PartialEq>(value: Value) -> T {
    let parsed: T =
        serde_json::from_value(value.clone()).unwrap_or_else(|err| panic!("{err}\n{value:#}"));
    let written = serde_json::to_value(&parsed).unwrap();
    assert_eq!(written, value, "the JSON changed on the way through");
    let again: T = serde_json::from_value(written).unwrap();
    assert_eq!(again, parsed);
    parsed
}

fn envelope(kind: Value) -> Value {
    json!({
        "seq": 7,
        "ts": "2026-09-29T12:00:00Z",
        "run_id": "run-01",
        "task_id": "task-002",
        "session": "s-003",
        "source": "argonauts-code",
        "kind": kind,
    })
}

fn spend() -> Value {
    json!({"input_tokens": 1200, "output_tokens": 300, "cached_tokens": 800, "cost_usd": 0.25,
           "cost_partial": false, "cost_estimated": true})
}

fn diff_stat() -> Value {
    json!({"files_changed": 2, "insertions": 18, "deletions": 4})
}

fn usd(amount: f64) -> Value {
    json!({"unit": "usd", "amount": amount})
}

fn goal_file() -> Value {
    json!({"run": "run-01", "version": 1, "summary": "Per-key rate limiting on the public API.",
           "criteria": [{"id": "G1", "text": "429 with Retry-After", "short": "Retry header", "check": {"test": "test_retry_after"}}], "in_scope": ["public API"],
           "out_of_scope": ["admin API"],
           "assumptions": [{"text": "Retry-After is in seconds", "basis": "a detail, 2 of 3"}],
           "answers": ["per API key"], "source": "readings",
           "path": "/repo/.argonauts/goal/run-01.md", "sha256": "ab12"})
}

fn task_spec() -> Value {
    json!({"id": "task-002", "title": "Redis store", "task": "Add the token bucket store",
           "kind": {"kind": "acceptance_tests", "for_task": "task-003"},
           "owned_paths": ["src/limits/store.ts"], "read_only_paths": ["src/limits/types.ts"],
           "forbidden_paths": [], "hidden_paths": ["tests/hidden"], "depends_on": ["task-001"],
           "criteria": ["G1"], "worker": "codex/gpt-x", "reviewers": ["claude-code/opus"],
           "limit": {"limit": usd(3.5), "max_minutes": 20.0, "max_tool_calls": null,
                      "inactivity_min": 5.0},
           "predictions": {"p_pass_checks_first_try": 0.7, "p_within_time_cap": 0.8,
                           "p_at_most_two_tries": 0.9}})
}

fn queued() -> Value {
    json!({"id": "m-001", "from": {"party": "user"}, "to": {"target": "task", "value": "task-002"},
           "text": "keep a fallback", "queued_at": "2026-09-29T12:00:00Z", "delivery": "next_turn"})
}

fn limit_status() -> Value {
    json!({"level": "stop", "limit": {"unit": "usd", "amount": 2.0}, "spend": spend(),
           "fraction_used": 1.0, "work": {"minutes": 3.5, "tool_calls": 4},
           "work_limits": {"max_minutes": null, "max_tool_calls": null}, "binding": "spend"})
}

fn plan_usage_reading() -> Value {
    json!({"agent_id": "claude-code", "status": "warning", "used": 0.71,
           "resets_at": "2026-09-29T18:00:00Z", "window": "five_hour",
           "source": "rate_limit_event", "observed_at": "2026-09-29T12:00:00Z",
           "detail": {"status": "allowed_warning"}})
}

fn fallback_plan() -> Value {
    json!({"task": "task-002", "title": "Redis store", "stopped": "claude-code/opus",
           "reason": "usage limit, resets in 2h 10m", "resets_at": "2026-09-29T18:00:00Z",
           "work": diff_stat(), "checks_ran": false,
           "candidates": [
               {"worker": "codex/gpt-x", "plan_usage_left": 0.62, "plan_usage_text": "62%",
                "track_record": {"passed": 9, "of": 11}, "cost": {"unit": "plan_usage", "amount": 0.1},
                "needs_confirm": false, "may_run_out": false},
               {"worker": "openrouter/z-ai/glm-5", "plan_usage_left": null, "plan_usage_text": "API key",
                "track_record": null, "cost": {"unit": "usd", "amount": 1.25},
                "needs_confirm": true, "may_run_out": false},
               {"worker": "ollama/qwen3", "plan_usage_left": null, "plan_usage_text": "local",
                "track_record": null, "cost": {"unit": "free"}, "needs_confirm": false,
                "may_run_out": false}],
           "mode": "ask"})
}

fn plan_draft() -> Value {
    json!({"summary": "Redis token bucket", "approach": "middleware after auth",
           "pieces": [{"name": "store", "description": "d", "paths": ["src/store.ts"],
                       "interfaces": ["Store"], "depends_on": []}],
           "choices": [{"topic": "where counters live", "choice": "Redis", "rationale": "r"}],
           "risks": ["latency"], "test_strategy": "unit and e2e", "new_dependencies": ["ioredis"]})
}

fn every_event_kind() -> Vec<Value> {
    let run = |data: Value| json!({"type": "run", "data": data});
    let goal = |data: Value| json!({"type": "goal", "data": data});
    let plan = |data: Value| json!({"type": "plan", "data": data});
    let divide = |data: Value| json!({"type": "divide", "data": data});
    let task = |data: Value| json!({"type": "task", "data": data});
    let session = |data: Value| json!({"type": "session", "data": data});
    let review = |data: Value| json!({"type": "review", "data": data});
    let merge_event = |data: Value| json!({"type": "merge", "data": data});
    let approval = |data: Value| json!({"type": "approval", "data": data});
    let fallback = |data: Value| json!({"type": "fallback", "data": data});
    let agent_status = |data: Value| json!({"type": "agent_status", "data": data});
    let limit = |data: Value| json!({"type": "spend", "data": data});
    let blackboard = |data: Value| json!({"type": "blackboard", "data": data});
    let run_review = |data: Value| json!({"type": "run_review", "data": data});
    let with = |mut base: Value, extra: Value| {
        let (Value::Object(base_map), Value::Object(extra_map)) = (&mut base, extra) else {
            unreachable!()
        };
        base_map.extend(extra_map);
        base
    };

    let approval_opened = |kind: &str, packet: Value, options: Value, free_text: &str| {
        approval(
            json!({"event": "opened", "id": "approval-003", "kind": kind,
            "title": "Approval · run-01", "task": "task-002", "packet": packet,
            "evidence_sha256": "e3b0", "mode": "supervised", "decide_with": "rules",
            "options": options, "free_text": free_text, "needs_you": true}),
        )
    };
    let option = |key: &str, label: &str, action: Value| json!({"key": key, "label": label, "action": action, "recommended": false, "detail": null});

    vec![
        run(
            json!({"event": "started", "request": "add per-key rate limiting",
            "title": "Per-key rate limiting", "mode": "crew", "forced": false,
            "workflow": "council",
            "estimate": {"task_type": "api-feature", "summary": "middleware and a store",
                         "files_likely": ["src/limits/store.ts"], "file_count": 5,
                         "risk_labels": ["api", "storage"], "tests_exist": true, "unknowns": []},
            "captain_or_crew": {"mode": "crew", "move_to_crew": false, "rule": "crew_labels",
                       "reason": "touches api and storage"},
            "conversation_turn": "t-012", "continues": null, "repo_root": "/repo",
            "base_branch": "main", "base_commit": "8b41d07", "uncommitted": ["notes.txt"],
            "uncommitted_included": false, "settings_sha256": "5e7f",
            "settings": {"workflow": "council"},
            "overrides": [{"key": "critique.rounds", "value": "0", "from": "flag"}],
            "cli_versions": {"codex": "0.158.0"}, "version": "0.0.1"}),
        ),
        run(
            json!({"event": "resumed", "cause": "answer", "settings_changed": false,
            "cli_versions_changed": ["codex"], "main_moved_by": 2,
            "sessions": [{"session": "s-009", "task": "task-002", "cli_session": "abc",
                          "resumed": true}]}),
        ),
        run(json!({"event": "pause_requested", "by": {"party": "user"}, "now": false})),
        run(
            json!({"event": "approval_mode_changed", "mode": "auto_preview", "by": {"party": "user"}}),
        ),
        run(json!({"event": "pause_cancelled"})),
        run(json!({"event": "paused", "sessions_stopped": 2, "committed": ["task-002"]})),
        run(json!({"event": "stop_requested", "by": {"party": "user"}})),
        run(
            json!({"event": "waiting", "reason": {"why": "approval"}, "waiting_on": ["approval-003"]}),
        ),
        run(json!({"event": "waiting", "reason": {"why": "paused"}, "waiting_on": []})),
        run(json!({"event": "waiting",
            "reason": {"why": "all_agents_out", "until": "2026-09-29T18:00:00Z"},
            "waiting_on": []})),
        run(json!({"event": "waiting",
            "reason": {"why": "waiting_for_agent", "agent_id": "codex", "until": null},
            "waiting_on": []})),
        run(json!({"event": "closing_answer",
            "text": "Fixed parse() in invoice/money.py; the six tests pass.",
            "by": "claude-code/claude-haiku-4-5",
            "files": [{"path": "invoice/money.py", "insertions": 19, "deletions": 3}],
            "checks": [{"what": "tests", "passed": true, "result": "6 passed"}],
            "tests": ["python3 -m unittest tests.test_money: all 6 passed"],
            "left": ["format_cents() still rounds half down"]})),
        run(json!({"event": "closing_answer", "text": "The run stopped before any change."})),
        run(json!({"event": "finished", "outcome": "merged", "detail": "merged into main"})),
        run(
            json!({"event": "resumed", "cause": "ask_timeout", "settings_changed": false,
            "cli_versions_changed": [], "main_moved_by": null, "sessions": []}),
        ),
        run(
            json!({"event": "snapshot", "upto": 4999, "engine": {"version": 1, "next_session": 12},
            "summary": {"run": "run-01", "goal": [], "plan": null, "decisions": [], "tasks": [],
                        "recent": [], "spend": null, "plan_windows": [], "waiting": []},
            "open_approvals": [], "running": ["s-011"]}),
        ),
        json!({"type": "phase", "data": {"phase": "final_check", "status": "started",
                                         "summary": null}}),
        goal(
            json!({"event": "read", "label": "A", "worker": "claude-code/opus",
            "reading": {"summary": "s", "success_criteria": ["429"], "in_scope": [],
                            "out_of_scope": [], "assumptions": [], "open_questions": ["which key?"]}}),
        ),
        goal(json!({"event": "compared", "worker": "codex",
            "differences": [{"topic": "limit key",
                "readings": [{"label": "A", "reading": "per API key"}], "tag": "scope",
                "question": {"text": "Per user or per API key?", "options": ["per API key", "per user"],
                             "recommended": 0, "basis": "2 of 3 read it so"}}]})),
        goal(
            json!({"event": "settled", "agreements": ["429 with Retry-After"],
            "assumptions": [{"text": "Retry-After is in seconds", "basis": "a detail, 2 of 3"}],
            "questions": [{"n": 1, "of": 1, "text": "Per user or per API key?",
                "options": [{"key": "a", "label": "per API key", "recommended": true,
                             "basis": "2 of 3"},
                            {"key": "b", "label": "per user", "recommended": false, "basis": null}],
                "tag": "scope"}],
            "dropped": 2}),
        ),
        goal(with(json!({"event": "drafted"}), goal_file())),
        goal(json!({"event": "approved", "file": goal_file(), "by": {"party": "user"}})),
        goal(json!({"event": "change_requested", "id": "m-031",
            "from": {"party": "agent", "session": "s-004", "worker": "codex"},
            "text": "also limit the admin API", "clarification": false})),
        goal(
            json!({"event": "change_resolved", "id": "m-031", "resolution": "asked_you",
            "new_sha256": null}),
        ),
        plan(
            json!({"event": "sealed", "draft": "dr-002", "label": 2, "worker": "codex",
            "round": 1, "summary": "in-memory sliding window", "path": "/state/drafts/dr-002.md",
            "sha256": "aa"}),
        ),
        plan(
            json!({"event": "mapped", "round": 1, "worker": "claude-code/opus",
            "topics": [{"name": "where counters live", "disputed": true,
                "positions": [{"label": 1, "summary": "Redis", "quote": "store counters in Redis",
                               "quote_verified": true}]}]}),
        ),
        plan(json!({"event": "fast_path", "merged_by": "codex"})),
        plan(
            json!({"event": "critiqued", "critic": "antigravity-cli", "designated": true,
            "points": [{"plan": 1, "topic": "restarts", "claim": "counters vanish",
                        "severity": "high", "suggestion": "persist them"}]}),
        ),
        plan(json!({"event": "experiment_proposed", "fits_limit": true,
            "proposal": {"topic": "where counters live", "question": "is Redis fast enough?",
                "options": [{"label": "redis", "build": "a bucket in Redis"}], "measure": "p50",
                "decides_by": "under 5 ms wins", "est_minutes": 10.0, "est_cost_usd": 0.5}})),
        plan(
            json!({"event": "experiment_finished", "task": "experiment-001",
            "report": {"topic": "where counters live", "option": "redis",
                "measurements": [{"metric": "p50", "value": 2.1, "unit": "ms"}],
                "finding": "adds 2.1 ms", "works": true}}),
        ),
        plan(
            json!({"event": "scored", "judge": "codex", "judge_provider": "openai", "plan": 1,
            "order": 0, "scores": [4.0, 3.5], "rationale": "clear", "flagged": false}),
        ),
        plan(json!({"event": "judged", "method": "cross",
            "plans": [{"label": 1, "author": "claude-code/opus", "mean": 4.2, "spread": 0.5,
                       "counted": 4, "dropped": 0}],
            "winner": 1, "margin": 0.8, "near_tie": false})),
        plan(
            json!({"event": "decided", "id": "D-003", "title": "Redis token bucket",
            "choice": "Redis", "rationale": "survives restarts", "alternatives": ["in-memory"],
            "path": "/repo/.argonauts/decisions/D-003-redis.md"}),
        ),
        plan(json!({"event": "orchestrated", "orchestrator": "codex",
            "rejection": "task-004 owns a protected path"})),
        divide(json!({"event": "graph", "tasks": [task_spec()], "critical_path": ["task-002"]})),
        divide(
            json!({"event": "worker", "task": "task-002", "worker": "codex",
            "algorithm": "round-robin", "explored": false, "reason": "9 of 11 passed"}),
        ),
        divide(json!({"event": "reviewers", "task": "task-002",
            "reviewers": ["claude-code/opus"], "blind": true})),
        divide(json!({"event": "predicted", "task": "task-002",
            "predictions": {"p_pass_checks_first_try": 0.7, "p_within_time_cap": 0.8,
                            "p_at_most_two_tries": 0.9}})),
        divide(
            json!({"event": "limit", "lines": [{"line": "tasks", "amount": usd(6.5)}],
            "total": usd(20.0), "estimate_low": 9.0, "estimate_high": 12.0}),
        ),
        task(with(json!({"event": "created"}), task_spec())),
        task(
            json!({"event": "created", "id": "task-001", "title": "Captain", "task": "fix it",
            "kind": {"kind": "captain"}, "owned_paths": [], "read_only_paths": [],
            "forbidden_paths": [], "hidden_paths": [], "depends_on": [], "criteria": [],
            "worker": null, "reviewers": [],
            "limit": {"limit": null, "max_minutes": null, "max_tool_calls": null,
                       "inactivity_min": null},
            "predictions": null}),
        ),
        task(
            json!({"event": "state", "from": "running", "to": "checking", "reason": "report done"}),
        ),
        task(json!({"event": "state", "from": null, "to": "waiting", "reason": "created"})),
        task(json!({"event": "try", "try_number": 2, "worker": "codex", "cause": "check_retry"})),
        task(json!({"event": "worktree", "path": "/wt/task-002",
            "branch": "argonauts/run-01/task-002", "base": "8b41d07"})),
        task(
            json!({"event": "committed", "branch": "argonauts/run-01/task-002", "sha": "c0ffee",
            "message": "task-002: Redis store", "stat": diff_stat()}),
        ),
        task(
            json!({"event": "reported", "status": "done", "summary": "store added",
            "files_changed": ["src/limits/store.ts"], "tests_run": ["npm test -- store"],
            "concerns": [], "questions": ["keep a fallback?"],
            "change_requests": [{"text": "widen the Store trait", "paths": ["src/types.ts"]}],
            "memory_proposals": [{"topic": "redis", "text": "use EVAL for atomicity"}]}),
        ),
        task(json!({"event": "diff", "stat": diff_stat(), "path": "/state/tasks/task-002/diff"})),
        task(json!({"event": "taken_over", "command": ["codex", "resume", "abc"]})),
        task(json!({"event": "take_over_ended", "changed": diff_stat(), "commit": null})),
        task(
            json!({"event": "reassigned", "from": "claude-code/opus", "to": "codex",
            "by": {"party": "user"}, "rule_override": null}),
        ),
        session(
            json!({"event": "started", "worker": "codex", "agent_id": "codex", "provider": "openai",
            "kind": "cli", "role": "worker", "step": "build", "access": "build",
            "lifetime": "task", "resumed_from": null, "follow_key": 2}),
        ),
        session(json!({"event": "input",
            "launch": {"program": "codex", "args": ["exec", "--json", "<prompt>"],
                "cwd": "/wt/task-002", "env_set": ["ARGONAUTS_SESSION"],
                "env_removed": ["ANTHROPIC_API_KEY"], "prompt_via": "stdin",
                "sandbox": "seatbelt: write worktree+tmp"},
            "parts": [{"kind": "task", "label": "task", "text": "Add the store",
                       "approx_tokens": 3, "source": null},
                      {"kind": "notes", "label": "notes", "text": "Use EVAL",
                       "approx_tokens": 2, "source": ".argonauts/notes/redis.md"}],
            "queued": [queued()]})),
        session(json!({"event": "cli_session", "id": "019a-abc",
            "transcript": "/home/.codex/sessions/019a.jsonl"})),
        session(with(json!({"event": "queued"}), queued())),
        session(json!({"event": "delivered", "message": "m-001", "via": "steer"})),
        session(
            json!({"event": "file_diff", "path": "src/limits/store.ts", "added": 18,
            "removed": 4, "hunk": "@@ -40,2 +40,3 @@", "truncated": false}),
        ),
        session(with(
            json!({"event": "limit", "kind": "spend"}),
            limit_status(),
        )),
        session(
            json!({"event": "limit", "kind": "wrap_up_sent", "via": "interrupt_resume",
            "dimension": "spend"}),
        ),
        session(json!({"event": "limit", "kind": "nudged", "call": "Bash(npm test)", "times": 3})),
        session(json!({"event": "limit", "kind": "native_cap", "usd": 2.5})),
        session(
            json!({"event": "ended", "cli_session": "019a-abc", "exit_code": 1,
            "signal": null, "duration_ms": 372000,
            "reason": {"why": "usage_limit", "resets_at": "2026-09-29T18:00:00Z"},
            "spend": spend(), "tool_calls": 14, "result_text": null, "structured": null,
            "last_message": "Adding the header now", "raw_log": "/state/s-003/raw.stdout.jsonl"}),
        ),
        session(
            json!({"event": "ended", "cli_session": null, "exit_code": null,
            "signal": 9, "duration_ms": 10, "reason": {"why": "stuck", "watchdog": "repeat"},
            "spend": spend(), "tool_calls": 0, "result_text": null,
            "structured": {"status": "done"}, "last_message": null, "raw_log": null}),
        ),
        json!({"type": "agent", "data": {"type": "text_delta", "text": "hi", "parent": null}}),
        json!({"type": "agent", "data": {"type": "subagent", "update": {"id": "thread-2",
            "parent": null, "kind": "thread", "label": "explorer", "prompt": "map the code",
            "status": "done", "last_message": "found 3 callers", "log_uri": null,
            "steps_visible": false}}}),
        json!({"type": "agent", "data": {"type": "plan_usage", "reading": plan_usage_reading()}}),
        json!({"type": "check", "data": {"name": "secret scan", "kind": "secret_scan",
            "passed": false, "command": null, "exit_code": null, "duration_ms": 12,
            "summary": "1 secret", "output_tail": "",
            "violations": [{"limit": "secret", "detail": "an AWS key", "paths": ["src/a.ts"]}],
            "sandboxed": false}}),
        review(
            json!({"event": "reviewed", "reviewer": "claude-code/opus", "blind": true,
            "round": 1, "review": {"verdict": "request_changes", "summary": "one bug",
                "comments": [{"path": "src/a.ts", "line": 3, "severity": "medium",
                              "text": "off by one"}],
                "tamper_suspected": false}}),
        ),
        review(json!({"event": "settled", "judge": "codex",
            "ruling": {"verdict": "approve", "reasons": ["fixed"], "required_changes": []}})),
        review(
            json!({"event": "tests_written", "reviewer": "codex", "for_task": "task-003",
            "files": ["tests/limits.test.ts"]}),
        ),
        merge_event(
            json!({"event": "branch_created", "branch": "argonauts/run-01/merged",
            "base": "8b41d07"}),
        ),
        merge_event(json!({"event": "merged", "task": "task-002", "sha": "a1"})),
        merge_event(
            json!({"event": "conflict", "task": "task-003", "files": ["src/types.ts"],
            "assigned_to": "claude-code/opus"}),
        ),
        merge_event(
            json!({"event": "main_moved", "from": "8b41d07", "to": "9c52e18",
            "commits": 3}),
        ),
        merge_event(json!({"event": "main_merged_in", "sha": "b2"})),
        merge_event(json!({"event": "final_check", "passed": true})),
        merge_event(
            json!({"event": "run_merged", "target": "main", "sha": "d4", "fast_forward": true}),
        ),
        merge_event(
            json!({"event": "pull_request", "branch": "argonauts/run-01/merged",
            "url": null, "command": ["gh", "pr", "create"]}),
        ),
        merge_event(json!({"event": "abandoned", "branches": ["argonauts/run-01/task-002"]})),
        merge_event(json!({"event": "dependencies_added", "packages": ["ioredis"], "sha": "c1"})),
        approval_opened(
            "question",
            json!({"kind": "question", "from": "codex", "task": "task-002",
                   "text": "keep a fallback?", "options": ["yes", "no"]}),
            json!([option(
                "a",
                "yes",
                json!({"action": "choose", "value": "yes"})
            )]),
            "answer",
        ),
        approval_opened(
            "understand",
            json!({"kind": "understand", "goal": goal_file(), "agreements": ["429"], "assumptions": [],
                   "questions": []}),
            json!([option("y", "approve", json!({"action": "approve"}))]),
            "edits",
        ),
        approval_opened(
            "plan",
            json!({"kind": "plan", "summary": "Redis plan", "judging": null, "near_tie": true,
                   "decisions": [], "graph": {"tasks": [task_spec()], "critical_path": []},
                   "acceptance_tests": ["429 after the limit"],
                   "limit": {"lines": [], "total": usd(20.0), "estimate_low": 9.0,
                              "estimate_high": 12.0},
                   "new_dependencies": ["ioredis"], "flags": []}),
            json!([
                option("y", "approve", json!({"action": "approve"})),
                option(
                    "e",
                    "approve with edits",
                    json!({"action": "approve_with_edits"})
                ),
                option("b", "send back", json!({"action": "send_back"})),
                option(
                    "s",
                    "run it as Captain",
                    json!({"action": "switch_to_captain"})
                ),
                option("x", "stop", json!({"action": "stop"})),
                option("d", "show the plan", json!({"action": "show_diff"}))
            ]),
            "guidance",
        ),
        approval_opened(
            "merge",
            json!({"kind": "merge", "target": "main", "branch": "argonauts/run-01/merged",
                   "rows": [{"task": "task-002", "title": "Redis store",
                             "kind": {"kind": "build"}, "stat": diff_stat(), "worker": "codex",
                             "checks_passed": true, "review": "approve"}],
                   "suite_passed_on_latest_main": true, "checks": [], "combined": diff_stat(),
                   "diff_path": "/state/combined.diff", "tree_sha": "t1", "spent": spend(),
                   "limit": usd(20.0), "partial": false}),
            json!([
                option("y", "merge", json!({"action": "merge"})),
                option(
                    "p",
                    "open a pull request",
                    json!({"action": "pull_request"})
                ),
                option("b", "request changes", json!({"action": "request_changes"})),
                option("k", "keep the branch", json!({"action": "keep_branch"})),
                option("x", "abandon", json!({"action": "abandon"}))
            ]),
            "guidance",
        ),
        approval_opened(
            "fallback",
            with(json!({"kind": "fallback"}), fallback_plan()),
            json!([
                option(
                    "a",
                    "codex",
                    json!({"action": "candidate", "worker": "codex/gpt-x"})
                ),
                option("t", "take over myself", json!({"action": "take_over"})),
                option("w", "wait", json!({"action": "wait"})),
                option("o", "other agents", json!({"action": "other_candidates"}))
            ]),
            "none",
        ),
        approval_opened(
            "agent_down",
            json!({"kind": "agent_down", "agent_id": "claude-code",
                   "evidence": "overload errors on 3 sessions in 2 min", "test_at": null,
                   "paused": [{"task": "task-003", "title": "Middleware", "role": "worker"}]}),
            json!([
                option("a", "one agent for all", json!({"action": "one_for_all"})),
                option("e", "choose per task", json!({"action": "per_task"})),
                option("w", "wait", json!({"action": "wait"}))
            ]),
            "none",
        ),
        approval_opened(
            "limit",
            json!({"kind": "limit", "task": "task-002", "status": limit_status(),
                   "dimension": "spend", "reserve_left": usd(2.0)}),
            json!([
                option("m", "raise the limit", json!({"action": "raise_limit"})),
                option("h", "fall back", json!({"action": "fallback"})),
                option("x", "ask_you", json!({"action": "ask_you"}))
            ]),
            "answer",
        ),
        approval_opened(
            "stuck",
            json!({"kind": "stuck", "task": "task-002", "title": "Redis store",
                   "state": "needs_you", "tries": ["codex: checks failed"],
                   "last_failure": "tests failed"}),
            json!([
                option("r", "retry", json!({"action": "retry"})),
                option("s", "split", json!({"action": "split"})),
                option("f", "defer", json!({"action": "defer"})),
                option("x", "cancel", json!({"action": "cancel"}))
            ]),
            "guidance",
        ),
        approval_opened(
            "change_request",
            json!({"kind": "change_request", "from": {"party": "user"}, "task": "task-002",
                   "text": "widen the trait", "paths": ["src/types.ts"],
                   "why_you": "changes a frozen interface", "paused": ["task-003"]}),
            json!([]),
            "none",
        ),
        approval_opened(
            "goal_change",
            json!({"kind": "goal_change", "from": {"party": "user"}, "text": "also admin",
                   "clarification": false, "goal": goal_file()}),
            json!([]),
            "none",
        ),
        approval_opened(
            "take_over_return",
            json!({"kind": "take_over_return", "task": "task-002", "worker": "codex",
                   "changed": diff_stat(), "commit": "f00d"}),
            json!([option(
                "c",
                "let the worker continue",
                json!({"action": "let_continue"})
            )]),
            "none",
        ),
        approval_opened(
            "workflow_promotion",
            json!({"kind": "workflow_promotion", "learned_workflow": "api-feature", "gain": 3.5}),
            json!([]),
            "none",
        ),
        approval(
            json!({"event": "checked", "id": "approval-003", "violations": [],
            "verdict": "approve", "by": "rules", "judge": null,
            "reasons": ["all checks passed"], "auto_preview": false}),
        ),
        approval(json!({"event": "decided", "id": "approval-003",
            "action": {"action": "choose", "value": "per API key"}, "text": null,
            "by": {"party": "judge", "model": "codex/gpt-x"}, "evidence_sha256": "e3b0",
            "auto_preview": {"would": "approve", "by": "rules_and_model", "reasons": ["checks passed"]},
            "tasks": []})),
        approval(json!({"event": "decided", "id": "approval-004",
            "action": {"action": "request_changes"}, "text": "cap at 100/min",
            "by": {"party": "user"}, "evidence_sha256": "e3b1", "auto_preview": null,
            "tasks": ["task-002"]})),
        approval(json!({"event": "withdrawn", "id": "approval-003", "reason": "evidence changed"})),
        approval(json!({"event": "spot_check_queued", "id": "approval-003"})),
        fallback(with(json!({"event": "needed"}), fallback_plan())),
        fallback(
            json!({"event": "note", "task": "task-002", "path": "/state/tasks/p.md",
            "sha256": "5a"}),
        ),
        fallback(
            json!({"event": "fell_back", "task": "task-002", "to": "codex/gpt-x",
            "mode": "auto", "by": {"party": "rules"}}),
        ),
        fallback(
            json!({"event": "waiting", "task": "task-002", "agent_id": "claude-code",
            "until": "2026-09-29T18:00:00Z"}),
        ),
        agent_status(with(json!({"event": "plan_usage"}), plan_usage_reading())),
        agent_status(
            json!({"event": "health", "agent_id": "codex", "state": "down",
            "since": "2026-09-29T12:00:00Z", "reason": "overload on 3 sessions",
            "next_test_at": "2026-09-29T12:10:00Z", "disabled": null}),
        ),
        agent_status(json!({"event": "disabled", "agent_id": "codex",
            "reason": "untested version 0.170.0; pin 0.158.0"})),
        limit(json!({"event": "run", "spent": spend(), "limit": usd(20.0),
            "lines": [{"line": "tasks", "allocated": usd(6.5), "used": 1.25}],
            "reserve_left": null})),
        limit(
            json!({"event": "hit", "task": "task-002", "dimension": "spend",
            "status": limit_status()}),
        ),
        limit(
            json!({"event": "extended", "task": "task-002", "amount": usd(1.0),
            "by": {"party": "user"}}),
        ),
        limit(json!({"event": "deferred", "tasks": ["task-004"]})),
        blackboard(json!({"event": "posted", "id": "m-002", "kind": "question",
            "from": {"party": "agent", "session": "s-004", "worker": "codex"},
            "to": {"to": "task", "task": "task-003"}, "text": "which header name?",
            "in_reply_to": null, "refs": ["src/types.ts"]})),
        blackboard(json!({"event": "posted", "id": "m-003", "kind": "guidance",
            "from": {"party": "user"}, "to": {"to": "all"}, "text": "keep it small",
            "in_reply_to": null, "refs": []})),
        blackboard(json!({"event": "delivered", "message": "m-002", "to": "s-005"})),
        run_review(
            json!({"event": "scored", "task": "task-002", "metric": "p_pass_checks_first_try",
            "predicted": 0.7, "outcome": true, "brier": 0.09}),
        ),
        run_review(
            json!({"event": "outcome", "task": "task-002", "worker": "codex",
            "task_type": "api-feature", "first_try": true, "tries": 1, "merged": true}),
        ),
        run_review(json!({"event": "memory", "from": "codex",
            "proposal": {"topic": "redis", "text": "use EVAL"}, "accepted": false,
            "reason": "names a credential"})),
        run_review(
            json!({"event": "committed", "sha": "e5", "paths": [".argonauts/notes/redis.md"]}),
        ),
        run_review(
            json!({"event": "candidate", "setting": "critique.rounds", "value": "0",
            "why": "critique changed no plan in 5 runs"}),
        ),
        json!({"type": "note", "data": {"level": "warn", "text": "2 uncommitted files left out"}}),
        run(json!({"event": "named", "name": "fix-slugify-median",
            "description": "Fix slugify's dashes and median's empty list", "style": "workflow",
            "based_on": "council"})),
        json!({"type": "workflow", "data": {"event": "declared", "id": "wf-001",
            "name": "fix-slugify-median", "description": "Fix slugify and median",
            "phases": [{"index": 0, "name": "Fix", "detail": "one worker per function",
                        "standard": "do"},
                       {"index": 1, "name": "Merge", "standard": "decide"}]}}),
        json!({"type": "workflow", "data": {"event": "phase_started", "id": "wf-001",
            "index": 0, "name": "Fix"}}),
        json!({"type": "workflow", "data": {"event": "phase_ended", "id": "wf-001",
            "index": 0, "name": "Fix", "end": "done"}}),
        json!({"type": "workflow", "data": {"event": "phase_added", "id": "wf-001",
            "phase": {"index": 1, "name": "Review", "detail": "a second reviewer",
                      "standard": "check"},
            "why": "the reviewers disagreed"}}),
        json!({"type": "workflow", "data": {"event": "phase_skipped", "id": "wf-001",
            "index": 0, "name": "Understand", "why": "Your prompt is the goal"}}),
        json!({"type": "workflow", "data": {"event": "note",
            "text": "review:median found median([]) returns None, so a second round",
            "workflow": "wf-001"}}),
        goal(
            json!({"event": "set", "objective": "median handles empty lists",
            "criteria": [{"id": "G1", "text": "median([]) raises", "short": "Empty list",
                          "check": {"test": "test_median_empty"}}],
            "tries": [], "status": "pursuing", "scope": "workflow",
            "kept_by": "argonauts", "check": "auto"}),
        ),
        // A task worked as a goal, kept by Codex's own, with a cap you set.
        goal(
            json!({"event": "set", "objective": "add the overdue command",
            "criteria": [{"id": "G1", "text": "every test in tests/overdue.rs passes",
                          "check": {"command": "cargo test overdue"}},
                         {"id": "G2", "text": "its own tests pass", "check": "checks"}],
            "tries": [], "status": "pursuing", "max_rounds": 5, "task": "task-002",
            "scope": "task", "kept_by": "cli", "check": "both",
            "commands": ["cargo test --test acceptance"], "from": "role"}),
        ),
        goal(json!({"event": "try_started", "n": 1, "title": "Empty List", "phase": 1})),
        goal(
            json!({"event": "try_started", "n": 2, "title": "try 2", "phase": 0,
            "task": "task-002"}),
        ),
        goal(json!({"event": "checked", "try_number": 1,
            "verdicts": [{"id": "G1", "verdict": "cant_tell", "evidence": "",
                          "missing": "no test runs it"}],
            "overall": "not_yet", "reason": "G1 has no test yet"})),
        goal(json!({"event": "checked", "try_number": 3,
            "verdicts": [{"id": "G1", "verdict": "not_yet", "evidence": "",
                          "missing": "cargo test overdue: 1 of 12 fails"}],
            "overall": "not_yet", "reason": "one test still fails", "task": "task-002",
            "checks": [
                {"name": "cargo test overdue", "by": "command", "lines": ["G1"],
                 "passed": false, "summary": "11 passed, 1 failed", "failed": 1, "total": 12,
                 "ms": 4100, "output": "overdue::skips_tasks_without_a_date"},
                {"name": "the judge", "by": "judge", "lines": ["G2"], "passed": true,
                 "summary": "1 of 1 line met", "ms": 9000, "agent": "claude-code/opus"}]})),
        goal(
            json!({"event": "status_changed", "status": "stuck", "by": {"party": "argonauts_code"}}),
        ),
        goal(
            json!({"event": "status_changed", "status": "stuck", "by": {"party": "argonauts_code"},
            "task": "task-002",
            "why": "the same failure 3 rounds running, after a change of approach"}),
        ),
        goal(json!({"event": "approach_changed", "after": 3,
            "why": "the same failure 3 rounds running", "task": "task-002"})),
        goal(json!({"event": "cli_said", "task": "task-002",
            "said": {"cli": "codex", "status": "active",
                     "note": "one test left: tasks without a date", "turns": 14}})),
        goal(json!({"event": "answered", "question": "Accented letters?",
            "answer": "Keep them", "by": "main_agent"})),
        goal(json!({"event": "assumption_changed", "id": 2, "to": "Strip them", "by": "you"})),
        json!({"type": "agent", "data": {"type": "workflow", "workflow": {"id": "w4d273qmn",
            "call": "toolu_01", "name": "probe", "description": "Asks and answers",
            "phases": [{"index": 1, "title": "Ask"}, {"index": 2, "title": "Answer"}],
            "agents": [{"index": 1, "label": "ask:one", "phase": 1,
                        "model": "claude-haiku-4-5-20251001", "state": "done", "tokens": 10311,
                        "tool_calls": 0, "duration_ms": 3266, "started_at_ms": 1_791_076_604_610_u64,
                        "try_number": 1, "prompt": "Respond with a single word",
                        "result": "Ready"}],
            "status": "running", "tokens": 10311, "tool_calls": 0, "duration_ms": 3252,
            "summary": "Ask: ask:one"}}}),
    ]
}

#[test]
fn every_event_kind_round_trips_in_the_contract_wire_format() {
    let kinds = every_event_kind();
    assert!(kinds.len() > 100, "{} kinds", kinds.len());
    for kind in kinds {
        round_trip::<RunEvent>(envelope(kind));
    }
}

#[test]
fn optional_envelope_fields_are_left_out_when_empty() {
    let event = RunEvent {
        seq: 0,
        ts: OffsetDateTime::UNIX_EPOCH,
        run_id: RunId::new("run-01").unwrap(),
        task_id: None,
        session: None,
        source: "user".into(),
        kind: RunEventKind::Run(RunLifecycle::PauseCancelled),
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(
        value,
        json!({"seq": 0, "ts": "1970-01-01T00:00:00Z", "run_id": "run-01", "source": "user",
               "kind": {"type": "run", "data": {"event": "pause_cancelled"}}})
    );
    assert_eq!(round_trip::<RunEvent>(value), event);
}

#[test]
fn a_decided_approval_built_in_rust_round_trips() {
    let decided = RunEventKind::Approval(ApprovalEvent::Decided(ApprovalDecided {
        id: ApprovalId::numbered(1),
        action: AnswerAction::Choose {
            value: "per API key".into(),
        },
        text: None,
        by: Party::User,
        evidence_sha256: "ab".into(),
        auto_preview: None,
        tasks: Vec::new(),
    }));
    let value = serde_json::to_value(&decided).unwrap();
    assert_eq!(value["type"], "approval");
    assert_eq!(value["data"]["event"], "decided");
    assert_eq!(
        serde_json::from_value::<RunEventKind>(value).unwrap(),
        decided
    );
}

/// The run's journal redacts an event as a JSON value and reads it back from that value, so a
/// report has to survive the same way, with the session behind each of its agents.
#[test]
fn a_workflow_report_built_in_rust_round_trips_through_a_json_value() {
    use argonauts_core::events::WorkflowEvent;
    use argonauts_core::events::session::{
        CliAgent, CliAgentState, CliPhase, CliWorkflowRun, SubagentStatus,
    };
    use argonauts_core::events::workflow::{AgentSession, WorkflowReport};
    use argonauts_core::names::WorkflowId;
    use argonauts_core::workflow::StepRef;

    let report = WorkflowReport {
        step: StepRef {
            workflow: WorkflowId::numbered(1),
            phase: 0,
            path: vec![0],
        },
        workflow: CliWorkflowRun {
            id: "wf-001".into(),
            call: None,
            name: "add-due-dates".into(),
            description: "Add due dates to tasks".into(),
            phases: vec![CliPhase {
                index: 1,
                title: "Build".into(),
                detail: "one worker per task".into(),
            }],
            agents: vec![CliAgent {
                index: 1,
                label: "build:due-field".into(),
                phase: Some(1),
                model: Some("GPT 5.6 on Codex".into()),
                state: CliAgentState::Running,
                tokens: 1200,
                tool_calls: 3,
                duration_ms: 4100,
                started_at_ms: Some(1_791_076_604_610),
                try_number: 1,
                prompt: None,
                result: None,
                ..CliAgent::default()
            }],
            status: SubagentStatus::Running,
            tokens: 1200,
            tool_calls: 3,
            duration_ms: 4100,
            summary: None,
            started_at_ms: Some(1_791_076_604_000),
        },
        sessions: vec![AgentSession {
            agent: 1,
            session: SessionKey::numbered(2),
        }],
        size_asked: None,
    };
    assert_eq!(report.session_of(1), Some(&SessionKey::numbered(2)));
    assert_eq!(report.session_of(2), None);
    let reported = RunEventKind::Workflow(WorkflowEvent::Reported(Box::new(report)));
    let value = serde_json::to_value(&reported).unwrap();
    assert_eq!(value["type"], "workflow");
    assert_eq!(value["data"]["event"], "reported");
    assert_eq!(
        serde_json::from_value::<RunEventKind>(value).unwrap(),
        reported
    );
}

#[test]
fn every_ui_command_round_trips() {
    let commands = vec![
        json!({"type": "submit", "text": "hi"}),
        json!({"type": "slash", "command": {"command": "captain", "task": "fix the typo"}}),
        json!({"type": "slash", "command": {"command": "reassign", "task": null,
               "from": "claude-code", "to": "codex/gpt-x"}}),
        json!({"type": "slash", "command": {"command": "narrator",
               "change": {"change": "model", "model": "ollama/qwen3"}}}),
        json!({"type": "mention", "target": {"target": "role", "value": "reviewer"},
               "text": "be strict"}),
        json!({"type": "answer", "to": {"target": "approval", "run": "run-01", "id": "approval-003"},
               "action": {"action": "approve_with_edits"}, "text": "cap at 100/min",
               "evidence_sha256": "e3b0", "tasks": []}),
        json!({"type": "answer", "to": {"target": "approval", "run": "run-01", "id": "approval-009"},
               "action": {"action": "request_changes"}, "text": "tighten the limit",
               "evidence_sha256": "e3b0", "tasks": ["task-002", "task-004"]}),
        json!({"type": "answer", "to": {"target": "choice", "id": "ch-002"},
               "action": {"action": "choose", "value": "captain"}, "text": null,
               "evidence_sha256": null, "tasks": []}),
        json!({"type": "answer", "to": {"target": "approval", "run": "run-01", "id": "approval-005"},
               "action": {"action": "candidate", "worker": "codex/gpt-x"}, "text": null,
               "evidence_sha256": "e3b0", "tasks": [], "remember": true}),
        json!({"type": "set_aside", "to": {"target": "choice", "id": "ch-002"}}),
        json!({"type": "steer", "run": "run-01", "session": "s-003", "text": "use EVAL"}),
        json!({"type": "take_over", "run": "run-01", "task": "task-002"}),
        json!({"type": "take_over_ended", "run": "run-01", "task": "task-002", "exit_code": 0}),
        json!({"type": "reassign", "run": "run-01", "task": "task-002", "to": "codex"}),
        json!({"type": "pause", "now": true}),
        json!({"type": "cancel_pause"}),
        json!({"type": "resume", "run": null}),
        json!({"type": "stop"}),
        json!({"type": "stop_agent", "run": "run-01", "session": "s-003"}),
        json!({"type": "take_back", "run": "run-01", "messages": ["m-001", "m-002"]}),
        json!({"type": "approval_mode", "mode": "auto"}),
        json!({"type": "verbosity", "level": "full"}),
        json!({"type": "vim", "on": true}),
        json!({"type": "narrator", "mode": "live"}),
        json!({"type": "cancel_turn"}),
        json!({"type": "focus", "focused": false}),
        json!({"type": "spot_check", "run": "run-01", "id": "approval-003", "correct": true}),
        json!({"type": "request", "view": {"view": "resume_list"}}),
        json!({"type": "request", "view": {"view": "all_repos"}}),
        json!({"type": "request", "view": {"view": "workflow_library"}}),
        json!({"type": "request", "view": {"view": "diff", "run": "run-01", "task": "task-002",
               "scope": "last_turn"}}),
        json!({"type": "request", "view": {"view": "raw", "run": "run-01", "session": "s-003"}}),
        json!({"type": "request", "view": {"view": "settings", "section": "limit"}}),
        json!({"type": "request", "view": {"view": "models", "role": "reviewer"}}),
        json!({"type": "set_setting", "key": "approvals.merge.mode", "value": "\"auto\"",
               "scope": "project"}),
        json!({"type": "set_setting", "key": "roles.reviewer.effort", "value": "\"high\"",
               "scope": "project_local"}),
        json!({"type": "set_setting", "key": "limits.spend_per_run", "value": "30.0",
               "scope": "session"}),
        json!({"type": "set_setting", "key": "ui.theme", "value": "\"light\"",
               "scope": "global"}),
        json!({"type": "slash", "command": {"command": "workflow", "name": "orchestrator",
               "action": "copy_to_project"}}),
        json!({"type": "slash", "command": {"command": "workflow", "name": null, "action": "use"}}),
        json!({"type": "slash", "command": {"command": "workflow", "name": "council",
               "action": "show"}}),
        json!({"type": "slash", "command": {"command": "workflow", "name": "council",
               "action": "copy"}}),
        json!({"type": "slash", "command": {"command": "workflow", "name": "council",
               "action": "diff"}}),
        json!({"type": "slash", "command": {"command": "effort", "role": null,
               "level": "high"}}),
        json!({"type": "slash", "command": {"command": "effort", "role": "reviewer",
               "level": null}}),
        json!({"type": "slash", "command": {"command": "agents",
               "action": {"kind": "add", "name": "ollama"}}}),
        json!({"type": "slash", "command": {"command": "agents", "action": {"kind": "list"}}}),
        json!({"type": "slash", "command": {"command": "limits", "key": "spend_per_run",
               "value": "30"}}),
        json!({"type": "slash", "command": {"command": "fallback", "role": "worker",
               "key": "mode", "value": "auto"}}),
        json!({"type": "slash", "command": {"command": "permissions", "key": null,
               "value": null}}),
        json!({"type": "slash", "command": {"command": "permissions",
               "key": "sandbox.network", "value": "true"}}),
        json!({"type": "slash", "command": {"command": "permissions", "role": "worker",
               "key": "mode", "value": "tool"}}),
        json!({"type": "slash", "command": {"command": "approvals", "key": null,
               "value": null}}),
        json!({"type": "slash", "command": {"command": "approvals", "key": "merge.mode",
               "value": "auto"}}),
        json!({"type": "slash", "command": {"command": "theme", "name": "light"}}),
        json!({"type": "slash", "command": {"command": "setup"}}),
        json!({"type": "slash", "command": {"command": "clear"}}),
        json!({"type": "slash", "command": {"command": "doctor"}}),
        json!({"type": "slash", "command": {"command": "connect", "outside_agent": "openclaw"}}),
        json!({"type": "slash", "command": {"command": "verbosity", "level": "quiet"}}),
        json!({"type": "slash", "command": {"command": "verbosity", "level": null}}),
        json!({"type": "slash", "command": {"command": "resume",
               "target": {"kind": "conversation", "id": "conversation-003"}}}),
        json!({"type": "slash", "command": {"command": "resume",
               "target": {"kind": "run", "id": "run-01"}}}),
        json!({"type": "slash", "command": {"command": "stop", "run": null}}),
        json!({"type": "slash", "command": {"command": "workflows"}}),
        json!({"type": "rewind", "task": "task-002", "checkpoint": 1}),
        json!({"type": "summary"}),
        json!({"type": "workflow_file", "name": "captain", "action": "copy_to_project"}),
        json!({"type": "slash", "command": {"command": "config", "key": "narrator.model",
               "value": null, "reset": true}}),
        json!({"type": "reset_setting", "key": "ui.theme", "scope": "global"}),
        json!({"type": "reset_setting", "key": "roles.reviewer.models",
               "scope": "session"}),
        json!({"type": "exit"}),
    ];
    for command in commands {
        round_trip::<UiCommand>(command);
    }
    // The empty forms keep their argument names on the wire.
    let empty = [
        json!({"command": "config", "key": null, "value": null, "reset": false}),
        json!({"command": "resume", "target": null}),
        json!({"command": "rewind", "task": null}),
        json!({"command": "theme", "name": null}),
        json!({"command": "limits", "key": null, "value": null}),
        json!({"command": "model", "role": null, "worker": null}),
        json!({"command": "compact", "instructions": null}),
        json!({"command": "copy", "nth": null}),
        json!({"command": "cd", "path": null}),
        json!({"command": "usage", "period": null}),
    ];
    for command in empty {
        round_trip::<UiCommand>(json!({"type": "slash", "command": command}));
    }
    // The commands the agent CLIs share, a CLI's own command, and its sessions.
    let shared = [
        json!({"command": "model", "role": "reviewer", "worker": "claude-code/opus",
               "effort": "xhigh"}),
        json!({"command": "resume", "target": {"kind": "cli_session",
               "id": {"agent_id": "codex", "id": "019a2f-4c"}}}),
        json!({"command": "rename", "name": "Login work"}),
        json!({"command": "branch", "name": null}),
        json!({"command": "fork", "prompt": "try the other parser"}),
        json!({"command": "btw", "question": "what is a rope?"}),
        json!({"command": "recap"}),
        json!({"command": "copy", "nth": 2}),
        json!({"command": "export", "file": "conversation.txt"}),
        json!({"command": "cd", "path": "../api"}),
        json!({"command": "add_dir", "path": "../shared"}),
        json!({"command": "plan", "task": "split the parser"}),
        json!({"command": "goal", "text": "clear"}),
        json!({"command": "fast", "on": true}),
        json!({"command": "output_style", "style": "brief"}),
        json!({"command": "diff"}),
        json!({"command": "review", "target": "main..HEAD"}),
        json!({"command": "security_review", "target": null}),
        json!({"command": "usage", "period": "weekly"}),
        json!({"command": "login", "agent": "codex"}),
        json!({"command": "logout", "agent": null}),
        json!({"command": "release_notes"}),
        json!({"command": "feedback", "text": "the menu is great"}),
        json!({"command": "cli", "name": "simplify", "args": "the parser"}),
        json!({"command": "cli", "name": "anthropic-skills:example-a"}),
    ];
    for command in shared {
        round_trip::<UiCommand>(json!({"type": "slash", "command": command}));
    }
    round_trip::<UiCommand>(json!({"type": "request", "view": {"view": "crew"}}));
    round_trip::<UiCommand>(json!({"type": "request", "view": {"view": "settings",
        "section": null}}));
    round_trip::<UiCommand>(json!({"type": "request", "view": {"view": "models",
        "role": null}}));
}

fn run_summary() -> Value {
    json!({"run": "run-01", "title": "Per-key rate limiting", "mode": "crew",
           "workflow": "council", "state": "waiting", "stopped_at": "plan approval",
           "updated": "2026-09-29T12:00:00Z", "spent": spend(), "sessions_to_resume": 2,
           "waiting": ["approval-003"], "live": false, "remind": true})
}

fn narrator_line() -> Value {
    json!({"ts": "2026-09-29T12:00:00Z", "run": "run-01",
           "kind": {"kind": "answer", "question": "why Redis?"},
           "text": "Counters survive restarts.", "model": "ollama/qwen3", "spend": spend()})
}

/// A welcome from an app that predates the header's agent and effort still reads.
#[test]
fn an_older_welcome_reads_without_the_agent_and_effort() {
    let event: AppEvent = serde_json::from_value(json!({"type": "welcome", "version": "0.0.1",
        "repo": "~/code/api", "workflow": "council", "agents": [], "runs_going": 0}))
    .unwrap();
    assert!(matches!(
        event,
        AppEvent::Welcome {
            main_agent: None,
            effort: None,
            ..
        }
    ));
}

#[test]
fn every_app_event_round_trips() {
    let mut events = vec![
        json!({"type": "welcome", "version": "0.0.1", "repo": "~/code/api", "workflow": "council",
               "agents": [{"id": "codex", "version": "0.158.0", "ready": true, "note": null}],
               "runs_going": 1, "runs_need_you": 1, "example_prompt": "fix the flaky retry test",
               "main_agent": "codex/gpt-5.6-luna", "effort": "high"}),
        json!({"type": "attached", "summary": run_summary()}),
        json!({"type": "attached", "summary": run_summary(), "started_here": true}),
        json!({"type": "detached", "run": "run-01"}),
        json!({"type": "background_runs", "runs": [run_summary()]}),
        json!({"type": "run", "event": envelope(json!({"type": "note",
               "data": {"level": "info", "text": "x"}}))}),
        json!({"type": "conversation", "event": {"conversation": "turn_started", "turn": "t-012", "text": "hi",
               "worker": "claude-code", "read_only": true, "commit": "8b41d07",
               "uncommitted": ["notes.txt"], "settings_sha256": "5e7f"}}),
        json!({"type": "conversation", "event": {"conversation": "agent", "turn": "t-012",
               "event": {"type": "text_delta", "text": "Hel", "parent": null}}}),
        json!({"type": "conversation", "event": {"conversation": "turn_finished", "turn": "t-012",
               "reply": {"reply": "unclear", "markdown": "Could be either.", "estimate": null,
                         "would_pick": "captain"},
               "spend": spend(), "elapsed_ms": 4100}}),
        json!({"type": "conversation", "event": {"conversation": "turn_failed", "turn": "t-013",
               "error": "usage limit"}}),
        json!({"type": "conversation", "event": {"conversation": "turn_cancelled", "turn": "t-014"}}),
        json!({"type": "choice", "choice": {"id": "ch-002", "kind": "start_work",
               "title": "Start work on this?", "body": ["Argonauts Code would pick Captain."],
               "options": [{"key": "s", "label": "captain", "action": {"action": "choose",
                            "value": "captain"}, "recommended": true, "detail": null}],
               "free_text": "none", "bell": true}}),
        json!({"type": "choice_closed", "id": "ch-002", "outcome": "No run started."}),
        json!({"type": "narrator", "line": narrator_line()}),
        json!({"type": "narrator_status", "status": {"mode": "live", "model": "ollama/qwen3",
               "spent": spend(), "cap": "$0.50", "capped": false, "interval_s": 30}}),
        json!({"type": "mode", "mode": {"mode": "idle"}}),
        json!({"type": "mode", "mode": {"mode": "waiting", "run": "run-01",
               "reason": {"why": "approval"}}}),
        json!({"type": "status", "status": {"title": "run-01", "repo": "~/code/api",
               "workflow": "council", "run": "run-01", "main_agent": "claude-code",
               "plan_chips": [{"label": "Claude", "reading": plan_usage_reading(), "health": "healthy"}],
               "spent": spend(), "limit": usd(20.0), "narrator": "on_demand",
               "approval_mode": "supervised"}}),
        json!({"type": "status", "status": {"title": "", "repo": "~/code/api",
               "workflow": "captain", "ultracode": true, "run": null, "main_agent": "claude-code",
               "plan_chips": [{"label": "Claude", "reading": plan_usage_reading(), "health": "healthy",
                           "windows": [plan_usage_reading()]}],
               "spent": null, "limit": null, "narrator": "off",
               "approval_mode": "auto",
               "line": {"settings": {"form": "compact", "left": ["mode", "branch"],
                                     "right": ["plan-usage", "spend"],
                                     "plan-usage": {"show": "main", "windows": "5h",
                                                    "as": "left", "resets": "always"},
                                     "narrow": {"drop": ["spend"]}},
                        "branch": "main", "spend_days": [0.0, 0.42, 1.44],
                        "artifact": {"name": "weekly-usage-chart", "others": 2}}}}),
        json!({"type": "notice", "level": "warn", "text": "codex is untested at 0.170.0"}),
        json!({"type": "view", "data": {"data": "config", "entries": [{"key": "ui.keys",
               "value": "\"vim\"", "layer": "global", "safety": false}]}}),
        json!({"type": "view", "data": {"data": "approvals", "view": {"points": [{"kind": "merge",
               "mode": "auto_preview", "decide_with": "rules", "matches_in_a_row": 4, "misses": 0,
               "suggest_auto": false}], "spot_checks_waiting": 1, "judge": null}}}),
        json!({"type": "view", "data": {"data": "diff", "diff": {"task": "task-002",
               "stat": diff_stat(), "text": "diff --git", "path": "/state/tasks/d"}}}),
        json!({"type": "view", "data": {"data": "raw", "raw": {"session": "s-003",
               "lines": ["{}"], "truncated": false, "path": "/state/raw", "cli_transcript": null}}}),
        json!({"type": "take_over", "ticket": {"task": "task-002",
               "command": {"program": "codex", "args": ["resume", "abc"], "env_set": [],
                           "env_remove": [], "stdin": null, "cwd": "/wt/task-002", "files": [],
                           "keep_stdin_open": false},
               "worktree": "/wt/task-002", "agent_id": "codex"},
               "place": "tmux-split", "run": "run-01"}),
        json!({"type": "ui_settings", "vim": true, "verbosity": "normal", "ui": null}),
        json!({"type": "ui_settings", "vim": false, "verbosity": "normal",
               "ui": serde_json::to_value(UiSettings::default()).unwrap()}),
        json!({"type": "recap", "text": "Task 2 is in review.", "model": null}),
        json!({"type": "taken_back", "texts": ["keep a fallback"]}),
        json!({"type": "setting_changed", "key": "approvals.merge.mode", "value": "\"auto\"",
               "scope": "project", "previous": "\"supervised\"", "reset": false}),
        json!({"type": "replay_done", "run": "run-01", "seq": 412}),
        json!({"type": "past_begins"}),
        json!({"type": "past_drawn"}),
        json!({"type": "view", "data": {"data": "resume_list",
               "conversations": [{"conversation": "conversation-002", "title": "hi", "updated": "2026-09-29T12:00:00Z",
                          "worker": "claude-code", "turns": 3, "needs_you": true}]}}),
        json!({"type": "conversation_resumed", "conversation": "conversation-002", "title": "hi", "worker": "claude-code",
               "turns": [{"from": {"party": "user"}, "text": "what does export do?"},
                         {"from": {"party": "agent", "session": "t-002", "worker": "claude-code"},
                          "text": "It writes CSV."}]}),
        json!({"type": "report", "title": "Doctor",
               "lines": ["claude-code 2.1.300 · tested · logged in", "git 2.51.2"]}),
        json!({"type": "choice", "choice": {"id": "ch-003", "kind": "confirm_connect",
               "title": "Add the `argonauts` server to ~/.hermes/config.yaml?",
               "body": ["~/.hermes/config.yaml is copied to ~/.hermes/config.yaml.argonauts-backup first."],
               "options": [{"key": "y", "label": "Write it", "action": {"action": "approve"},
                            "recommended": false, "detail": null}],
               "free_text": "none", "bell": false}}),
        json!({"type": "view", "data": {"data": "resume_list", "sessions": [
               {"agent_id": "claude-code", "id": "0a1b-2c3d", "title": "Fix the flaky test",
                "folder": "/repo", "updated": "2026-09-29T12:00:00Z", "model": "claude-opus-5-5"}]}}),
        json!({"type": "view", "data": {"data": "effort", "slider": {"role": "reviewer",
               "worker": "claude-code/claude-opus-5-5", "model": "Opus 5.5",
               "stops": [{"level": "low", "description": "Quick answers with light reasoning"},
                         {"level": "xhigh", "description": "Extra depth"}],
               "chosen": 1, "default": 1}}}),
        json!({"type": "view", "data": {"data": "crew", "panel": {"repo": "~/app", "rows": [
               {"target": {"kind": "conversation", "conversation": "conversation-002"}, "state": "ready",
                "title": "Explain the input handling", "repo": "~/app",
                "worker": "claude-code", "latest": "It reads keys in one loop.",
                "updated": "2026-09-29T12:00:00Z", "details": {}}]}}}),
        json!({"type": "cli_commands", "set": {"agent_id": "claude-code", "title": "Claude Code",
               "commands": [
                   {"name": "clear", "aliases": ["reset", "new"], "help": "New conversation",
                    "kind": "argonauts", "command": "clear", "source": "built_in"},
                   {"name": "ship", "args": "[branch]", "help": "Ship it", "kind": "cli",
                    "source": "custom", "scope": "project", "path": "/repo/.claude/commands/ship.md"},
                   {"name": "voice", "help": "Talk to it", "kind": "screen_only",
                    "source": "built_in"}],
               "keywords": [{"word": "ultrathink", "look": "rainbow", "echoed": true}]}}),
        json!({"type": "cli_commands", "set": {"agent_id": "codex", "title": "Codex",
               "commands": []}}),
        json!({"type": "view", "data": {"data": "all_repos", "repos": [
               {"repo": "~/src/api", "runs": [run_summary()]},
               {"repo": "~/src/web", "runs": []}]}}),
        json!({"type": "view", "data": {"data": "workflow_library", "workflows": [
               {"name": "captain", "origin": "yours", "source": "~/.argonauts/workflows/captain.yaml",
                "description": "One agent", "text": "name: captain\n", "edited": true,
                "in_use": false}], "refresh": true}}),
        json!({"type": "view", "data": {"data": "checkpoints", "run": "run-01",
               "task": "task-002", "checkpoints": [{"task": "task-002", "n": 1,
               "at": "2026-09-29T12:00:00Z", "commit": "0123456",
               "from": "claude-code/opus", "to": ["codex"], "reason": "usage limit",
               "rewound_here": false}]}}),
        json!({"type": "view", "data": {"data": "settings", "rows": [{"key": "limits.spend_per_run",
               "value": "20.0", "layer": "built-in", "default_scope": "project", "safety": true,
               "allowed": [], "description": "Run ceiling in dollars"}], "command": "limits",
               "section": null}}),
        json!({"type": "view", "data": {"data": "models", "choices": [{"worker": "codex/gpt-x",
               "provider": "openai", "access": "subscription", "cost": {"unit": "plan_usage", "amount": 0.02},
               "plan_usage": null, "efforts": ["low", "high"], "strengths": ["refactor"],
               "current": true}], "role": "reviewer", "refreshed": "2026-10-01T09:30:00Z",
               "refreshing": true, "update": false}}),
        json!({"type": "narrator", "line": {"ts": "2026-09-29T12:00:00Z", "run": "run-01",
               "kind": {"kind": "phase_end", "phase": "planning"},
               "text": "Plan 2 won.", "model": "ollama/qwen3", "spend": spend()}}),
        json!({"type": "choice", "choice": {"id": "ch-003", "kind": "setting_scope",
               "title": "Save where?", "body": [], "options": [], "free_text": "none",
               "bell": false}}),
        json!({"type": "choice", "choice": {"id": "ch-004", "kind": "setting_scope",
               "title": "Save effort = \"high\" where?",
               "body": ["Your own project file sets this, so saving it for the project won't change it for you."],
               "options": [
                   {"key": "s", "label": "Just this session",
                    "action": {"action": "choose", "value": "session"}, "recommended": false,
                    "detail": null},
                   {"key": "m", "label": "This project, just me",
                    "action": {"action": "choose", "value": "project_local"}, "recommended": true,
                    "detail": ".argonauts/config.local.toml"}],
               "free_text": "none", "bell": false}}),
        json!({"type": "setting_changed", "key": "roles.reviewer.effort", "value": "\"high\"",
               "scope": "project_local", "previous": null, "reset": false}),
        json!({"type": "setting_changed", "key": "limits.spend_per_run", "value": "30.0",
               "scope": "session", "previous": "20.0", "reset": false}),
        json!({"type": "setting_changed", "key": "ui.keys", "value": "\"vim\"",
               "scope": "global", "previous": "\"default\"", "reset": false}),
        // A reset: the value now in effect from the layers below, empty when unset.
        json!({"type": "setting_changed", "key": "ui.theme", "value": "\"dark\"",
               "scope": "global", "previous": "\"light\"", "reset": true}),
        json!({"type": "setting_changed", "key": "narrator.model", "value": "",
               "scope": "project_local", "previous": "\"ollama/llama3.2\"", "reset": true}),
        json!({"type": "view", "data": {"data": "settings", "rows": [{"key": "agents.codex.model",
               "value": "\"gpt-x\"", "layer": "project-local", "default_scope": "project_local",
               "safety": false, "allowed": [], "description": "The model this CLI uses"},
               {"key": "approvals.merge.mode", "value": "\"supervised\"", "layer": "session",
               "default_scope": "project", "safety": true,
               "allowed": ["\"supervised\"", "\"auto_preview\"", "\"auto\""],
               "description": "The merge approval"}], "command": null,
               "section": "approvals"}}),
        json!({"type": "view", "data": {"data": "models", "choices": [{"worker": "ollama/llama3.2",
               "provider": "meta", "access": "local", "cost": {"unit": "free"},
               "plan_usage": null, "efforts": [], "strengths": [], "current": false}],
               "role": null, "refreshed": null, "refreshing": false, "update": true}}),
        json!({"type": "view", "data": {"data": "models", "choices": [
               {"worker": "claude-code", "provider": "anthropic", "access": "subscription",
                "cost": {"unit": "plan_usage", "amount": 0.02}, "plan_usage": null, "efforts": [],
                "strengths": [], "current": false, "description": "Best for everyday, complex tasks",
                "recommended": true, "runs": "Opus 5.5"},
               {"worker": "claude-code/claude-opus-5-5", "provider": "anthropic",
                "access": "subscription", "cost": {"unit": "plan_usage", "amount": 0.02},
                "plan_usage": null, "efforts": ["low", "medium", "high", "xhigh", "max"],
                "strengths": [], "current": true, "name": "Opus 5.5",
                "description": "For complex work and everyday tasks", "default_effort": "xhigh",
                "effort": "max"},
               {"worker": "claude-code/opus", "provider": "anthropic", "access": "subscription",
                "cost": {"unit": "unknown"}, "plan_usage": null, "efforts": [], "strengths": [],
                "current": false, "alias": true, "runs": "Opus 5.5"},
               {"worker": "claude-code/claude-opus-6", "provider": "anthropic",
                "access": "subscription", "cost": {"unit": "unknown"}, "plan_usage": null,
                "efforts": [], "strengths": [], "current": false, "name": "Opus 6", "new": true}],
               "role": null, "refreshed": null, "refreshing": false, "update": false}}),
        json!({"type": "exit"}),
    ];
    for kind in [
        "start_work",
        "confirm_take_over",
        "confirm_safety_setting",
        "terms_note",
        "init",
        "setting_scope",
        "detected_checks",
        "trust_project",
    ] {
        events.push(
            json!({"type": "choice", "choice": {"id": "ch-009", "kind": kind,
               "title": "t", "body": [], "options": [], "free_text": "none", "bell": true}}),
        );
    }
    for event in events {
        round_trip::<AppEvent>(event);
    }
    let status = json!({"type": "view", "data": {"data": "status", "summary": {
        "run": "run-01", "goal": ["429 with Retry-After"], "plan": "Redis",
        "decisions": [["D-003", "Redis token bucket"]],
        "tasks": [{"task": "task-002", "title": "Redis store", "state": "running",
                   "worker": "codex", "note": "6m 12s · 38k tok"}],
        "recent": [], "spend": null, "plan_windows": [], "waiting": []}}});
    round_trip::<AppEvent>(status);
}

#[test]
fn trace_lines_round_trip() {
    let command = UiCommand::Slash {
        command: SlashCommand::Captain {
            task: "fix it".into(),
        },
    };
    let line = TraceLine::In { t_ms: 3, command };
    let value = serde_json::to_value(&line).unwrap();
    assert_eq!(value["dir"], "in");
    round_trip::<TraceLine>(value);
    let event = AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 1,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            task_id: None,
            session: None,
            source: "argonauts-code".into(),
            kind: RunEventKind::Note(Note {
                level: NoteLevel::Info,
                text: "x".into(),
            }),
        }),
    };
    let line = TraceLine::Out { t_ms: 9, event };
    round_trip::<TraceLine>(serde_json::to_value(&line).unwrap());
}

#[test]
fn control_frames_round_trip() {
    let answer = json!({"id": "approval-003", "action": {"action": "merge"}, "text": null,
                        "evidence_sha256": "e3b0", "tasks": []});
    let requests = vec![
        json!({"request": "hello", "client": "app", "version": "0.0.1"}),
        json!({"request": "subscribe", "from_seq": 12, "from_snapshot": false}),
        json!({"request": "subscribe", "from_seq": 0, "from_snapshot": true}),
        json!({"request": "steer", "session": "s-003", "text": "use EVAL"}),
        json!({"request": "mention", "target": {"target": "all"}, "text": "wrap up"}),
        json!({"request": "guidance", "text": "keep it small"}),
        json!({"request": "guidance", "text": "keep it small", "outside_agent": "conversation-bot"}),
        json!({"request": "pause", "now": false}),
        json!({"request": "pause", "now": true, "outside_agent": "conversation-bot"}),
        json!({"request": "cancel_pause"}),
        json!({"request": "stop"}),
        json!({"request": "stop_session", "session": "s-003"}),
        json!({"request": "take_back", "messages": ["m-001"]}),
        json!({"request": "approval_mode", "mode": "auto", "token": "t0k"}),
        json!({"request": "reassign", "task": "task-002", "to": "codex", "rule_override": null,
               "token": null}),
        json!({"request": "reassign", "task": "task-002", "to": "claude-code/opus",
               "rule_override": "review.other_provider", "token": "t0k"}),
        json!({"request": "take_over", "task": "task-002", "token": "t0k"}),
        json!({"request": "take_over_ended", "task": "task-002", "token": "t0k"}),
        json!({"request": "answer", "answer": answer, "token": "t0k"}),
        json!({"request": "status"}),
        json!({"request": "diff", "task": "task-002", "scope": "task"}),
        json!({"request": "raw", "session": "s-003"}),
        json!({"request": "rewind", "task": "task-002", "checkpoint": 2}),
        json!({"request": "spot_check", "id": "approval-003", "correct": false}),
    ];
    for (id, body) in requests.into_iter().enumerate() {
        round_trip::<Frame<ControlRequest>>(json!({"id": id, "body": body}));
    }
    let replies = vec![
        json!({"reply": "welcome", "run": "run-01", "pid": 4242, "version": "0.0.1"}),
        json!({"reply": "ok"}),
        json!({"reply": "error", "message": "no such session"}),
        json!({"reply": "event", "event": envelope(json!({"type": "note",
               "data": {"level": "info", "text": "x"}}))}),
        json!({"reply": "queued", "id": "m-004"}),
        json!({"reply": "taken_back", "messages": [queued()]}),
        json!({"reply": "steered", "ack": {"session": "s-003", "how": "interrupt_resume"}}),
        json!({"reply": "take_over", "ticket": {"task": "task-002", "command": null,
               "worktree": "/wt/task-002", "agent_id": "openrouter"}}),
        json!({"reply": "status", "summary": {"run": null, "goal": [], "plan": null,
               "decisions": [], "tasks": [], "recent": [], "spend": null, "plan_windows": [],
               "waiting": []}}),
        json!({"reply": "diff", "diff": {"task": "task-002", "stat": diff_stat(), "text": "",
               "path": "/d"}}),
        json!({"reply": "raw", "raw": {"session": "s-003", "lines": [], "truncated": true,
               "path": "/r", "cli_transcript": "/v"}}),
    ];
    for body in replies {
        round_trip::<Frame<ControlReply>>(json!({"id": 1, "body": body}));
    }
}

#[test]
fn api_requests_round_trip() {
    round_trip::<StartRun>(
        json!({"request": "fix the typo", "mode": "captain", "forced": true,
        "estimate": null, "captain_or_crew": null, "conversation_turn": "t-001", "continues": null,
        "overrides": ["captain.review=true"], "include_uncommitted": false,
        "main_agent": "codex/gpt-x", "approval_mode": "supervised"}),
    );
    round_trip::<ApprovalAnswer>(
        json!({"id": "approval-001", "action": {"action": "send_back"},
        "text": "split the store", "evidence_sha256": "e3b0", "tasks": []}),
    );
    round_trip::<ApprovalAnswer>(
        json!({"id": "approval-009", "action": {"action": "request_changes"},
        "text": "tighten the limit", "evidence_sha256": "e3b0", "tasks": ["task-002"]}),
    );
    // Older answers without `questions` still read, as every question.
    let old: ApprovalAnswer = serde_json::from_value(json!({"id": "approval-001",
        "action": {"action": "merge"}, "text": null, "evidence_sha256": "e3b0"}))
    .unwrap();
    assert!(old.tasks.is_empty());
    let summary: RunSummary = round_trip(run_summary());
    assert_eq!(summary.state, RunState::Waiting);
    assert_eq!(StatusSummary::default().tasks.len(), 0);
}

#[test]
fn parsed_input_wire_format() {
    round_trip::<ParsedInput>(json!({"input": "empty"}));
    round_trip::<ParsedInput>(json!({"input": "plain", "text": "hi"}));
    round_trip::<ParsedInput>(json!({"input": "mention",
        "target": {"target": "number", "value": 2}, "text": "stop"}));
    round_trip::<ParsedInput>(json!({"input": "slash",
        "command": {"command": "follow_up", "run": "run-01", "request": "add tests"}}));
    round_trip::<MentionTarget>(json!({"target": "worker", "value": "claude-code/opus"}));
}

#[test]
fn setting_scopes_and_metadata_round_trip() {
    let scopes: Vec<SettingScope> =
        round_trip(json!(["session", "project_local", "project", "global"]));
    assert_eq!(
        scopes,
        [
            SettingScope::Session,
            SettingScope::ProjectLocal,
            SettingScope::Project,
            SettingScope::Global
        ]
    );
    let meta: SettingMeta = round_trip(json!({"key": "approvals.*.mode",
        "description": "Supervised, auto preview, or auto",
        "allowed": ["\"supervised\"", "\"auto_preview\"", "\"auto\""],
        "default_scope": "project", "safety": true}));
    assert!(meta.safety);
    round_trip::<SettingMeta>(json!({"key": "agents.*.model", "description": "",
        "allowed": [], "default_scope": "project_local", "safety": false}));
    round_trip::<ParsedInput>(json!({"input": "slash",
        "command": {"command": "workflow", "name": "council", "action": "copy_to_project"}}));
    round_trip::<ParsedInput>(json!({"input": "slash",
        "command": {"command": "fallback", "role": null, "key": null, "value": null}}));
    // An older `workflow` command without an action reads as switching to it.
    let old: SlashCommand =
        serde_json::from_value(json!({"command": "workflow", "name": "captain"})).unwrap();
    assert_eq!(
        old,
        SlashCommand::Workflow {
            name: Some("captain".into()),
            action: WorkflowVerb::Use
        }
    );
}

#[test]
fn older_messages_read_with_the_review_additions_empty() {
    let command = |value: Value| serde_json::from_value::<SlashCommand>(value).unwrap();
    assert_eq!(
        command(json!({"command": "approvals"})),
        SlashCommand::Approvals {
            key: None,
            value: None
        }
    );
    assert_eq!(
        command(json!({"command": "permissions"})),
        SlashCommand::Permissions {
            role: None,
            key: None,
            value: None
        }
    );
    assert_eq!(
        command(json!({"command": "config", "key": "ui.keys", "value": "vim"})),
        SlashCommand::Config {
            key: Some("ui.keys".into()),
            value: Some("vim".into()),
            reset: false
        }
    );
    let event = |value: Value| serde_json::from_value::<AppEvent>(value).unwrap();
    assert!(matches!(
        event(
            json!({"type": "setting_changed", "key": "ui.keys", "value": "\"vim\"",
                     "scope": "global", "previous": null})
        ),
        AppEvent::SettingChanged { reset: false, .. }
    ));
    assert!(matches!(
        event(json!({"type": "ui_settings", "vim": true, "verbosity": "quiet"})),
        AppEvent::UiSettings { ui: None, .. }
    ));
    let AppEvent::View { data } = event(json!({"type": "view", "data": {"data": "models",
        "choices": [{"worker": "codex", "provider": "openai", "access": "subscription",
                     "cost": {"unit": "unknown"}, "plan_usage": null, "efforts": [],
                     "strengths": []}]}}))
    else {
        panic!("not a view");
    };
    let ViewData::Models {
        choices,
        role,
        refreshed,
        refreshing,
        update,
        sources: _,
        memory: _,
    } = *data
    else {
        panic!("not the model picker");
    };
    assert!(role.is_none() && !choices[0].current);
    assert!(refreshed.is_none() && !refreshing && !update);
    let AppEvent::View { data } = event(json!({"type": "view", "data": {"data": "models",
        "choices": [], "refreshed": "2026-10-01T09:30:00Z", "update": true}}))
    else {
        panic!("not a view");
    };
    let ViewData::Models {
        refreshed, update, ..
    } = *data
    else {
        panic!("not the model picker");
    };
    assert_eq!(
        refreshed,
        Some(time::macros::datetime!(2026-10-01 09:30 UTC))
    );
    assert!(update);
    let AppEvent::View { data } = event(json!({"type": "view", "data": {"data": "settings",
        "rows": []}}))
    else {
        panic!("not a view");
    };
    assert_eq!(
        *data,
        ViewData::Settings {
            rows: Vec::new(),
            command: None,
            section: None,
            not_installed: Vec::new(),
        }
    );
    let AppEvent::View { data } = event(json!({"type": "view", "data": {"data": "settings",
        "rows": [], "command": "agents",
        "not_installed": [{"id": "opencode", "install": "npm i -g opencode-ai@latest"}]}}))
    else {
        panic!("not a view");
    };
    let ViewData::Settings { not_installed, .. } = *data else {
        panic!("not settings");
    };
    assert_eq!(
        not_installed,
        [argonauts_core::app::MissingCli {
            id: "opencode".into(),
            install: Some("npm i -g opencode-ai@latest".into()),
        }]
    );
}

#[test]
fn a_workflow_promotion_packet_is_an_object_without_kind() {
    use argonauts_core::events::approval::ApprovalPacket;
    let packet =
        ApprovalPacket::WorkflowPromotion(json!({"learned_workflow": "small-fix", "gain": 4.0}));
    let value = serde_json::to_value(&packet).unwrap();
    assert_eq!(value["kind"], "workflow_promotion");
    assert_eq!(
        serde_json::from_value::<ApprovalPacket>(value).unwrap(),
        packet
    );
    // Anything but an object has nowhere to put the tag, so it can't be written.
    for shape in [json!("text"), json!([1, 2]), json!(true), json!(3)] {
        assert!(serde_json::to_value(ApprovalPacket::WorkflowPromotion(shape)).is_err());
    }
}

#[test]
fn agent_events_keep_their_flat_wire_format() {
    round_trip::<AgentEvent>(json!({"type": "plan_usage", "reading": plan_usage_reading()}));
    round_trip::<AgentEvent>(json!({"type": "subagent", "update": {"id": "toolu_1",
        "parent": "toolu_0", "kind": "subagent", "label": "Explore", "prompt": null,
        "status": "running", "last_message": null, "log_uri": null, "steps_visible": true}}));
}

#[test]
fn names_parse_and_display() {
    let worker: ModelRef = "openrouter/z-ai/glm-5".parse().unwrap();
    assert_eq!(worker.agent, "openrouter");
    assert_eq!(worker.model.as_deref(), Some("z-ai/glm-5"));
    assert_eq!(worker.to_string(), "openrouter/z-ai/glm-5");
    assert_eq!(worker.agent_id(), AgentId("openrouter".into()));
    assert!("".parse::<ModelRef>().is_err());
    assert!("codex/-x".parse::<ModelRef>().is_err());
    assert!("codex/".parse::<ModelRef>().is_err());
    assert!("co dex".parse::<ModelRef>().is_err());
    assert_eq!(
        serde_json::to_value(&worker).unwrap(),
        json!("openrouter/z-ai/glm-5")
    );
    assert!(serde_json::from_value::<ModelRef>(json!("")).is_err());

    assert_eq!(SessionKey::numbered(7).as_str(), "s-007");
    assert_eq!(ApprovalId::numbered(3).as_str(), "approval-003");
    assert_eq!(ChoiceId::numbered(2).as_str(), "ch-002");
    assert_eq!(TurnId::numbered(12).as_str(), "t-012");
    assert_eq!(ConversationId::numbered(4).as_str(), "conversation-004");
    assert_eq!(MessageId::numbered(31).as_str(), "m-031");
    assert_eq!(DecisionId::numbered(3).as_str(), "D-003");
    assert_eq!(DraftId::numbered(2).as_str(), "dr-002");
    assert!(SessionKey::parse("s-007").is_some());
    assert!(SessionKey::parse("../x").is_none());
    assert!(SessionKey::parse("").is_none());

    let roles = [
        RoleName::Main,
        RoleName::Orchestrator,
        RoleName::TieBreaker,
        RoleName::Reader,
        RoleName::Comparer,
        RoleName::Draft,
        RoleName::Map,
        RoleName::Critic,
        RoleName::Experiment,
        RoleName::Judge,
        RoleName::Worker,
        RoleName::Consultant,
        RoleName::Reviewer,
        RoleName::RunReview,
        RoleName::Narrator,
    ];
    for role in roles {
        assert_eq!(serde_json::to_value(role).unwrap(), json!(role.as_str()));
    }
}

#[test]
fn settings_default_matches_the_frozen_defaults_file() {
    let text = toml::to_string_pretty(&Settings::default()).unwrap();
    let frozen = include_str!("../fixtures/settings-defaults.toml");
    assert_eq!(text.trim_end(), frozen.trim_end());
    let back: Settings = toml::from_str(frozen).unwrap();
    assert_eq!(back, Settings::default());
    assert_eq!(back.workflow, "council");
}

#[test]
fn partial_settings_fill_in_defaults() {
    let partial: Settings = toml::from_str(
        r#"
workflow = "orchestrator"
native_fanout = "off"
[roles.worker]
models = ["claude-code/opus"]
[fallback.worker]
mode = "auto"
order = ["codex/gpt-x", { model = "openrouter/a/b", max_spend_usd = 2.0 }]
[roles.reviewer]
models = ["openrouter/m-a", "openrouter/m-b"]
blind = true
native_fanout = { max = 2 }
[approvals.merge]
mode = "auto_preview"
decide_with = "rules_and_model"
otherwise = "open_pull_request"
[approvals.rules.merge]
min_test_coverage_of_changed_lines = 0.8
max_changed_lines = 400
[ui]
narrator = "live"
keys = "vim"
take_over = "tmux-split"
"#,
    )
    .unwrap();
    assert_eq!(partial.workflow, "orchestrator");
    assert_eq!(partial.roles[&RoleName::Reviewer].models.len(), 2);
    assert_eq!(
        partial
            .fallback
            .for_role(RoleName::Worker)
            .unwrap()
            .order
            .len(),
        2
    );
    // No limit unless you set one.
    assert_eq!(partial.limits.spend.spend_per_run, None);
    assert_eq!(partial.limits.spend_per_day, None);
    assert_eq!(partial.limits.spend.plan_usage, None);
    let json = serde_json::to_value(&partial).unwrap();
    assert_eq!(serde_json::from_value::<Settings>(json).unwrap(), partial);
}

#[test]
fn settings_and_registry_schemas_generate_and_validate() {
    let schema = schemars::schema_for!(Settings).to_value();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let defaults = serde_json::to_value(Settings::default()).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&defaults)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");

    let schema = schemars::schema_for!(RegistryFile).to_value();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let registry: RegistryFile = toml::from_str(
        r#"
[[model]]
id = "claude-code/opus"
provider = "anthropic"
kind = "cli"
access = "subscription"
tools = true
cost = { unit = "plan_usage" }
strengths = ["refactor"]
good_for = ["worker"]
efforts = ["low", "medium", "high", "max"]

[[model]]
id = "codex/local-model"
provider = "openai"
kind = "cli"
access = "local"
cost = { unit = "none" }
launch = { oss = true, local_provider = "ollama" }

[[cli]]
id = "my-agent"
display_name = "My Agent"
binary = "my-agent"
read_only_args = ["--read-only", "--model", "{model}"]
build_args = ["--model", "{model}"]
provider = "mine"
"#,
    )
    .unwrap();
    assert_eq!(registry.models.len(), 2);
    assert_eq!(registry.models[0].efforts, ["low", "medium", "high", "max"]);
    // A model whose levels aren't known lists none, and `/effort` then offers every level.
    assert!(registry.models[1].efforts.is_empty());
    assert_eq!(registry.clis[0].id, "my-agent");
    let value = serde_json::to_value(&registry).unwrap();
    assert!(validator.is_valid(&value));
    let back: RegistryFile = serde_json::from_value(value).unwrap();
    assert_eq!(back, registry);
}

#[test]
fn spans_parse() {
    assert_eq!(
        Span::new("45s").parse().unwrap(),
        Some(std::time::Duration::from_secs(45))
    );
    assert_eq!(
        Span::new("30m").parse().unwrap(),
        Some(std::time::Duration::from_secs(1800))
    );
    assert_eq!(
        Span::new("2h").parse().unwrap(),
        Some(std::time::Duration::from_secs(7200))
    );
    assert_eq!(Span::new("none").parse().unwrap(), None);
    assert!(Span::new("5").parse().is_err());
    assert!(Span::new("5d").parse().is_err());
}

#[test]
fn safety_keys_cover_the_contract_list() {
    for key in [
        "limits.protected_paths",
        "limits.dependency_allowlist",
        "limits.spend_per_day",
        "limits.auto_merges_per_day",
        "limits.irreversible_allowlist",
        "limits.evolution_usd_per_month",
        "limits.spend_per_run",
        "limits.plan_usage",
        "sandbox.*",
        "approvals.*",
        "agents.*.auth",
        "agents.*.binary",
        "agents.*.extra_args",
        "endpoints.*.base_url",
        "endpoints.*.header_env.*",
        "fallback.*.allow_api_billing",
        "fallback.allow_api_billing",
        "roles.*.sandbox.*",
        "mcp.*",
        "merge.*",
        "checks.*",
        "notifications.conversation.*",
    ] {
        assert!(SAFETY_KEYS.contains(&key), "{key}");
    }
}

/// `[limits]` holds the hard limits and the run and task limits. A key added to it is a safety
/// key unless it is listed here as one a workflow may set.
#[test]
fn every_limits_key_is_a_safety_key_or_a_task_limit() {
    const A_WORKFLOW_MAY_SET: &[&str] = &[
        "per_task_usd",
        "per_task_tokens",
        "per_task_plan_usage",
        "per_task_minutes",
        "soft_stop",
        "when_reached",
        "from_history",
        "history_min",
        "lines",
    ];
    let limits = serde_json::to_value(Settings::default().limits).unwrap();
    for name in limits.as_object().unwrap().keys() {
        let key = format!("limits.{name}");
        assert!(
            SAFETY_KEYS.contains(&key.as_str()) || A_WORKFLOW_MAY_SET.contains(&name.as_str()),
            "{key} is neither a safety key nor listed as one a workflow may set"
        );
    }
}

fn evidence() -> EvidencePacket {
    EvidencePacket {
        kind: argonauts_core::events::approval::ApprovalKind::Merge,
        goal: None,
        facts: EvidenceFacts {
            subject: Some(EvidenceSubject {
                base_commit: "8b41d07".into(),
                head_commit: "c3".into(),
                tree_sha: "t9".into(),
            }),
            checks_passed: Some(true),
            agent_paths: vec!["src/a.ts".into()],
            conductor_paths: vec![".argonauts/goal/run-01.md".into()],
            spent: Spend::default(),
            ..EvidenceFacts::default()
        },
        agent_outputs: vec!["reviewer: approve".into()],
        documents: vec![
            EvidenceDocument {
                label: "combined diff".into(),
                text: "diff --git".into(),
                path: None,
                sha256: None,
            },
            EvidenceDocument {
                label: "full diff".into(),
                text: String::new(),
                path: Some("/state/evidence/approval-003.diff".into()),
                sha256: Some("aa".into()),
            },
        ],
    }
}

#[test]
fn evidence_hashes_are_stable_and_bind_every_field() {
    let packet = evidence();
    let hash = packet.sha256();
    assert_eq!(hash.len(), 64);
    assert_eq!(hash, evidence().sha256());
    let value = serde_json::to_value(&packet).unwrap();
    let back: EvidencePacket = serde_json::from_value(value).unwrap();
    assert_eq!(back.sha256(), hash);
    let mut changed = evidence();
    changed.facts.checks_passed = Some(false);
    assert_ne!(changed.sha256(), hash);
    let mut changed = evidence();
    changed.documents[0].text.push(' ');
    assert_ne!(changed.sha256(), hash);
    let mut changed = evidence();
    changed.documents[1].sha256 = Some("ab".into());
    assert_ne!(
        changed.sha256(),
        hash,
        "a stored document is bound by its hash"
    );
    let mut changed = evidence();
    if let Some(subject) = changed.facts.subject.as_mut() {
        subject.head_commit = "c4".into();
    }
    assert_ne!(
        changed.sha256(),
        hash,
        "the approval is bound to the head commit"
    );
}

#[test]
fn policy_facts_are_left_out_until_the_engine_knows_them() {
    // Absent facts leave the packet, and so its hash, as it was before they existed.
    let unknown = serde_json::to_value(evidence()).unwrap();
    for key in [
        "stuck",
        "spend",
        "criteria_unchanged",
        "promotion",
        "irreversible_actions",
    ] {
        assert!(unknown["facts"].get(key).is_none(), "{key}");
    }
    let usd = |amount| LimitAmount {
        unit: LimitUnit::Usd,
        amount,
    };
    let mut known = evidence();
    known.facts.stuck = Some(StuckFacts {
        fallback_mode: FallbackMode::Auto,
        providers_tried: vec!["anthropic".into()],
        next_provider: Some("openai".into()),
        next_name: Some("Codex".into()),
        extra_tries: 1,
        fits_reserve: Some(true),
    });
    known.facts.spend = Some(SpendFacts {
        remaining_tasks: usd(4.5),
        reserve_left: usd(6.0),
    });
    known.facts.criteria_unchanged = Some(true);
    known.facts.promotion = Some(PromotionFacts {
        change_class: "checklist_item".into(),
        last_promoted: Some(OffsetDateTime::from_unix_timestamp(1_759_200_000).unwrap()),
    });
    known.facts.irreversible_actions = Some(vec!["npm publish".into()]);
    let back: EvidencePacket =
        serde_json::from_value(serde_json::to_value(&known).unwrap()).unwrap();
    assert_eq!(back, known);
    assert_ne!(known.sha256(), evidence().sha256());
}

#[test]
fn run_modes_and_parties_use_snake_case() {
    assert_eq!(serde_json::to_value(RunMode::Crew).unwrap(), json!("crew"));
    assert_eq!(
        serde_json::to_value(Party::Agent {
            session: SessionKey::numbered(1),
            worker: "codex".parse().unwrap()
        })
        .unwrap(),
        json!({"party": "agent", "session": "s-001", "worker": "codex"})
    );
}

/// A sample answer for a step: it must pass the strict schema the agent gets, and decode.
fn check_step_output<S: argonauts_core::steps::Step>(value: Value)
where
    S::Output: std::fmt::Debug + PartialEq,
{
    let schema = argonauts_core::steps::output_schema::<S>();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{:?}: {errors:#?}", S::NAME);
    round_trip::<S::Output>(value);
}

#[test]
fn sample_step_outputs_pass_their_strict_schemas_and_decode() {
    use argonauts_core::steps::{intake, judgment, narrate, plan, task, understand};
    check_step_output::<intake::Estimate>(json!({"kind": "change",
        "answer_markdown": "I'd add a middleware.",
        "estimate": {"task_type": "api-feature", "summary": "s", "files_likely": ["a"],
                     "file_count": 3, "risk_labels": [], "tests_exist": true, "unknowns": []},
        "how_to_run": [{"says": "have codex orchestrate", "kind": "orchestrate", "who": "codex",
                        "model": "", "effort": "", "value": "", "scope": "this_request"}]}));
    check_step_output::<intake::ConversationAnswer>(json!({"answer_markdown": "Hello!"}));
    check_step_output::<understand::Read>(json!({"summary": "s", "success_criteria": ["c"],
        "in_scope": [], "out_of_scope": [], "assumptions": [], "open_questions": []}));
    check_step_output::<understand::Compare>(
        json!({"agreements": ["a"], "differences": [{"topic": "t",
        "readings": [{"label": "A", "reading": "r"}], "tag": "detail", "question": null}]}),
    );
    check_step_output::<plan::Draft>(plan_draft());
    check_step_output::<plan::Map>(json!({"topics": [{"name": "n", "disputed": false,
        "positions": [{"label": 1, "summary": "s", "quote": "q"}]}]}));
    check_step_output::<plan::Critique>(json!({"points": [{"plan": 2, "topic": "t",
        "claim": "c", "severity": "low", "suggestion": "s"}]}));
    check_step_output::<plan::ProposeExperiment>(json!({"proposals": []}));
    check_step_output::<plan::ExperimentReportStep>(json!({"topic": "t", "option": "o",
        "measurements": [{"metric": "p50", "value": 2.5, "unit": "ms"}], "finding": "f",
        "works": false}));
    check_step_output::<plan::Judge>(json!({"scores": [{"plan": 1, "criteria": [4.0, 3.5],
        "rationale": "r", "addressed_to_judges": false}]}));
    let tasks = json!([{"key": "store", "title": "Store", "task": "t", "kind": "build",
        "owned_paths": ["src/store.ts"], "read_only_paths": [], "depends_on": [],
        "criteria": ["G1"]}]);
    check_step_output::<plan::PlanToTasks>(json!({"tasks": tasks}));
    check_step_output::<plan::Orchestrate>(json!({"plan": plan_draft(), "tasks": tasks,
        "assignments": [{"task": "store", "worker": "claude-code/opus",
                         "reviewers": ["openrouter/m-a"]}]}));
    check_step_output::<task::Predict>(json!({"p_pass_checks_first_try": 0.6,
        "p_within_time_cap": 0.8, "p_at_most_two_tries": 0.9}));
    check_step_output::<task::Build>(json!({"status": "needs_decision", "summary": "s",
        "files_changed": [], "tests_run": [], "concerns": [], "questions": [],
        "change_requests": [], "memory_proposals": [],
        "answer": "Which store should the limiter use? The rest waits on that."}));
    check_step_output::<task::CloseRun>(
        json!({"answer": "Rate limiting is in, and the tests pass."}),
    );
    check_step_output::<task::AcceptanceTests>(json!({"files": [{"path": "tests/a.rs",
        "content": "#[test] fn a() {}"}], "criteria_covered": ["G1"]}));
    check_step_output::<task::RunReview>(json!({"change": {"setting": "critique.rounds",
        "value": "0", "why": "w"}}));
    check_step_output::<task::RunReview>(json!({"change": null}));
    check_step_output::<judgment::Review>(json!({"verdict": "reject", "summary": "s",
        "comments": [{"path": "a", "line": null, "severity": "high", "text": "t"}],
        "tamper_suspected": true}));
    check_step_output::<judgment::Settle>(json!({"verdict": "request_changes",
        "reasons": ["r"], "required_changes": ["c"]}));
    check_step_output::<judgment::ModelJudge>(json!({"verdict": "ask_you",
        "reasons": ["r"], "rules_cited": ["rule 2"]}));
    check_step_output::<narrate::Narrate>(json!({"lines": ["Task 2 passed its checks."]}));
}
