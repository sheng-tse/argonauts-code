//! Control requests written to Claude Code's stream-json input, and what their replies say.
//! Each request's id names what it asked, so a reply reads on its own: `initialize`, send now
//! and take back by the message's uuid, a screen's attach, render, and press, and the context
//! by category.

use std::sync::atomic::{AtomicU64, Ordering};

use argonauts_core::events::context::ContextDetail;
use argonauts_core::screen::{ScreenChange, ScreenRequest};
use argonauts_core::{AgentEvent, MessageStage, ReportedCommand};
use serde_json::{Value, json};

use crate::json::{array_at, bool_at, str_at, string_at, strings_at};
use crate::{context, models, surface};

/// How Argonauts joins a session as a screen. Only `desktop`, `mobile`, and `vscode` are
/// accepted, and a mod that draws for a terminal draws the desktop look closest.
const SURFACE: &str = "desktop";
const CLIENT_ID: &str = "argonauts";
/// The component above the input, where You should know draws.
const ABOVE_PROMPT: &str = "AbovePrompt";

const INITIALIZE: &str = "initialize";
const SEND_NOW: &str = "send-now-";
const CANCEL: &str = "cancel-";
const ATTACH: &str = "attach-";
const RENDER: &str = "render-";
const PRESS: &str = "press-";
const CONTEXT: &str = "ctx-";
const STOP_TASK: &str = "stop-task-";
const STOP_TURN: &str = "stop-turn-";

/// Keeps screen request ids apart within a process.
static NEXT: AtomicU64 = AtomicU64::new(1);

fn request(id: &str, body: Value) -> String {
    let line = json!({"type": "control_request", "request_id": id, "request": body});
    format!("{line}\n")
}

/// The first line of a main-session turn. Its reply lists the CLI's commands and models.
///
/// It also declares that this screen can stop one background task (`perTaskStopAffordance`).
/// Without that, Claude Code 2.1.292 kills every background agent and workflow when the turn
/// is interrupted, since nothing else could stop a runaway one. With it, an interrupt stops
/// the turn alone and they keep running, as Esc does on Claude Code's own screen.
pub fn initialize_line() -> String {
    request(
        INITIALIZE,
        json!({"subtype": "initialize", "perTaskStopAffordance": true}),
    )
}

/// Stops one background task, a subagent or a workflow, by the id `task_started` gave it. It
/// keeps its progress, and Claude Code tells the model it was stopped.
pub fn stop_task_line(task: &str) -> String {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    request(
        &format!("{STOP_TASK}{n}"),
        json!({"subtype": "stop_task", "task_id": task}),
    )
}

/// Stops the running turn, as Esc does on Claude Code's own screen. In a session that opened
/// with [`initialize_line`] the background agents and workflows go on. In any other they are
/// killed with the turn, so only such a session is sent this ([`opened_by`]).
pub fn stop_turn_line() -> String {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    request(&format!("{STOP_TURN}{n}"), json!({"subtype": "interrupt"}))
}

/// Whether `line` is Claude Code taking [`initialize_line`], and with it this screen's word
/// that it stops background tasks one by one.
pub fn opened_by(line: &Value) -> bool {
    line.get("response").is_some_and(|reply| {
        str_at(reply, "request_id") == Some(INITIALIZE)
            && str_at(reply, "subtype") == Some("success")
    })
}

/// Hands what waits to the model now, moving a running tool to the background.
pub fn send_now_line(uuid: &str) -> String {
    request(
        &format!("{SEND_NOW}{uuid}"),
        json!({"subtype": "interrupt", "send_now": true, "message_uuid": uuid}),
    )
}

pub fn cancel_line(uuid: &str) -> String {
    request(
        &format!("{CANCEL}{uuid}"),
        json!({"subtype": "cancel_async_message", "message_uuid": uuid}),
    )
}

