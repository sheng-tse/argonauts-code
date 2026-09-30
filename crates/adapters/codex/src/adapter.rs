use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};
use argonauts_core::{
    Adapter, AuthMode, Capabilities, CommandSpec, SessionRequest, SessionRole, SpecFile, StopPlan,
    StreamParser, env_remove_except, extract_version,
};
use serde_json::Value;

use crate::parser::CodexParser;

pub const ID: &str = "codex";

pub const CHEAPEST_MODEL: &str = "gpt-5.6-luna";

pub const FALLBACK_CHEAPEST_MODEL: &str = "gpt-6-luna";

pub const CHEAPEST_EFFORT: &str = "low";

pub const OUTPUT_SCHEMA_FILE: &str = "codex-output-schema.json";

pub const TESTED_VERSIONS: &[&str] = &["0.158.0"];

pub const API_KEY_ENV_VARS: &[&str] = &["CODEX_API_KEY", "OPENAI_API_KEY", "AZURE_OPENAI_API_KEY"];

/// A ChatGPT workspace access token, which signs Codex in to the user's plan.
pub const SUBSCRIPTION_ENV_VARS: &[&str] = &["CODEX_ACCESS_TOKEN"];

/// `extra_args` go before the `resume` subcommand, where `codex exec` accepts its own flags.
#[derive(Debug, Clone, Copy, Default)]
pub struct CodexAdapter;

impl CodexAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Adapter for CodexAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn binary(&self) -> &'static str {
        "codex"
    }

    fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
        build_command(self.binary(), req)
    }

    fn parser(&self) -> Box<dyn StreamParser> {
        Box::new(CodexParser::new())
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
        (CHEAPEST_MODEL, Some(CHEAPEST_EFFORT))
    }

    fn stop_plan(&self) -> StopPlan {
        // Codex turns SIGINT into a clean turn interrupt. It has no SIGTERM handler.
        StopPlan::default()
    }

    fn capabilities(&self) -> Capabilities {
        // Usage comes only with turn.completed, and thread.started doesn't name the model.
        Capabilities {
            live_usage: false,
            reports_model: false,
            ..Capabilities::default()
        }
    }
}

fn build_command(program: &str, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
    ensure!(
        !req.workdir.as_os_str().is_empty(),
        "a Codex session needs a working directory"
    );
    ensure!(
        !req.prompt.trim().is_empty(),
        "a Codex session needs a prompt"
    );

    // Always pass --sandbox, because without it Codex picks one from the user's trust settings.
    let mut spec = CommandSpec::new(program, &req.workdir)
        .args(["exec", "--json", "--color", "never", "--cd"])
        .arg(utf8(&req.workdir)?)
        .args(["--sandbox", sandbox_mode(req.role), "--skip-git-repo-check"]);

    if let Some(model) = present(req.model.as_deref()) {
        spec = spec.args(["--model", flag_value("model", model)?]);
    }
    if let Some(effort) = present(req.effort.as_deref()) {
        spec = spec.args(["--config", &reasoning_effort_override(effort)?]);
    }
    if let Some(schema) = &req.json_schema {
        let file = schema_file(req, schema)?;
        spec = spec.args(["--output-schema", utf8(&file.path)?]);
        spec.files.push(file);
    }
    spec = spec.args(req.extra_args.iter().cloned());
    if let Some(session) = &req.resume_session {
        spec = spec.args(["resume", flag_value("session id", session.trim())?]);
    }

    // `-` makes Codex read the prompt from stdin, which keeps it out of the process list.
    spec = spec.arg("-");
    spec.stdin = Some(req.prompt.clone());
    spec.env_remove = env_remove_except(kept_key_vars(req.auth));
    Ok(spec)
}

fn sandbox_mode(role: SessionRole) -> &'static str {
    match role {
        SessionRole::ReadOnly => "read-only",
        SessionRole::Build => "workspace-write",
    }
}

fn kept_key_vars(auth: AuthMode) -> &'static [&'static str] {
    match auth {
        AuthMode::Subscription => SUBSCRIPTION_ENV_VARS,
        AuthMode::ApiKey => API_KEY_ENV_VARS,
    }
}

fn present(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

fn flag_value<'a>(what: &str, value: &'a str) -> anyhow::Result<&'a str> {
    ensure!(!value.is_empty(), "the Codex {what} is empty");
    ensure!(
        !value.starts_with('-'),
        "the Codex {what} {value:?} starts with '-'"
    );
    Ok(value)
}

/// `model_reasoning_effort="low"`, with the value checked so it stays a plain TOML string.
fn reasoning_effort_override(effort: &str) -> anyhow::Result<String> {
    ensure!(
        effort
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "unsupported Codex reasoning effort {effort:?}"
    );
    Ok(format!("model_reasoning_effort=\"{effort}\""))
}

