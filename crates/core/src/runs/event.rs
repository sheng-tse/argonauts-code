//! The CLI-neutral events every adapter's parser produces.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::screen::ScreenChange;
use crate::models::limits::PlanUsageReading;
use crate::runs::events::context::ContextUsage;
use crate::runs::events::session::{CliWorkflowRun, SubagentUpdate};

/// `parent` is the id of the tool call that spawned a subagent, when the event came from one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Init {
        /// Used to resume.
        session_id: Option<String>,
        model: Option<String>,
        tools: Vec<String>,
        /// The plugins and mods the CLI loaded, by name.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        plugins: Vec<String>,
        /// What the CLI says this session can do, such as Claude Code's `interrupt_send_now_v1`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
        /// The CLI's version, where its first line says it, so an update that lands mid-day is
        /// noticed by the next session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
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
    /// The model began writing a tool call: its name is known and its input is still coming.
    /// `ToolStarted` with the same id follows once the call is whole, or `ToolDropped` when it
    /// never lands. Only CLIs that stream the call as it's written send it.
    ToolPending {
        id: String,
        name: String,
        parent: Option<String>,
    },
    /// A call announced by `ToolPending` that the model never finished.
    ToolDropped {
        id: String,
        parent: Option<String>,
    },
    ToolStarted {
        /// Matched by `ToolFinished`.
        id: String,
        /// As the agent reports it, such as `Bash` or `command_execution`.
        name: String,
        input: Value,
        parent: Option<String>,
    },
    ToolFinished {
        id: String,
        ok: bool,
        /// Possibly truncated by the agent.
        output: Option<String>,
        parent: Option<String>,
    },
    /// What a finished edit changed, as the CLI's own result says, with the file's line
    /// numbers and the lines around each change: Claude Code's structured patch. It comes just
    /// before the call's `ToolFinished`, so the call is drawn with it.
    FilePatch {
        /// The edit's tool call.
        id: String,
        /// As the CLI names the file.
        path: String,
        /// Unified hunks: `@@ -a,b +c,d @@` headers, then lines marked ` `, `-`, or `+`.
        hunks: String,
        parent: Option<String>,
    },
    FileChanged {
        /// Relative to the workdir where the agent gives one.
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
        /// The window's size. `None` when the stream doesn't say, and the session runner then fills
        /// it in from the registry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
        /// What Argonauts Code sent the session so far (brief, task, fallback note, messages), by
        /// the limit's estimate. The session runner fills it in; a CLI never reports it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sent: Option<u64>,
    },
    /// The session's context as the CLI's own /context counts it: Claude Code's
    /// `get_context_usage` reply. Each one replaces the last.
    ContextBreakdown {
        usage: Box<ContextUsage>,
    },
    /// The account's plan, as the CLI names it, such as Claude Code's `Claude Max`.
    Plan {
        plan: String,
    },
    Retry {
        reason: RetryReason,
        delay_ms: Option<u64>,
    },
    /// Plan usage or rate limit information, kept as the agent sent it.
    PlanUsageSignal {
        detail: Value,
    },
    /// An agent's subagent, workflow, or thread changed state: Claude Code's
    /// `parent_tool_use_id`, Codex's `collab_tool_call`, agy's `subagent_info`.
    Subagent {
        update: SubagentUpdate,
    },
    /// A workflow the agent's own CLI runs, with its phases and agents, replacing any earlier
    /// state of the same id.
    Workflow {
        workflow: Box<CliWorkflowRun>,
    },
    /// A normalized plan usage reading. `PlanUsageSignal` still carries raw payloads.
    PlanUsage {
        reading: PlanUsageReading,
    },
    /// Something a plugin's mod pushed for the screen, or a notice the CLI shows outside the
    /// conversation. Never sent to the model.
    PluginOutput {
        kind: PluginOutputKind,
        /// The plugin that pushed it, when one did.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin: Option<String>,
        /// `None` clears a plugin's status line.
        text: Option<String>,
        /// How long a toast or notice stays, when the CLI says.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// The models the CLI offers, as it lists them itself, so the picker can mirror it.
    ModelsReported {
        models: Vec<ReportedModel>,
    },
    /// An isolated session loaded what it must not see: your plugins and their mods, or the
    /// skills synced from your account. Only an isolated session's parser reports it.
    IsolationLeak {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        plugins: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skills: Vec<String>,
        /// Built-in plugins the adapter doesn't know, which a later version of the CLI loads
        /// by default. Not yours, so no leak, but learned, and turned off from then on.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        builtins: Vec<String>,
    },
    /// The CLI's own commands as it lists them now, which replace the list before. Claude Code
    /// sends them in its `initialize` reply, and again when a mod registers one.
    CommandsChanged {
        commands: Vec<ReportedCommand>,
    },
    /// Where a message of yours stands that went into a running session, by the id it went
    /// with.
    MessageState {
        uuid: String,
        state: MessageStage,
    },
    /// What the CLI draws for a screen that joined its session, such as You should know above
    /// the input.
    ScreenChanged {
        change: ScreenChange,
    },
    /// The tasks the CLI runs in the background now, by id, such as a command send now moved
    /// there. They outlive the turn, and the CLI stops what still runs a while after its input
    /// closes. A task leaves the list when it ends.
    BackgroundTasks {
        running: Vec<String>,
        /// A task just ended and the CLI tells the model. Between turns that is a turn of the
        /// CLI's own, whose result follows. A task the CLI stopped isn't counted as told:
        /// Claude Code never tells a workflow or an agent it stopped, and tells a command it
        /// stopped at its deadline or for memory only in a turn that comes after the input
        /// closed.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        ended: bool,
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
            Self::ToolPending { .. } => "tool_pending",
            Self::ToolDropped { .. } => "tool_dropped",
            Self::ToolStarted { .. } => "tool_started",
            Self::ToolFinished { .. } => "tool_finished",
            Self::FilePatch { .. } => "file_patch",
            Self::FileChanged { .. } => "file_changed",
            Self::Todo { .. } => "todo",
            Self::Usage { .. } => "usage",
            Self::ContextFill { .. } => "context_fill",
            Self::ContextBreakdown { .. } => "context_breakdown",
            Self::Plan { .. } => "plan",
            Self::Retry { .. } => "retry",
            Self::PlanUsageSignal { .. } => "plan_usage_signal",
            Self::Subagent { .. } => "subagent",
            Self::Workflow { .. } => "workflow",
            Self::PlanUsage { .. } => "plan_usage",
            Self::PluginOutput { .. } => "plugin_output",
            Self::ModelsReported { .. } => "models_reported",
            Self::IsolationLeak { .. } => "isolation_leak",
            Self::CommandsChanged { .. } => "commands_changed",
            Self::MessageState { .. } => "message_state",
            Self::ScreenChanged { .. } => "screen_changed",
            Self::BackgroundTasks { .. } => "background_tasks",
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

/// Where Claude Code draws what a plugin pushes, which is where Argonauts Code draws it too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginOutputKind {
    /// The notification bar under the input, for a few seconds.
    Toast,
    /// A line pinned under the input, one per plugin.
    Status,
    /// A dim line in the transcript under the plugin's name.
    Log,
    /// The notification bar.
    Notice,
    /// A notice worth the bell, such as a `high` or `immediate` notification.
    Urgent,
    /// A dim transcript line.
    Info,
    /// A transcript line in the warning color.
    Warning,
}

/// One model as the CLI lists it, such as Claude Code's `initialize` reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedModel {
    /// What to pass the CLI, such as `opus` or `claude-sonnet-4-6`.
    pub value: String,
    /// The model it runs today, such as `claude-opus-5-5` for `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Empty when the model takes no effort setting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<String>,
}