pub fn screen_line(screen: &ScreenRequest) -> String {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    match screen {
        ScreenRequest::Attach => request(
            &format!("{ATTACH}{n}"),
            json!({"subtype": "ui_attach", "surface": SURFACE, "client_id": CLIENT_ID}),
        ),
        ScreenRequest::Render => request(
            &format!("{RENDER}{n}"),
            json!({"subtype": "ui_render", "surface": SURFACE, "client_id": CLIENT_ID,
                   "component": ABOVE_PROMPT, "instance_id": "above-prompt", "props": {}}),
        ),
        ScreenRequest::Press { plugin, handle } => request(
            &format!("{PRESS}{n}"),
            json!({"subtype": "ui_press", "surface": SURFACE, "plugin": plugin,
                   "handle": handle}),
        ),
    }
}

/// Asks for the context by category, the numbers Claude Code's /context draws. `summary`
/// answers from the last response's usage and local estimates, with no token-count calls.
pub fn context_usage_line(detail: ContextDetail) -> String {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    request(
        &format!("{CONTEXT}{n}"),
        json!({"subtype": "get_context_usage", "detail": detail.as_str()}),
    )
}

/// What a `control_response` line says, by the request it answers.
pub fn from_response(line: &Value) -> Vec<AgentEvent> {
    let Some(reply) = line.get("response") else {
        return Vec::new();
    };
    let id = str_at(reply, "request_id").unwrap_or_default();
    let ok = str_at(reply, "subtype") == Some("success");
    let body = reply.get("response").unwrap_or(&Value::Null);
    if let Some(uuid) = id.strip_prefix(SEND_NOW) {
        let state = match str_at(body, "send_now") {
            Some("delivering") => MessageStage::SendingNow,
            _ => MessageStage::NothingWaiting,
        };
        return vec![message(uuid, state)];
    }
    if id.starts_with(CONTEXT) {
        // An error, such as a session that can't answer it, gives nothing, and nothing asks
        // again until the next fill.
        if !ok {
            return Vec::new();
        }
        return context::breakdown(body).into_iter().collect();
    }
    if let Some(uuid) = id.strip_prefix(CANCEL) {
        let state = if ok && bool_at(body, "cancelled") {
            MessageStage::TakenBack
        } else {
            MessageStage::AlreadyRead
        };
        return vec![message(uuid, state)];
    }
    let change = if id.starts_with(ATTACH) {
        let surfaces = strings_at(body, "surfaces");
        ScreenChange::Attached {
            ok: ok && surfaces.iter().any(|surface| surface == SURFACE),
        }
    } else if id.starts_with(RENDER) {
        match (ok, body.get("tree")) {
            (true, Some(tree)) => match surface::read(tree) {
                Ok(drawn) => ScreenChange::Drawn { drawn },
                Err(why) => ScreenChange::Unreadable { why },
            },
            _ => ScreenChange::Unreadable {
                why: string_at(reply, "error").unwrap_or_else(|| "no tree came back".to_owned()),
            },
        }
    } else if id.starts_with(PRESS) || id.starts_with(STOP_TASK) || id.starts_with(STOP_TURN) {
        // A press that didn't land was on a button a redraw took away. A stop that didn't was
        // of a task that had ended, and the task's own events say when one did land. A turn
        // that stopped says so in its result.
        return Vec::new();
    } else {
        // `initialize`, whatever its id.
        return initialized(line, body, ok);
    };
    vec![AgentEvent::ScreenChanged { change }]
}

fn message(uuid: &str, state: MessageStage) -> AgentEvent {
    AgentEvent::MessageState {
        uuid: uuid.to_owned(),
        state,
    }
}

/// `initialize`'s reply: the commands, the models, then the account's plan. Nothing else
/// about the account is kept.
fn initialized(line: &Value, body: &Value, ok: bool) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    if ok && body.get("commands").is_some() {
        events.push(AgentEvent::CommandsChanged {
            commands: commands(body),
        });
    }
    events.extend(models::from_control_response(line));
    if ok
        && let Some(plan) = body
            .get("account")
            .and_then(|account| str_at(account, "subscriptionType"))
            .map(str::trim)
            .filter(|plan| !plan.is_empty())
    {
        events.push(AgentEvent::Plan {
            plan: plan.to_owned(),
        });
    }
    events
}

