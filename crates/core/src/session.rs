//! What Argonauts Code asks an adapter for, and the process spec an adapter returns.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::registry::LaunchHints;
use crate::session_runner::{SessionPreferences, SessionShell};
use crate::settings::context::ContextAsk;
use crate::settings::sessions::{Compaction, NativeFanout};
use crate::settings::workflows::WorkflowSize;

/// Whether a session may change files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionRole {
    /// Reads and answers only, as far as the CLI's flags can enforce it
    /// (see [`crate::Capabilities::enforces_read_only`]).
    #[default]
    ReadOnly,
    Build,
    /// Direct Captain: the CLI's default tools in your checkout, asking as [`Asks`] says.
    Direct,
}

/// What a direct session asks you about before it acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Asks {
    /// Nothing: auto mode, where the sandbox and the hard limits hold.
    #[default]
    Never,
    /// What the CLI itself asks about when you run it.
    LikeCli,
    /// Every edit and command, whatever the CLI would ask.
    EveryChange,
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
    /// Pictures that go with the prompt, as files, for a CLI that takes them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pictures: Vec<crate::pictures::Picture>,
    /// `None` uses the CLI's default.
    pub model: Option<String>,
    /// Such as `low`. `None` uses the CLI's default.
    pub effort: Option<String>,
    pub role: SessionRole,
    pub auth: AuthMode,
    /// Report schema to enforce where the CLI supports it.
    pub json_schema: Option<serde_json::Value>,
    pub resume_session: Option<String>,
    /// Resume `resume_session` as a copy with an id of its own, which leaves the original as
    /// it was, where the CLI can (Claude Code `--fork-session`, `codex exec fork`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fork_session: bool,
    /// Native spend cap where the CLI has one, such as Claude Code `--max-budget-usd`.
    pub spend_cap_usd: Option<f64>,
    pub extra_args: Vec<String>,
    /// A private directory outside the worktree for files the command needs, such as a schema file.
    pub scratch_dir: Option<PathBuf>,
    /// SHA-256 of the approved goal. Argonauts Code refuses a build session without one.
    pub goal_hash: Option<String>,
    /// Keep stdin open with the CLI's stream input, for steering and the wrap-up message.
    /// Also keeps the prompt out of the process list.
    #[serde(default)]
    pub keep_input_open: bool,
    /// Choose the CLI session id up front where the CLI allows (Claude `--session-id`).
    #[serde(default)]
    pub session_id: Option<String>,
    /// A per-run project name where the CLI shares one store (agy `--project`).
    #[serde(default)]
    pub project: Option<String>,
    /// The context window to ask the CLI for, where it has a setting for it: the largest it
    /// offers for the model, or less when `sessions.context` lowers it.
    #[serde(default, skip_serializing_if = "ContextAsk::is_as_it_is")]
    pub context: ContextAsk,
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
    /// [`SessionSpec::isolated`](crate::session_runner::SessionSpec::isolated).
    #[serde(default)]
    pub isolated: bool,
    /// More of the CLI's own plugins an isolated session turns off, by name: built-ins a later
    /// version added, learned when an earlier isolated session loaded them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub builtins_off: Vec<String>,
    /// The CLI's own sandbox stays off and the commands it runs need no approval. Our sandbox
    /// holds the session (macOS won't start one sandbox inside another), or `sandbox.mode =
    /// "none"` asked for no protection at all.
    #[serde(default)]
    pub tool_sandbox_off: bool,
    /// A direct session's asking. For Claude Code anything but `Never` needs the approval tool
    /// in `mcp_config`.
    #[serde(default)]
    pub asks: Asks,
    /// One of the CLI's own commands, which must reach it as a command (agy's slash commands).
    #[serde(default)]
    pub cli_command: bool,
    /// Your conversation's own session. Claude Code's opens with `initialize`, which brings
    /// its commands and models, so messages you send later can join it as in its own screen.
    #[serde(default)]
    pub main_session: bool,
    /// The CLI's own ultracode for the whole session, where it has one (Claude Code's
    /// `ultracode` setting): a workflow on every real task.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ultracode: bool,
    /// The prompt goes out marked as typed by you, so a keyword in it, such as `ultracode`,
    /// works as it does when you type it into the CLI itself.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub typed_by_you: bool,
    /// No workflow or background agent of the CLI's own: its tools for them are left out and
    /// switched off. See [`CliWorkflow::background_off`](crate::session_runner::CliWorkflow).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub background_off: bool,
    /// What a read-only session may run in a shell.
    #[serde(default, skip_serializing_if = "SessionShell::is_none")]
    pub shell: SessionShell,
    /// How big a workflow of the CLI's own may be, where it takes a guideline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_size: Option<WorkflowSize>,
    /// A few lines for the end of the session's instructions, given the way its CLI takes such
    /// a note (Claude Code `--append-system-prompt`). An adapter with no such way leaves it to
    /// the caller, which puts the lines in the prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standing_note: Option<String>,
    /// See [`SessionSpec::autopilot`](crate::session_runner::SessionSpec::autopilot).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub autopilot: bool,
    /// See [`SessionSpec::preferences`](crate::session_runner::SessionSpec::preferences).
    #[serde(default, skip_serializing_if = "SessionPreferences::is_default")]
    pub preferences: SessionPreferences,
    /// The installed CLI's version, as its adapter parses it, for an adapter whose flags
    /// depend on it ([`Adapter::launch_reads_version`](crate::Adapter::launch_reads_version)).
    /// `None` when it wasn't read or couldn't be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_version: Option<String>,
    /// What the CLI said of its own config in the session's folder, as
    /// [`Adapter::config_query`](crate::Adapter::config_query) asked it. `None` when it wasn't
    /// asked or didn't answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_config: Option<String>,
    /// In tokens: the window the model's server holds it to, as the registry's
    /// [`ModelEntry::window`](crate::registry::ModelEntry::window) says. `None` until a server
    /// has said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
}

