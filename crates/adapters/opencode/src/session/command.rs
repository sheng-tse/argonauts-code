//! `opencode run --format json --model <provider/model>` with the prompt on stdin, resumed with
//! `--session <id>`, and copied with `--session <id> --fork`. Never `--continue`, which picks the
//! last session of whoever ran OpenCode last.
//!
//! OpenCode 2 sends a plain `run` to a background service it keeps for the whole computer,
//! which runs the session's tools outside Argonauts' sandbox and never sees the config
//! Argonauts sends. So every OpenCode 2 session, new or resumed, gets `--standalone`: a private
//! server that is the client's own child, starts inside the sandbox with the session's
//! variables, and ends when the client is stopped. OpenCode 1.18 has neither the service nor
//! the flag. `run --help` at 2.0.32 lists `--standalone`, `--session`, `--fork`, `--model`,
//! `--format` and `--thinking`; 1.18.33's flags were read from `packages/opencode/src/cli/cmd/run.ts`,
//! where `--fork` has been since 1.1.54.
//! A level goes inside the model, as [`level`] says.

use std::path::PathBuf;

use anyhow::{bail, ensure};
use argonauts_core::{AuthMode, CommandSpec, SessionRequest, env_remove_except};

use crate::models::private_server::{ApiCall, PrivateServer, private_server_flags};
use crate::models::provider::{self, LocalUrls};
use crate::models::user_config;
use crate::session::level;
use crate::session::outside;

/// Keeps OpenCode from updating itself, so a session never switches versions halfway.
pub(crate) const AUTO_UPDATE_ENV: &str = "OPENCODE_DISABLE_AUTOUPDATE";

/// The key variable each API provider reads, from the `env` lists in OpenCode's models.dev
/// catalog, not run. Local routes and every other provider keep none.
const PROVIDER_KEY_ENV: &[(&str, &str)] = &[
    ("anthropic", "ANTHROPIC_API_KEY"),
    ("openai", "OPENAI_API_KEY"),
    ("openrouter", "OPENROUTER_API_KEY"),
    ("google", "GEMINI_API_KEY"),
    ("xai", "XAI_API_KEY"),
    ("amazon-bedrock", "AWS_BEARER_TOKEN_BEDROCK"),
];

pub(crate) fn build(req: &SessionRequest, urls: &LocalUrls) -> anyhow::Result<CommandSpec> {
    let config_files = user_config::config_files(&req.workdir, |name| std::env::var(name).ok());
    build_with(req, &config_files, urls)
}

/// OpenCode 1.18.33 wraps a message argument that contains a space in double quotes before
/// sending it, so a prompt passed as an argument reaches the model changed. A piped stdin is
/// taken as the message unchanged when no argument is given.
///
/// `--thinking` isn't passed, since no run has shown this version accepts it. Add it through
/// the agent's `extra_args` to see reasoning parts.
pub(crate) fn build_with(
    req: &SessionRequest,
    config_files: &[PathBuf],
    urls: &LocalUrls,
) -> anyhow::Result<CommandSpec> {
    let mut cmd = CommandSpec::new("opencode", &req.workdir)
        .arg("run")
        .args(
            private_server_flags(req.cli_version.as_deref())
                .iter()
                .copied(),
        )
        .args(["--format", "json"]);

    let route = launch_route(req, model_route(req)?, &mut cmd, config_files, urls);
    // The level goes inside the model, `provider/model#level`, on OpenCode 2, and after
    // `--variant` on 1.18.
    let model = level::model_args(
        route.as_deref(),
        req.effort.as_deref(),
        req.cli_version.as_deref(),
    );
    if let Some(model) = &model.model {
        cmd = cmd.arg("--model").arg(model);
    }
    cmd = cmd.args(model.flags);
    ensure!(
        !req.fork_session || req.resume_session.is_some(),
        "a fork needs the session to copy"
    );
    if let Some(session) = &req.resume_session {
        cmd = cmd.arg("--session").arg(session);
        // The copy gets an id of its own, which its first event names, and the session it
        // came from is left as it was.
        if req.fork_session {
            cmd = cmd.arg("--fork");
        }
    }
    cmd = cmd.args(req.extra_args.iter().cloned());
    outside::apply(&mut cmd, req);
    guard_env(&mut cmd, req, route.as_deref());
    // OpenCode reads stdin to its end before it starts, so it can't stay open for steering.
    // It has no flag that holds a run to a schema.
    cmd.stdin = Some(req.prompt_with_schema());
    Ok(cmd)
}

