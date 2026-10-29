//! App-level events: the header, the bottom line, modes, conversation turns, notices, the narrator,
//! and app choices. Each is written as its wire JSON and read back, so a builder that drifts
//! from the contract fails loudly.

use argonauts_core::app::AppEvent;
use serde_json::{Value, json};

pub const MAIN_AGENT: &str = "claude-code/haiku";
pub const WORKFLOW: &str = "council";
pub const REPO: &str = "~/code/api";

pub fn app(value: Value) -> AppEvent {
    serde_json::from_value(value.clone())
        .unwrap_or_else(|err| panic!("not an AppEvent: {err}\n{value:#}"))
}

pub fn spend(output_tokens: u64, cost_usd: f64) -> Value {
    json!({"input_tokens": 1200, "output_tokens": output_tokens, "cached_tokens": 0,
           "cost_usd": cost_usd, "cost_partial": false, "cost_estimated": false})
}

pub fn welcome() -> AppEvent {
    app(
        json!({"type": "welcome", "version": "0.1.0", "repo": REPO, "workflow": WORKFLOW,
        "agents": [
            {"id": "claude-code", "version": "2.1.284", "ready": true, "note": null},
            {"id": "codex", "version": "0.158.0", "ready": true, "note": null},
            {"id": "antigravity-cli", "version": "1.2.13", "ready": true, "note": null}],
        "runs_going": 0}),
    )
}

/// The welcome with the example the app made from recent commit subjects.
pub fn welcome_with_example(example: &str) -> AppEvent {
    let mut value = serde_json::to_value(welcome()).expect("an AppEvent writes as JSON");
    value["example_prompt"] = json!(example);
    app(value)
}

pub fn welcome_with_runs(runs_going: u32) -> AppEvent {
    let mut value = serde_json::to_value(welcome()).expect("an AppEvent writes as JSON");
    value["runs_going"] = json!(runs_going);
    app(value)
}

/// An agent's plan usage reading, `used` from 0 to 1, for the five-hour window: read now, in a
/// window still open, since the bottom line drops a reading once its window has reset.
pub fn plan_usage(agent_id: &str, used: f64) -> Value {
    use time::format_description::well_known::Rfc3339;
    let status = if used >= 0.9 { "warning" } else { "allowed" };
    let now = time::OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .unwrap();
    let resets_at = (now + time::Duration::hours(3)).format(&Rfc3339).unwrap();
    json!({"agent_id": agent_id, "status": status, "used": used,
           "resets_at": resets_at, "window": "five_hour",
           "source": "rate_limit_event", "observed_at": now.format(&Rfc3339).unwrap(),
           "detail": null})
}

pub fn chip(label: &str, reading: Option<Value>, health: &str) -> Value {
    json!({"label": label, "reading": reading, "health": health})
}

/// The bottom line's settings with the spend part after plan usage, which the narrator's
/// spend follows.
pub fn line_with_spend() -> Value {
    json!({"settings": {"right": ["plan-usage", "spend"]}})
}

/// The bottom line of the idle screen: Claude 71%, Codex 38%, `$0.00`.
pub fn bottom_line() -> AppEvent {
    status_with(json!({}))
}

/// The idle bottom line with some fields replaced, such as `{"approval_mode": "auto"}`. The
/// line's own facts are laid over its defaults key by key, so Codex stays the CLI used most.
pub fn status_with(changes: Value) -> AppEvent {
    let mut status = json!({"title": "Argonauts Code", "repo": REPO, "workflow": WORKFLOW, "run": null,
        "main_agent": MAIN_AGENT,
        "plan_chips": [chip("Claude", Some(plan_usage("claude-code", 0.71)), "healthy"),
                   chip("Codex", Some(plan_usage("codex", 0.38)), "healthy")],
        "line": {"most_used": ["claude-code", "codex"]},
        "spent": spend(0, 0.0), "limit": null, "narrator": "on_demand",
        "approval_mode": "supervised"});
    if let (Value::Object(status), Value::Object(mut changes)) = (&mut status, changes) {
        if let (Some(Value::Object(line)), Some(Value::Object(changed))) =
            (status.get_mut("line"), changes.remove("line"))
        {
            line.extend(changed);
        }
        status.extend(changes);
    }
    app(json!({"type": "status", "status": status}))
}

