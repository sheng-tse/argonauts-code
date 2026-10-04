//! The frozen contracts, checked from outside: every event and message survives a JSON round
//! trip unchanged, settings defaults match the frozen file, and the schemas generate.

use argonauts_core::api::{CheckpointAnswer, RunState, RunSummary, StartRun, StatusSummary};
use argonauts_core::app::{AppEvent, TraceLine, UiCommand, ViewData};
use argonauts_core::control::{ControlReply, ControlRequest, Frame};
use argonauts_core::events::checkpoint::{AnswerAction, CheckpointDecided};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode};
use argonauts_core::events::{CheckpointEvent, RunEvent, RunEventKind};
use argonauts_core::input::{MentionTarget, ParsedInput, PresetAction, SlashCommand};
use argonauts_core::names::{
    ChatId, CheckpointId, ChoiceId, DecisionId, DraftId, MessageId, RoleName, SessionKey, TurnId,
    VendorId, WorkerRef,
};
use argonauts_core::policy::{
    BudgetFacts, EscalationFacts, EvidenceDocument, EvidenceFacts, EvidencePacket, EvidenceSubject,
    PromotionFacts,
};
use argonauts_core::registry::RegistryFile;
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
use argonauts_core::settings::handoff::HandoffMode;
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
        "card_id": "card-002",
        "session": "s-003",
        "source": "conductor",
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
           "answers": ["per API key"], "source": "restatements",
           "path": "/repo/.argonauts/goal/run-01.md", "sha256": "ab12"})
}

fn card_spec() -> Value {
    json!({"id": "card-002", "title": "Redis store", "task": "Add the token bucket store",
           "kind": {"kind": "acceptance_tests", "for_card": "card-003"},
           "owned_paths": ["src/limits/store.ts"], "read_only_paths": ["src/limits/types.ts"],
           "forbidden_paths": [], "hidden_paths": ["tests/hidden"], "depends_on": ["card-001"],
           "criteria": ["G1"], "owner": "codex/gpt-x", "reviewers": ["claude-code/opus"],
           "budget": {"limit": usd(3.5), "max_minutes": 20.0, "max_tool_calls": null,
                      "inactivity_min": 5.0},
           "predictions": {"p_pass_gates_first_try": 0.7, "p_within_time_cap": 0.8,
                           "p_at_most_two_attempts": 0.9}})
}

fn queued() -> Value {
    json!({"id": "m-001", "from": {"party": "user"}, "to": {"target": "card", "value": "card-002"},
           "text": "keep a fallback", "queued_at": "2026-09-29T12:00:00Z", "delivery": "next_turn"})
}

fn budget_status() -> Value {
    json!({"level": "stop", "limit": {"unit": "usd", "amount": 2.0}, "spend": spend(),
           "fraction_used": 1.0, "work": {"minutes": 3.5, "tool_calls": 4},
           "work_limits": {"max_minutes": null, "max_tool_calls": null}, "binding": "spend"})
}

fn quota_reading() -> Value {
    json!({"vendor": "claude-code", "status": "warning", "used": 0.71,
           "resets_at": "2026-09-29T18:00:00Z", "window": "five_hour",
           "source": "rate_limit_event", "observed_at": "2026-09-29T12:00:00Z",
           "detail": {"status": "allowed_warning"}})
}

