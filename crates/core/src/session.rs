//! What the conductor asks an adapter for, and the process spec an adapter returns.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::registry::LaunchHints;
use crate::settings::sessions::{Compaction, NativeFanout};

/// Whether a session may change files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionRole {
    /// Reads and answers only, as far as the CLI's flags can enforce it
    /// (see [`crate::Capabilities::enforces_read_only`]).
    #[default]
    ReadOnly,
    Build,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMode {
    /// The CLI's own login, such as a Claude or ChatGPT plan. API key variables are removed.
    #[default]
    Subscription,
    ApiKey,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionRequest {
    pub workdir: PathBuf,
    pub prompt: String,
    /// `None` uses the CLI's default.
    pub model: Option<String>,
    /// Such as `low`. `None` uses the CLI's default.
    pub effort: Option<String>,
    pub role: SessionRole,
    pub auth: AuthMode,
    /// Report schema to enforce where the harness supports it.
    pub json_schema: Option<serde_json::Value>,
    pub resume_session: Option<String>,
    /// Native spend cap where the harness has one, such as Claude Code `--max-budget-usd`.
    pub max_budget_usd: Option<f64>,
    pub extra_args: Vec<String>,
    /// A private directory outside the worktree for files the command needs, such as a schema file.
    pub scratch_dir: Option<PathBuf>,
    /// SHA-256 of the approved goal. The conductor refuses a build session without one.
    pub goal_hash: Option<String>,
    /// Keep stdin open with the harness's stream input, for steering and the wrap-up message.
    /// Also keeps the prompt out of the process list.
    #[serde(default)]
    pub keep_input_open: bool,
    /// Choose the vendor session id up front where the harness allows (Claude `--session-id`).
    #[serde(default)]
    pub session_id: Option<String>,
    /// A per-run project name where the harness shares one store (agy `--project`).
    #[serde(default)]
    pub project: Option<String>,
    /// `None` passes nothing.
    #[serde(default)]
    pub compaction: Option<Compaction>,
    /// `None` passes nothing.
    #[serde(default)]
    pub native_fanout: Option<NativeFanout>,
    /// An MCP config file registering the read-only `argonauts_status` tool.
    #[serde(default)]
    pub mcp_config: Option<PathBuf>,
    #[serde(default)]
    pub env_set: Vec<(String, String)>,
    /// Registry launch hints, such as Codex `--oss --local-provider ollama`.
    #[serde(default)]
    pub launch: Option<LaunchHints>,
    #[serde(default)]
    pub user_hooks_off: bool,
    /// Load none of the CLI's own MCP servers, plugins, or project settings. See
    /// [`SessionSpec::isolated`](crate::worker::SessionSpec::isolated).
    #[serde(default)]
    pub isolated: bool,
    /// The CLI's own sandbox stays off and the commands it runs need no approval. Our sandbox
    /// holds the session (macOS won't start one sandbox inside another), or `sandbox.mode =
    /// "none"` asked for no protection at all.
    #[serde(default)]
    pub tool_sandbox_off: bool,
}

/// A file the conductor writes before spawning the command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecFile {
    /// Parent directories are created.
    pub path: PathBuf,
    pub contents: String,
}

impl SessionRequest {
    /// The prompt, ending with the report's schema when the session has one. For a CLI with no
    /// flag that holds a run to a schema, the model learns the report's shape only from this:
    /// without it, a live gemma4 session on OpenCode wrote its report as plain lines.
    pub fn prompt_with_schema(&self) -> String {
        let Some(schema) = &self.json_schema else {
            return self.prompt.clone();
        };
        let schema = serde_json::to_string_pretty(schema).unwrap_or_else(|_| schema.to_string());
        format!(
            "{}\n\nEnd your final message with your report: one JSON object that matches this \
             schema.\n\n```json\n{schema}\n```\n",
            self.prompt.trim_end()
        )
    }
}

/// A process to spawn, as an argument vector. No shell is involved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env_set: Vec<(String, String)>,
    pub env_remove: Vec<String>,
    /// `None` means stdin is null.
    pub stdin: Option<String>,
    pub cwd: PathBuf,
    pub files: Vec<SpecFile>,
    pub keep_stdin_open: bool,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            cwd: cwd.into(),
            ..Self::default()
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn display_line(&self) -> String {
        std::iter::once(&self.program)
            .chain(&self.args)
            .map(|part| quote_for_display(part))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn quote_for_display(part: &str) -> String {
    let plain = !part.is_empty()
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c));
    if plain {
        part.to_owned()
    } else {
        format!("'{}'", part.replace('\'', r"'\''"))
    }
}

