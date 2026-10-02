use std::path::Path;

use anyhow::{Context, bail};
use argonauts_core::{AuthMode, CommandSpec, SessionRequest, SessionRole, env_remove_except};
use serde_json::json;

use crate::agents_file::{AgentProfile, agent_launch};
use crate::tweaks::fanout_off;

/// Turns off agy's background self-updater, so a run uses the version `doctor` checked.
pub const AUTO_UPDATE_ENV: &str = "AGY_CLI_DISABLE_AUTO_UPDATE";

/// agy 1.2.13 ignores `1` and still spawns its updater.
pub const AUTO_UPDATE_VALUE: &str = "true";

/// The only API key variable agy reads. It ignores `GOOGLE_API_KEY`.
pub const API_KEY_ENV: &str = "GEMINI_API_KEY";

/// One user turn for `--input-format stream-json`, newline included.
pub fn user_message_line(content: &str) -> String {
    let message = json!({"event": "user", "message": {"content": content}});
    format!("{message}\n")
}

/// On its own, agy runs in its sandbox without skipping permissions, so it soft-denies commands
/// that need approval. Inside ours its sandbox can't start (macOS won't nest one in another), so
/// commands run without asking, held by ours, and a read-only session's agent file still has no
/// tool that writes. `sandbox.mode = "none"` asks for the same, with nothing holding them.
fn own_sandbox(req: &SessionRequest) -> &'static str {
    if req.tool_sandbox_off {
        "--dangerously-skip-permissions"
    } else {
        "--sandbox"
    }
}

/// The prompt goes on stdin as the first user message, never as an argument, so other local
/// users can't read it in the process list.
pub fn build_command(program: &str, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
    if req.prompt.trim().is_empty() {
        bail!("agy needs a non-empty prompt");
    }
    let mut spec = CommandSpec::new(program, &req.workdir)
        .args(["--input-format", "stream-json"])
        .args(["--output-format", "stream-json"])
        .arg(own_sandbox(req))
        .arg("--disable-slash-commands");
    if req.role == SessionRole::Build {
        spec = spec.args(["--mode", "accept-edits"]);
    }
    if let Some(profile) = agent_profile(req) {
        let scratch = req.scratch_dir.as_deref().with_context(|| {
            format!(
                "agy needs a scratch directory for its {} agent file",
                profile.name()
            )
        })?;
        let launch = agent_launch(profile, scratch)?;
        spec = spec.args(launch.args);
        spec.files.push(launch.file);
    }
    // Not isolation. agy 1.2.13 quietly uses `default-cli-project` when no project has this
    // name, and nothing creates one, so every run still shares agy's default project state.
    // `--new-project` creates one per session, but the init line names no project, so a later
    // session of the run couldn't find it.
    if let Some(project) = &req.project {
        spec = spec.args(["--project", flag_value("project", project)?]);
    }
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
    // `max_budget_usd` is not passed on, because agy has no spend cap. Compaction and user
    // hooks have no per-run control either (see `tweaks`).
    spec = spec.args(&req.extra_args);

    spec.stdin = Some(user_message_line(&req.prompt));
    spec.keep_stdin_open = req.keep_input_open;
    spec.env_remove = match req.auth {
        AuthMode::Subscription => env_remove_except(&[]),
        AuthMode::ApiKey => env_remove_except(&[API_KEY_ENV]),
    };
    spec.env_set = session_env(req, &spec.env_remove);
    Ok(spec)
}

/// `agy --conversation <id>` opens the conversation in agy's own UI. agy scopes history to the
/// working directory, so it must start in the session's worktree.
pub fn interactive_resume_command(
    program: &str,
    conversation: &str,
    workdir: &Path,
) -> Option<CommandSpec> {
    let conversation = flag_value("conversation id", conversation).ok()?;
    let mut spec = CommandSpec::new(program, workdir).args(["--conversation", conversation]);
    spec.env_set = vec![(AUTO_UPDATE_ENV.to_owned(), AUTO_UPDATE_VALUE.to_owned())];
    Some(spec)
}

/// Read-only sessions always get the restricted agent, which has no subagent tools either.
fn agent_profile(req: &SessionRequest) -> Option<AgentProfile> {
    match req.role {
        SessionRole::ReadOnly => Some(AgentProfile::ReadOnly),
        SessionRole::Build if fanout_off(req.native_fanout) => Some(AgentProfile::NoSubagents),
        SessionRole::Build => None,
    }
}

