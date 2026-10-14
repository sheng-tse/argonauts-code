//! The fake ACP agent's script. When the fake agent's first argument is `acp`, it speaks the
//! Agent Client Protocol (version 1, newline-delimited JSON-RPC 2.0 on stdio) and plays the
//! first `[[acp]]` rule whose program and arguments match.
//!
//! ```toml
//! [[acp]]
//! argv0 = "hermes"
//! session_id = "s-1"
//!
//! [acp.reply."session/resume"]
//! error = { code = -32000, message = "Authentication required" }
//!
//! [[acp.turn]]
//! stop_reason = "end_turn"
//! result = { usage = { inputTokens = 12, outputTokens = 3 } }
//! [[acp.turn.step]]
//! update = { sessionUpdate = "agent_message_chunk", content = { type = "text", text = "hi" } }
//! [[acp.turn.step]]
//! update_file = "fixtures/hermes/updates.jsonl"
//! [[acp.turn.step]]
//! permission = { tool_call = { toolCallId = "t1", title = "Edit a.txt" }, expect = "allow-once" }
//! [[acp.turn.step]]
//! report = { status = "done" }
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Exit code when the client picked a permission option other than the one expected, so a
/// test sees it even if it never reads the log.
pub const WRONG_PICK_EXIT: i32 = 96;

pub const PROTOCOL_VERSION: u64 = 1;

pub const DEFAULT_SESSION_ID: &str = "fake-session";

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AcpRule {
    /// Such as `hermes`. Any program when unset.
    #[serde(default)]
    pub argv0: Option<String>,
    /// Each must appear as a whole argument.
    #[serde(default)]
    pub args_contain: Vec<String>,
    /// What `session/new` returns. Defaults to [`DEFAULT_SESSION_ID`].
    #[serde(default)]
    pub session_id: Option<String>,
    /// Keyed by method, such as `session/new`. Without an entry, `initialize`, `authenticate`,
    /// `session/new`, `session/load`, `session/resume`, and `session/set_mode` succeed, and any
    /// other method is not found.
    #[serde(default, rename = "reply")]
    pub replies: BTreeMap<String, AcpReply>,
    /// One per `session/prompt`, in order. Prompts after the last end their turn at once.
    #[serde(default, rename = "turn")]
    pub turns: Vec<AcpTurn>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AcpReply {
    /// `session/update`s sent before the reply, as `session/load` replays a session.
    #[serde(default)]
    pub updates: Vec<Value>,
    /// Replaces the default result. `session/new` adds the session id when it's missing.
    #[serde(default)]
    pub result: Option<Value>,
    /// Sent instead of a result.
    #[serde(default)]
    pub error: Option<AcpError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpError {
    /// Such as `-32000` for "authentication required".
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AcpTurn {
    /// Defaults to `end_turn`. A cancelled turn always ends `cancelled`.
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// More fields for the prompt's result, such as a `usage` draft.
    #[serde(default)]
    pub result: Option<Value>,
    /// Answers the prompt with an error once the steps are played.
    #[serde(default)]
    pub error: Option<AcpError>,
    #[serde(default, rename = "step")]
    pub steps: Vec<AcpStep>,
}

impl AcpTurn {
    pub fn stop_reason(&self) -> &str {
        self.stop_reason.as_deref().unwrap_or("end_turn")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpStep {
    /// Sends one `session/update` with this as its `update`.
    Update(Value),
    /// Sends each line of a file: an `update` object, or a whole frame when it has a `method`.
    /// Relative paths start at the scenario file's folder.
    UpdateFile(String),
    /// Stops early on `session/cancel`.
    SleepMs(u64),
    /// Sends `session/request_permission` and waits for the answer.
    Permission(AcpPermission),
    /// Calls `argonauts_report` with this report, on the first stdio MCP server the client
    /// named in `session/new`, `session/load`, or `session/resume`.
    Report(Value),
    /// Writes a file under the working directory. Relative only; `..` is refused.
    Write {
        path: String,
        content: String,
    },
    Stderr(String),
    /// Exits mid-turn without answering the prompt, as a crash would.
    Exit(i32),
    /// Sends nothing more until `session/cancel`.
    Hang(bool),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpPermission {
    /// The request's `toolCall`, with at least a `toolCallId`.
    pub tool_call: Value,
    /// Defaults to one option of each kind, with ids unlike their kinds: `allow-once`,
    /// `allow-always`, `reject-once`, and `reject-always`.
    #[serde(default)]
    pub options: Vec<Value>,
    /// The option id the client should pick. After a cancel the right answer is always
    /// `cancelled`. Any answer passes when this is unset.
    #[serde(default)]
    pub expect: Option<String>,
}

impl AcpPermission {
    pub fn options(&self) -> Vec<Value> {
        if !self.options.is_empty() {
            return self.options.clone();
        }
        [
            ("allow-once", "Allow once", "allow_once"),
            ("allow-always", "Always allow", "allow_always"),
            ("reject-once", "Reject", "reject_once"),
            ("reject-always", "Always reject", "reject_always"),
        ]
        .into_iter()
        .map(|(id, name, kind)| serde_json::json!({ "optionId": id, "name": name, "kind": kind }))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::FakeScenario;

    #[test]
    fn the_documented_shape_parses_and_round_trips() {
        let text = include_str!("acp.rs")
            .lines()
            .skip_while(|line| !line.starts_with("//! ```toml"))
            .skip(1)
            .take_while(|line| !line.starts_with("//! ```"))
            .map(|line| line.trim_start_matches("//!").trim_start_matches(' '))
            .collect::<Vec<_>>()
            .join("\n");
        let scenario: FakeScenario = toml::from_str(&text).unwrap();
        let rule = &scenario.acp[0];
        assert_eq!(rule.argv0.as_deref(), Some("hermes"));
        assert_eq!(
            rule.replies["session/resume"].error.as_ref().unwrap().code,
            -32000
        );
        let turn = &rule.turns[0];
        assert_eq!(turn.stop_reason(), "end_turn");
        assert_eq!(turn.steps.len(), 4);
        let AcpStep::Permission(permission) = &turn.steps[2] else {
            panic!("{:?}", turn.steps[2]);
        };
        assert_eq!(permission.expect.as_deref(), Some("allow-once"));
        assert_eq!(permission.options().len(), 4);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.toml");
        scenario.save(&path).unwrap();
        assert_eq!(FakeScenario::load(&path).unwrap(), scenario);
    }
}