/// A command list, as the `initialize` reply and `commands_changed` carry it.
pub fn commands(holder: &Value) -> Vec<ReportedCommand> {
    array_at(holder, "commands")
        .iter()
        .filter_map(|entry| {
            Some(ReportedCommand {
                name: string_at(entry, "name")?,
                description: string_at(entry, "description").unwrap_or_default(),
                argument_hint: string_at(entry, "argumentHint").unwrap_or_default(),
                aliases: strings_at(entry, "aliases"),
                builtin: bool_at(entry, "builtin"),
            })
        })
        .collect()
}

/// `command_lifecycle`: a message's way through Claude Code's queue.
pub fn lifecycle(line: &Value) -> Option<AgentEvent> {
    let uuid = str_at(line, "command_uuid")?;
    let state = match str_at(line, "state")? {
        "queued" => MessageStage::Queued,
        "started" => MessageStage::Started,
        "completed" => MessageStage::Completed,
        _ => return None,
    };
    Some(message(uuid, state))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(line: &str) -> Value {
        serde_json::from_str(line.trim()).unwrap()
    }

    #[test]
    fn requests_name_what_they_ask() {
        let now = parsed(&send_now_line("u-1"));
        assert_eq!(now["request_id"], "send-now-u-1");
        assert_eq!(now["request"]["subtype"], "interrupt");
        assert_eq!(now["request"]["send_now"], true);
        assert_eq!(now["request"]["message_uuid"], "u-1");
        let cancel = parsed(&cancel_line("u-2"));
        assert_eq!(cancel["request"]["subtype"], "cancel_async_message");
        assert_eq!(cancel["request"]["message_uuid"], "u-2");
        let attach = parsed(&screen_line(&ScreenRequest::Attach));
        assert_eq!(attach["request"]["surface"], "desktop");
        let press = parsed(&screen_line(&ScreenRequest::Press {
            plugin: "cc-plugin-you-should-know".into(),
            handle: 725_020_656,
        }));
        assert_eq!(press["request"]["subtype"], "ui_press");
        assert_eq!(press["request"]["handle"], 725_020_656);
        assert_eq!(
            parsed(&initialize_line())["request"]["subtype"],
            "initialize"
        );
    }

    #[test]
    fn replies_read_by_the_request_they_answer() {
        let delivering = json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "send-now-u-1",
            "response": {"still_queued": ["u-1"], "send_now": "delivering"}}});
        assert_eq!(
            from_response(&delivering),
            [message("u-1", MessageStage::SendingNow)]
        );
        let late = json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "send-now-u-1", "response": {"send_now": "nothing_waiting"}}});
        assert_eq!(
            from_response(&late),
            [message("u-1", MessageStage::NothingWaiting)]
        );
        let read = json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "cancel-u-2", "response": {"cancelled": false}}});
        assert_eq!(
            from_response(&read),
            [message("u-2", MessageStage::AlreadyRead)]
        );
        let taken = json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "cancel-u-2", "response": {"cancelled": true}}});
        assert_eq!(
            from_response(&taken),
            [message("u-2", MessageStage::TakenBack)]
        );
        let attached = json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "attach-3", "response": {"surfaces": ["desktop"]}}});
        assert_eq!(
            from_response(&attached),
            [AgentEvent::ScreenChanged {
                change: ScreenChange::Attached { ok: true }
            }]
        );
        let refused = json!({"type": "control_response", "response": {"subtype": "error",
            "request_id": "render-4", "error": "unknown subtype"}});
        assert!(matches!(
            &from_response(&refused)[..],
            [AgentEvent::ScreenChanged {
                change: ScreenChange::Unreadable { .. }
            }]
        ));
    }

    #[test]
    fn the_context_request_and_its_reply() {
        let asked = parsed(&context_usage_line(ContextDetail::Summary));
        assert_eq!(asked["request"]["subtype"], "get_context_usage");
        assert_eq!(asked["request"]["detail"], "summary");
        let id = asked["request_id"].as_str().unwrap().to_owned();
        assert!(id.starts_with(CONTEXT));
        let reply = json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": id, "response": {"maxTokens": 200000, "categories": [
                {"name": "Messages", "tokens": 900, "kind": "used"}]}}});
        assert!(matches!(
            from_response(&reply).as_slice(),
            [AgentEvent::ContextBreakdown { window: 200000, .. }]
        ));
        // An error gives no breakdown, and nothing else that would ask again.
        let refused = json!({"type": "control_response", "response": {"subtype": "error",
            "request_id": id, "error": "get_context_usage is not supported in this context"}});
        assert_eq!(from_response(&refused), []);
    }

    #[test]
    fn a_stop_names_the_task_and_its_reply_adds_nothing() {
        let line: Value = serde_json::from_str(stop_task_line("a1b2c3").trim_end()).unwrap();
        assert_eq!(line["type"], "control_request");
        assert_eq!(
            line["request"],
            json!({"subtype": "stop_task", "task_id": "a1b2c3"})
        );
        let id = line["request_id"].as_str().unwrap();
        assert!(id.starts_with("stop-task-"), "{id}");
        for subtype in ["success", "error"] {
            let reply = json!({"type": "control_response",
                "response": {"subtype": subtype, "request_id": id, "response": {}}});
            assert!(from_response(&reply).is_empty(), "{subtype}");
        }
        // The session is told at its start that this screen stops tasks one by one, so an
        // interrupt leaves the background agents running.
        let first: Value = serde_json::from_str(initialize_line().trim_end()).unwrap();
        assert_eq!(first["request"]["perTaskStopAffordance"], true);
    }

    #[test]
    fn a_turn_stops_alone_only_in_a_session_that_took_initialize() {
        let line: Value = serde_json::from_str(stop_turn_line().trim_end()).unwrap();
        assert_eq!(line["type"], "control_request");
        assert_eq!(line["request"], json!({"subtype": "interrupt"}));
        let id = line["request_id"].as_str().unwrap();
        assert!(id.starts_with("stop-turn-"), "{id}");
        // Its reply says nothing: the stopped turn's own result does.
        let reply = json!({"type": "control_response",
            "response": {"subtype": "success", "request_id": id, "response": {}}});
        assert!(from_response(&reply).is_empty());
        assert!(!opened_by(&reply));

        let taken = |subtype: &str| {
            json!({"type": "control_response", "response": {"subtype": subtype,
                "request_id": "initialize", "response": {"commands": []}}})
        };
        assert!(opened_by(&taken("success")));
        assert!(!opened_by(&taken("error")));
    }

    #[test]
    fn the_initialize_reply_names_the_plan_and_keeps_nothing_else_of_the_account() {
        let reply = json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "initialize", "response": {"account": {
                "email": "someone@example.com", "subscriptionType": "Claude Max"}}}});
        assert_eq!(
            from_response(&reply),
            [AgentEvent::Plan {
                plan: "Claude Max".into()
            }]
        );
        let api_key = json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "initialize", "response": {"account": {"apiProvider": "firstParty"}}}});
        assert_eq!(from_response(&api_key), []);
    }

    #[test]
    fn the_initialize_reply_lists_a_mods_command() {
        let reply = json!({"type": "control_response", "response": {"subtype": "success",
        "request_id": "initialize", "response": {"commands": [
            {"name": "compact", "description": "Compact", "argumentHint": "", "builtin": true},
            {"name": "probe-cmd", "description": "Argonauts probe", "argumentHint": ""},
            {"name": "example-c", "description": "An example skill (claude.ai sync)",
             "argumentHint": "", "aliases": ["anthropic-skills:example-c"]}
        ]}}});
        let events = from_response(&reply);
        let AgentEvent::CommandsChanged { commands } = &events[0] else {
            panic!("{events:?}");
        };
        assert_eq!(commands.len(), 3);
        assert!(commands[0].builtin && !commands[1].builtin);
        assert_eq!(commands[1].plugin(), None);
        assert_eq!(commands[2].plugin(), Some("anthropic-skills"));
    }

    #[test]
    fn lifecycle_lines_follow_a_message() {
        let started = json!({"type": "command_lifecycle", "command_uuid": "u-1",
                             "state": "started"});
        assert_eq!(
            lifecycle(&started),
            Some(message("u-1", MessageStage::Started))
        );
        assert_eq!(lifecycle(&json!({"type": "command_lifecycle"})), None);
    }
}
