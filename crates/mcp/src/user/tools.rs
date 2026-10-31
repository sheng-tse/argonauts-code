//! The user server's tools: `argonauts_start_run`, `argonauts_list_runs`,
//! `argonauts_run_status`, `argonauts_run_events`, `argonauts_request_decision`,
//! `argonauts_send_guidance`, and `argonauts_pause_run`. None takes an approval mode, and none
//! grants anything.

use std::sync::Arc;
use std::time::Duration;

use argonauts_core::RunId;
use argonauts_core::runs::api::{EventStream, StartRun};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::{ApprovalEvent, ApprovalOpened};
use argonauts_core::runs::events::run::RunMode;
use argonauts_core::safety::sanitize::sanitize_for_terminal;
use rmcp::RoleServer;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool, ToolAnnotations};
use rmcp::service::RequestContext;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::time::Instant;

use super::{UserServer, caps, results};
use crate::OutsideAgentOptions;

pub const START_RUN: &str = "argonauts_start_run";
pub const LIST_RUNS: &str = "argonauts_list_runs";
pub const RUN_STATUS: &str = "argonauts_run_status";
pub const RUN_EVENTS: &str = "argonauts_run_events";
pub const REQUEST_DECISION: &str = "argonauts_request_decision";
pub const SEND_GUIDANCE: &str = "argonauts_send_guidance";
pub const PAUSE_RUN: &str = "argonauts_pause_run";

/// The longest `argonauts_run_events` waits for a new event.
pub const MAX_WAIT_MS: u64 = 30_000;
/// Events one `argonauts_run_events` call returns at most; `more` says there are others.
pub const MAX_EVENTS: usize = 200;
/// Characters in a run request and in guidance, at most.
pub const REQUEST_MAX: usize = 20_000;
pub const GUIDANCE_MAX: usize = 4_000;
/// Characters kept of a client's self-reported name.
pub const CLIENT_NAME_MAX: usize = 64;
pub const UNKNOWN_CLIENT: &str = "unknown";
/// Replaying a run's history reads local files, so it never takes this long unless something
/// is stuck.
const HISTORY_TIMEOUT: Duration = Duration::from_secs(10);

/// What the server tells a client it's for, in its `initialize` answer.
pub const INSTRUCTIONS: &str = "Start and watch Argonauts Code runs in this repo. Decisions are \
made in the Argonauts Code app; this server can say what's waiting but never answer it.";

/// Why every tool is refused once the repo's trust is revoked while the server runs.
pub const NOT_TRUSTED: &str = "this repo isn't trusted any more, so this server does nothing; run \
`argonauts setup` here and trust its settings again";