/// One of the CLI's own commands as it lists them, such as one a plugin's mod registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedCommand {
    /// Without the slash.
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Such as `[base]`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub argument_hint: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// One of the CLI's own, not a plugin's or a mod's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub builtin: bool,
}

impl ReportedCommand {
    /// The plugin it comes from, where its name or an alias says, as `pr-tools:review` does.
    pub fn plugin(&self) -> Option<&str> {
        std::iter::once(&self.name)
            .chain(&self.aliases)
            .find_map(|name| name.split_once(':').map(|(plugin, _)| plugin))
            .filter(|plugin| !plugin.is_empty())
    }
}

/// How far a message of yours got once it went into a running session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStage {
    /// The CLI holds it until the model can read it.
    Queued,
    /// The model read it.
    Started,
    Completed,
    /// Send now worked: what waited goes to the model at once.
    SendingNow,
    /// Send now found nothing waiting, since the model already had it.
    NothingWaiting,
    /// Taken back before the model read it.
    TakenBack,
    /// Too late to take back: the model read it.
    AlreadyRead,
    /// The session's input had closed, so it never went.
    NotSent,
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
    /// The agent's description.
    Other(String),
}

impl RetryReason {
    /// A retry after a sign-in error, such as Claude Code's `authentication_failed`: it says
    /// the account needs signing in, not that the model is in trouble.
    pub fn is_sign_in(&self) -> bool {
        matches!(self, Self::Other(why) if why.contains("auth"))
    }
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
    /// The CLI no longer has the session it was asked to resume, so a fresh one takes over.
    SessionLost,
    ModelUnavailable,
    InvalidRequest,
    /// The model missed an answer format the CLI had accepted. Another try may give it.
    AnswerMissed,
    /// A tool or action was denied.
    Permission,
    /// A native spend cap was hit.
    Limit,
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
    use crate::base::names::AgentId;
    use crate::models::limits::{PlanUsageSource, PlanUsageStatus};
    use crate::runs::events::context::{ContextCategory, ContextKind, MemoryFile};
    use crate::runs::events::session::{SubagentKind, SubagentStatus};
    use serde_json::json;
    use time::macros::datetime;