pub fn mode_idle() -> AppEvent {
    app(json!({"type": "mode", "mode": {"mode": "idle"}}))
}

pub fn mode_answering(turn: &str) -> AppEvent {
    app(json!({"type": "mode", "mode": {"mode": "answering", "turn": turn}}))
}

pub fn mode_working(run: &str) -> AppEvent {
    app(json!({"type": "mode", "mode": {"mode": "working", "run": run}}))
}

pub fn mode_pausing(run: &str) -> AppEvent {
    app(json!({"type": "mode", "mode": {"mode": "pausing", "run": run}}))
}

/// `why` is `approval`, `paused`, and so on.
pub fn mode_waiting(run: &str, why: &str) -> AppEvent {
    app(json!({"type": "mode", "mode": {"mode": "waiting", "run": run, "reason": {"why": why}}}))
}

pub fn turn_started(turn: &str, text: &str) -> AppEvent {
    app(
        json!({"type": "conversation", "event": {"conversation": "turn_started", "turn": turn, "text": text,
        "worker": MAIN_AGENT, "read_only": true, "commit": "8b41d07", "uncommitted": [],
        "settings_sha256": null}}),
    )
}

pub fn conversation_agent(turn: &str, event: Value) -> AppEvent {
    app(
        json!({"type": "conversation", "event": {"conversation": "agent", "turn": turn, "event": event}}),
    )
}

pub fn text_delta(turn: &str, text: &str) -> AppEvent {
    conversation_agent(
        turn,
        json!({"type": "text_delta", "text": text, "parent": null}),
    )
}

pub fn conversation_usage(turn: &str, output_tokens: u64) -> AppEvent {
    conversation_agent(
        turn,
        json!({"type": "usage", "input_tokens": 1200, "output_tokens": output_tokens,
               "cached_tokens": 0, "cost_usd": null, "cumulative": true}),
    )
}

pub fn conversation_tool_started(turn: &str, id: &str, name: &str, input: Value) -> AppEvent {
    conversation_agent(
        turn,
        json!({"type": "tool_started", "id": id, "name": name, "input": input, "parent": null}),
    )
}

pub fn conversation_tool_finished(turn: &str, id: &str, output: &str) -> AppEvent {
    conversation_agent(
        turn,
        json!({"type": "tool_finished", "id": id, "ok": true, "output": output, "parent": null}),
    )
}

pub fn turn_finished(turn: &str, markdown: &str, elapsed_ms: u64) -> AppEvent {
    app(
        json!({"type": "conversation", "event": {"conversation": "turn_finished", "turn": turn,
        "reply": {"reply": "answer", "markdown": markdown},
        "spend": spend(300, 0.01), "elapsed_ms": elapsed_ms}}),
    )
}

pub fn turn_cancelled(turn: &str) -> AppEvent {
    app(json!({"type": "conversation", "event": {"conversation": "turn_cancelled", "turn": turn}}))
}

/// A whole answer: the turn starts, its words stream `step_ms` apart, and it finishes.
/// Times start at 0, for a script line after `await_command("submit")`.
pub fn conversation_turn(
    turn: &str,
    prompt: &str,
    answer: &str,
    step_ms: u64,
) -> Vec<(u64, AppEvent)> {
    let mut events = vec![(0, mode_answering(turn)), (0, turn_started(turn, prompt))];
    let mut at = 0;
    for word in words(answer) {
        at += step_ms;
        events.push((at, text_delta(turn, &word)));
    }
    at += step_ms;
    events.push((at, conversation_usage(turn, 300)));
    events.push((at, turn_finished(turn, answer, 3_000)));
    events.push((at, mode_idle()));
    events
}

