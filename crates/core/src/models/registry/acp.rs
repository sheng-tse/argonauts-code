//! ACP profiles: the `[cli.acp]` table of a `[[cli]]` entry with `transport = "acp"`.
//! A profile is data, so a new ACP agent needs no code. It stays experimental until it passes
//! `acp-tck` and Argonauts Code's conformance suite on its pinned version.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Unknown keys are refused, so a key that belongs to the `[[cli]]` entry, such as
/// `errors` or `interactive_resume`, can't sit here unnoticed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AcpProfile {
    pub modes: AcpModes,
    pub model: ModelRoute,
    pub effort: EffortRoute,
    pub report: ReportRoute,
    pub resume: ResumeRoute,
    pub steer: SteerRoute,
    pub usage: Vec<UsageSource>,
    #[schemars(with = "String")]
    pub plan_usage: PlanUsageRoute,
    /// The argument vector `plan_usage = "probe"` runs, outside any task session.
    pub plan_usage_probe: Vec<String>,
    /// Whether the agent accepts stdio MCP servers. Without them there's no status tool, and
    /// the report comes back as a reply holding only the JSON object.
    pub mcp_servers: bool,
}

/// Defaults claim nothing an agent hasn't shown in conformance.
impl Default for AcpProfile {
    fn default() -> Self {
        Self {
            modes: AcpModes::default(),
            model: ModelRoute::ConfigOption,
            effort: EffortRoute::None,
            report: ReportRoute::McpTool,
            resume: ResumeRoute::None,
            steer: SteerRoute::CancelAndPrompt,
            usage: vec![UsageSource::UsageUpdate],
            plan_usage: PlanUsageRoute::None,
            plan_usage_probe: Vec::new(),
            mcp_servers: true,
        }
    }
}

/// The agent's session mode for each access, such as Hermes' `default`. Unset sends none, and
/// no build mode means the agent can't own a task.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AcpModes {
    pub read_only: Option<String>,
    pub build: Option<String>,
    pub experiment: Option<String>,
}

/// How the model is set. The driver reads it back and ends the session on a mismatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ModelRoute {
    /// A session config option of category `model`.
    ConfigOption,
    /// `session/set_model`, sent as a raw request, as Hermes takes it.
    LegacySetModel,
    /// `{model}` in the launch arguments.
    LaunchArg,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EffortRoute {
    /// A session config option of category `thought_level`.
    ConfigOption,
    None,
}

/// How the structured report comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ReportRoute {
    /// The `argonauts_report` tool of `argonauts mcp session`.
    McpTool,
    /// One more prompt asking for a reply that holds only the JSON object.
    FinalPrompt,
}

/// Without a route, the settings check refuses `task`, `run`, and `project` lifetimes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ResumeRoute {
    /// `session/resume`.
    Resume,
    /// `session/load`. Its replay goes to the raw log and isn't emitted again.
    Load,
    None,
}

/// How a message is delivered mid-turn (`s` in a session view).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SteerRoute {
    /// A `session/prompt` during the turn, which the agent folds into it.
    Input,
    /// Wait for the running tool call, `session/cancel`, then `session/prompt` with the message.
    CancelAndPrompt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum UsageSource {
    /// Usage in the `session/prompt` response.
    EndTurn,
    /// `usage_update` notifications.
    UsageUpdate,
}

/// Where plan usage readings come from. Plan usage belongs to a login, not a transport.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum PlanUsageRoute {
    /// No reading, so the breaker falls back to what it does for weak signals.
    #[default]
    None,
    /// `plan_usage_probe`.
    Probe,
    /// Counts against another CLI's probe, such as `codex` for `codex-acp`.
    SameLoginAs(String),
}

const SAME_LOGIN_AS: &str = "same-login-as:";

impl TryFrom<String> for PlanUsageRoute {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        match text.as_str() {
            "none" => Ok(Self::None),
            "probe" => Ok(Self::Probe),
            other => match other.strip_prefix(SAME_LOGIN_AS) {
                Some(cli) if !cli.is_empty() => Ok(Self::SameLoginAs(cli.to_owned())),
                _ => Err(format!(
                    "plan usage {text:?}: use \"none\", \"probe\", or \"same-login-as:<agent>\""
                )),
            },
        }
    }
}

impl fmt::Display for PlanUsageRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::Probe => f.write_str("probe"),
            Self::SameLoginAs(cli) => write!(f, "{SAME_LOGIN_AS}{cli}"),
        }
    }
}

impl From<PlanUsageRoute> for String {
    fn from(route: PlanUsageRoute) -> Self {
        route.to_string()
    }
}
