//! `opencode run --format json --model <provider/model>` with the prompt on stdin, resumed with
//! `--session <id>`. Never `--continue`, which picks the last session of whoever ran OpenCode
//! last, and never `--fork`.
//!
//! Flags other than `run`, `--format json`, `--model` and `--session` were read from
//! `packages/opencode/src/cli/cmd/run.ts` at v1.18.33, not run: `--variant`, and reading a
//! piped stdin as the message.

use std::path::PathBuf;

use anyhow::bail;
use argonauts_core::{AuthMode, CommandSpec, SessionRequest, env_remove_except};

use crate::provider::{self, LocalUrls};
use crate::user_config;

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
    let mut cmd = CommandSpec::new("opencode", &req.workdir).args(["run", "--format", "json"]);

    let route = model_route(req)?;
    if let Some(model) = &route {
        if let Some(config) = provider::inline_config(model, config_files, urls) {
            cmd.env_set
                .push((provider::CONFIG_CONTENT_ENV.to_owned(), config.to_string()));
        }
        cmd = cmd.arg("--model").arg(model);
    }
    if let Some(effort) = &req.effort {
        cmd = cmd.arg("--variant").arg(effort);
    }
    if let Some(session) = &req.resume_session {
        cmd = cmd.arg("--session").arg(session);
    }
    cmd = cmd.args(req.extra_args.iter().cloned());

    // An update mid-run would replace the binary a crew session is using.
    cmd.env_set
        .push(("OPENCODE_DISABLE_AUTOUPDATE".to_owned(), "1".to_owned()));
    cmd.env_set.extend(req.env_set.iter().cloned());
    // OpenCode connects any provider whose key variable is set, so an inherited key could bill
    // an API the session wasn't meant to use. GitHub tokens and the parent session's
    // messaging variables go too.
    let keep = kept_key_env(req.auth, route.as_deref());
    cmd.env_remove = env_remove_except(keep.as_slice());
    // OpenCode reads stdin to its end before it starts, so it can't stay open for steering.
    // It has no flag that holds a run to a schema.
    cmd.stdin = Some(req.prompt_with_schema());
    Ok(cmd)
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
    use argonauts_core::registry::LaunchHints;
    use argonauts_core::{PARENT_SESSION_ENV_VARS, SessionRole, VENDOR_KEY_ENV_VARS};

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
                "--format",
                "json",
                "--model",
                "openrouter/z-ai/glm-5",
                "--variant",
                "high",
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

    fn removes(cmd: &CommandSpec, var: &str) -> bool {
        cmd.env_remove.iter().any(|v| v == var)
    }

    #[test]
    fn inherited_keys_and_parent_session_are_removed() {
        let cmd = build_with(&request(), &[], &LocalUrls::new()).unwrap();
        for var in VENDOR_KEY_ENV_VARS.iter().chain(PARENT_SESSION_ENV_VARS) {
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
            let all = VENDOR_KEY_ENV_VARS.len() + PARENT_SESSION_ENV_VARS.len();
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