/// The tools `options` allow: guidance and pause only when `mcp.guidance` and `mcp.pause` say
/// so.
pub fn definitions(options: &OutsideAgentOptions) -> Vec<Tool> {
    let mut tools = vec![
        tool(
            START_RUN,
            "Starts a run in this repo from a request, Captain (one agent) or crew. The run waits \
             for your decisions in the Argonauts Code app. `settings` takes `key=value` \
             overrides; safety settings are refused.",
            json!({
                "type": "object",
                "properties": {
                    "request": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": REQUEST_MAX,
                        "description": "What the run should do."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["captain", "crew"],
                        "description": "Captain gives the request to one agent; crew plans it and splits it into tasks."
                    },
                    "settings": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "key=value overrides for this run, such as ui.theme=dark."
                    }
                },
                "required": ["request", "mode"],
                "additionalProperties": false
            }),
            false,
        ),
        tool(
            LIST_RUNS,
            "Lists this repo's runs, newest first, with their state, spend, and whether one \
             waits for a decision.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            true,
        ),
        tool(
            RUN_STATUS,
            "One run's state, spend, tasks, and whether it waits for a decision.",
            run_only_schema(),
            true,
        ),
        tool(
            RUN_EVENTS,
            "A run's events from `since` on. With `wait_ms` it waits up to that long for the \
             next one. Pass the returned `next_since` to the next call.",
            json!({
                "type": "object",
                "properties": {
                    "run": run_property(),
                    "since": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "The first event's seq; 0 reads from the start."
                    },
                    "wait_ms": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": MAX_WAIT_MS,
                        "description": "How long to wait for a new event when there is none yet."
                    }
                },
                "required": ["run"],
                "additionalProperties": false
            }),
            true,
        ),
        tool(
            REQUEST_DECISION,
            "Says what in a run waits for a decision and where you answer it. This \
             server can't answer it.",
            run_only_schema(),
            true,
        ),
    ];
    if options.mcp.guidance {
        tools.push(tool(
            SEND_GUIDANCE,
            "Queues guidance for every working agent's next turn in a run.",
            json!({
                "type": "object",
                "properties": {
                    "run": run_property(),
                    "text": { "type": "string", "minLength": 1, "maxLength": GUIDANCE_MAX }
                },
                "required": ["run", "text"],
                "additionalProperties": false
            }),
            false,
        ));
    }
    if options.mcp.pause {
        tools.push(tool(
            PAUSE_RUN,
            "Pauses a run at its next safe point, or with `now` stops its agents at once and \
             commits their work. It resumes from the Argonauts Code app.",
            json!({
                "type": "object",
                "properties": {
                    "run": run_property(),
                    "now": { "type": "boolean" }
                },
                "required": ["run"],
                "additionalProperties": false
            }),
            false,
        ));
    }
    tools
}

fn tool(name: &'static str, description: &'static str, schema: Value, read_only: bool) -> Tool {
    let schema = match schema {
        Value::Object(schema) => schema,
        _ => JsonObject::new(),
    };
    Tool::new(name, description, Arc::new(schema))
        .annotate(ToolAnnotations::new().read_only(read_only))
}

fn run_property() -> Value {
    json!({
        "type": "string",
        "description": "A run id from argonauts_list_runs."
    })
}

fn run_only_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "run": run_property() },
        "required": ["run"],
        "additionalProperties": false
    })
}

/// The name the client gave in `initialize`, or `unknown` for a client that never sent one.
pub fn client_name(context: &RequestContext<RoleServer>) -> String {
    context.client_info().map_or_else(
        || UNKNOWN_CLIENT.to_owned(),
        |info| clean_client_name(&info.name),
    )
}

/// A self-reported name as the journal records it: one line, no control sequences, cut short.
/// A name that reads as another party, such as "you" or "your rules", gets " (outside agent)"
/// added, so a client can't pass off its guidance as yours.
pub fn clean_client_name(name: &str) -> String {
    let short = one_short_line(name);
    if short.is_empty() {
        return UNKNOWN_CLIENT.to_owned();
    }
    if !names_another_party(&short) {
        return short;
    }
    const MARK: &str = " (outside agent)";
    let kept: String = short
        .chars()
        .take(CLIENT_NAME_MAX - MARK.chars().count())
        .collect();
    format!("{}{MARK}", kept.trim_end())
}

/// Labels the run log and the narrator give parties other than an outside agent.
const OTHER_PARTIES: &[&str] = &[
    "you",
    "user",
    "the user",
    "me",
    "your rules",
    "rules",
    "a setting",
    "setting",
    "config",
    "Argonauts Code",
    "argonauts-code",
    "argonauts",
    "the narrator",
    "narrator",
];

fn names_another_party(name: &str) -> bool {
    let bare = name
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    let bare = bare.split_whitespace().collect::<Vec<_>>().join(" ");
    OTHER_PARTIES.contains(&bare.as_str())
        || ["the judge", "judge", "the outside agent", "outside agent"]
            .iter()
            .any(|label| bare.starts_with(label))
}

fn one_short_line(text: &str) -> String {
    let sanitized = sanitize_for_terminal(text);
    let one_line = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(CLIENT_NAME_MAX).collect()
}

