//! Harness agents, direct endpoints, the registry, and the MCP servers.

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Span;
use crate::{AuthMode, TokenPrice};

/// One harness adapter's settings, `[agents.<id>]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AgentSettings {
    pub enabled: bool,
    /// Model id or alias; unset uses the CLI's default.
    pub model: Option<String>,
    pub effort: Option<String>,
    pub auth: AuthMode,
    /// Token prices when the CLI reports only tokens.
    pub price: Option<TokenPrice>,
    pub binary: Option<PathBuf>,
    /// The version you pinned; `doctor` warns when the installed one differs.
    pub version_pin: Option<String>,
    pub extra_args: Vec<String>,
    /// Paths this agent must never see (removed from its worktrees with sparse checkout).
    pub exclude_paths: Vec<String>,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            model: None,
            effort: None,
            auth: AuthMode::Subscription,
            price: None,
            binary: None,
            version_pin: None,
            extra_args: Vec::new(),
            exclude_paths: Vec::new(),
        }
    }
}

/// One OpenAI-compatible endpoint, `[endpoints.<id>]`. Built-in ids with preset URLs:
/// `openrouter`, `ollama`, `lmstudio`; anything else needs `base_url`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct EndpointSettings {
    pub base_url: Option<String>,
    /// The environment variable holding the key, never the key itself.
    pub api_key_env: Option<String>,
    pub api: EndpointApi,
    pub structured_output: StructuredMode,
    /// Local endpoints send nothing off your machine.
    pub local: bool,
    /// Moving work to a paid API needs your OK unless this is set.
    pub allow_api_billing: bool,
    pub timeout: Span,
    pub max_concurrency: u32,
    /// Header names mapped to environment variable names that hold their values.
    pub header_env: BTreeMap<String, String>,
}

impl Default for EndpointSettings {
    fn default() -> Self {
        Self {
            base_url: None,
            api_key_env: None,
            api: EndpointApi::ChatCompletions,
            structured_output: StructuredMode::JsonSchema,
            local: false,
            allow_api_billing: false,
            timeout: Span::new("5m"),
            max_concurrency: 4,
            header_env: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EndpointApi {
    ChatCompletions,
    Responses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StructuredMode {
    /// `response_format: json_schema`, strict.
    JsonSchema,
    /// `response_format: json_object`, then validated.
    JsonObject,
    /// The schema goes in the prompt, then validated.
    Prompt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RegistrySettings {
    /// Run discovery at `argonauts init` and `/config models`.
    pub discover: bool,
}

impl Default for RegistrySettings {
    fn default() -> Self {
        Self { discover: true }
    }
}

/// `argonauts mcp serve` and the tools crew sessions get. Every key is a safety key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct McpSettings {
    /// Give crew sessions the read-only `argonauts_status` tool.
    pub status_tool: bool,
    pub start_runs: StartRuns,
    /// Front ends may send guidance. It raises no autonomy.
    pub guidance: bool,
    /// Front ends may pause runs.
    pub pause: bool,
    /// How much of a run leaves the machine through the server. Never diffs, file contents,
    /// prompts, or raw logs.
    pub share: Share,
    /// The time budget of each run a front end starts.
    pub run_limit: Span,
    /// Runs front ends may start per day, across every repo.
    pub runs_per_day: u32,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            status_tool: true,
            start_runs: StartRuns::TrustedRepos,
            guidance: true,
            pause: true,
            share: Share::Summaries,
            run_limit: Span::new("60m"),
            runs_per_day: 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum StartRuns {
    /// Only in a repo set up with `argonauts init` whose settings you trusted.
    TrustedRepos,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Share {
    /// Ids, states, counts, spend, and needs-you flags.
    StatusOnly,
    /// Also short, sanitized agent text in fields named `agent_text`.
    Summaries,
}