/// The caller's variables, minus any the auth mode scrubs, then the updater pin, which always
/// wins.
fn session_env(req: &SessionRequest, removed: &[String]) -> Vec<(String, String)> {
    req.env_set
        .iter()
        .filter(|(name, _)| name != AUTO_UPDATE_ENV && !removed.contains(name))
        .cloned()
        .chain([(AUTO_UPDATE_ENV.to_owned(), AUTO_UPDATE_VALUE.to_owned())])
        .collect()
}

pub(crate) fn flag_value<'a>(what: &str, value: &'a str) -> anyhow::Result<&'a str> {
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
    use argonauts_core::settings::sessions::{FanoutSwitch, NativeFanout};
    use serde_json::Value;
    use std::path::PathBuf;

    fn request(role: SessionRole) -> SessionRequest {
        SessionRequest {
            workdir: PathBuf::from("/work/card-001"),
            prompt: "Create hello.txt".into(),
            role,
            scratch_dir: Some(PathBuf::from("/state/sessions/s1")),
            ..SessionRequest::default()
        }
    }

    fn build(req: &SessionRequest) -> CommandSpec {
        build_command("agy", req).unwrap()
    }

    fn value_after<'a>(spec: &'a CommandSpec, flag: &str) -> Option<&'a str> {
        let at = spec.args.iter().position(|a| a == flag)?;
        spec.args.get(at + 1).map(String::as_str)
    }

    fn has_arg(spec: &CommandSpec, arg: &str) -> bool {
        spec.args.iter().any(|a| a == arg)
    }

    fn stdin_message(spec: &CommandSpec) -> Value {
        let line = spec.stdin.as_deref().unwrap();
        assert!(line.ends_with('\n') && line.matches('\n').count() == 1);
        serde_json::from_str(line.trim_end()).unwrap()
    }

    #[test]
    fn agy_prompt_on_stdin() {
        let secret = "Create hello.txt with the launch code 12345";
        for role in [SessionRole::Build, SessionRole::ReadOnly] {
            let req = SessionRequest {
                prompt: secret.into(),
                ..request(role)
            };
            let spec = build(&req);
            assert_eq!(spec.program, "agy");
            assert!(
                !spec.args.iter().any(|a| a.contains("launch code")),
                "the prompt must never be an argument"
            );
            for flag in ["-p", "--print", "--prompt"] {
                assert!(!has_arg(&spec, flag), "{flag}");
            }
            assert_eq!(value_after(&spec, "--input-format"), Some("stream-json"));
            assert_eq!(value_after(&spec, "--output-format"), Some("stream-json"));
            assert_eq!(
                stdin_message(&spec),
                json!({"event": "user", "message": {"content": secret}})
            );
            assert!(!spec.keep_stdin_open);
        }

        let open = SessionRequest {
            keep_input_open: true,
            ..request(SessionRole::Build)
        };
        assert!(build(&open).keep_stdin_open);
    }

    #[test]
    fn prompts_with_quotes_and_newlines_stay_one_message() {
        let req = SessionRequest {
            prompt: "line one\nit's \"quoted\"".into(),
            ..request(SessionRole::Build)
        };
        let spec = build(&req);
        assert_eq!(
            stdin_message(&spec)["message"]["content"],
            "line one\nit's \"quoted\""
        );
        assert_eq!(
            user_message_line("hi"),
            "{\"event\":\"user\",\"message\":{\"content\":\"hi\"}}\n"
        );
    }

    #[test]
    fn agy_project_is_passed_on_and_auto_update_is_off() {
        let req = SessionRequest {
            project: Some("run-20260930-abc".into()),
            ..request(SessionRole::Build)
        };
        let spec = build(&req);
        assert_eq!(value_after(&spec, "--project"), Some("run-20260930-abc"));
        assert!(!has_arg(&spec, "--new-project"));
        assert_eq!(
            spec.env_set,
            [(AUTO_UPDATE_ENV.to_owned(), AUTO_UPDATE_VALUE.to_owned())]
        );

        let without = build(&request(SessionRole::ReadOnly));
        assert!(!has_arg(&without, "--project"));
        assert!(
            without
                .env_set
                .contains(&(AUTO_UPDATE_ENV.into(), "true".into()))
        );

        let sneaky = SessionRequest {
            project: Some("--dangerously-skip-permissions".into()),
            ..request(SessionRole::Build)
        };
        assert!(build_command("agy", &sneaky).is_err());
    }

    #[test]
    fn the_updater_pin_wins_over_the_callers_env() {
        let req = SessionRequest {
            env_set: vec![
                (AUTO_UPDATE_ENV.into(), "false".into()),
                ("ARGONAUTS_SESSION".into(), "s1".into()),
            ],
            ..request(SessionRole::Build)
        };
        assert_eq!(
            build(&req).env_set,
            [
                ("ARGONAUTS_SESSION".to_owned(), "s1".to_owned()),
                (AUTO_UPDATE_ENV.to_owned(), AUTO_UPDATE_VALUE.to_owned()),
            ]
        );
    }

    #[test]
    fn agy_readonly_uses_restricted_agent_file() {
        let spec = build(&request(SessionRole::ReadOnly));
        let root = "/state/sessions/s1/agy-agent";
        assert_eq!(value_after(&spec, "--add-dir"), Some(root));
        assert_eq!(value_after(&spec, "--agent"), Some("argonauts-read-only"));
        assert_eq!(spec.files.len(), 1);
        let file = &spec.files[0];
        assert!(file.path.starts_with(root));
        assert!(file.path.ends_with(".agents/agents/argonauts-read-only.md"));
        assert!(file.contents.contains("  - view_file\n"));
        for tool in ["write_to_file", "run_command", "invoke_subagent"] {
            assert!(!file.contents.contains(tool), "{tool}");
        }
        // Plan mode only puts /plan in front of the prompt and still writes files.
        assert!(!has_arg(&spec, "--mode"));
        assert!(has_arg(&spec, "--sandbox"));
    }

    #[test]
    fn with_its_own_sandbox_off_commands_run_unasked() {
        for role in [SessionRole::ReadOnly, SessionRole::Build] {
            let spec = build(&SessionRequest {
                goal_hash: Some("abc".into()),
                tool_sandbox_off: true,
                ..request(role)
            });
            assert!(!has_arg(&spec, "--sandbox"), "{role:?}");
            assert!(has_arg(&spec, "--dangerously-skip-permissions"), "{role:?}");
        }
        let read_only = build(&SessionRequest {
            tool_sandbox_off: true,
            ..request(SessionRole::ReadOnly)
        });
        assert_eq!(
            value_after(&read_only, "--agent"),
            Some("argonauts-read-only")
        );
        assert!(!read_only.files[0].contents.contains("run_command"));
    }

    #[test]
    fn read_only_without_a_scratch_dir_fails_closed() {
        let req = SessionRequest {
            scratch_dir: None,
            ..request(SessionRole::ReadOnly)
        };
        let err = build_command("agy", &req).unwrap_err().to_string();
        assert!(err.contains("scratch directory"), "{err}");
    }

    #[test]
    fn read_only_flags_until_approved() {
        // A request that doesn't say otherwise is read-only, as every session is until the
        // goal is approved.
        let unapproved = SessionRequest {
            workdir: PathBuf::from("/work/base"),
            prompt: "Restate the goal".into(),
            scratch_dir: Some(PathBuf::from("/state/sessions/s2")),
            ..SessionRequest::default()
        };
        let spec = build(&unapproved);
        assert_eq!(value_after(&spec, "--agent"), Some("argonauts-read-only"));
        assert!(has_arg(&spec, "--sandbox"));
        for flag in ["--mode", "accept-edits", "--dangerously-skip-permissions"] {
            assert!(!has_arg(&spec, flag), "{flag}");
        }

        let approved = SessionRequest {
            goal_hash: Some("abc".into()),
            ..request(SessionRole::Build)
        };
        let spec = build(&approved);
        assert_eq!(value_after(&spec, "--mode"), Some("accept-edits"));
        assert!(!has_arg(&spec, "--agent") && spec.files.is_empty());
        assert!(has_arg(&spec, "--sandbox"));
        assert!(!has_arg(&spec, "--dangerously-skip-permissions"));
    }

    #[test]
    fn fanout_off_selects_the_agent_without_subagent_tools() {
        for setting in [
            NativeFanout::Switch(FanoutSwitch::Off),
            NativeFanout::Limit { max: 0 },
        ] {
            let req = SessionRequest {
                native_fanout: Some(setting),
                ..request(SessionRole::Build)
            };
            let spec = build(&req);
            assert_eq!(
                value_after(&spec, "--agent"),
                Some("argonauts-no-subagents")
            );
            assert_eq!(value_after(&spec, "--mode"), Some("accept-edits"));
            let contents = &spec.files[0].contents;
            assert!(!contents.contains("invoke_subagent") && contents.contains("write_to_file"));
        }
        for setting in [
            NativeFanout::Switch(FanoutSwitch::On),
            NativeFanout::Limit { max: 2 },
        ] {
            let req = SessionRequest {
                native_fanout: Some(setting),
                ..request(SessionRole::Build)
            };
            assert!(!has_arg(&build(&req), "--agent"), "{setting:?}");
        }
    }

    #[test]
    fn cheapest_model_and_effort_are_passed() {
        let req = SessionRequest {
            model: Some("gemini-3.6-flash-low".into()),
            effort: Some("low".into()),
            ..request(SessionRole::ReadOnly)
        };
        let spec = build(&req);
        assert_eq!(value_after(&spec, "--model"), Some("gemini-3.6-flash-low"));
        assert_eq!(value_after(&spec, "--effort"), Some("low"));
    }

    #[test]
    fn defaults_leave_model_effort_schema_project_and_resume_out() {
        let spec = build(&request(SessionRole::Build));
        for flag in [
            "--model",
            "--effort",
            "--json-schema",
            "--conversation",
            "--project",
        ] {
            assert!(!has_arg(&spec, flag), "{flag}");
        }
    }

    #[test]
    fn schema_is_passed_inline_as_compact_json() {
        let req = SessionRequest {
            json_schema: Some(report_json_schema()),
            ..request(SessionRole::Build)
        };
        let spec = build(&req);
        let schema = value_after(&spec, "--json-schema").unwrap();
        assert!(!schema.contains('\n'));
        let parsed: Value = serde_json::from_str(schema).unwrap();
        assert_eq!(parsed, report_json_schema());
    }

    #[test]
    fn resume_passes_every_flag_again() {
        let fresh = SessionRequest {
            model: Some("gemini-3.6-flash-low".into()),
            project: Some("run-1".into()),
            ..request(SessionRole::ReadOnly)
        };
        let resumed = SessionRequest {
            resume_session: Some("055a398f-db14-4c5f-abbb-1bf03f8120a7".into()),
            prompt: "Carry on".into(),
            ..fresh.clone()
        };
        let first = build(&fresh);
        let again = build(&resumed);
        assert_eq!(
            value_after(&again, "--conversation"),
            Some("055a398f-db14-4c5f-abbb-1bf03f8120a7")
        );
        assert!(!has_arg(&again, "-c") && !has_arg(&again, "--continue"));
        assert_eq!(again.args[..first.args.len()], first.args[..]);
        assert_eq!(again.files, first.files);
    }

    #[test]
    fn extra_args_come_last() {
        let req = SessionRequest {
            extra_args: vec!["--print-timeout".into(), "10m".into()],
            ..request(SessionRole::Build)
        };
        let spec = build(&req);
        assert_eq!(spec.args[spec.args.len() - 2..], ["--print-timeout", "10m"]);
    }

    #[test]
    fn env_scrubbed_per_vendor() {
        let spec = build(&request(SessionRole::Build));
        for name in [
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "ANTHROPIC_API_KEY",
            "CLAUDECODE",
        ] {
            assert!(spec.env_remove.iter().any(|v| v == name), "{name}");
        }

        let req = SessionRequest {
            auth: AuthMode::ApiKey,
            env_set: vec![
                ("GEMINI_API_KEY".into(), "from-caller".into()),
                ("OPENAI_API_KEY".into(), "leak".into()),
            ],
            ..request(SessionRole::Build)
        };
        let spec = build(&req);
        assert!(!spec.env_remove.iter().any(|v| v == "GEMINI_API_KEY"));
        for name in ["GOOGLE_API_KEY", "OPENAI_API_KEY", "CLAUDE_CODE_SESSION_ID"] {
            assert!(spec.env_remove.iter().any(|v| v == name), "{name}");
        }
        let names: Vec<&str> = spec.env_set.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["GEMINI_API_KEY", AUTO_UPDATE_ENV]);

        let subscription = SessionRequest {
            env_set: vec![("GEMINI_API_KEY".into(), "sneaked".into())],
            ..request(SessionRole::Build)
        };
        let spec = build(&subscription);
        assert!(!spec.env_set.iter().any(|(n, _)| n == "GEMINI_API_KEY"));
    }

    #[test]
    fn native_budget_is_ignored() {
        let req = SessionRequest {
            max_budget_usd: Some(1.0),
            ..request(SessionRole::Build)
        };
        assert!(!build(&req).args.iter().any(|a| a.contains("budget")));
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
    fn take_over_opens_the_conversation_in_the_worktree() {
        let spec =
            interactive_resume_command("agy", "055a398f", Path::new("/work/card-001")).unwrap();
        assert_eq!(spec.program, "agy");
        assert_eq!(spec.args, ["--conversation", "055a398f"]);
        assert_eq!(spec.cwd, PathBuf::from("/work/card-001"));
        assert!(spec.stdin.is_none());
        assert!(interactive_resume_command("agy", "-c", Path::new("/w")).is_none());
    }
}