/// Splits text into words that keep their trailing spaces, so joined they give it back.
pub fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        let word_ended = current.ends_with(char::is_whitespace) && !current.trim().is_empty();
        if !c.is_whitespace() && word_ended {
            words.push(std::mem::take(&mut current));
        }
        current.push(c);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// `level` is `info`, `warn`, or `error`.
pub fn notice(level: &str, text: &str) -> AppEvent {
    app(json!({"type": "notice", "level": level, "text": text}))
}

pub fn recap(text: &str) -> AppEvent {
    app(json!({"type": "recap", "text": text, "model": null}))
}

pub fn narrator_line(run: &str, text: &str) -> AppEvent {
    app(
        json!({"type": "narrator", "line": {"ts": "2026-09-30T12:00:00Z", "run": run,
        "kind": {"kind": "update"}, "text": text, "model": "ollama/llama3.2",
        "spend": spend(40, 0.0)}}),
    )
}

pub fn narrator_status(spent_usd: f64, cap: &str) -> AppEvent {
    app(json!({"type": "narrator_status", "status": {"mode": "live",
        "model": "ollama/llama3.2", "spent": spend(400, spent_usd), "cap": cap,
        "capped": false, "interval_s": 45}}))
}

pub fn taken_back(texts: &[&str]) -> AppEvent {
    app(json!({"type": "taken_back", "texts": texts}))
}

pub fn exit() -> AppEvent {
    app(json!({"type": "exit"}))
}

/// The `[ui]` settings in effect changed; `ui` is a `UiSettings` as JSON, only the fields that
/// are different from the defaults.
pub fn ui_settings(ui: Value) -> AppEvent {
    let mut settings = serde_json::to_value(argonauts_core::settings::ui::UiSettings::default())
        .expect("settings write as JSON");
    if let (Value::Object(settings), Value::Object(ui)) = (&mut settings, ui) {
        settings.extend(ui);
    }
    let vim = settings["keys"] == "vim";
    let verbosity = settings["verbosity"].clone();
    app(json!({"type": "ui_settings", "vim": vim, "verbosity": verbosity, "ui": settings}))
}

pub fn option(key: char, label: &str, action: Value, recommended: bool) -> Value {
    json!({"key": key.to_string(), "label": label, "action": action,
           "recommended": recommended, "detail": null})
}

pub fn choose(value: &str) -> Value {
    json!({"action": "choose", "value": value})
}

/// An app-level question. `kind` is a `ChoiceKind` in snake case.
pub fn choice(id: &str, kind: &str, title: &str, body: &[&str], options: Vec<Value>) -> AppEvent {
    app(
        json!({"type": "choice", "choice": {"id": id, "kind": kind, "title": title,
        "body": body, "options": options, "free_text": "none", "bell": true}}),
    )
}

pub fn choice_closed(id: &str, outcome: &str) -> AppEvent {
    app(json!({"type": "choice_closed", "id": id, "outcome": outcome}))
}

/// The unclear-request question: Captain `s`, Crew run `c`, No, just asking `n`.
pub fn start_work_choice(id: &str) -> AppEvent {
    choice(
        id,
        "start_work",
        "Start work on this?",
        &["Argonauts Code would pick Captain: one file, tests exist."],
        vec![
            option('s', "Captain", choose("captain"), true),
            option('c', "Crew run", choose("crew"), false),
            option('n', "No, just asking", choose("no"), false),
        ],
    )
}

/// A safety change asking to confirm: Change it `y`, Keep it as it is `n`.
pub fn safety_choice(id: &str, change: &str) -> AppEvent {
    choice(
        id,
        "confirm_safety_setting",
        "Change a safety setting?",
        &[change],
        vec![
            option('y', "Change it", json!({"action": "approve"}), false),
            option('n', "Keep it as it is", json!({"action": "cancel"}), false),
        ],
    )
}
