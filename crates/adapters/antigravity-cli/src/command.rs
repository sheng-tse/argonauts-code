use anyhow::{Context, bail};
use argonauts_core::{AuthMode, CommandSpec, SessionRequest, SessionRole, env_remove_except};

/// Turns off agy's background self-updater, so a run uses the version `doctor` checked.
pub const AUTO_UPDATE_ENV: &str = "AGY_CLI_DISABLE_AUTO_UPDATE";

/// agy 1.2.13 ignores `1` and still spawns its updater.
pub const AUTO_UPDATE_VALUE: &str = "true";

/// The only API key variable agy reads. It ignores `GOOGLE_API_KEY`.
pub const API_KEY_ENV: &str = "GEMINI_API_KEY";

/// The prompt is the value of `-p`, so other local users can see it in the process list.
pub fn build_command(program: &str, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
    if req.prompt.trim().is_empty() {
        bail!("agy needs a non-empty prompt");
    }
    // Both roles run sandboxed without skipping permissions, so agy soft-denies shell commands.
    let mut spec = CommandSpec::new(program, &req.workdir)
        .args(["-p", req.prompt.as_str()])
        .args(["--output-format", "stream-json"])
        .arg("--sandbox");
    spec = match req.role {
        // Plan mode is only advisory, and it needs slash commands left on.
        SessionRole::ReadOnly => spec.args(["--mode", "plan"]),
        SessionRole::Build => spec
            .args(["--mode", "accept-edits"])
            .arg("--disable-slash-commands"),
    };

    if let Some(model) = &req.model {
        spec = spec.args(["--model", flag_value("model", model)?]);
    }
    if let Some(effort) = &req.effort {
        spec = spec.args(["--effort", flag_value("effort", effort)?]);
    }
    if let Some(schema) = &req.json_schema {
        let schema = serde_json::to_string(schema).context("serializing the JSON schema")?;
        spec = spec.args(["--json-schema".to_owned(), schema]);
    }
    if let Some(conversation) = &req.resume_session {
        spec = spec.args([
            "--conversation",
            flag_value("conversation id", conversation)?,
        ]);
    }
    // `max_budget_usd` is not passed on, because agy has no spend cap.
    spec = spec.args(&req.extra_args);

    spec.env_set = vec![(AUTO_UPDATE_ENV.to_owned(), AUTO_UPDATE_VALUE.to_owned())];
    spec.env_remove = match req.auth {
        AuthMode::Subscription => env_remove_except(&[]),
        AuthMode::ApiKey => env_remove_except(&[API_KEY_ENV]),
    };
    Ok(spec)
}