fn handoff_plan() -> Value {
    json!({"card": "card-002", "title": "Redis store", "stopped": "claude-code/opus",
           "reason": "usage limit, resets in 2h 10m", "resets_at": "2026-09-29T18:00:00Z",
           "work": diff_stat(), "gates_ran": false,
           "candidates": [
               {"worker": "codex/gpt-x", "quota_left": 0.62, "quota_text": "62%",
                "track_record": {"passed": 9, "of": 11}, "cost": {"unit": "quota", "amount": 0.1},
                "needs_confirm": false, "may_run_out": false},
               {"worker": "openrouter/z-ai/glm-5", "quota_left": null, "quota_text": "API key",
                "track_record": null, "cost": {"unit": "usd", "amount": 1.25},
                "needs_confirm": true, "may_run_out": false},
               {"worker": "ollama/qwen3", "quota_left": null, "quota_text": "local",
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
    let card = |data: Value| json!({"type": "card", "data": data});
    let session = |data: Value| json!({"type": "session", "data": data});
    let review = |data: Value| json!({"type": "review", "data": data});
    let integration = |data: Value| json!({"type": "integration", "data": data});
    let checkpoint = |data: Value| json!({"type": "checkpoint", "data": data});
    let handoff = |data: Value| json!({"type": "handoff", "data": data});
    let vendor = |data: Value| json!({"type": "vendor", "data": data});
    let budget = |data: Value| json!({"type": "budget", "data": data});
    let blackboard = |data: Value| json!({"type": "blackboard", "data": data});
    let retro = |data: Value| json!({"type": "retro", "data": data});
    let with = |mut base: Value, extra: Value| {
        let (Value::Object(base_map), Value::Object(extra_map)) = (&mut base, extra) else {
            unreachable!()
        };
        base_map.extend(extra_map);
        base
    };

    let checkpoint_opened = |kind: &str, packet: Value, options: Value, free_text: &str| {
        checkpoint(json!({"event": "opened", "id": "cp-003", "kind": kind,
            "title": "Checkpoint · run-01", "card": "card-002", "packet": packet,
            "evidence_sha256": "e3b0", "mode": "supervised", "decide_with": "rules",
            "options": options, "free_text": free_text, "needs_you": true}))
    };
    let option = |key: &str, label: &str, action: Value| json!({"key": key, "label": label, "action": action, "recommended": false, "detail": null});

    vec![
        run(
            json!({"event": "started", "request": "add per-key rate limiting",
            "title": "Per-key rate limiting", "mode": "crew", "forced": false,
            "preset": "peer-crew",
            "estimate": {"task_type": "api-feature", "summary": "middleware and a store",
                         "files_likely": ["src/limits/store.ts"], "file_count": 5,
                         "risk_labels": ["api", "storage"], "tests_exist": true, "unknowns": []},
            "triage": {"mode": "crew", "escalate_to_crew": false, "rule": "crew_labels",
                       "reason": "touches api and storage"},
            "chat_turn": "t-012", "continues": null, "repo_root": "/repo",
            "base_branch": "main", "base_commit": "8b41d07", "uncommitted": ["notes.txt"],
            "uncommitted_included": false, "settings_sha256": "5e7f",
            "settings": {"preset": "peer-crew"},
            "overrides": [{"key": "critique.rounds", "value": "0", "from": "flag"}],
            "harness_versions": {"codex": "0.158.0"}, "version": "0.0.1"}),
        ),
        run(
            json!({"event": "resumed", "cause": "answer", "settings_changed": false,
            "harness_versions_changed": ["codex"], "main_moved_by": 2,
            "sessions": [{"session": "s-009", "card": "card-002", "vendor_session": "abc",
                          "resumed": true}]}),
        ),
        run(json!({"event": "pause_requested", "by": {"party": "user"}, "now": false})),
        run(json!({"event": "approval_mode_changed", "mode": "shadow", "by": {"party": "user"}})),
        run(json!({"event": "pause_cancelled"})),
        run(json!({"event": "paused", "sessions_stopped": 2, "committed": ["card-002"]})),
        run(json!({"event": "stop_requested", "by": {"party": "user"}})),
        run(json!({"event": "parked", "reason": {"why": "checkpoint"}, "waiting_on": ["cp-003"]})),
        run(json!({"event": "parked", "reason": {"why": "paused"}, "waiting_on": []})),
        run(json!({"event": "parked",
            "reason": {"why": "all_vendors_out", "until": "2026-09-29T18:00:00Z"},
            "waiting_on": []})),
        run(json!({"event": "parked",
            "reason": {"why": "waiting_for_vendor", "vendor": "codex", "until": null},
            "waiting_on": []})),
        run(json!({"event": "closing_answer",
            "text": "Fixed parse() in invoice/money.py; the six tests pass.",
            "by": "claude-code/claude-haiku-4-5",
            "files": [{"path": "invoice/money.py", "insertions": 19, "deletions": 3}],
            "checks": [{"what": "tests", "passed": true, "result": "6 passed"}],
            "tests": ["python3 -m unittest tests.test_money: all 6 passed"],
            "left": ["format_cents() still rounds half down"]})),
        run(json!({"event": "closing_answer", "text": "The run stopped before any change."})),
        run(json!({"event": "finished", "outcome": "merged", "detail": "landed on main"})),
        run(
            json!({"event": "resumed", "cause": "ask_timeout", "settings_changed": false,
            "harness_versions_changed": [], "main_moved_by": null, "sessions": []}),
        ),
        run(
            json!({"event": "snapshot", "upto": 4999, "engine": {"version": 1, "next_session": 12},
            "summary": {"run": "run-01", "goal": [], "plan": null, "decisions": [], "cards": [],
                        "recent": [], "budget": null, "quotas": [], "waiting": []},
            "open_checkpoints": [], "running": ["s-011"]}),
        ),
        json!({"type": "phase", "data": {"phase": "final_check", "status": "started",
                                         "summary": null}}),
        goal(
            json!({"event": "restated", "label": "A", "worker": "claude-code/opus",
            "restatement": {"summary": "s", "success_criteria": ["429"], "in_scope": [],
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
        plan(json!({"event": "spike_proposed", "fits_budget": true,
            "proposal": {"topic": "where counters live", "question": "is Redis fast enough?",
                "options": [{"label": "redis", "build": "a bucket in Redis"}], "measure": "p50",
                "decides_by": "under 5 ms wins", "est_minutes": 10.0, "est_cost_usd": 0.5}})),
        plan(json!({"event": "spike_finished", "card": "spike-001",
            "report": {"topic": "where counters live", "option": "redis",
                "measurements": [{"metric": "p50", "value": 2.1, "unit": "ms"}],
                "finding": "adds 2.1 ms", "works": true}})),
        plan(
            json!({"event": "scored", "judge": "codex", "judge_family": "gpt", "plan": 1,
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
        plan(json!({"event": "coordinated", "coordinator": "codex",
            "rejection": "card-004 owns a protected path"})),
        divide(json!({"event": "graph", "cards": [card_spec()], "critical_path": ["card-002"]})),
        divide(
            json!({"event": "owner", "card": "card-002", "worker": "codex",
            "algorithm": "round-robin", "explored": false, "reason": "9 of 11 passed"}),
        ),
        divide(json!({"event": "reviewers", "card": "card-002",
            "reviewers": ["claude-code/opus"], "blind": true})),
        divide(json!({"event": "predicted", "card": "card-002",
            "predictions": {"p_pass_gates_first_try": 0.7, "p_within_time_cap": 0.8,
                            "p_at_most_two_attempts": 0.9}})),
        divide(
            json!({"event": "budget", "lines": [{"line": "cards", "amount": usd(6.5)}],
            "total": usd(20.0), "estimate_low": 9.0, "estimate_high": 12.0}),
        ),
        card(with(json!({"event": "created"}), card_spec())),
        card(
            json!({"event": "created", "id": "card-001", "title": "Solo", "task": "fix it",
            "kind": {"kind": "solo"}, "owned_paths": [], "read_only_paths": [],
            "forbidden_paths": [], "hidden_paths": [], "depends_on": [], "criteria": [],
            "owner": null, "reviewers": [],
            "budget": {"limit": null, "max_minutes": null, "max_tool_calls": null,
                       "inactivity_min": null},
            "predictions": null}),
        ),
        card(json!({"event": "state", "from": "running", "to": "gating", "reason": "report done"})),
        card(json!({"event": "state", "from": null, "to": "waiting", "reason": "created"})),
        card(json!({"event": "attempt", "attempt": 2, "worker": "codex", "cause": "gate_retry"})),
        card(json!({"event": "worktree", "path": "/wt/card-002",
            "branch": "argonauts/run-01/card-002", "base": "8b41d07"})),
        card(
            json!({"event": "committed", "branch": "argonauts/run-01/card-002", "sha": "c0ffee",
            "message": "card-002: Redis store", "stat": diff_stat()}),
        ),
        card(
            json!({"event": "reported", "status": "done", "summary": "store added",
            "files_changed": ["src/limits/store.ts"], "tests_run": ["npm test -- store"],
            "concerns": [], "questions": ["keep a fallback?"],
            "change_requests": [{"text": "widen the Store trait", "paths": ["src/types.ts"]}],
            "memory_proposals": [{"topic": "redis", "text": "use EVAL for atomicity"}]}),
        ),
        card(json!({"event": "diff", "stat": diff_stat(), "path": "/state/cards/card-002/diff"})),
        card(json!({"event": "taken_over", "command": ["codex", "resume", "abc"]})),
        card(json!({"event": "take_over_ended", "changed": diff_stat(), "commit": null})),
        card(
            json!({"event": "reassigned", "from": "claude-code/opus", "to": "codex",
            "by": {"party": "user"}, "rule_override": null}),
        ),
        session(
            json!({"event": "started", "worker": "codex", "vendor": "codex", "family": "gpt",
            "kind": "harness", "role": "owner", "step": "build", "access": "build",
            "lifetime": "card", "resumed_from": null, "follow_key": 2}),
        ),
        session(json!({"event": "input",
            "launch": {"program": "codex", "args": ["exec", "--json", "<prompt>"],
                "cwd": "/wt/card-002", "env_set": ["ARGONAUTS_SESSION"],
                "env_removed": ["ANTHROPIC_API_KEY"], "prompt_via": "stdin",
                "sandbox": "seatbelt: write worktree+tmp"},
            "parts": [{"kind": "card", "label": "card", "text": "Add the store",
                       "approx_tokens": 3, "source": null},
                      {"kind": "notes", "label": "notes", "text": "Use EVAL",
                       "approx_tokens": 2, "source": ".argonauts/notes/redis.md"}],
            "queued": [queued()]})),
        session(json!({"event": "vendor_session", "id": "019a-abc",
            "transcript": "/home/.codex/sessions/019a.jsonl"})),
        session(with(json!({"event": "queued"}), queued())),
        session(json!({"event": "delivered", "message": "m-001", "via": "steer"})),
        session(
            json!({"event": "file_diff", "path": "src/limits/store.ts", "added": 18,
            "removed": 4, "hunk": "@@ -40,2 +40,3 @@", "truncated": false}),
        ),
        session(with(
            json!({"event": "limit", "kind": "budget"}),
            budget_status(),
        )),
        session(
            json!({"event": "limit", "kind": "wrap_up_sent", "via": "interrupt_resume",
            "dimension": "spend"}),
        ),
        session(json!({"event": "limit", "kind": "nudged", "call": "Bash(npm test)", "times": 3})),
        session(json!({"event": "limit", "kind": "native_cap", "usd": 2.5})),
        session(
            json!({"event": "ended", "vendor_session": "019a-abc", "exit_code": 1,
            "signal": null, "duration_ms": 372000,
            "reason": {"why": "usage_limit", "resets_at": "2026-09-29T18:00:00Z"},
            "spend": spend(), "tool_calls": 14, "result_text": null, "structured": null,
            "last_message": "Adding the header now", "raw_log": "/state/s-003/raw.stdout.jsonl"}),
        ),
        session(
            json!({"event": "ended", "vendor_session": null, "exit_code": null,
            "signal": 9, "duration_ms": 10, "reason": {"why": "stuck", "watchdog": "repeat"},
            "spend": spend(), "tool_calls": 0, "result_text": null,
            "structured": {"status": "done"}, "last_message": null, "raw_log": null}),
        ),
        json!({"type": "agent", "data": {"type": "text_delta", "text": "hi", "parent": null}}),
        json!({"type": "agent", "data": {"type": "subagent", "update": {"id": "thread-2",
            "parent": null, "kind": "thread", "label": "explorer", "prompt": "map the code",
            "status": "done", "last_message": "found 3 callers", "log_uri": null,
            "steps_visible": false}}}),
        json!({"type": "agent", "data": {"type": "quota", "reading": quota_reading()}}),
        json!({"type": "gate", "data": {"name": "secret scan", "kind": "secret_scan",
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
        review(json!({"event": "adjudicated", "adjudicator": "codex",
            "ruling": {"verdict": "approve", "reasons": ["fixed"], "required_changes": []}})),
        review(
            json!({"event": "tests_written", "reviewer": "codex", "for_card": "card-003",
            "files": ["tests/limits.test.ts"]}),
        ),
        integration(
            json!({"event": "branch_created", "branch": "argonauts/run-01/integration",
            "base": "8b41d07"}),
        ),
        integration(json!({"event": "merged", "card": "card-002", "sha": "a1"})),
        integration(
            json!({"event": "conflict", "card": "card-003", "files": ["src/types.ts"],
            "assigned_to": "claude-code/opus"}),
        ),
        integration(
            json!({"event": "main_moved", "from": "8b41d07", "to": "9c52e18",
            "commits": 3}),
        ),
        integration(json!({"event": "main_merged_in", "sha": "b2"})),
        integration(json!({"event": "final_check", "passed": true})),
        integration(
            json!({"event": "landed", "target": "main", "sha": "d4", "fast_forward": true}),
        ),
        integration(
            json!({"event": "pull_request", "branch": "argonauts/run-01/integration",
            "url": null, "command": ["gh", "pr", "create"]}),
        ),
        integration(json!({"event": "abandoned", "branches": ["argonauts/run-01/card-002"]})),
        integration(json!({"event": "dependencies_added", "packages": ["ioredis"], "sha": "c1"})),
        checkpoint_opened(
            "question",
            json!({"kind": "question", "from": "codex", "card": "card-002",
                   "text": "keep a fallback?", "options": ["yes", "no"]}),
            json!([option(
                "a",
                "yes",
                json!({"action": "choose", "value": "yes"})
            )]),
            "answer",
        ),
        checkpoint_opened(
            "goal",
            json!({"kind": "goal", "goal": goal_file(), "agreements": ["429"], "assumptions": [],
                   "questions": []}),
            json!([option("y", "approve", json!({"action": "approve"}))]),
            "edits",
        ),
        checkpoint_opened(
            "plan",
            json!({"kind": "plan", "summary": "Redis plan", "judging": null, "near_tie": true,
                   "decisions": [], "graph": {"cards": [card_spec()], "critical_path": []},
                   "acceptance_tests": ["429 after the limit"],
                   "budget": {"lines": [], "total": usd(20.0), "estimate_low": 9.0,
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
                option("s", "run it solo", json!({"action": "switch_to_solo"})),
                option("x", "stop", json!({"action": "stop"})),
                option("d", "show the plan", json!({"action": "show_diff"}))
            ]),
            "guidance",
        ),
        checkpoint_opened(
            "merge",
            json!({"kind": "merge", "target": "main", "branch": "argonauts/run-01/integration",
                   "rows": [{"card": "card-002", "title": "Redis store",
                             "kind": {"kind": "build"}, "stat": diff_stat(), "owner": "codex",
                             "gates_passed": true, "review": "approve"}],
                   "suite_passed_on_latest_main": true, "gates": [], "combined": diff_stat(),
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
        checkpoint_opened(
            "handoff",
            with(json!({"kind": "handoff"}), handoff_plan()),
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
        checkpoint_opened(
            "vendor_down",
            json!({"kind": "vendor_down", "vendor": "claude-code",
                   "evidence": "overload errors on 3 sessions in 2 min", "test_at": null,
                   "paused": [{"card": "card-003", "title": "Middleware", "role": "owner"}]}),
            json!([
                option("a", "one agent for all", json!({"action": "one_for_all"})),
                option("e", "choose per card", json!({"action": "per_card"})),
                option("w", "wait", json!({"action": "wait"}))
            ]),
            "none",
        ),
        checkpoint_opened(
            "budget",
            json!({"kind": "budget", "card": "card-002", "status": budget_status(),
                   "dimension": "spend", "reserve_left": usd(2.0)}),
            json!([
                option("m", "more budget", json!({"action": "more_budget"})),
                option("h", "hand off", json!({"action": "hand_off"})),
                option("x", "escalate", json!({"action": "escalate"}))
            ]),
            "answer",
        ),
        checkpoint_opened(
            "escalation",
            json!({"kind": "escalation", "card": "card-002", "title": "Redis store",
                   "state": "escalated", "attempts": ["codex: gates failed"],
                   "last_failure": "tests failed"}),
            json!([
                option("r", "retry", json!({"action": "retry"})),
                option("s", "split", json!({"action": "split"})),
                option("f", "defer", json!({"action": "defer"})),
                option("x", "cancel", json!({"action": "cancel"}))
            ]),
            "guidance",
        ),
        checkpoint_opened(
            "change_request",
            json!({"kind": "change_request", "from": {"party": "user"}, "card": "card-002",
                   "text": "widen the trait", "paths": ["src/types.ts"],
                   "why_you": "changes a frozen interface", "paused": ["card-003"]}),
            json!([]),
            "none",
        ),
        checkpoint_opened(
            "goal_change",
            json!({"kind": "goal_change", "from": {"party": "user"}, "text": "also admin",
                   "clarification": false, "goal": goal_file()}),
            json!([]),
            "none",
        ),
        checkpoint_opened(
            "take_over_return",
            json!({"kind": "take_over_return", "card": "card-002", "owner": "codex",
                   "changed": diff_stat(), "commit": "f00d"}),
            json!([option(
                "c",
                "let the owner continue",
                json!({"action": "let_continue"})
            )]),
            "none",
        ),
        checkpoint_opened(
            "recipe_promotion",
            json!({"kind": "recipe_promotion", "recipe": "api-feature", "gain": 3.5}),
            json!([]),
            "none",
        ),
        checkpoint(json!({"event": "checked", "id": "cp-003", "violations": [],
            "verdict": "approve", "by": "rules", "approver": null,
            "reasons": ["all gates passed"], "shadow": false})),
        checkpoint(json!({"event": "decided", "id": "cp-003",
            "action": {"action": "choose", "value": "per API key"}, "text": null,
            "by": {"party": "approver", "model": "codex/gpt-x"}, "evidence_sha256": "e3b0",
            "shadow": {"would": "approve", "by": "rules_and_model", "reasons": ["gates passed"]},
            "cards": []})),
        checkpoint(json!({"event": "decided", "id": "cp-004",
            "action": {"action": "request_changes"}, "text": "cap at 100/min",
            "by": {"party": "user"}, "evidence_sha256": "e3b1", "shadow": null,
            "cards": ["card-002"]})),
        checkpoint(json!({"event": "withdrawn", "id": "cp-003", "reason": "evidence changed"})),
        checkpoint(json!({"event": "spot_check_queued", "id": "cp-003"})),
        handoff(with(json!({"event": "needed"}), handoff_plan())),
        handoff(
            json!({"event": "packet", "card": "card-002", "path": "/state/cards/p.md",
            "sha256": "5a"}),
        ),
        handoff(
            json!({"event": "handed_off", "card": "card-002", "to": "codex/gpt-x",
            "mode": "auto", "by": {"party": "rules"}}),
        ),
        handoff(
            json!({"event": "waiting", "card": "card-002", "vendor": "claude-code",
            "until": "2026-09-29T18:00:00Z"}),
        ),
        vendor(with(json!({"event": "quota"}), quota_reading())),
        vendor(
            json!({"event": "health", "vendor": "codex", "state": "down",
            "since": "2026-09-29T12:00:00Z", "reason": "overload on 3 sessions",
            "next_test_at": "2026-09-29T12:10:00Z", "disabled": null}),
        ),
        vendor(json!({"event": "disabled", "vendor": "codex",
            "reason": "untested version 0.170.0; pin 0.158.0"})),
        budget(json!({"event": "run", "spent": spend(), "limit": usd(20.0),
            "lines": [{"line": "cards", "allocated": usd(6.5), "used": 1.25}],
            "reserve_left": null})),
        budget(
            json!({"event": "hit", "card": "card-002", "dimension": "spend",
            "status": budget_status()}),
        ),
        budget(
            json!({"event": "extended", "card": "card-002", "amount": usd(1.0),
            "by": {"party": "user"}}),
        ),
        budget(json!({"event": "deferred", "cards": ["card-004"]})),
        blackboard(json!({"event": "posted", "id": "m-002", "kind": "question",
            "from": {"party": "agent", "session": "s-004", "worker": "codex"},
            "to": {"to": "card", "card": "card-003"}, "text": "which header name?",
            "in_reply_to": null, "refs": ["src/types.ts"]})),
        blackboard(json!({"event": "posted", "id": "m-003", "kind": "guidance",
            "from": {"party": "user"}, "to": {"to": "all"}, "text": "keep it small",
            "in_reply_to": null, "refs": []})),
        blackboard(json!({"event": "delivered", "message": "m-002", "to": "s-005"})),
        retro(
            json!({"event": "scored", "card": "card-002", "metric": "p_pass_gates_first_try",
            "predicted": 0.7, "outcome": true, "brier": 0.09}),
        ),
        retro(
            json!({"event": "outcome", "card": "card-002", "worker": "codex",
            "task_type": "api-feature", "first_try": true, "attempts": 1, "integrated": true}),
        ),
        retro(json!({"event": "memory", "from": "codex",
            "proposal": {"topic": "redis", "text": "use EVAL"}, "accepted": false,
            "reason": "names a credential"})),
        retro(json!({"event": "committed", "sha": "e5", "paths": [".argonauts/notes/redis.md"]})),
        retro(
            json!({"event": "candidate", "setting": "critique.rounds", "value": "0",
            "why": "critique changed no plan in 5 runs"}),
        ),
        json!({"type": "note", "data": {"level": "warn", "text": "2 uncommitted files left out"}}),
        run(json!({"event": "named", "name": "fix-slugify-median",
            "description": "Fix slugify's dashes and median's empty list", "style": "workflow",
            "based_on": "peer-crew"})),
        json!({"type": "workflow", "data": {"event": "declared", "id": "wf-001",
            "name": "fix-slugify-median", "description": "Fix slugify and median",
            "phases": [{"index": 0, "title": "Fix", "detail": "one owner per function",
                        "standard": "do"},
                       {"index": 1, "title": "Merge", "standard": "decide"}]}}),
        json!({"type": "workflow", "data": {"event": "phase_started", "id": "wf-001",
            "index": 0, "title": "Fix"}}),
        json!({"type": "workflow", "data": {"event": "phase_ended", "id": "wf-001",
            "index": 0, "title": "Fix", "end": "done"}}),
        json!({"type": "workflow", "data": {"event": "phase_added", "id": "wf-001",
            "phase": {"index": 1, "title": "Review", "detail": "a second reviewer",
                      "standard": "check"},
            "why": "the reviewers disagreed"}}),
        json!({"type": "workflow", "data": {"event": "phase_skipped", "id": "wf-001",
            "index": 0, "title": "Understand", "why": "Your prompt is the goal"}}),
        json!({"type": "workflow", "data": {"event": "note",
            "text": "review:median found median([]) returns None, so a second round",
            "workflow": "wf-001"}}),
        goal(
            json!({"event": "set", "objective": "median handles empty lists",
            "criteria": [{"id": "G1", "text": "median([]) raises", "short": "Empty list",
                          "check": {"test": "test_median_empty"}}],
            "attempts": [], "status": "pursuing", "max_attempts": 5}),
        ),
        goal(json!({"event": "attempt_started", "n": 1, "title": "Empty List", "phase": 1})),
        goal(json!({"event": "checked", "attempt": 1,
            "verdicts": [{"id": "G1", "verdict": "cant_tell", "evidence": "",
                          "missing": "no test runs it"}],
            "overall": "not_yet", "reason": "G1 has no test yet"})),
        goal(json!({"event": "status_changed", "status": "stuck", "by": {"party": "conductor"}})),
        goal(json!({"event": "answered", "question": "Accented letters?",
            "answer": "Keep them", "by": "lead"})),
        goal(json!({"event": "assumption_changed", "id": 2, "to": "Strip them", "by": "you"})),
        json!({"type": "agent", "data": {"type": "workflow", "workflow": {"id": "w4d273qmn",
            "call": "toolu_01", "name": "probe", "description": "Asks and answers",
            "phases": [{"index": 1, "title": "Ask"}, {"index": 2, "title": "Answer"}],
            "agents": [{"index": 1, "label": "ask:one", "phase": 1,
                        "model": "claude-haiku-4-5-20251001", "state": "done", "tokens": 10311,
                        "tool_calls": 0, "duration_ms": 3266, "started_at_ms": 1_791_076_604_610_u64,
                        "attempt": 1, "prompt": "Respond with a single word",
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
        card_id: None,
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
fn a_decided_checkpoint_built_in_rust_round_trips() {
    let decided = RunEventKind::Checkpoint(CheckpointEvent::Decided(CheckpointDecided {
        id: CheckpointId::numbered(1),
        action: AnswerAction::Choose {
            value: "per API key".into(),
        },
        text: None,
        by: Party::User,
        evidence_sha256: "ab".into(),
        shadow: None,
        cards: Vec::new(),
    }));
    let value = serde_json::to_value(&decided).unwrap();
    assert_eq!(value["type"], "checkpoint");
    assert_eq!(value["data"]["event"], "decided");
    assert_eq!(
        serde_json::from_value::<RunEventKind>(value).unwrap(),
        decided
    );
}

#[test]
fn every_ui_command_round_trips() {
    let commands = vec![
        json!({"type": "submit", "text": "hi"}),
        json!({"type": "slash", "command": {"command": "solo", "task": "fix the typo"}}),
        json!({"type": "slash", "command": {"command": "reassign", "card": null,
               "from": "claude-code", "to": "codex/gpt-x"}}),
        json!({"type": "slash", "command": {"command": "narrator",
               "change": {"change": "model", "model": "ollama/qwen3"}}}),
        json!({"type": "mention", "target": {"target": "role", "value": "reviewer"},
               "text": "be strict"}),
        json!({"type": "answer", "to": {"target": "checkpoint", "run": "run-01", "id": "cp-003"},
               "action": {"action": "approve_with_edits"}, "text": "cap at 100/min",
               "evidence_sha256": "e3b0", "cards": []}),
        json!({"type": "answer", "to": {"target": "checkpoint", "run": "run-01", "id": "cp-009"},
               "action": {"action": "request_changes"}, "text": "tighten the limit",
               "evidence_sha256": "e3b0", "cards": ["card-002", "card-004"]}),
        json!({"type": "answer", "to": {"target": "choice", "id": "ch-002"},
               "action": {"action": "choose", "value": "solo"}, "text": null,
               "evidence_sha256": null, "cards": []}),
        json!({"type": "answer", "to": {"target": "checkpoint", "run": "run-01", "id": "cp-005"},
               "action": {"action": "candidate", "worker": "codex/gpt-x"}, "text": null,
               "evidence_sha256": "e3b0", "cards": [], "remember": true}),
        json!({"type": "set_aside", "to": {"target": "choice", "id": "ch-002"}}),
        json!({"type": "steer", "run": "run-01", "session": "s-003", "text": "use EVAL"}),
        json!({"type": "take_over", "run": "run-01", "card": "card-002"}),
        json!({"type": "take_over_ended", "run": "run-01", "card": "card-002", "exit_code": 0}),
        json!({"type": "reassign", "run": "run-01", "card": "card-002", "to": "codex"}),
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
        json!({"type": "spot_check", "run": "run-01", "id": "cp-003", "correct": true}),
        json!({"type": "request", "view": {"view": "runs"}}),
        json!({"type": "request", "view": {"view": "all_repos"}}),
        json!({"type": "request", "view": {"view": "workflow_library"}}),
        json!({"type": "request", "view": {"view": "diff", "run": "run-01", "card": "card-002",
               "scope": "last_turn"}}),
        json!({"type": "request", "view": {"view": "raw", "run": "run-01", "session": "s-003"}}),
        json!({"type": "request", "view": {"view": "settings", "section": "budget"}}),
        json!({"type": "request", "view": {"view": "models", "role": "reviewer"}}),
        json!({"type": "set_setting", "key": "approvals.merge.mode", "value": "\"auto\"",
               "scope": "project"}),
        json!({"type": "set_setting", "key": "roles.reviewer.effort", "value": "\"high\"",
               "scope": "project_local"}),
        json!({"type": "set_setting", "key": "budget.run_usd", "value": "30.0",
               "scope": "session"}),
        json!({"type": "set_setting", "key": "ui.theme", "value": "\"light\"",
               "scope": "global"}),
        json!({"type": "slash", "command": {"command": "preset", "name": "coordinated-crew",
               "action": "copy_to_project"}}),
        json!({"type": "slash", "command": {"command": "preset", "name": null, "action": "use"}}),
        json!({"type": "slash", "command": {"command": "preset", "name": "peer-crew",
               "action": "show"}}),
        json!({"type": "slash", "command": {"command": "preset", "name": "peer-crew",
               "action": "copy"}}),
        json!({"type": "slash", "command": {"command": "preset", "name": "peer-crew",
               "action": "diff"}}),
        json!({"type": "slash", "command": {"command": "effort", "role": null,
               "level": "high"}}),
        json!({"type": "slash", "command": {"command": "effort", "role": "reviewer",
               "level": null}}),
        json!({"type": "slash", "command": {"command": "agents",
               "action": {"kind": "add", "name": "ollama"}}}),
        json!({"type": "slash", "command": {"command": "agents", "action": {"kind": "list"}}}),
        json!({"type": "slash", "command": {"command": "budget", "key": "run_usd",
               "value": "30"}}),
        json!({"type": "slash", "command": {"command": "handoff", "role": "owner",
               "key": "mode", "value": "auto"}}),
        json!({"type": "slash", "command": {"command": "permissions", "key": null,
               "value": null}}),
        json!({"type": "slash", "command": {"command": "permissions",
               "key": "sandbox.network", "value": "true"}}),
        json!({"type": "slash", "command": {"command": "permissions", "role": "owner",
               "key": "mode", "value": "tool"}}),
        json!({"type": "slash", "command": {"command": "approvals", "key": null,
               "value": null}}),
        json!({"type": "slash", "command": {"command": "approvals", "key": "merge.mode",
               "value": "auto"}}),
        json!({"type": "slash", "command": {"command": "theme", "name": "light"}}),
        json!({"type": "slash", "command": {"command": "setup"}}),
        json!({"type": "slash", "command": {"command": "clear"}}),
        json!({"type": "slash", "command": {"command": "doctor"}}),
        json!({"type": "slash", "command": {"command": "connect", "front_end": "openclaw"}}),
        json!({"type": "slash", "command": {"command": "verbosity", "level": "quiet"}}),
        json!({"type": "slash", "command": {"command": "verbosity", "level": null}}),
        json!({"type": "slash", "command": {"command": "resume",
               "target": {"kind": "conversation", "id": "chat-003"}}}),
        json!({"type": "slash", "command": {"command": "resume",
               "target": {"kind": "run", "id": "run-01"}}}),
        json!({"type": "slash", "command": {"command": "stop", "run": null}}),
        json!({"type": "slash", "command": {"command": "workflows"}}),
        json!({"type": "rewind", "card": "card-002", "checkpoint": 1}),
        json!({"type": "summary"}),
        json!({"type": "workflow_file", "name": "solo", "action": "copy_to_project"}),
        json!({"type": "slash", "command": {"command": "config", "key": "narrator.model",
               "value": null, "reset": true}}),
        json!({"type": "reset_setting", "key": "ui.theme", "scope": "global"}),
        json!({"type": "reset_setting", "key": "roles.reviewer.workers",
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
        json!({"command": "rewind", "card": null}),
        json!({"command": "theme", "name": null}),
        json!({"command": "budget", "key": null, "value": null}),
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
        json!({"command": "resume", "target": {"kind": "vendor_session",
               "id": {"vendor": "codex", "id": "019a2f-4c"}}}),
        json!({"command": "rename", "name": "Login work"}),
        json!({"command": "branch", "name": null}),
        json!({"command": "fork", "prompt": "try the other parser"}),
        json!({"command": "btw", "question": "what is a rope?"}),
        json!({"command": "recap"}),
        json!({"command": "copy", "nth": 2}),
        json!({"command": "export", "file": "chat.txt"}),
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
        json!({"command": "vendor", "name": "simplify", "args": "the parser"}),
        json!({"command": "vendor", "name": "anthropic-skills:example-a"}),
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
           "preset": "peer-crew", "state": "parked", "stopped_at": "plan checkpoint",
           "updated": "2026-09-29T12:00:00Z", "spent": spend(), "sessions_to_resume": 2,
           "waiting": ["cp-003"], "live": false, "remind": true})
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
        "repo": "~/code/api", "preset": "peer-crew", "agents": [], "runs_going": 0}))
    .unwrap();
    assert!(matches!(
        event,
        AppEvent::Welcome {
            default_agent: None,
            effort: None,
            ..
        }
    ));
}

#[test]
fn every_app_event_round_trips() {
    let mut events = vec![
        json!({"type": "welcome", "version": "0.0.1", "repo": "~/code/api", "preset": "peer-crew",
               "agents": [{"id": "codex", "version": "0.158.0", "ready": true, "note": null}],
               "runs_going": 1, "example_prompt": "fix the flaky retry test",
               "default_agent": "codex/gpt-5.6-luna", "effort": "high"}),
        json!({"type": "attached", "summary": run_summary()}),
        json!({"type": "attached", "summary": run_summary(), "started_here": true}),
        json!({"type": "detached", "run": "run-01"}),
        json!({"type": "run", "event": envelope(json!({"type": "note",
               "data": {"level": "info", "text": "x"}}))}),
        json!({"type": "chat", "event": {"chat": "turn_started", "turn": "t-012", "text": "hi",
               "worker": "claude-code", "read_only": true, "commit": "8b41d07",
               "uncommitted": ["notes.txt"], "settings_sha256": "5e7f"}}),
        json!({"type": "chat", "event": {"chat": "agent", "turn": "t-012",
               "event": {"type": "text_delta", "text": "Hel", "parent": null}}}),
        json!({"type": "chat", "event": {"chat": "turn_finished", "turn": "t-012",
               "reply": {"reply": "unclear", "markdown": "Could be either.", "estimate": null,
                         "would_pick": "solo"},
               "spend": spend(), "elapsed_ms": 4100}}),
        json!({"type": "chat", "event": {"chat": "turn_failed", "turn": "t-013",
               "error": "usage limit"}}),
        json!({"type": "chat", "event": {"chat": "turn_cancelled", "turn": "t-014"}}),
        json!({"type": "choice", "choice": {"id": "ch-002", "kind": "start_work",
               "title": "Start work on this?", "body": ["Triage would pick solo."],
               "options": [{"key": "s", "label": "solo", "action": {"action": "choose",
                            "value": "solo"}, "recommended": true, "detail": null}],
               "free_text": "none", "bell": true}}),
        json!({"type": "choice_closed", "id": "ch-002", "outcome": "No run started."}),
        json!({"type": "narrator", "line": narrator_line()}),
        json!({"type": "narrator_status", "status": {"mode": "live", "model": "ollama/qwen3",
               "spent": spend(), "cap": "$0.50", "capped": false, "interval_s": 30}}),
        json!({"type": "mode", "mode": {"mode": "idle"}}),
        json!({"type": "mode", "mode": {"mode": "parked", "run": "run-01",
               "reason": {"why": "checkpoint"}}}),
        json!({"type": "status", "status": {"title": "run-01", "repo": "~/code/api",
               "preset": "peer-crew", "run": "run-01", "default_agent": "claude-code",
               "quotas": [{"label": "Claude", "reading": quota_reading(), "health": "healthy"}],
               "spent": spend(), "limit": usd(20.0), "narrator": "on_demand",
               "approval_mode": "supervised"}}),
        json!({"type": "notice", "level": "warn", "text": "codex is untested at 0.170.0"}),
        json!({"type": "view", "data": {"data": "runs", "runs": [run_summary()]}}),
        json!({"type": "view", "data": {"data": "config", "entries": [{"key": "ui.keys",
               "value": "\"vim\"", "layer": "global", "safety": false}]}}),
        json!({"type": "view", "data": {"data": "approvals", "view": {"points": [{"kind": "merge",
               "mode": "shadow", "decide_with": "rules", "matches_in_a_row": 4, "misses": 0,
               "suggest_auto": false}], "spot_checks_waiting": 1, "approver": null}}}),
        json!({"type": "view", "data": {"data": "diff", "diff": {"card": "card-002",
               "stat": diff_stat(), "text": "diff --git", "path": "/state/cards/d"}}}),
        json!({"type": "view", "data": {"data": "raw", "raw": {"session": "s-003",
               "lines": ["{}"], "truncated": false, "path": "/state/raw", "vendor_transcript": null}}}),
        json!({"type": "take_over", "ticket": {"card": "card-002",
               "command": {"program": "codex", "args": ["resume", "abc"], "env_set": [],
                           "env_remove": [], "stdin": null, "cwd": "/wt/card-002", "files": [],
                           "keep_stdin_open": false},
               "worktree": "/wt/card-002", "vendor": "codex"},
               "place": "tmux-split", "run": "run-01"}),
        json!({"type": "ui_settings", "vim": true, "verbosity": "normal", "ui": null}),
        json!({"type": "ui_settings", "vim": false, "verbosity": "normal",
               "ui": serde_json::to_value(UiSettings::default()).unwrap()}),
        json!({"type": "recap", "text": "Card 2 is in review.", "model": null}),
        json!({"type": "taken_back", "texts": ["keep a fallback"]}),
        json!({"type": "setting_changed", "key": "approvals.merge.mode", "value": "\"auto\"",
               "scope": "project", "previous": "\"supervised\"", "reset": false}),
        json!({"type": "condensed_replay", "run": "run-01",
               "turns": [{"from": {"party": "user"}, "text": "add per-key rate limiting"},
                         {"from": {"party": "agent", "session": "s-003", "worker": "codex/gpt-x"},
                          "text": "Redis keeps the counters across restarts."}],
               "milestones": ["Goal approved", "Plan approved · 5 cards"],
               "recap": "Card 2 is in review; the plan checkpoint waits for you.",
               "recap_model": null}),
        json!({"type": "replay_done", "run": "run-01", "seq": 412}),
        json!({"type": "view", "data": {"data": "runs", "runs": [run_summary()],
               "chats": [{"chat": "chat-002", "title": "hi", "updated": "2026-09-29T12:00:00Z",
                          "worker": "claude-code", "turns": 3}]}}),
        json!({"type": "chat_resumed", "chat": "chat-002", "title": "hi", "worker": "claude-code",
               "earlier": [{"from": {"party": "user"}, "text": "hi"}],
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
        json!({"type": "view", "data": {"data": "runs", "runs": [], "sessions": [
               {"vendor": "claude-code", "id": "0a1b-2c3d", "title": "Fix the flaky test",
                "folder": "/repo", "updated": "2026-09-29T12:00:00Z", "model": "claude-opus-5-5"}]}}),
        json!({"type": "view", "data": {"data": "effort", "slider": {"role": "reviewer",
               "worker": "claude-code/claude-opus-5-5", "model": "Opus 5.5",
               "stops": [{"level": "low", "description": "Quick answers with light reasoning"},
                         {"level": "xhigh", "description": "Extra depth"}],
               "chosen": 1, "default": 1}}}),
        json!({"type": "view", "data": {"data": "crew", "panel": {"repo": "~/app", "rows": [
               {"target": {"kind": "conversation", "chat": "chat-002"}, "state": "ready",
                "title": "Explain the input handling", "repo": "~/app",
                "worker": "claude-code", "latest": "It reads keys in one loop.",
                "updated": "2026-09-29T12:00:00Z", "details": {}}]}}}),
        json!({"type": "vendor_commands", "set": {"vendor": "claude-code", "title": "Claude Code",
               "commands": [
                   {"name": "clear", "aliases": ["reset", "new"], "help": "New conversation",
                    "kind": "argonauts", "command": "clear", "source": "built_in"},
                   {"name": "ship", "args": "[branch]", "help": "Ship it", "kind": "vendor",
                    "source": "custom", "scope": "project", "path": "/repo/.claude/commands/ship.md"},
                   {"name": "voice", "help": "Talk to it", "kind": "screen_only",
                    "source": "built_in"}],
               "keywords": [{"word": "ultrathink", "look": "rainbow", "echoed": true}]}}),
        json!({"type": "vendor_commands", "set": {"vendor": "codex", "title": "Codex",
               "commands": []}}),
        json!({"type": "view", "data": {"data": "all_repos", "repos": [
               {"repo": "~/src/api", "runs": [run_summary()]},
               {"repo": "~/src/web", "runs": []}]}}),
        json!({"type": "view", "data": {"data": "workflow_library", "workflows": [
               {"name": "solo", "origin": "yours", "source": "~/.argonauts/workflows/solo.yaml",
                "description": "One agent", "text": "name: solo\n", "edited": true,
                "in_use": false}], "refresh": true}}),
        json!({"type": "view", "data": {"data": "checkpoints", "run": "run-01",
               "card": "card-002", "checkpoints": [{"card": "card-002", "n": 1,
               "at": "2026-09-29T12:00:00Z", "commit": "0123456",
               "from": "claude-code/opus", "to": ["codex"], "reason": "usage limit",
               "reverted_here": false}]}}),
        json!({"type": "view", "data": {"data": "settings", "rows": [{"key": "budget.run_usd",
               "value": "20.0", "layer": "built-in", "default_scope": "project", "safety": true,
               "allowed": [], "description": "Run ceiling in dollars"}], "command": "budget",
               "section": null}}),
        json!({"type": "view", "data": {"data": "models", "choices": [{"worker": "codex/gpt-x",
               "family": "gpt", "access": "subscription", "cost": {"unit": "quota", "amount": 0.02},
               "quota": null, "efforts": ["low", "high"], "strengths": ["refactor"],
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
        json!({"type": "setting_changed", "key": "budget.run_usd", "value": "30.0",
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
               "safety": false, "allowed": [], "description": "The model this harness uses"},
               {"key": "approvals.merge.mode", "value": "\"supervised\"", "layer": "session",
               "default_scope": "project", "safety": true,
               "allowed": ["\"supervised\"", "\"shadow\"", "\"auto\""],
               "description": "The merge checkpoint"}], "command": null,
               "section": "approvals"}}),
        json!({"type": "view", "data": {"data": "models", "choices": [{"worker": "ollama/llama3.2",
               "family": "llama", "access": "local", "cost": {"unit": "free"},
               "quota": null, "efforts": [], "strengths": [], "current": false}],
               "role": null, "refreshed": null, "refreshing": false, "update": true}}),
        json!({"type": "view", "data": {"data": "models", "choices": [
               {"worker": "claude-code", "family": "claude", "access": "subscription",
                "cost": {"unit": "quota", "amount": 0.02}, "quota": null, "efforts": [],
                "strengths": [], "current": false, "description": "Best for everyday, complex tasks",
                "recommended": true, "runs": "Opus 5.5"},
               {"worker": "claude-code/claude-opus-5-5", "family": "claude",
                "access": "subscription", "cost": {"unit": "quota", "amount": 0.02},
                "quota": null, "efforts": ["low", "medium", "high", "xhigh", "max"],
                "strengths": [], "current": true, "name": "Opus 5.5",
                "description": "For complex work and everyday tasks", "default_effort": "xhigh",
                "effort": "max"},
               {"worker": "claude-code/opus", "family": "claude", "access": "subscription",
                "cost": {"unit": "unknown"}, "quota": null, "efforts": [], "strengths": [],
                "current": false, "alias": true, "runs": "Opus 5.5"},
               {"worker": "claude-code/claude-opus-6", "family": "claude",
                "access": "subscription", "cost": {"unit": "unknown"}, "quota": null,
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
        "detected_gates",
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
        "cards": [{"card": "card-002", "title": "Redis store", "state": "running",
                   "owner": "codex", "note": "6m 12s · 38k tok"}],
        "recent": [], "budget": null, "quotas": [], "waiting": []}}});
    round_trip::<AppEvent>(status);
}

#[test]
fn trace_lines_round_trip() {
    let command = UiCommand::Slash {
        command: SlashCommand::Solo {
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
            card_id: None,
            session: None,
            source: "conductor".into(),
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
    let answer = json!({"id": "cp-003", "action": {"action": "merge"}, "text": null,
                        "evidence_sha256": "e3b0", "cards": []});
    let requests = vec![
        json!({"request": "hello", "client": "app", "version": "0.0.1"}),
        json!({"request": "subscribe", "from_seq": 12, "from_snapshot": false}),
        json!({"request": "subscribe", "from_seq": 0, "from_snapshot": true}),
        json!({"request": "steer", "session": "s-003", "text": "use EVAL"}),
        json!({"request": "mention", "target": {"target": "all"}, "text": "wrap up"}),
        json!({"request": "guidance", "text": "keep it small"}),
        json!({"request": "guidance", "text": "keep it small", "front_end": "chat-bot"}),
        json!({"request": "pause", "now": false}),
        json!({"request": "pause", "now": true, "front_end": "chat-bot"}),
        json!({"request": "cancel_pause"}),
        json!({"request": "stop"}),
        json!({"request": "stop_session", "session": "s-003"}),
        json!({"request": "take_back", "messages": ["m-001"]}),
        json!({"request": "approval_mode", "mode": "auto", "token": "t0k"}),
        json!({"request": "reassign", "card": "card-002", "to": "codex", "rule_override": null,
               "token": null}),
        json!({"request": "reassign", "card": "card-002", "to": "claude-code/opus",
               "rule_override": "review.other_family", "token": "t0k"}),
        json!({"request": "take_over", "card": "card-002", "token": "t0k"}),
        json!({"request": "take_over_ended", "card": "card-002", "token": "t0k"}),
        json!({"request": "answer", "answer": answer, "token": "t0k"}),
        json!({"request": "status"}),
        json!({"request": "diff", "card": "card-002", "scope": "card"}),
        json!({"request": "raw", "session": "s-003"}),
        json!({"request": "revert", "card": "card-002", "checkpoint": 2}),
        json!({"request": "spot_check", "id": "cp-003", "correct": false}),
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
        json!({"reply": "take_over", "ticket": {"card": "card-002", "command": null,
               "worktree": "/wt/card-002", "vendor": "openrouter"}}),
        json!({"reply": "status", "summary": {"run": null, "goal": [], "plan": null,
               "decisions": [], "cards": [], "recent": [], "budget": null, "quotas": [],
               "waiting": []}}),
        json!({"reply": "diff", "diff": {"card": "card-002", "stat": diff_stat(), "text": "",
               "path": "/d"}}),
        json!({"reply": "raw", "raw": {"session": "s-003", "lines": [], "truncated": true,
               "path": "/r", "vendor_transcript": "/v"}}),
    ];
    for body in replies {
        round_trip::<Frame<ControlReply>>(json!({"id": 1, "body": body}));
    }
}

#[test]
fn api_requests_round_trip() {
    round_trip::<StartRun>(
        json!({"request": "fix the typo", "mode": "solo", "forced": true,
        "estimate": null, "triage": null, "chat_turn": "t-001", "continues": null,
        "overrides": ["solo.review=true"], "include_uncommitted": false,
        "default_agent": "codex/gpt-x", "approval_mode": "supervised"}),
    );
    round_trip::<CheckpointAnswer>(json!({"id": "cp-001", "action": {"action": "send_back"},
        "text": "split the store", "evidence_sha256": "e3b0", "cards": []}));
    round_trip::<CheckpointAnswer>(
        json!({"id": "cp-009", "action": {"action": "request_changes"},
        "text": "tighten the limit", "evidence_sha256": "e3b0", "cards": ["card-002"]}),
    );
    // Older answers without `cards` still read, as every card.
    let old: CheckpointAnswer = serde_json::from_value(json!({"id": "cp-001",
        "action": {"action": "merge"}, "text": null, "evidence_sha256": "e3b0"}))
    .unwrap();
    assert!(old.cards.is_empty());
    let summary: RunSummary = round_trip(run_summary());
    assert_eq!(summary.state, RunState::Parked);
    assert_eq!(StatusSummary::default().cards.len(), 0);
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
        "description": "Supervised, shadow, or auto",
        "allowed": ["\"supervised\"", "\"shadow\"", "\"auto\""],
        "default_scope": "project", "safety": true}));
    assert!(meta.safety);
    round_trip::<SettingMeta>(json!({"key": "agents.*.model", "description": "",
        "allowed": [], "default_scope": "project_local", "safety": false}));
    round_trip::<ParsedInput>(json!({"input": "slash",
        "command": {"command": "preset", "name": "peer-crew", "action": "copy_to_project"}}));
    round_trip::<ParsedInput>(json!({"input": "slash",
        "command": {"command": "handoff", "role": null, "key": null, "value": null}}));
    // An older `preset` command without an action reads as switching to it.
    let old: SlashCommand =
        serde_json::from_value(json!({"command": "preset", "name": "solo"})).unwrap();
    assert_eq!(
        old,
        SlashCommand::Preset {
            name: Some("solo".into()),
            action: PresetAction::Use
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
        "choices": [{"worker": "codex", "family": "gpt", "access": "subscription",
                     "cost": {"unit": "unknown"}, "quota": null, "efforts": [],
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
fn a_recipe_promotion_packet_is_an_object_without_kind() {
    use argonauts_core::events::checkpoint::CheckpointPacket;
    let packet = CheckpointPacket::RecipePromotion(json!({"recipe": "small-fix", "gain": 4.0}));
    let value = serde_json::to_value(&packet).unwrap();
    assert_eq!(value["kind"], "recipe_promotion");
    assert_eq!(
        serde_json::from_value::<CheckpointPacket>(value).unwrap(),
        packet
    );
    // Anything but an object has nowhere to put the tag, so it can't be written.
    for shape in [json!("text"), json!([1, 2]), json!(true), json!(3)] {
        assert!(serde_json::to_value(CheckpointPacket::RecipePromotion(shape)).is_err());
    }
}

#[test]
fn agent_events_keep_their_flat_wire_format() {
    round_trip::<AgentEvent>(json!({"type": "quota", "reading": quota_reading()}));
    round_trip::<AgentEvent>(json!({"type": "subagent", "update": {"id": "toolu_1",
        "parent": "toolu_0", "kind": "subagent", "label": "Explore", "prompt": null,
        "status": "running", "last_message": null, "log_uri": null, "steps_visible": true}}));
}

#[test]
fn names_parse_and_display() {
    let worker: WorkerRef = "openrouter/z-ai/glm-5".parse().unwrap();
    assert_eq!(worker.worker, "openrouter");
    assert_eq!(worker.model.as_deref(), Some("z-ai/glm-5"));
    assert_eq!(worker.to_string(), "openrouter/z-ai/glm-5");
    assert_eq!(worker.vendor(), VendorId("openrouter".into()));
    assert!("".parse::<WorkerRef>().is_err());
    assert!("codex/-x".parse::<WorkerRef>().is_err());
    assert!("codex/".parse::<WorkerRef>().is_err());
    assert!("co dex".parse::<WorkerRef>().is_err());
    assert_eq!(
        serde_json::to_value(&worker).unwrap(),
        json!("openrouter/z-ai/glm-5")
    );
    assert!(serde_json::from_value::<WorkerRef>(json!("")).is_err());

    assert_eq!(SessionKey::numbered(7).as_str(), "s-007");
    assert_eq!(CheckpointId::numbered(3).as_str(), "cp-003");
    assert_eq!(ChoiceId::numbered(2).as_str(), "ch-002");
    assert_eq!(TurnId::numbered(12).as_str(), "t-012");
    assert_eq!(ChatId::numbered(4).as_str(), "chat-004");
    assert_eq!(MessageId::numbered(31).as_str(), "m-031");
    assert_eq!(DecisionId::numbered(3).as_str(), "D-003");
    assert_eq!(DraftId::numbered(2).as_str(), "dr-002");
    assert!(SessionKey::parse("s-007").is_some());
    assert!(SessionKey::parse("../x").is_none());
    assert!(SessionKey::parse("").is_none());

    let roles = [
        RoleName::Default,
        RoleName::Coordinator,
        RoleName::PhaseLead,
        RoleName::Restater,
        RoleName::Differ,
        RoleName::Drafter,
        RoleName::Mapper,
        RoleName::Critic,
        RoleName::Spike,
        RoleName::Judge,
        RoleName::Owner,
        RoleName::Consultant,
        RoleName::Reviewer,
        RoleName::Adjudicator,
        RoleName::Retro,
        RoleName::Approver,
        RoleName::Narrator,
    ];
    for role in roles {
        assert_eq!(serde_json::to_value(role).unwrap(), json!(role.as_str()));
    }
}

#[test]
fn settings_default_matches_the_frozen_defaults_file() {
    let text = toml::to_string_pretty(&Settings::default()).unwrap();
    let frozen = include_str!("fixtures/settings-defaults.toml");
    assert_eq!(text.trim_end(), frozen.trim_end());
    let back: Settings = toml::from_str(frozen).unwrap();
    assert_eq!(back, Settings::default());
    assert_eq!(back.preset, "peer-crew");
}

#[test]
fn partial_settings_fill_in_defaults() {
    let partial: Settings = toml::from_str(
        r#"
preset = "coordinated-crew"
native_fanout = "off"
[roles.owner]
workers = ["claude-code/opus"]
[roles.owner.handoff]
mode = "auto"
fallback = ["codex/gpt-x", { worker = "openrouter/a/b", max_budget_usd = 2.0 }]
[roles.reviewer]
workers = ["openrouter/m-a", "openrouter/m-b"]
blind = true
native_fanout = { max = 2 }
[approvals.merge]
mode = "shadow"
decide_with = "rules_and_model"
rules = { min_changed_line_coverage = 0.8, max_combined_diff_lines = 400 }
otherwise = "open_pull_request"
[ui]
narrator = "live"
keys = "vim"
takeover = "tmux-split"
"#,
    )
    .unwrap();
    assert_eq!(partial.preset, "coordinated-crew");
    assert_eq!(partial.roles[&RoleName::Reviewer].workers.len(), 2);
    assert_eq!(partial.roles[&RoleName::Owner].handoff.fallback.len(), 2);
    assert_eq!(partial.budget.run_usd, Some(20.0));
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
family = "claude"
kind = "harness"
access = "subscription"
tools = true
cost = { unit = "quota" }
strengths = ["refactor"]
good_for = ["owner"]
efforts = ["low", "medium", "high", "max"]

[[model]]
id = "codex/local-model"
family = "gpt"
kind = "harness"
access = "local"
cost = { unit = "none" }
launch = { oss = true, local_provider = "ollama" }

[[harness]]
id = "my-agent"
display_name = "My Agent"
binary = "my-agent"
read_only_args = ["--read-only", "--model", "{model}"]
build_args = ["--model", "{model}"]
family = "mine"
"#,
    )
    .unwrap();
    assert_eq!(registry.models.len(), 2);
    assert_eq!(registry.models[0].efforts, ["low", "medium", "high", "max"]);
    // A model whose levels aren't known lists none, and `/effort` then offers every level.
    assert!(registry.models[1].efforts.is_empty());
    assert_eq!(registry.harnesses[0].id, "my-agent");
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
        "limits.*",
        "sandbox.*",
        "approvals.*",
        "budget.enabled",
        "agents.*.auth",
        "agents.*.binary",
        "agents.*.extra_args",
        "endpoints.*.base_url",
        "endpoints.*.header_env.*",
        "roles.*.handoff.allow_api_billing",
        "handoff.allow_api_billing",
        "roles.*.sandbox.*",
        "mcp.*",
        "merge.*",
        "gates.*",
        "notifications.chat.*",
    ] {
        assert!(SAFETY_KEYS.contains(&key), "{key}");
    }
}

fn evidence() -> EvidencePacket {
    EvidencePacket {
        kind: argonauts_core::events::checkpoint::CheckpointKind::Merge,
        goal: None,
        facts: EvidenceFacts {
            subject: Some(EvidenceSubject {
                base_commit: "8b41d07".into(),
                head_commit: "c3".into(),
                tree_sha: "t9".into(),
            }),
            gates_passed: Some(true),
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
                path: Some("/state/evidence/cp-003.diff".into()),
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
    changed.facts.gates_passed = Some(false);
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
        "escalation",
        "budget",
        "criteria_unchanged",
        "promotion",
        "irreversible_actions",
    ] {
        assert!(unknown["facts"].get(key).is_none(), "{key}");
    }
    let usd = |amount| BudgetAmount {
        unit: BudgetUnit::Usd,
        amount,
    };
    let mut known = evidence();
    known.facts.escalation = Some(EscalationFacts {
        handoff_mode: HandoffMode::Auto,
        families_tried: vec!["claude".into()],
        next_family: Some("gpt".into()),
        extra_attempts: 1,
        fits_reserve: true,
    });
    known.facts.budget = Some(BudgetFacts {
        remaining_cards: usd(4.5),
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

/// A sample answer for a step: it must pass the strict schema the worker gets, and decode.
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
    use argonauts_core::steps::{card, goal, intake, judgment, narrate, plan};
    check_step_output::<intake::TriageEstimate>(json!({"kind": "change",
        "answer_markdown": "I'd add a middleware.",
        "estimate": {"task_type": "api-feature", "summary": "s", "files_likely": ["a"],
                     "file_count": 3, "risk_labels": [], "tests_exist": true, "unknowns": []},
        "how_to_run": [{"says": "have codex coordinate", "kind": "coordinate", "who": "codex",
                        "model": "", "effort": "", "value": "", "scope": "this_request"}]}));
    check_step_output::<intake::ChatAnswer>(json!({"answer_markdown": "Hello!"}));
    check_step_output::<goal::Restate>(json!({"summary": "s", "success_criteria": ["c"],
        "in_scope": [], "out_of_scope": [], "assumptions": [], "open_questions": []}));
    check_step_output::<goal::Differ>(json!({"agreements": ["a"], "differences": [{"topic": "t",
        "readings": [{"label": "A", "reading": "r"}], "tag": "detail", "question": null}]}));
    check_step_output::<plan::Draft>(plan_draft());
    check_step_output::<plan::Map>(json!({"topics": [{"name": "n", "disputed": false,
        "positions": [{"label": 1, "summary": "s", "quote": "q"}]}]}));
    check_step_output::<plan::Critique>(json!({"points": [{"plan": 2, "topic": "t",
        "claim": "c", "severity": "low", "suggestion": "s"}]}));
    check_step_output::<plan::ProposeSpike>(json!({"proposals": []}));
    check_step_output::<plan::SpikeReportStep>(json!({"topic": "t", "option": "o",
        "measurements": [{"metric": "p50", "value": 2.5, "unit": "ms"}], "finding": "f",
        "works": false}));
    check_step_output::<plan::Judge>(json!({"scores": [{"plan": 1, "criteria": [4.0, 3.5],
        "rationale": "r", "addressed_to_judges": false}]}));
    let cards = json!([{"key": "store", "title": "Store", "task": "t", "kind": "build",
        "owned_paths": ["src/store.ts"], "read_only_paths": [], "depends_on": [],
        "criteria": ["G1"]}]);
    check_step_output::<plan::PlanToCards>(json!({"cards": cards}));
    check_step_output::<plan::Coordinate>(json!({"plan": plan_draft(), "cards": cards,
        "assignments": [{"card": "store", "owner": "claude-code/opus",
                         "reviewers": ["openrouter/m-a"]}]}));
    check_step_output::<card::Predict>(json!({"p_pass_gates_first_try": 0.6,
        "p_within_time_cap": 0.8, "p_at_most_two_attempts": 0.9}));
    check_step_output::<card::Build>(json!({"status": "needs_decision", "summary": "s",
        "files_changed": [], "tests_run": [], "concerns": [], "questions": [],
        "change_requests": [], "memory_proposals": [],
        "answer": "Which store should the limiter use? The rest waits on that."}));
    check_step_output::<card::CloseRun>(
        json!({"answer": "Rate limiting is in, and the tests pass."}),
    );
    check_step_output::<card::AcceptanceTests>(json!({"files": [{"path": "tests/a.rs",
        "content": "#[test] fn a() {}"}], "criteria_covered": ["G1"]}));
    check_step_output::<card::Retro>(json!({"change": {"setting": "critique.rounds",
        "value": "0", "why": "w"}}));
    check_step_output::<card::Retro>(json!({"change": null}));
    check_step_output::<judgment::Review>(json!({"verdict": "reject", "summary": "s",
        "comments": [{"path": "a", "line": null, "severity": "high", "text": "t"}],
        "tamper_suspected": true}));
    check_step_output::<judgment::Adjudicate>(json!({"verdict": "request_changes",
        "reasons": ["r"], "required_changes": ["c"]}));
    check_step_output::<judgment::ModelApprover>(json!({"verdict": "escalate",
        "reasons": ["r"], "rules_cited": ["rule 2"]}));
    check_step_output::<narrate::Narrate>(json!({"lines": ["Card 2 passed its gates."]}));
}