    fn all_events() -> Vec<AgentEvent> {
        vec![
            AgentEvent::Init {
                session_id: Some("s1".into()),
                model: Some("haiku".into()),
                tools: vec!["Read".into()],
                plugins: Vec::new(),
                capabilities: Vec::new(),
                version: None,
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
            AgentEvent::ToolPending {
                id: "t1".into(),
                name: "Bash".into(),
                parent: None,
            },
            AgentEvent::ToolDropped {
                id: "t0".into(),
                parent: Some("t9".into()),
            },
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
            AgentEvent::FilePatch {
                id: "t2".into(),
                path: "/w/hello.txt".into(),
                hunks: "@@ -1,1 +1,1 @@\n-hi\n+hello".into(),
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
            AgentEvent::ContextFill {
                used: 212_000,
                size: Some(1_000_000),
                sent: Some(3_400),
            },
            AgentEvent::ContextBreakdown {
                usage: Box::new(ContextUsage {
                    categories: vec![
                        ContextCategory {
                            name: "Messages".into(),
                            tokens: 186_000,
                            color: "purple_FOR_SUBAGENTS_ONLY".into(),
                            kind: ContextKind::Used,
                        },
                        ContextCategory {
                            name: "Free space".into(),
                            tokens: 788_000,
                            color: "promptBorder".into(),
                            kind: ContextKind::Free,
                        },
                    ],
                    total_tokens: 186_000,
                    window: 1_000_000,
                    raw_window: 1_000_000,
                    percentage: 19,
                    model: Some("claude-opus-5-5".into()),
                    auto_compact: true,
                    auto_compact_threshold: Some(950_000),
                    memory_files: vec![MemoryFile {
                        path: "CLAUDE.md".into(),
                        scope: "Project".into(),
                        tokens: 4_900,
                    }],
                    ..ContextUsage::default()
                }),
            },
            AgentEvent::Plan {
                plan: "Claude Max".into(),
            },
            AgentEvent::Retry {
                reason: RetryReason::Other("flaky".into()),
                delay_ms: Some(500),
            },
            AgentEvent::PlanUsageSignal {
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
                    tokens: None,
                },
            },
            AgentEvent::PlanUsage {
                reading: PlanUsageReading {
                    agent_id: AgentId("claude-code".into()),
                    status: PlanUsageStatus::Warning,
                    used: Some(0.71),
                    resets_at: Some(datetime!(2026-09-29 18:00 UTC)),
                    window: Some("five_hour".into()),
                    source: PlanUsageSource::RateLimitEvent,
                    observed_at: datetime!(2026-09-29 12:00 UTC),
                    detail: None,
                    wrap_up: false,
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
    fn an_old_context_fill_reads_with_nothing_sent() {
        let old: AgentEvent =
            serde_json::from_value(json!({"type": "context_fill", "used": 40, "size": 200}))
                .unwrap();
        assert_eq!(
            old,
            AgentEvent::ContextFill {
                used: 40,
                size: Some(200),
                sent: None
            }
        );
        assert_eq!(
            serde_json::to_value(&old).unwrap(),
            json!({"type": "context_fill", "used": 40, "size": 200})
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
