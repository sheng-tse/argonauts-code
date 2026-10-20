//! CLI manifests: a `[[cli]]` entry in `models.toml` that runs a CLI with no adapter
//! code. Every field past the first few is optional, so older entries keep working.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::acp::AcpProfile;
use crate::ErrorCategory;
use crate::adapter::Transport;
use crate::settings::Span;

/// A project's entries count only after you trust `.argonauts/models.toml`, and `binary` must
/// be an absolute path outside the repo.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct CliManifest {
    pub id: String,
    pub display_name: String,
    pub binary: String,
    #[serde(default)]
    pub version_args: Vec<String>,
    #[serde(default)]
    pub tested_versions: Vec<String>,
    /// Placeholders: `{model}`, `{effort}`, `{workdir}`, `{session}`, `{home}`,
    /// `{schema_file}`, `{prompt_file}`, and `{prompt}` only with `prompt = "argument"`.
    pub read_only_args: Vec<String>,
    pub build_args: Vec<String>,
    /// `{session}` is filled in.
    #[serde(default)]
    pub resume_args: Vec<String>,
    /// `from-model` takes the provider from each model's registry entry, for a CLI that runs
    /// many providers.
    pub provider: String,
    /// Environment variable names it needs, never values.
    #[serde(default)]
    pub env: Vec<String>,
    /// `acp` makes the entry an ACP profile, run through the `acp` table.
    #[serde(default)]
    pub transport: Transport,
    #[serde(default)]
    pub prompt: PromptChannel,
    #[serde(default)]
    pub output: OutputKind,
    /// With `output = "parser"`: the built-in adapter whose parser to reuse, such as
    /// `claude-code`. Its plan usage and health signals are stamped with this CLI's id.
    #[serde(default)]
    pub parser: Option<String>,
    #[serde(default)]
    pub final_json: Option<FinalJsonPointers>,
    /// For a CLI that enforces an output schema itself.
    #[serde(default)]
    pub schema: Option<SchemaFlag>,
    /// Checked against stderr and error messages, in order. Each is pinned by a recorded fixture.
    #[serde(default)]
    pub errors: Vec<ErrorPattern>,
    /// Exit codes as text, such as `"2"`, to error categories.
    #[serde(default)]
    pub exit_codes: BTreeMap<String, ErrorCategory>,
    /// Fixed values, such as a self-update switch. Names that look like secrets and known
    /// approval-bypass switches are refused.
    #[serde(default)]
    pub env_set: BTreeMap<String, String>,
    #[serde(default)]
    pub home: Option<HomeTemplate>,
    #[serde(default)]
    pub stop: Option<StopWaits>,
    /// Its sessions talk to a server of the CLI's own, on a loopback port it picks as it
    /// starts, as OpenCode's do. A session with the network off still reaches loopback then.
    #[serde(default)]
    pub own_server: bool,
    /// The take-over command, with `{session}`. Empty means none.
    #[serde(default)]
    pub interactive_resume: Vec<String>,
    #[serde(default)]
    pub acp: Option<AcpProfile>,
}

/// How the prompt reaches the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum PromptChannel {
    #[default]
    Stdin,
    /// Written to `<session_dir>/prompt.md` for `{prompt_file}`.
    File,
    /// On the command line, where any of your processes can read it. A last resort, and
    /// doctor warns about it.
    Argument,
}

/// How stdout is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum OutputKind {
    /// One normalized `AgentEvent` per line.
    #[default]
    ArgonautsJsonl,
    /// Another CLI's stream format, read by the built-in parser `parser` names.
    Parser,
    /// One JSON document at exit, read through `final_json`. File diffs are the only live view.
    FinalJson,
    /// Only a final answer. The step runner looks for the last JSON object in it.
    Text,
}

/// JSON pointers into the document a `final-json` CLI prints at exit.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct FinalJsonPointers {
    pub text: Option<String>,
    pub status: Option<String>,
    /// Values of `status` that mean success.
    pub ok_status: Vec<String>,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub input_tokens: Option<String>,
    pub output_tokens: Option<String>,
    pub cached_tokens: Option<String>,
    pub cost_usd: Option<String>,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SchemaFlag {
    /// Passed with the schema file's path, such as `--json-schema`.
    pub flag: String,
    /// The result field that holds the structured output, such as `structured_output`.
    pub field: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorPattern {
    /// A regular expression.
    pub pattern: String,
    pub category: ErrorCategory,
    #[serde(default)]
    pub retryable: bool,
}

/// A home made fresh from a template for each task or session, so the agent's own memory,
/// skills, and logins never leak between sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HomeTemplate {
    /// The variable pointed at `<session_dir>/home`, such as `HERMES_HOME`.
    pub env: String,
    /// A folder in `~/.argonauts/clis/`, or one built into the binary. Never a repo's.
    pub template: String,
    pub keep: HomeKeep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum HomeKeep {
    /// Kept while the task lives, so a resume finds its session.
    Task,
    Session,
}

/// The stop plan's waits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StopWaits {
    pub after_interrupt: Span,
    pub after_terminate: Span,
}
