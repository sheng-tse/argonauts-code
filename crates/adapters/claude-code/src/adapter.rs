use std::time::Duration;

use anyhow::ensure;
use argonauts_core::{
    Adapter, AuthMode, Capabilities, CommandSpec, SessionRequest, SessionRole, StopPlan,
    StreamParser, env_remove_except, extract_version,
};

use crate::parser::ClaudeStreamParser;

pub const TESTED_VERSIONS: &[&str] = &["2.1.284"];

const READ_ONLY_TOOLS: &str = "Read,Grep,Glob";
const BUILD_TOOLS: &str = "Read,Grep,Glob,Edit,Write,Bash";

/// An unattended session must not take instructions from the user's other Claude Code sessions,
/// which non-bypass `-p` sessions otherwise accept at once.
const SESSION_SETTINGS: &str = r#"{"crossSessionInbound":"refuse"}"#;

/// Only the user's own settings load. `-p` skips the trust dialog, so a repository's
/// `.claude/settings.json` would otherwise run its hooks, including ones an agent wrote.
const SETTING_SOURCES: &str = "user";

/// The key a subscription login may come as in headless setups, from `claude setup-token`.
const SUBSCRIPTION_KEYS: &[&str] = &["CLAUDE_CODE_OAUTH_TOKEN"];

const API_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
];

#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeCodeAdapter;

impl ClaudeCodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Adapter for ClaudeCodeAdapter {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn binary(&self) -> &'static str {
        "claude"
    }

    fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
        build_command(self.binary(), req)
    }

    fn parser(&self) -> Box<dyn StreamParser> {
        Box::new(ClaudeStreamParser::new())
    }

    fn version_args(&self) -> &'static [&'static str] {
        &["--version"]
    }

    fn parse_version(&self, output: &str) -> Option<String> {
        extract_version(output)
    }

    fn tested_versions(&self) -> &'static [&'static str] {
        TESTED_VERSIONS
    }

    fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
        ("haiku", Some("low"))
    }

    fn stop_plan(&self) -> StopPlan {
        // SIGINT ends the turn cleanly. SIGTERM exits with 143 and kills running Bash commands.
        StopPlan::escalating(Duration::from_secs(5), Duration::from_secs(5))
    }

    fn capabilities(&self) -> Capabilities {
        // The result carries total_cost_usd, and after --resume it covers the whole conversation.
        Capabilities {
            reports_cost: true,
            ..Capabilities::default()
        }
    }
}

/// The prompt goes on stdin so it never looks like a flag.
fn build_command(program: &str, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
    ensure!(
        !req.workdir.as_os_str().is_empty(),
        "the session needs a working directory"
    );
    ensure!(!req.prompt.trim().is_empty(), "the prompt is empty");

    let mut spec = with_request_flags(headless_command(program, req), req)?
        .args(req.extra_args.iter().cloned());
    spec.env_remove = removed_env_vars(req);
    // Auto memory is shared by every worktree of a repository, so it would leak between cards.
    spec.env_set = vec![("CLAUDE_CODE_DISABLE_AUTO_MEMORY".to_owned(), "1".to_owned())];
    spec.stdin = Some(req.prompt.clone());
    Ok(spec)
}

fn headless_command(program: &str, req: &SessionRequest) -> CommandSpec {
    let (permission_mode, tools) = match req.role {
        SessionRole::ReadOnly => ("dontAsk", READ_ONLY_TOOLS),
        SessionRole::Build => ("acceptEdits", BUILD_TOOLS),
    };
    CommandSpec::new(program, &req.workdir)
        .args([
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
        ])
        .args(["--permission-mode", permission_mode])
        .args(["--permission-prompts", "none"])
        .args(["--tools", tools])
        // --tools doesn't cover MCP tools, so user and project MCP servers are left out.
        .arg("--strict-mcp-config")
        .args(["--setting-sources", SETTING_SOURCES])
        .args(["--settings", SESSION_SETTINGS])
}

