//! The vendor-neutral events every adapter's parser produces.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::events::session::{SubagentUpdate, VendorWorkflow};
use crate::limits::QuotaReading;

/// `parent` is the id of the tool call that spawned a subagent, when the event came from one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Init {
        /// Used to resume.
        session_id: Option<String>,
        model: Option<String>,
        tools: Vec<String>,
    },
    TextDelta {
        text: String,
        parent: Option<String>,
    },
    Message {
        text: String,
        parent: Option<String>,
    },
    Reasoning {
        text: String,
    },
    ToolStarted {
        /// Matched by `ToolFinished`.
        id: String,
        /// As the vendor reports it, such as `Bash` or `command_execution`.
        name: String,
        input: Value,
        parent: Option<String>,
    },
    ToolFinished {
        id: String,
        ok: bool,
        /// Possibly truncated by the vendor.
        output: Option<String>,
        parent: Option<String>,
    },
    FileChanged {
        /// Relative to the workdir where the vendor gives one.
        path: String,
        /// Such as `add`, `update`, or `delete`.
        kind: String,
    },
    /// The agent's current to-do list, replacing any earlier one.
    Todo {
        items: Vec<TodoItem>,
    },
    /// Cumulative numbers replace earlier totals, others add to them.
    Usage {
        /// All input tokens, cached ones included.
        input_tokens: u64,
        /// Reasoning included.
        output_tokens: u64,
        /// The part of `input_tokens` served from cache.
        cached_tokens: u64,
        cost_usd: Option<f64>,
        cumulative: bool,
    },
    /// How full the session's context is, in tokens, so rotation can act before it's full.
    ContextFill {
        used: u64,
        /// The window's size. `None` when the stream doesn't say, and the worker then fills it
        /// in from the registry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },
    Retry {
        reason: RetryReason,
        delay_ms: Option<u64>,
    },
    /// Quota or rate limit information, kept as the vendor sent it.
    QuotaSignal {
        detail: Value,
    },
    /// A vendor subagent, workflow, or thread changed state: Claude Code's
    /// `parent_tool_use_id`, Codex's `collab_tool_call`, agy's `subagent_info`.
    Subagent {
        update: SubagentUpdate,
    },
    /// A workflow the agent's own CLI runs, with its phases and agents, replacing any earlier
    /// state of the same id.
    Workflow {
        workflow: Box<VendorWorkflow>,
    },
    /// A normalized quota reading. `QuotaSignal` still carries raw payloads.
    Quota {
        reading: QuotaReading,
    },
    Error {
        category: ErrorCategory,
        message: String,
        retryable: bool,
    },
    Result {
        text: Option<String>,
        /// Set when a schema was enforced.
        structured: Option<Value>,
        is_error: bool,
    },
    /// A line the parser did not recognize, kept so nothing is dropped.
    Unknown {
        /// JSON if the line parsed, else a string.
        raw: Value,
    },
}