/// Kept in the scratch directory so it never ends up in the worktree, where the conductor
/// would commit it.
fn schema_file(req: &SessionRequest, schema: &Value) -> anyhow::Result<SpecFile> {
    let scratch = req
        .scratch_dir
        .as_deref()
        .context("Codex needs a scratch directory to hold the --output-schema file")?;
    ensure!(
        !scratch.starts_with(&req.workdir),
        "the scratch directory {} is inside the worktree",
        scratch.display()
    );
    let mut contents = serde_json::to_string_pretty(schema).context("serializing the schema")?;
    contents.push('\n');
    Ok(SpecFile {
        path: output_schema_path(scratch),
        contents,
    })
}

fn utf8(path: &Path) -> anyhow::Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn output_schema_path(scratch_dir: &Path) -> PathBuf {
    scratch_dir.join(OUTPUT_SCHEMA_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::{VENDOR_KEY_ENV_VARS, report_json_schema};

    fn request(role: SessionRole) -> SessionRequest {
        SessionRequest {
            workdir: PathBuf::from("/work/tree"),
            prompt: "Create hello.txt".into(),
            role,
            ..SessionRequest::default()
        }
    }

    fn command(req: &SessionRequest) -> CommandSpec {
        CodexAdapter.command(req).unwrap()
    }

    fn position(args: &[String], needle: &str) -> usize {
        args.iter()
            .position(|a| a == needle)
            .unwrap_or_else(|| panic!("{needle} missing from {args:?}"))
    }

    #[test]
    fn read_only_role_uses_read_only_sandbox() {
        let spec = command(&request(SessionRole::ReadOnly));
        assert_eq!(spec.program, "codex");
        assert_eq!(
            spec.args,
            [
                "exec",
                "--json",
                "--color",
                "never",
                "--cd",
                "/work/tree",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
                "-",
            ]
        );
        assert_eq!(spec.cwd, PathBuf::from("/work/tree"));
        assert_eq!(spec.stdin.as_deref(), Some("Create hello.txt"));
        assert!(!spec.keep_stdin_open);
        assert!(spec.files.is_empty());
    }

    #[test]
    fn build_role_uses_workspace_write_sandbox() {
        let spec = command(&request(SessionRole::Build));
        let at = position(&spec.args, "--sandbox");
        assert_eq!(spec.args[at + 1], "workspace-write");
        assert!(!spec.args.iter().any(|a| a == "-a" || a == "--full-auto"));
    }

    #[test]
    fn cheapest_model_and_effort_become_flags() {
        let (model, effort) = CodexAdapter.cheapest_model();
        assert_eq!((model, effort), ("gpt-5.6-luna", Some("low")));
        let req = SessionRequest {
            model: Some(model.into()),
            effort: effort.map(Into::into),
            ..request(SessionRole::ReadOnly)
        };
        let spec = command(&req);
        let at = position(&spec.args, "--model");
        assert_eq!(spec.args[at + 1], "gpt-5.6-luna");
        let at = position(&spec.args, "--config");
        assert_eq!(spec.args[at + 1], "model_reasoning_effort=\"low\"");
        assert_eq!(spec.args.last().map(String::as_str), Some("-"));
    }

    #[test]
    fn blank_model_and_effort_are_left_to_codex() {
        let req = SessionRequest {
            model: Some("  ".into()),
            effort: Some(String::new()),
            ..request(SessionRole::ReadOnly)
        };
        let spec = command(&req);
        assert!(!spec.args.iter().any(|a| a == "--model" || a == "--config"));
    }

    #[test]
    fn odd_effort_and_flag_like_values_are_rejected() {
        let bad_effort = SessionRequest {
            effort: Some("low\" --x=\"".into()),
            ..request(SessionRole::ReadOnly)
        };
        assert!(CodexAdapter.command(&bad_effort).is_err());
        let bad_model = SessionRequest {
            model: Some("--dangerously-bypass-approvals-and-sandbox".into()),
            ..request(SessionRole::ReadOnly)
        };
        assert!(CodexAdapter.command(&bad_model).is_err());
        let bad_resume = SessionRequest {
            resume_session: Some("--last".into()),
            ..request(SessionRole::ReadOnly)
        };
        assert!(CodexAdapter.command(&bad_resume).is_err());
    }

    #[test]
    fn empty_prompt_or_workdir_is_an_error() {
        let no_prompt = SessionRequest {
            prompt: " \n".into(),
            ..request(SessionRole::Build)
        };
        assert!(CodexAdapter.command(&no_prompt).is_err());
        let no_workdir = SessionRequest {
            workdir: PathBuf::new(),
            ..request(SessionRole::Build)
        };
        assert!(CodexAdapter.command(&no_workdir).is_err());
    }

    #[test]
    fn schema_is_written_to_the_scratch_dir() {
        let req = SessionRequest {
            json_schema: Some(report_json_schema()),
            scratch_dir: Some(PathBuf::from("/state/cards/card-001")),
            ..request(SessionRole::Build)
        };
        let spec = command(&req);
        let path = output_schema_path(Path::new("/state/cards/card-001"));
        let at = position(&spec.args, "--output-schema");
        assert_eq!(spec.args[at + 1], path.to_str().unwrap());
        assert_eq!(spec.files.len(), 1);
        assert_eq!(spec.files[0].path, path);
        let written: Value = serde_json::from_str(&spec.files[0].contents).unwrap();
        assert_eq!(written, report_json_schema());
    }

    #[test]
    fn schema_needs_a_scratch_dir_outside_the_worktree() {
        let missing = SessionRequest {
            json_schema: Some(report_json_schema()),
            ..request(SessionRole::Build)
        };
        assert!(CodexAdapter.command(&missing).is_err());
        let inside = SessionRequest {
            json_schema: Some(report_json_schema()),
            scratch_dir: Some(PathBuf::from("/work/tree/.scratch")),
            ..request(SessionRole::Build)
        };
        assert!(CodexAdapter.command(&inside).is_err());
    }

    #[test]
    fn resume_puts_exec_flags_before_the_subcommand() {
        let req = SessionRequest {
            resume_session: Some("0199a213-81c0-7800-8aa1-bbab2a035a53".into()),
            model: Some("gpt-5.6-luna".into()),
            json_schema: Some(report_json_schema()),
            scratch_dir: Some(PathBuf::from("/state/card")),
            extra_args: vec!["--add-dir".into(), "/extra".into()],
            ..request(SessionRole::Build)
        };
        let spec = command(&req);
        let resume = position(&spec.args, "resume");
        for flag in [
            "--cd",
            "--sandbox",
            "--model",
            "--output-schema",
            "--add-dir",
        ] {
            assert!(position(&spec.args, flag) < resume, "{flag} after resume");
        }
        assert_eq!(
            &spec.args[resume..],
            ["resume", "0199a213-81c0-7800-8aa1-bbab2a035a53", "-"]
        );
    }

    #[test]
    fn subscription_mode_removes_every_key_variable() {
        let spec = command(&request(SessionRole::ReadOnly));
        for var in VENDOR_KEY_ENV_VARS {
            let removed = spec.env_remove.iter().any(|v| v == var);
            assert_eq!(removed, *var != "CODEX_ACCESS_TOKEN", "{var}");
        }
        assert!(
            spec.env_remove
                .iter()
                .any(|v| v == "CLAUDE_CODE_MESSAGING_TOKEN")
        );
        assert!(spec.env_set.is_empty());
    }

    #[test]
    fn api_key_mode_keeps_only_openai_keys() {
        let req = SessionRequest {
            auth: AuthMode::ApiKey,
            ..request(SessionRole::ReadOnly)
        };
        let spec = command(&req);
        for kept in API_KEY_ENV_VARS {
            assert!(!spec.env_remove.iter().any(|v| v == kept), "{kept} removed");
        }
        assert!(spec.env_remove.iter().any(|v| v == "ANTHROPIC_API_KEY"));
        assert!(spec.env_remove.iter().any(|v| v == "GEMINI_API_KEY"));
        assert!(spec.env_remove.iter().any(|v| v == "CODEX_ACCESS_TOKEN"));
        for var in VENDOR_KEY_ENV_VARS {
            let removed = spec.env_remove.iter().any(|v| v == var);
            assert_eq!(removed, !API_KEY_ENV_VARS.contains(var), "{var}");
        }
    }

    #[test]
    fn prompt_never_appears_in_args() {
        let req = SessionRequest {
            prompt: "-rf secret plan".into(),
            ..request(SessionRole::Build)
        };
        let spec = command(&req);
        assert!(!spec.args.iter().any(|a| a.contains("secret plan")));
        assert_eq!(spec.stdin.as_deref(), Some("-rf secret plan"));
    }

    #[test]
    fn identity_and_versions() {
        let adapter = CodexAdapter::new();
        assert_eq!(adapter.id(), "codex");
        assert_eq!(adapter.binary(), "codex");
        assert_eq!(adapter.display_name(), "Codex");
        assert_eq!(adapter.version_args(), ["--version"]);
        assert_eq!(
            adapter.parse_version("codex-cli 0.158.0\n").as_deref(),
            Some("0.158.0")
        );
        assert_eq!(adapter.parse_version("codex-cli"), None);
        assert_eq!(adapter.tested_versions(), ["0.158.0"]);
        assert!(adapter.wrap_up_input("wrap up").is_none());
        assert_eq!(adapter.stop_plan(), StopPlan::default());
        let caps = adapter.capabilities();
        assert!(!caps.live_usage && !caps.reports_model && !caps.reports_cost);
    }
}