/// OpenCode's own screen on the session, `opencode [--standalone] --session <id> <folder>`,
/// with the config and variables its sessions get, so a local model is found as it was. The
/// session keeps its own model.
pub(crate) fn take_over(
    req: &SessionRequest,
    config_files: &[PathBuf],
    urls: &LocalUrls,
) -> Option<CommandSpec> {
    let session = req
        .resume_session
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty() && !id.starts_with('-'))?;
    let folder = req.workdir.to_str().filter(|dir| dir.starts_with('/'))?;
    let mut cmd = CommandSpec::new("opencode", &req.workdir)
        .args(
            private_server_flags(req.cli_version.as_deref())
                .iter()
                .copied(),
        )
        .args(["--session", session, folder]);
    let route = launch_route(req, model_route(req).ok()?, &mut cmd, config_files, urls);
    guard_env(&mut cmd, req, route.as_deref());
    Some(cmd)
}

/// The route `cmd` runs, under your own provider for a local runtime when you have one, with
/// the provider config yours lacks set on `cmd`.
fn launch_route(
    req: &SessionRequest,
    route: Option<String>,
    cmd: &mut CommandSpec,
    config_files: &[PathBuf],
    urls: &LocalUrls,
) -> Option<String> {
    let launch = provider::launch(
        &route?,
        req.cli_config.as_deref(),
        config_files,
        req.window,
        urls,
    );
    if let Some(config) = launch.config {
        cmd.env_set
            .push((provider::CONFIG_CONTENT_ENV.to_owned(), config.to_string()));
    }
    Some(launch.route)
}

fn guard_env(cmd: &mut CommandSpec, req: &SessionRequest, route: Option<&str>) {
    // An update mid-run would replace the binary a crew session is using.
    cmd.env_set
        .push((AUTO_UPDATE_ENV.to_owned(), "1".to_owned()));
    cmd.env_set.extend(req.env_set.iter().cloned());
    // OpenCode connects any provider whose key variable is set, so an inherited key could bill
    // an API the session wasn't meant to use. GitHub tokens and the parent session's
    // messaging variables go too.
    let keep = kept_key_env(req.auth, route);
    cmd.env_remove = env_remove_except(keep.as_slice());
}

/// OpenCode 2 is asked what its config holds before a session on a local route, on a private
/// server that makes no model call, so both of its file shapes count the way it reads them.
/// 1.18 has no `opencode api`, and its files are read directly. Any other route is sent no
/// config to merge, so nothing needs asking.
pub(crate) fn config_query(req: &SessionRequest, urls: &LocalUrls) -> Option<CommandSpec> {
    if private_server_flags(req.cli_version.as_deref()).is_empty() {
        return None;
    }
    let route = model_route(req).ok()??;
    provider::local_provider_config(&route, urls)?;
    let server = PrivateServer::new("opencode", req.cli_version.as_deref(), &req.workdir);
    Some(server.api_command(&ApiCall::request("GET", "/api/config")))
}

/// Under a subscription OpenCode uses the logins saved with `/connect`, so no key variable
/// reaches it. With an API key, only the chosen provider's own variable does.
fn kept_key_env(auth: AuthMode, route: Option<&str>) -> Option<&'static str> {
    if auth == AuthMode::Subscription {
        return None;
    }
    let (provider, _) = route?.split_once('/')?;
    PROVIDER_KEY_ENV
        .iter()
        .find(|(id, _)| *id == provider)
        .map(|&(_, var)| var)
}

