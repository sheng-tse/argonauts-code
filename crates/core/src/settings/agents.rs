//! CLI agents, direct endpoints, the registry, and the MCP servers.

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Span;
use crate::{AuthMode, TokenPrice};

/// One CLI adapter's settings, `[agents.<id>]`.
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
    /// The version you pinned; `doctor` warns when the installed one is different.
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

impl super::Settings {
    /// The model `worker` really runs: its own, else `agents.<id>.model`. A bare CLI with
    /// neither runs its CLI's own default, which settings can't name, so it stays bare. Two
    /// refs that run the same model are one fallback: a bare `opencode` whose OpenCode is set to
    /// the Qwen that stopped would only run that Qwen again.
    pub fn model_run_by(&self, worker: &crate::names::ModelRef) -> crate::names::ModelRef {
        let configured = || {
            let agent = self.agents.get(&worker.agent)?;
            agent.model.clone()
        };
        match worker.model.is_none().then(configured).flatten() {
            Some(model) => crate::names::ModelRef::new(worker.agent.clone(), Some(model)),
            None => worker.clone(),
        }
    }
}

/// One OpenAI-compatible endpoint, `[endpoints.<id>]`. Built-in ids with known URLs:
/// `openrouter` and the local runtimes `ollama`, `lmstudio`, `llamacpp` and `vllm`; anything
/// else needs `base_url`.
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
    /// Most requests the server takes at once, counted across every model on it and every
    /// process. Unset, a local runtime takes its own number and any other endpoint 4.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
    /// Free the models Argonauts Code loaded on this server once nothing uses them. Only a
    /// local runtime that can let one go does.
    pub free_memory: bool,
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
            max_concurrency: None,
            free_memory: true,
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
    /// Run discovery at `argonauts setup` and `/config models`.
    pub discover: bool,
    /// Discovery also asks the model servers it knows on this computer, such as Ollama and LM
    /// Studio, whenever they answer, with or without an `[endpoints.<id>]` table.
    pub local_servers: bool,
}

impl Default for RegistrySettings {
    fn default() -> Self {
        Self {
            discover: true,
            local_servers: true,
        }
    }
}

/// `argonauts mcp serve` and the tools crew sessions get. Every key is a safety key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct McpSettings {
    /// Give crew sessions the read-only `argonauts_status` tool.
    pub status_tool: bool,
    pub start_runs: StartRuns,
    /// Outside agents may send guidance. It raises no autonomy.
    pub guidance: bool,
    /// Outside agents may pause runs.
    pub pause: bool,
    /// How much of a run leaves the machine through the server. Never diffs, file contents,
    /// prompts, or raw logs.
    pub share: Share,
    /// The time limit of each run an outside agent starts.
    pub run_limit: Span,
    /// Runs outside agents may start per day, across every repo.
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
    /// Only in a repo set up with `argonauts setup` whose settings you trusted.
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

#[cfg(test)]
mod tests {
    use crate::names::ModelRef;
    use crate::settings::Settings;

    use super::AgentSettings;

    #[test]
    fn a_bare_cli_runs_the_model_its_settings_name() {
        let mut settings = Settings::default();
        let qwen = ModelRef::new("opencode", Some("ollama/qwen3-coder:30b-64k".into()));
        let bare = ModelRef::new("opencode", None);
        assert_eq!(settings.model_run_by(&bare), bare);
        settings.agents.insert(
            "opencode".into(),
            AgentSettings {
                model: Some("ollama/qwen3-coder:30b-64k".into()),
                ..AgentSettings::default()
            },
        );
        assert_eq!(settings.model_run_by(&bare), qwen);
        assert_eq!(settings.model_run_by(&qwen), qwen);
        let llama = ModelRef::new("opencode", Some("ollama/llama3.2".into()));
        assert_eq!(settings.model_run_by(&llama), llama);
    }
}