fn with_request_flags(mut spec: CommandSpec, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
    if let Some(model) = &req.model {
        spec = spec.args(["--model", flag_value("model", model)?]);
    }
    if let Some(effort) = &req.effort {
        spec = spec.args(["--effort", flag_value("effort", effort)?]);
    }
    if let Some(schema) = &req.json_schema {
        ensure!(schema.is_object(), "the JSON schema must be an object");
        spec = spec.args(["--json-schema".to_owned(), schema.to_string()]);
    }
    if let Some(budget) = req.max_budget_usd {
        ensure!(
            budget.is_finite() && budget > 0.0,
            "the Claude Code budget must be a positive number of dollars, not {budget}"
        );
        spec = spec.args(["--max-budget-usd".to_owned(), budget.to_string()]);
    }
    if let Some(session) = &req.resume_session {
        spec = spec.args(["--resume", flag_value("session id", session)?]);
    }
    Ok(spec)
}

fn flag_value<'a>(what: &str, value: &'a str) -> anyhow::Result<&'a str> {
    ensure!(!value.trim().is_empty(), "the {what} is empty");
    ensure!(
        !value.starts_with('-'),
        "the {what} can't start with '-': {value}"
    );
    Ok(value)
}

fn removed_env_vars(req: &SessionRequest) -> Vec<String> {
    // With -p an API key or auth token wins over the login, so under a subscription both go.
    let mut vars = match req.auth {
        AuthMode::Subscription => env_remove_except(SUBSCRIPTION_KEYS),
        AuthMode::ApiKey => env_remove_except(API_KEYS),
    };
    if req.effort.is_some() {
        // The environment variable outranks --effort.
        vars.push("CLAUDE_CODE_EFFORT_LEVEL".to_owned());
    }
    vars
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::{VENDOR_KEY_ENV_VARS, report_json_schema};
    use serde_json::json;
    use std::path::PathBuf;

    fn request(role: SessionRole) -> SessionRequest {
        SessionRequest {
            workdir: PathBuf::from("/tmp/worktree"),
            prompt: "Fix the bug".into(),
            role,
            ..SessionRequest::default()
        }
    }

    fn value_after<'a>(spec: &'a CommandSpec, flag: &str) -> Option<&'a str> {
        let index = spec.args.iter().position(|a| a == flag)?;
        spec.args.get(index + 1).map(String::as_str)
    }

    fn removes(spec: &CommandSpec, var: &str) -> bool {
        spec.env_remove.iter().any(|v| v == var)
    }

    #[test]
    fn identity_and_versions() {
        let adapter = ClaudeCodeAdapter::new();
        assert_eq!(adapter.id(), "claude-code");
        assert_eq!(adapter.binary(), "claude");
        assert_eq!(adapter.display_name(), "Claude Code");
        assert_eq!(adapter.version_args(), ["--version"]);
        assert_eq!(
            adapter.parse_version("2.1.284 (Claude Code)\n").as_deref(),
            Some("2.1.284")
        );
        assert_eq!(adapter.parse_version("no version here"), None);
        assert_eq!(adapter.tested_versions(), ["2.1.284"]);
        assert_eq!(adapter.cheapest_model(), ("haiku", Some("low")));
        assert!(adapter.wrap_up_input("wrap up").is_none());
        assert_eq!(adapter.stop_plan(), StopPlan::default());
        let caps = adapter.capabilities();
        assert!(caps.reports_cost && caps.live_usage && caps.resume_totals_include_earlier);
    }

    #[test]
    fn read_only_sessions_use_read_tools_and_dont_ask() {
        let spec = ClaudeCodeAdapter
            .command(&request(SessionRole::ReadOnly))
            .unwrap();
        assert_eq!(spec.program, "claude");
        assert_eq!(spec.cwd, PathBuf::from("/tmp/worktree"));
        assert_eq!(
            spec.args,
            [
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--permission-mode",
                "dontAsk",
                "--permission-prompts",
                "none",
                "--tools",
                "Read,Grep,Glob",
                "--strict-mcp-config",
                "--setting-sources",
                "user",
                "--settings",
                r#"{"crossSessionInbound":"refuse"}"#,
            ]
        );
        assert_eq!(spec.stdin.as_deref(), Some("Fix the bug"));
        assert!(!spec.keep_stdin_open);
        assert!(spec.files.is_empty());
    }

    #[test]
    fn build_sessions_accept_edits() {
        let spec = ClaudeCodeAdapter
            .command(&request(SessionRole::Build))
            .unwrap();
        assert_eq!(value_after(&spec, "--permission-mode"), Some("acceptEdits"));
        assert_eq!(
            value_after(&spec, "--tools"),
            Some("Read,Grep,Glob,Edit,Write,Bash")
        );
    }

    #[test]
    fn cheapest_model_schema_budget_and_resume() {
        let adapter = ClaudeCodeAdapter;
        let (model, effort) = adapter.cheapest_model();
        let schema = report_json_schema();
        let req = SessionRequest {
            model: Some(model.into()),
            effort: effort.map(Into::into),
            json_schema: Some(schema.clone()),
            max_budget_usd: Some(0.5),
            resume_session: Some("0b5f6c1e-aaaa-bbbb-cccc-123456789abc".into()),
            extra_args: vec!["--add-dir".into(), "/tmp/extra".into()],
            ..request(SessionRole::Build)
        };
        let spec = adapter.command(&req).unwrap();
        assert_eq!(value_after(&spec, "--model"), Some("haiku"));
        assert_eq!(value_after(&spec, "--effort"), Some("low"));
        assert_eq!(value_after(&spec, "--max-budget-usd"), Some("0.5"));
        assert_eq!(
            value_after(&spec, "--resume"),
            Some("0b5f6c1e-aaaa-bbbb-cccc-123456789abc")
        );
        let passed: serde_json::Value =
            serde_json::from_str(value_after(&spec, "--json-schema").unwrap()).unwrap();
        assert_eq!(passed, schema);
        assert_eq!(
            spec.args[spec.args.len() - 2..],
            ["--add-dir", "/tmp/extra"]
        );
        assert!(removes(&spec, "CLAUDE_CODE_EFFORT_LEVEL"));
    }

    #[test]
    fn whole_dollar_budgets_print_plainly() {
        let req = SessionRequest {
            max_budget_usd: Some(3.0),
            ..request(SessionRole::Build)
        };
        let spec = ClaudeCodeAdapter.command(&req).unwrap();
        assert_eq!(value_after(&spec, "--max-budget-usd"), Some("3"));
    }

    #[test]
    fn subscription_auth_removes_every_key() {
        let spec = ClaudeCodeAdapter
            .command(&request(SessionRole::Build))
            .unwrap();
        for var in VENDOR_KEY_ENV_VARS {
            assert_eq!(
                removes(&spec, var),
                *var != "CLAUDE_CODE_OAUTH_TOKEN",
                "{var}"
            );
        }
        assert!(removes(&spec, "ANTHROPIC_AUTH_TOKEN"));
        assert!(removes(&spec, "CLAUDE_CODE_MESSAGING_TOKEN"));
        assert!(!removes(&spec, "CLAUDE_CODE_EFFORT_LEVEL"));
        assert_eq!(
            spec.env_set,
            [("CLAUDE_CODE_DISABLE_AUTO_MEMORY".to_owned(), "1".to_owned())]
        );
    }

    #[test]
    fn api_key_auth_keeps_only_the_anthropic_key() {
        let req = SessionRequest {
            auth: AuthMode::ApiKey,
            ..request(SessionRole::Build)
        };
        let spec = ClaudeCodeAdapter.command(&req).unwrap();
        assert!(!removes(&spec, "ANTHROPIC_API_KEY"));
        assert!(!removes(&spec, "ANTHROPIC_AUTH_TOKEN"));
        for var in [
            "CLAUDE_CODE_OAUTH_TOKEN",
            "OPENAI_API_KEY",
            "CODEX_ACCESS_TOKEN",
            "GITHUB_TOKEN",
            "CLAUDECODE",
        ] {
            assert!(removes(&spec, var), "{var}");
        }
    }

    #[test]
    fn bad_requests_are_rejected() {
        let adapter = ClaudeCodeAdapter;
        let cases = [
            SessionRequest {
                prompt: "  ".into(),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                workdir: PathBuf::new(),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                model: Some("--dangerously-skip-permissions".into()),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                effort: Some(String::new()),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                resume_session: Some("-x".into()),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                json_schema: Some(json!(["not", "an", "object"])),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                max_budget_usd: Some(0.0),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                max_budget_usd: Some(f64::NAN),
                ..request(SessionRole::Build)
            },
        ];
        for req in cases {
            assert!(adapter.command(&req).is_err(), "{req:?}");
        }
    }

    #[test]
    fn prompts_that_look_like_flags_stay_on_stdin() {
        let req = SessionRequest {
            prompt: "--help me".into(),
            ..request(SessionRole::ReadOnly)
        };
        let spec = ClaudeCodeAdapter.command(&req).unwrap();
        assert!(!spec.args.iter().any(|a| a.contains("help me")));
        assert_eq!(spec.stdin.as_deref(), Some("--help me"));
    }
}