fn flag_value<'a>(what: &str, value: &'a str) -> anyhow::Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        bail!("invalid agy {what}: {value:?}");
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::report_json_schema;
    use std::path::PathBuf;

    fn request(role: SessionRole) -> SessionRequest {
        SessionRequest {
            workdir: PathBuf::from("/work/card-001"),
            prompt: "Create hello.txt".into(),
            role,
            ..SessionRequest::default()
        }
    }

    fn value_after<'a>(spec: &'a CommandSpec, flag: &str) -> Option<&'a str> {
        let at = spec.args.iter().position(|a| a == flag)?;
        spec.args.get(at + 1).map(String::as_str)
    }

    fn has_arg(spec: &CommandSpec, arg: &str) -> bool {
        spec.args.iter().any(|a| a == arg)
    }

    #[test]
    fn prompt_is_the_value_of_p_and_output_is_stream_json() {
        let spec = build_command("agy", &request(SessionRole::Build)).unwrap();
        assert_eq!(spec.program, "agy");
        assert_eq!(spec.cwd, PathBuf::from("/work/card-001"));
        assert_eq!(spec.args[..2], ["-p", "Create hello.txt"]);
        assert_eq!(value_after(&spec, "--output-format"), Some("stream-json"));
        assert!(has_arg(&spec, "--sandbox"));
        assert!(spec.stdin.is_none());
        assert!(!spec.keep_stdin_open);
        assert!(spec.files.is_empty());
    }

    #[test]
    fn roles_pick_the_mode() {
        let build = build_command("agy", &request(SessionRole::Build)).unwrap();
        assert_eq!(value_after(&build, "--mode"), Some("accept-edits"));
        assert!(has_arg(&build, "--disable-slash-commands"));
        let read_only = build_command("agy", &request(SessionRole::ReadOnly)).unwrap();
        assert_eq!(value_after(&read_only, "--mode"), Some("plan"));
        assert!(
            !has_arg(&read_only, "--disable-slash-commands"),
            "plan mode needs slash command expansion"
        );
        for spec in [build, read_only] {
            assert!(!has_arg(&spec, "--dangerously-skip-permissions"));
        }
    }

    #[test]
    fn cheapest_model_and_effort_are_passed() {
        let req = SessionRequest {
            model: Some("gemini-3.6-flash-low".into()),
            effort: Some("low".into()),
            ..request(SessionRole::ReadOnly)
        };
        let spec = build_command("agy", &req).unwrap();
        assert_eq!(value_after(&spec, "--model"), Some("gemini-3.6-flash-low"));
        assert_eq!(value_after(&spec, "--effort"), Some("low"));
    }

    #[test]
    fn defaults_leave_model_effort_schema_and_resume_out() {
        let spec = build_command("agy", &request(SessionRole::Build)).unwrap();
        for flag in ["--model", "--effort", "--json-schema", "--conversation"] {
            assert!(!has_arg(&spec, flag), "{flag}");
        }
    }

    #[test]
    fn schema_is_passed_inline_as_compact_json() {
        let req = SessionRequest {
            json_schema: Some(report_json_schema()),
            ..request(SessionRole::Build)
        };
        let spec = build_command("agy", &req).unwrap();
        let schema = value_after(&spec, "--json-schema").unwrap();
        assert!(!schema.contains('\n'));
        let parsed: serde_json::Value = serde_json::from_str(schema).unwrap();
        assert_eq!(parsed, report_json_schema());
    }

    #[test]
    fn resume_uses_conversation_not_continue() {
        let req = SessionRequest {
            resume_session: Some("055a398f-db14-4c5f-abbb-1bf03f8120a7".into()),
            ..request(SessionRole::Build)
        };
        let spec = build_command("agy", &req).unwrap();
        assert_eq!(
            value_after(&spec, "--conversation"),
            Some("055a398f-db14-4c5f-abbb-1bf03f8120a7")
        );
        assert!(!has_arg(&spec, "-c") && !has_arg(&spec, "--continue"));
    }

    #[test]
    fn extra_args_come_last() {
        let req = SessionRequest {
            extra_args: vec!["--print-timeout".into(), "10m".into()],
            ..request(SessionRole::Build)
        };
        let spec = build_command("agy", &req).unwrap();
        assert_eq!(spec.args[spec.args.len() - 2..], ["--print-timeout", "10m"]);
    }

    #[test]
    fn subscription_removes_every_key_and_api_key_mode_keeps_gemini() {
        let spec = build_command("agy", &request(SessionRole::Build)).unwrap();
        assert!(spec.env_remove.iter().any(|v| v == "GEMINI_API_KEY"));
        assert!(spec.env_remove.iter().any(|v| v == "ANTHROPIC_API_KEY"));

        let req = SessionRequest {
            auth: AuthMode::ApiKey,
            ..request(SessionRole::Build)
        };
        let spec = build_command("agy", &req).unwrap();
        assert!(!spec.env_remove.iter().any(|v| v == "GEMINI_API_KEY"));
        assert!(spec.env_remove.iter().any(|v| v == "GOOGLE_API_KEY"));
        assert!(spec.env_remove.iter().any(|v| v == "OPENAI_API_KEY"));
    }

    #[test]
    fn auto_update_is_disabled() {
        let spec = build_command("agy", &request(SessionRole::ReadOnly)).unwrap();
        assert_eq!(
            spec.env_set,
            [(AUTO_UPDATE_ENV.to_owned(), AUTO_UPDATE_VALUE.to_owned())]
        );
    }

    #[test]
    fn native_budget_is_ignored() {
        let req = SessionRequest {
            max_budget_usd: Some(1.0),
            ..request(SessionRole::Build)
        };
        let spec = build_command("agy", &req).unwrap();
        assert!(!spec.args.iter().any(|a| a.contains("budget")));
    }

    #[test]
    fn empty_prompts_and_flag_like_values_are_rejected() {
        let empty = SessionRequest {
            prompt: "  ".into(),
            ..request(SessionRole::Build)
        };
        assert!(build_command("agy", &empty).is_err());

        let sneaky = SessionRequest {
            model: Some("--dangerously-skip-permissions".into()),
            ..request(SessionRole::Build)
        };
        assert!(build_command("agy", &sneaky).is_err());
    }

    #[test]
    fn prompts_with_quotes_and_newlines_stay_one_argument() {
        let req = SessionRequest {
            prompt: "line one\nit's \"quoted\"".into(),
            ..request(SessionRole::Build)
        };
        let spec = build_command("agy", &req).unwrap();
        assert_eq!(spec.args[1], "line one\nit's \"quoted\"");
    }
}