/// Where a report goes, for a model that would hand it to a tool.
pub const REPORT_IN_WORDS: &str =
    "Write the report as the text of your final message. A tool call never ends your turn.";

/// A file Argonauts Code writes before spawning the command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecFile {
    /// Parent directories are created.
    pub path: PathBuf,
    pub contents: String,
}

impl SessionRequest {
    /// A request to open `session_id` again in `workdir`, as a take-over does, with the CLI's
    /// defaults for the rest.
    pub fn reopening(session_id: &str, workdir: &Path) -> Self {
        Self {
            workdir: workdir.to_path_buf(),
            resume_session: Some(session_id.to_owned()),
            ..Self::default()
        }
    }

    /// The prompt, ending with the report's schema when the session has one. For a CLI with no
    /// flag that holds a run to a schema, the model learns the report's shape only from this:
    /// without it, a live gemma4 session on OpenCode wrote its report as plain lines. A live
    /// Qwen session on OpenCode 2 handed it to a tool instead and kept calling it, since a tool
    /// call doesn't end a turn, so the prompt says where the report goes.
    pub fn prompt_with_schema(&self) -> String {
        let Some(schema) = &self.json_schema else {
            return self.prompt.clone();
        };
        let schema = serde_json::to_string_pretty(schema).unwrap_or_else(|_| schema.to_string());
        format!(
            "{}\n\nEnd your final message with your report: one JSON object that matches this \
             schema. {REPORT_IN_WORDS}\n\n```json\n{schema}\n```\n",
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
pub const PROVIDER_KEY_ENV_VARS: &[&str] = &[
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

/// Every variable from [`PROVIDER_KEY_ENV_VARS`] except those in `keep`, plus all of
/// [`PARENT_SESSION_ENV_VARS`].
pub fn env_remove_except(keep: &[&str]) -> Vec<String> {
    PROVIDER_KEY_ENV_VARS
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
    fn the_prompt_says_the_report_is_the_final_message_never_a_tool_call() {
        let plain = SessionRequest {
            prompt: "Fix slugify.\n".into(),
            ..SessionRequest::default()
        };
        assert_eq!(plain.prompt_with_schema(), "Fix slugify.\n");
        let asked = SessionRequest {
            json_schema: Some(serde_json::json!({"type": "object"})),
            ..plain
        };
        let prompt = asked.prompt_with_schema();
        assert!(prompt.starts_with(
            "Fix slugify.\n\nEnd your final message with your report: one JSON object that \
             matches this schema. Write the report as the text of your final message. A tool \
             call never ends your turn.\n\n```json\n"
        ));
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
            "auth":"subscription","json_schema":null,"resume_session":null,"spend_cap_usd":null,
            "extra_args":[],"scratch_dir":null,"goal_hash":"g"}"#;
        let req: SessionRequest = serde_json::from_str(old).unwrap();
        assert_eq!(req.role, SessionRole::Build);
        assert!(!req.keep_input_open);
        let full = SessionRequest {
            keep_input_open: true,
            context: ContextAsk::Largest {
                known: Some(872_000),
            },
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
        let all = PROVIDER_KEY_ENV_VARS.len() + PARENT_SESSION_ENV_VARS.len();
        assert_eq!(removed.len(), all - 1);
        assert_eq!(env_remove_except(&[]).len(), all);
    }
}