/// The registry's `launch.provider_model` wins over the plain model name. OpenCode splits
/// `--model` at the first `/` into provider and model, so a name without one can't work.
fn model_route(req: &SessionRequest) -> anyhow::Result<Option<String>> {
    let hinted = req
        .launch
        .as_ref()
        .and_then(|launch| launch.provider_model.clone());
    let Some(model) = hinted.or_else(|| req.model.clone()) else {
        return Ok(None);
    };
    match model.split_once('/') {
        Some((provider, name)) if !provider.is_empty() && !name.is_empty() => Ok(Some(model)),
        _ => bail!(
            "OpenCode needs its model as provider/model, such as ollama/<model>, but got {model:?}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::models::registry::LaunchHints;
    use argonauts_core::{PARENT_SESSION_ENV_VARS, PROVIDER_KEY_ENV_VARS, SessionRole};

    use std::path::Path;

    use super::*;

    fn request() -> SessionRequest {
        SessionRequest {
            workdir: PathBuf::from("/work/tree"),
            prompt: "Fix the \"flaky\" test".to_owned(),
            model: Some("openrouter/z-ai/glm-5".to_owned()),
            ..SessionRequest::default()
        }
    }

    #[test]
    fn opencode_command() {
        let cmd = build_with(&request(), &[], &LocalUrls::new()).unwrap();
        assert_eq!(cmd.program, "opencode");
        assert_eq!(cmd.cwd, PathBuf::from("/work/tree"));
        assert_eq!(
            cmd.args,
            [
                "run",
                "--standalone",
                "--format",
                "json",
                "--model",
                "openrouter/z-ai/glm-5"
            ]
        );
        assert_eq!(cmd.stdin.as_deref(), Some("Fix the \"flaky\" test"));
        assert!(!cmd.keep_stdin_open);
        assert!(
            cmd.env_set
                .contains(&("OPENCODE_DISABLE_AUTOUPDATE".to_owned(), "1".to_owned()))
        );
        assert!(
            !cmd.env_set
                .iter()
                .any(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
        );

        let resumed = SessionRequest {
            resume_session: Some("ses_abc123".to_owned()),
            effort: Some("high".to_owned()),
            role: SessionRole::Build,
            extra_args: vec!["--agent".to_owned(), "build".to_owned()],
            keep_input_open: true,
            ..request()
        };
        let cmd = build_with(&resumed, &[], &LocalUrls::new()).unwrap();
        assert_eq!(
            cmd.args,
            [
                "run",
                "--standalone",
                "--format",
                "json",
                "--model",
                "openrouter/z-ai/glm-5#high",
                "--session",
                "ses_abc123",
                "--agent",
                "build"
            ]
        );
        assert!(!cmd.keep_stdin_open);
        for never in ["--continue", "-c", "--fork", "--auto"] {
            assert!(!cmd.args.iter().any(|a| a == never), "{never}");
        }

        let default_model = SessionRequest {
            model: None,
            ..request()
        };
        let cmd = build_with(&default_model, &[], &LocalUrls::new()).unwrap();
        assert!(!cmd.args.iter().any(|a| a == "--model"));

        let hinted = SessionRequest {
            model: Some("glm-5".to_owned()),
            launch: Some(LaunchHints {
                provider_model: Some("openrouter/z-ai/glm-5".to_owned()),
                ..LaunchHints::default()
            }),
            ..request()
        };
        assert!(
            build_with(&hinted, &[], &LocalUrls::new())
                .unwrap()
                .args
                .contains(&"openrouter/z-ai/glm-5".to_owned())
        );

        for bad in ["glm-5", "/glm-5", "openrouter/"] {
            let req = SessionRequest {
                model: Some(bad.to_owned()),
                ..request()
            };
            let err = build_with(&req, &[], &LocalUrls::new())
                .unwrap_err()
                .to_string();
            assert!(err.contains("provider/model"), "{bad}: {err}");
        }
    }

    /// A fork copies the session it names and carries the copy on with the new prompt, every
    /// flag given again as on a resume. Without a session there is nothing to copy.
    #[test]
    fn a_fork_carries_on_a_copy_of_the_session() {
        let forked = SessionRequest {
            resume_session: Some("ses_abc123".to_owned()),
            fork_session: true,
            prompt: "You are now the orchestrator of the run you just started.".to_owned(),
            ..request()
        };
        let cmd = build_with(&forked, &[], &LocalUrls::new()).unwrap();
        assert_eq!(
            cmd.args,
            [
                "run",
                "--standalone",
                "--format",
                "json",
                "--model",
                "openrouter/z-ai/glm-5",
                "--session",
                "ses_abc123",
                "--fork"
            ]
        );
        assert_eq!(
            cmd.stdin.as_deref(),
            Some("You are now the orchestrator of the run you just started.")
        );
        let resumed = SessionRequest {
            fork_session: false,
            ..forked.clone()
        };
        let cmd = build_with(&resumed, &[], &LocalUrls::new()).unwrap();
        assert!(!cmd.args.iter().any(|a| a == "--fork"));
        let nothing = SessionRequest {
            resume_session: None,
            ..forked
        };
        let err = build_with(&nothing, &[], &LocalUrls::new()).unwrap_err();
        assert!(
            err.to_string().contains("a fork needs the session"),
            "{err}"
        );
    }

    #[test]
    fn opencode_2_sessions_run_on_a_private_server_and_1_18_ones_as_before() {
        let on = |version: Option<&str>, resume: Option<&str>| {
            let req = SessionRequest {
                cli_version: version.map(str::to_owned),
                resume_session: resume.map(str::to_owned),
                ..request()
            };
            build_with(&req, &[], &LocalUrls::new()).unwrap().args
        };
        for resume in [None, Some("ses_abc123")] {
            let current = on(Some("2.0.32"), resume);
            assert_eq!(current[..2], ["run", "--standalone"], "{resume:?}");
            assert_eq!(
                current.iter().filter(|a| *a == "--standalone").count(),
                1,
                "{resume:?}"
            );
            assert_eq!(on(None, resume)[..2], ["run", "--standalone"], "{resume:?}");
            let old = on(Some("1.18.34"), resume);
            assert!(!old.iter().any(|a| a == "--standalone"), "{old:?}");
            assert_eq!(old[..3], ["run", "--format", "json"]);
        }
        let resumed = on(Some("2.0.32"), Some("ses_abc123"));
        assert!(resumed.ends_with(&["--session".to_owned(), "ses_abc123".to_owned()]));
    }

    #[test]
    fn the_level_goes_where_the_installed_version_reads_it() {
        let at = |version: &str| {
            let req = SessionRequest {
                cli_version: Some(version.to_owned()),
                effort: Some("high".to_owned()),
                ..request()
            };
            build_with(&req, &[], &LocalUrls::new()).unwrap().args
        };
        let current = at("2.0.32");
        assert!(current.contains(&"openrouter/z-ai/glm-5#high".to_owned()));
        assert!(!current.iter().any(|a| a == "--variant"), "{current:?}");

        let old = at("1.18.34");
        let model = old.iter().position(|a| a == "--model").unwrap();
        assert_eq!(old[model + 1], "openrouter/z-ai/glm-5");
        let variant = old.iter().position(|a| a == "--variant").unwrap();
        assert_eq!(old[variant + 1], "high");
    }

    #[test]
    fn opencode_2_is_asked_for_its_config_before_a_local_session() {
        let local = |version: Option<&str>, model: &str| SessionRequest {
            cli_version: version.map(str::to_owned),
            model: Some(model.to_owned()),
            ..request()
        };
        let urls = LocalUrls::new();
        let query = config_query(&local(Some("2.0.32"), "ollama/llama3.2"), &urls).unwrap();
        assert_eq!(query.program, "opencode");
        assert_eq!(query.cwd, PathBuf::from("/work/tree"));
        assert_eq!(query.args, ["api", "--standalone", "GET", "/api/config"]);
        assert!(query.stdin.is_none());
        // It answers with your config alone, never what Argonauts sends.
        assert!(
            !query
                .env_set
                .iter()
                .any(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
        );
        assert!(config_query(&local(None, "lmstudio/qwen/qwen3-8b"), &urls).is_some());

        assert!(config_query(&local(Some("1.18.34"), "ollama/llama3.2"), &urls).is_none());
        assert!(config_query(&local(Some("2.0.32"), "openrouter/z-ai/glm-5"), &urls).is_none());
        let default_model = SessionRequest {
            model: None,
            ..local(Some("2.0.32"), "x")
        };
        assert!(config_query(&default_model, &urls).is_none());
    }

    #[test]
    fn the_answer_decides_what_is_sent() {
        let answered = |answer: &str, window: Option<u64>| {
            let req = SessionRequest {
                cli_version: Some("2.0.32".to_owned()),
                model: Some("ollama/qwen3-coder:30b".to_owned()),
                cli_config: Some(answer.to_owned()),
                window,
                ..request()
            };
            let cmd = build_with(&req, &[], &LocalUrls::new()).unwrap();
            cmd.env_set
                .iter()
                .find(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
                .map(|(_, v)| serde_json::from_str::<serde_json::Value>(v).unwrap())
        };
        let yours = r#"[{"type": "document", "path": "/h/.config/opencode/opencode.json",
            "info": {"providers": {"ollama": {"settings": {"baseURL": "http://gpu:11434/v1"},
            "models": {"qwen3-coder:30b": {"limit": {"context": 65536, "output": 16384}}}}}}}]"#;
        assert_eq!(answered(yours, Some(32_768)), None);
        // Your provider at your own address: only the model goes, with the window it holds.
        let provider_only = r#"[{"type": "document", "info": {"providers": {"ollama":
            {"settings": {"baseURL": "http://gpu:11434/v1"}}}}}]"#;
        let sent = answered(provider_only, Some(65_536)).unwrap();
        let ollama = &sent["provider"]["ollama"];
        assert!(ollama["options"].is_null(), "{sent}");
        assert_eq!(
            ollama["models"]["qwen3-coder:30b"]["limit"]["context"],
            65_536
        );
    }

    #[test]
    fn take_over_opens_opencodes_own_screen_on_the_session() {
        let req = SessionRequest {
            cli_version: Some("2.0.32".to_owned()),
            model: Some("ollama/qwen3-coder:30b".to_owned()),
            ..SessionRequest::reopening("ses_abc123", Path::new("/work/task-001"))
        };
        let cmd = take_over(&req, &[], &LocalUrls::new()).unwrap();
        assert_eq!(cmd.program, "opencode");
        assert_eq!(cmd.cwd, PathBuf::from("/work/task-001"));
        assert_eq!(
            cmd.args,
            ["--standalone", "--session", "ses_abc123", "/work/task-001"]
        );
        assert!(cmd.stdin.is_none());
        // Its local model is declared as for its sessions, and keys stay out.
        assert!(
            cmd.env_set
                .iter()
                .any(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
        );
        assert!(
            cmd.env_set
                .contains(&("OPENCODE_DISABLE_AUTOUPDATE".to_owned(), "1".to_owned()))
        );
        assert!(removes(&cmd, "OPENAI_API_KEY") && removes(&cmd, "CLAUDECODE"));

        let old = SessionRequest {
            cli_version: Some("1.18.34".to_owned()),
            ..req.clone()
        };
        let cmd = take_over(&old, &[], &LocalUrls::new()).unwrap();
        assert_eq!(cmd.args, ["--session", "ses_abc123", "/work/task-001"]);
        for bad in ["", " ", "--continue"] {
            let req = SessionRequest {
                resume_session: Some(bad.to_owned()),
                ..req.clone()
            };
            assert!(take_over(&req, &[], &LocalUrls::new()).is_none(), "{bad:?}");
        }
    }

    fn removes(cmd: &CommandSpec, var: &str) -> bool {
        cmd.env_remove.iter().any(|v| v == var)
    }

    #[test]
    fn inherited_keys_and_parent_session_are_removed() {
        let cmd = build_with(&request(), &[], &LocalUrls::new()).unwrap();
        for var in PROVIDER_KEY_ENV_VARS.iter().chain(PARENT_SESSION_ENV_VARS) {
            assert!(removes(&cmd, var), "{var}");
        }

        let api_key = |model: Option<&str>| SessionRequest {
            auth: AuthMode::ApiKey,
            model: model.map(str::to_owned),
            ..request()
        };
        let cmd = build_with(
            &api_key(Some("openrouter/z-ai/glm-5")),
            &[],
            &LocalUrls::new(),
        )
        .unwrap();
        assert!(!removes(&cmd, "OPENROUTER_API_KEY"));
        for var in [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "CLAUDE_CODE_MESSAGING_TOKEN",
        ] {
            assert!(removes(&cmd, var), "{var}");
        }

        for model in [
            Some("ollama/llama3.2"),
            Some("lmstudio/qwen/qwen3-8b"),
            None,
        ] {
            let cmd = build_with(&api_key(model), &[], &LocalUrls::new()).unwrap();
            let all = PROVIDER_KEY_ENV_VARS.len() + PARENT_SESSION_ENV_VARS.len();
            assert_eq!(cmd.env_remove.len(), all, "{model:?}");
        }
    }

    #[test]
    fn the_report_schema_goes_in_the_prompt() {
        let cmd = build_with(&request(), &[], &LocalUrls::new()).unwrap();
        assert_eq!(cmd.stdin.as_deref(), Some("Fix the \"flaky\" test"));

        let schema = serde_json::json!({"type": "object", "required": ["status"]});
        let asked = SessionRequest {
            json_schema: Some(schema.clone()),
            ..request()
        };
        let cmd = build_with(&asked, &[], &LocalUrls::new()).unwrap();
        let stdin = cmd.stdin.unwrap();
        assert!(stdin.starts_with("Fix the \"flaky\" test\n\nEnd your final message"));
        let fenced = stdin
            .split("```json\n")
            .nth(1)
            .and_then(|rest| rest.split("\n```").next())
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(fenced).unwrap(),
            schema
        );
        assert!(!cmd.args.iter().any(|a| a.contains("schema")));
    }

    #[test]
    fn thinking_only_when_asked() {
        let cmd = build_with(&request(), &[], &LocalUrls::new()).unwrap();
        assert!(!cmd.args.iter().any(|a| a == "--thinking"));
        let asked = SessionRequest {
            extra_args: vec!["--thinking".to_owned()],
            ..request()
        };
        let cmd = build_with(&asked, &[], &LocalUrls::new()).unwrap();
        assert_eq!(cmd.args.last().map(String::as_str), Some("--thinking"));
    }
}