/// Runs one tool under the options as they are now. Every failure is a tool error the client
/// can read, never a crash.
pub async fn call(
    server: &UserServer,
    name: &str,
    arguments: JsonObject,
    client: &str,
) -> CallToolResult {
    let options = &server.current_options().await;
    if !options.repo_trusted {
        return CallToolResult::error(vec![ContentBlock::text(NOT_TRUSTED)]);
    }
    let outcome = match name {
        START_RUN => start_run(server, options, arguments, client).await,
        LIST_RUNS => list_runs(server, options, arguments).await,
        RUN_STATUS => run_status(server, options, arguments).await,
        RUN_EVENTS => run_events(server, options, arguments).await,
        REQUEST_DECISION => request_decision(server, options, arguments).await,
        SEND_GUIDANCE => send_guidance(server, options, arguments, client).await,
        PAUSE_RUN => pause_run(server, options, arguments, client).await,
        _ => Err(format!("there is no tool named {}", quoted(name))),
    };
    outcome.unwrap_or_else(|message| CallToolResult::error(vec![ContentBlock::text(message)]))
}

type Outcome = Result<CallToolResult, String>;

/// Client text in an error, one short line.
fn quoted(text: &str) -> String {
    format!("{:?}", one_short_line(text))
}

/// Parses a tool's arguments, refusing any it doesn't take. An argument that names an approval
/// mode or autonomy gets its own answer, since no tool here can change either.
fn parse<T: DeserializeOwned>(tool: &str, arguments: JsonObject) -> Result<T, String> {
    if let Some(key) = arguments.keys().find(|key| names_autonomy(key)) {
        return Err(format!(
            "{tool} takes no {} argument. Approval modes and autonomy change only in the \
             Argonauts Code app.",
            quoted(key)
        ));
    }
    serde_json::from_value(Value::Object(arguments)).map_err(|err| {
        let reason = sanitize_for_terminal(&err.to_string());
        let reason: String = reason.chars().take(300).collect();
        format!("{tool}: {reason}")
    })
}

fn names_autonomy(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "approv",
        "autonomy",
        "auto_mode",
        "automode",
        "take_over",
        "override",
    ]
    .iter()
    .any(|word| key.contains(word))
}

fn parse_run(run: &str) -> Result<RunId, String> {
    RunId::new(run.trim()).map_err(|_| format!("{} isn't a run id", quoted(run)))
}