/// API key and token variables that no child sees unless its own auth mode needs one.
pub const VENDOR_KEY_ENV_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "CODEX_ACCESS_TOKEN",
    "AZURE_OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "XAI_API_KEY",
    "OPENROUTER_API_KEY",
    "GH_TOKEN",
    "GITHUB_TOKEN",
];

/// Set by a parent Claude Code session. Children must not inherit them when `argonauts` runs
/// inside Claude Code, or they could message that session.
pub const PARENT_SESSION_ENV_VARS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
];

/// Every variable from [`VENDOR_KEY_ENV_VARS`] except those in `keep`, plus all of
/// [`PARENT_SESSION_ENV_VARS`].
pub fn env_remove_except(keep: &[&str]) -> Vec<String> {
    VENDOR_KEY_ENV_VARS
        .iter()
        .filter(|name| !keep.contains(name))
        .chain(PARENT_SESSION_ENV_VARS)
        .map(|name| (*name).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_safe_choices() {
        let req = SessionRequest::default();
        assert_eq!(req.role, SessionRole::ReadOnly);
        assert_eq!(req.auth, AuthMode::Subscription);
        assert!(req.json_schema.is_none());
    }

    #[test]
    fn new_request_fields_default_to_the_0_0_1_behavior() {
        let req = SessionRequest::default();
        assert!(!req.keep_input_open && !req.user_hooks_off);
        assert!(req.session_id.is_none() && req.project.is_none());
        assert!(req.compaction.is_none() && req.native_fanout.is_none());
        assert!(req.mcp_config.is_none() && req.launch.is_none());
        assert!(req.env_set.is_empty());
    }

    #[test]
    fn requests_written_before_the_new_fields_still_read() {
        let old = r#"{"workdir":"/w","prompt":"hi","model":null,"effort":null,"role":"build",
            "auth":"subscription","json_schema":null,"resume_session":null,"max_budget_usd":null,
            "extra_args":[],"scratch_dir":null,"goal_hash":"g"}"#;
        let req: SessionRequest = serde_json::from_str(old).unwrap();
        assert_eq!(req.role, SessionRole::Build);
        assert!(!req.keep_input_open);
        let full = SessionRequest {
            keep_input_open: true,
            compaction: Some(Compaction::Off),
            native_fanout: Some(NativeFanout::Limit { max: 2 }),
            launch: Some(LaunchHints {
                oss: true,
                local_provider: Some("ollama".into()),
                provider_model: None,
            }),
            env_set: vec![("ARGONAUTS_SESSION".into(), "s-001".into())],
            ..req
        };
        let json = serde_json::to_string(&full).unwrap();
        assert_eq!(serde_json::from_str::<SessionRequest>(&json).unwrap(), full);
    }

    #[test]
    fn role_and_auth_serialize_kebab_case() {
        assert_eq!(
            serde_json::to_string(&SessionRole::ReadOnly).unwrap(),
            "\"read-only\""
        );
        assert_eq!(
            serde_json::to_string(&AuthMode::ApiKey).unwrap(),
            "\"api-key\""
        );
    }

    #[test]
    fn builder_collects_args() {
        let spec = CommandSpec::new("claude", "/tmp/w")
            .arg("-p")
            .args(["--model", "haiku"]);
        assert_eq!(spec.program, "claude");
        assert_eq!(spec.args, ["-p", "--model", "haiku"]);
        assert_eq!(spec.cwd, PathBuf::from("/tmp/w"));
        assert!(spec.stdin.is_none());
        assert!(!spec.keep_stdin_open);
    }

    #[test]
    fn display_line_quotes_only_when_needed() {
        let spec = CommandSpec::new("agy", ".").args(["-p", "it's done", "--model=x", ""]);
        assert_eq!(spec.display_line(), r"agy -p 'it'\''s done' --model=x ''");
    }

    #[test]
    fn env_remove_keeps_requested_vars() {
        let removed = env_remove_except(&["ANTHROPIC_API_KEY"]);
        assert!(!removed.iter().any(|v| v == "ANTHROPIC_API_KEY"));
        assert!(removed.iter().any(|v| v == "OPENAI_API_KEY"));
        assert!(removed.iter().any(|v| v == "GITHUB_TOKEN"));
        assert!(removed.iter().any(|v| v == "CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(removed.iter().any(|v| v == "CLAUDE_CODE_MESSAGING_TOKEN"));
        let all = VENDOR_KEY_ENV_VARS.len() + PARENT_SESSION_ENV_VARS.len();
        assert_eq!(removed.len(), all - 1);
        assert_eq!(env_remove_except(&[]).len(), all);
    }
}