impl AgentEvent {
    /// The serialized `type` tag.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Init { .. } => "init",
            Self::TextDelta { .. } => "text_delta",
            Self::Message { .. } => "message",
            Self::Reasoning { .. } => "reasoning",
            Self::ToolStarted { .. } => "tool_started",
            Self::ToolFinished { .. } => "tool_finished",
            Self::FileChanged { .. } => "file_changed",
            Self::Todo { .. } => "todo",
            Self::Usage { .. } => "usage",
            Self::ContextFill { .. } => "context_fill",
            Self::Retry { .. } => "retry",
            Self::QuotaSignal { .. } => "quota_signal",
            Self::Subagent { .. } => "subagent",
            Self::Workflow { .. } => "workflow",
            Self::Quota { .. } => "quota",
            Self::Error { .. } => "error",
            Self::Result { .. } => "result",
            Self::Unknown { .. } => "unknown",
        }
    }

    pub fn unknown_line(line: &str) -> Self {
        let raw = serde_json::from_str(line).unwrap_or_else(|_| Value::String(line.to_owned()));
        Self::Unknown { raw }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub text: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryReason {
    RateLimit,
    Overloaded,
    Network,
    ServerError,
    /// The vendor's description.
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    /// Not logged in, or the token or key is invalid.
    Auth,
    /// The plan's usage limit is reached.
    UsageLimit,
    /// A short rate limit.
    RateLimit,
    /// Credits or a spend cap ran out.
    Billing,
    Overloaded,
    Server,
    /// A network or stream failure.
    Network,
    ContextFull,
    ModelUnavailable,
    InvalidRequest,
    /// A tool or action was denied.
    Permission,
    /// A native budget cap was hit.
    Budget,
    Timeout,
    Interrupted,
    /// The CLI failed in a way the adapter did not expect.
    Cli,
    /// Not a failure: the CLI went quiet past its own heartbeat while a tool ran. Shown dim,
    /// never acted on.
    Stalled,
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::session::{SubagentKind, SubagentStatus};
    use crate::limits::{QuotaSource, QuotaStatus};
    use crate::names::VendorId;
    use serde_json::json;
    use time::macros::datetime;

    fn all_events() -> Vec<AgentEvent> {
        vec![
            AgentEvent::Init {
                session_id: Some("s1".into()),
                model: Some("haiku".into()),
                tools: vec!["Read".into()],
            },
            AgentEvent::TextDelta {
                text: "hi".into(),
                parent: None,
            },
            AgentEvent::Message {
                text: "hello".into(),
                parent: Some("t1".into()),
            },
            AgentEvent::Reasoning { text: "hmm".into() },
            AgentEvent::ToolStarted {
                id: "t1".into(),
                name: "Bash".into(),
                input: json!({"command": "ls"}),
                parent: None,
            },
            AgentEvent::ToolFinished {
                id: "t1".into(),
                ok: true,
                output: Some("a\nb".into()),
                parent: None,
            },
            AgentEvent::FileChanged {
                path: "hello.txt".into(),
                kind: "add".into(),
            },
            AgentEvent::Todo {
                items: vec![TodoItem {
                    text: "write tests".into(),
                    status: TodoStatus::InProgress,
                }],
            },
            AgentEvent::Usage {
                input_tokens: 10,
                output_tokens: 5,
                cached_tokens: 2,
                cost_usd: Some(0.01),
                cumulative: false,
            },
            AgentEvent::Retry {
                reason: RetryReason::Other("flaky".into()),
                delay_ms: Some(500),
            },
            AgentEvent::QuotaSignal {
                detail: json!({"remaining": 3}),
            },
            AgentEvent::Subagent {
                update: SubagentUpdate {
                    id: "thread-2".into(),
                    parent: None,
                    kind: SubagentKind::Thread,
                    label: Some("explorer".into()),
                    prompt: Some("look around".into()),
                    status: SubagentStatus::Running,
                    last_message: None,
                    log_uri: None,
                    steps_visible: false,
                },
            },
            AgentEvent::Quota {
                reading: QuotaReading {
                    vendor: VendorId("claude-code".into()),
                    status: QuotaStatus::Warning,
                    used: Some(0.71),
                    resets_at: Some(datetime!(2026-09-29 18:00 UTC)),
                    window: Some("five_hour".into()),
                    source: QuotaSource::RateLimitEvent,
                    observed_at: datetime!(2026-09-29 12:00 UTC),
                    detail: None,
                },
            },
            AgentEvent::Error {
                category: ErrorCategory::UsageLimit,
                message: "limit".into(),
                retryable: false,
            },
            AgentEvent::Result {
                text: Some("done".into()),
                structured: Some(json!({"status": "done"})),
                is_error: false,
            },
            AgentEvent::Unknown {
                raw: json!({"weird": true}),
            },
        ]
    }

    #[test]
    fn every_variant_round_trips_and_tags_match() {
        for event in all_events() {
            let value = serde_json::to_value(&event).unwrap();
            assert_eq!(value["type"], event.type_name(), "{value}");
            let back: AgentEvent = serde_json::from_value(value).unwrap();
            assert_eq!(back, event);
        }
    }

    #[test]
    fn wire_format_is_flat_and_snake_case() {
        let event = AgentEvent::TextDelta {
            text: "x".into(),
            parent: None,
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"type":"text_delta","text":"x","parent":null}"#
        );
        let retry = AgentEvent::Retry {
            reason: RetryReason::RateLimit,
            delay_ms: None,
        };
        assert_eq!(
            serde_json::to_value(&retry).unwrap(),
            json!({"type": "retry", "reason": "rate_limit", "delay_ms": null})
        );
    }

    #[test]
    fn unknown_line_keeps_json_or_text() {
        assert_eq!(
            AgentEvent::unknown_line(r#"{"a":1}"#),
            AgentEvent::Unknown {
                raw: json!({"a": 1})
            }
        );
        assert_eq!(
            AgentEvent::unknown_line("not json {"),
            AgentEvent::Unknown {
                raw: json!("not json {")
            }
        );
    }
}