/// Request and guidance text: control sequences dropped, one size limit.
fn clean_text(text: &str, what: &str, max: usize) -> Result<String, String> {
    let text = sanitize_for_terminal(text).trim().to_owned();
    if text.is_empty() {
        return Err(format!("the {what} is empty"));
    }
    let length = text.chars().count();
    if length > max {
        return Err(format!(
            "the {what} has {length} characters; the limit is {max}"
        ));
    }
    Ok(text)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartArgs {
    request: String,
    mode: RunMode,
    #[serde(default)]
    settings: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    run: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventArgs {
    run: String,
    #[serde(default)]
    since: u64,
    #[serde(default)]
    wait_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuidanceArgs {
    run: String,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PauseArgs {
    run: String,
    #[serde(default)]
    now: bool,
}

/// The server's checks come before the daily count, and a start Argonauts Code refuses gives
/// its run back, so a refused start doesn't use up a run.
async fn start_run(
    server: &UserServer,
    options: &OutsideAgentOptions,
    arguments: JsonObject,
    client: &str,
) -> Outcome {
    let args: StartArgs = parse(START_RUN, arguments)?;
    caps::check_can_start(options)?;
    let request = clean_text(&args.request, "request", REQUEST_MAX)?;
    caps::check_overrides(&args.settings)?;
    let taken = server
        .take_outside_agent_run(options.mcp.runs_per_day)
        .await?;
    let today = taken.today;
    let start = StartRun {
        request,
        mode: args.mode,
        forced: true,
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        overrides: args.settings,
        include_uncommitted: false,
        main_agent: None,
        approval_mode: None,
        outside_agent: Some(client.to_owned()),
        instructions: Vec::new(),
        request_settings: Vec::new(),
        required_steps: Vec::new(),
        starts_from: None,
    };
    let handle = match server.api().start(start).await {
        Ok(handle) => handle,
        Err(err) => {
            let refused = results::api_error(&err);
            return Err(match server.give_back_outside_agent_run(taken).await {
                Ok(()) => refused,
                Err(give_back) => format!("{refused} (its run wasn't given back: {give_back})"),
            });
        }
    };
    Ok(CallToolResult::structured(json!({
        "run": handle.run.as_str(),
        "started": true,
        "time_limit": options.mcp.run_limit.0,
        "outside_agent_runs_today": today,
        "outside_agent_runs_per_day": options.mcp.runs_per_day,
        "next": format!("Watch it with {RUN_EVENTS}. Its decisions wait in the Argonauts Code app."),
    })))
}

async fn list_runs(
    server: &UserServer,
    options: &OutsideAgentOptions,
    arguments: JsonObject,
) -> Outcome {
    let NoArgs {} = parse(LIST_RUNS, arguments)?;
    let runs = server
        .api()
        .list()
        .await
        .map_err(|err| results::api_error(&err))?;
    Ok(CallToolResult::structured(results::run_list(
        &runs,
        options.mcp.share,
    )))
}

async fn run_status(
    server: &UserServer,
    options: &OutsideAgentOptions,
    arguments: JsonObject,
) -> Outcome {
    let args: RunArgs = parse(RUN_STATUS, arguments)?;
    let run = parse_run(&args.run)?;
    let api = server.api();
    let status = api
        .status(&run)
        .await
        .map_err(|err| results::api_error(&err))?;
    // The list gives state and spend; without it the status still answers.
    let summary = api
        .list()
        .await
        .ok()
        .and_then(|runs| runs.into_iter().find(|summary| summary.run == run));
    Ok(CallToolResult::structured(results::run_status(
        &run,
        &status,
        summary.as_ref(),
        options.mcp.share,
    )))
}

/// History comes first without waiting. Only when there is none past `since` does it wait,
/// up to `wait_ms`, for the next live event, and then takes whatever else is ready.
async fn run_events(
    server: &UserServer,
    options: &OutsideAgentOptions,
    arguments: JsonObject,
) -> Outcome {
    let args: EventArgs = parse(RUN_EVENTS, arguments)?;
    let run = parse_run(&args.run)?;
    let wait = Duration::from_millis(args.wait_ms.min(MAX_WAIT_MS));
    let mut stream = server
        .api()
        .subscribe(&run, args.since)
        .await
        .map_err(|err| results::api_error(&err))?;
    let share = options.mcp.share;
    let deadline = Instant::now() + wait;
    let mut events = Vec::new();
    let mut next_since = args.since;
    while events.len() < MAX_EVENTS {
        let in_history = stream.replay_until.is_some_and(|last| next_since <= last);
        let received = if in_history {
            tokio::time::timeout(HISTORY_TIMEOUT, stream.events.recv())
                .await
                .ok()
                .flatten()
        } else if events.is_empty() && !wait.is_zero() {
            tokio::time::timeout_at(deadline, stream.events.recv())
                .await
                .ok()
                .flatten()
        } else {
            stream.events.try_recv().ok()
        };
        let Some(event) = received else {
            break;
        };
        if event.seq < next_since {
            continue;
        }
        next_since = event.seq.saturating_add(1);
        events.push(results::event(&event, share));
    }
    let more = events.len() >= MAX_EVENTS;
    let live = *stream.live.borrow();
    Ok(CallToolResult::structured(json!({
        "run": run.as_str(),
        "events": events,
        "next_since": next_since,
        "more": more,
        "live": live,
    })))
}

async fn request_decision(
    server: &UserServer,
    options: &OutsideAgentOptions,
    arguments: JsonObject,
) -> Outcome {
    let args: RunArgs = parse(REQUEST_DECISION, arguments)?;
    let run = parse_run(&args.run)?;
    let stream = server
        .api()
        .subscribe(&run, 0)
        .await
        .map_err(|err| results::api_error(&err))?;
    let waiting = open_approvals(stream).await?;
    let (text, fields) = results::decision(&run, &waiting, options.mcp.share);
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.structured_content = Some(fields);
    Ok(result)
}

/// The approvals the run's log leaves open: opened and not yet decided or withdrawn.
async fn open_approvals(mut stream: EventStream) -> Result<Vec<ApprovalOpened>, String> {
    let Some(last) = stream.replay_until else {
        return Ok(Vec::new());
    };
    let deadline = Instant::now() + HISTORY_TIMEOUT;
    let mut open: Vec<ApprovalOpened> = Vec::new();
    loop {
        let event = match tokio::time::timeout_at(deadline, stream.events.recv()).await {
            Ok(Some(event)) => event,
            Ok(None) => break,
            Err(_) => return Err("reading the run's log took too long; try again".to_owned()),
        };
        if let RunEventKind::Approval(approval) = event.kind {
            match approval {
                ApprovalEvent::Opened(opened) => {
                    open.retain(|known| known.id != opened.id);
                    open.push(*opened);
                }
                ApprovalEvent::Decided(decided) => open.retain(|known| known.id != decided.id),
                ApprovalEvent::Withdrawn { id, .. } => open.retain(|known| known.id != id),
                ApprovalEvent::Checked(_) | ApprovalEvent::SpotCheckQueued { .. } => {}
            }
        }
        if event.seq >= last {
            break;
        }
    }
    Ok(open)
}

async fn send_guidance(
    server: &UserServer,
    options: &OutsideAgentOptions,
    arguments: JsonObject,
    client: &str,
) -> Outcome {
    let args: GuidanceArgs = parse(SEND_GUIDANCE, arguments)?;
    if !options.mcp.guidance {
        return Err("guidance from MCP is turned off here (mcp.guidance = false)".to_owned());
    }
    let run = parse_run(&args.run)?;
    let text = clean_text(&args.text, "guidance", GUIDANCE_MAX)?;
    let message = server
        .api()
        .guidance_from_outside_agent(&run, text, client.to_owned())
        .await
        .map_err(|err| results::api_error(&err))?;
    Ok(CallToolResult::structured(json!({
        "run": run.as_str(),
        "message": message.as_str(),
        "queued": true,
    })))
}

async fn pause_run(
    server: &UserServer,
    options: &OutsideAgentOptions,
    arguments: JsonObject,
    client: &str,
) -> Outcome {
    let args: PauseArgs = parse(PAUSE_RUN, arguments)?;
    if !options.mcp.pause {
        return Err("pausing from MCP is turned off here (mcp.pause = false)".to_owned());
    }
    let run = parse_run(&args.run)?;
    server
        .api()
        .pause_from_outside_agent(&run, args.now, client.to_owned())
        .await
        .map_err(|err| results::api_error(&err))?;
    Ok(CallToolResult::structured(json!({
        "run": run.as_str(),
        "pausing": true,
        "now": args.now,
        "next": "It resumes from the Argonauts Code app.",
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_named_like_another_party_is_marked_as_a_outside_agent() {
        for name in ["you", "YOU", " your rules ", "\"You.\"", "the judge claude"] {
            let cleaned = clean_client_name(name);
            assert!(
                cleaned.ends_with(" (outside agent)"),
                "{name:?} became {cleaned:?}"
            );
        }
        assert_eq!(clean_client_name("OpenClaw"), "OpenClaw");
        assert_eq!(clean_client_name("Your Rules Bot"), "Your Rules Bot");
        assert_eq!(clean_client_name("\n "), UNKNOWN_CLIENT);
        let long = format!("the outside agent {}", "x".repeat(100));
        assert!(clean_client_name(&long).chars().count() <= CLIENT_NAME_MAX);
    }
}
