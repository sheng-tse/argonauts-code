//! The `codex exec` argument vector for one session.

use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};
use argonauts_core::agents::runtimes::runtime;
use argonauts_core::models::registry::LaunchHints;
use argonauts_core::{AuthMode, CommandSpec, SessionRequest, SessionRole, SpecFile};
use argonauts_core::{env_remove_except, settings::sessions::NativeFanout};
use serde_json::Value;

use crate::launch::adapter::{API_KEY_ENV_VARS, OUTPUT_SCHEMA_FILE, SUBSCRIPTION_ENV_VARS};
use crate::launch::tweaks;

/// Flags that would loosen the sandbox or approvals, move the session out of its worktree, or
/// pick a session by recency. The adapter sets these itself or never passes them.
const REFUSED_EXTRA_FLAGS: &[&str] = &[
    "-a",
    "--ask-for-approval",
    "--autopilot",
    "--approve-for-me",
    "--dangerously-bypass-approvals-and-sandbox",
    "--dangerously-bypass-hook-trust",
    "-s",
    "--sandbox",
    "-C",
    "--cd",
    "--worktree",
    "--last",
    "--all",
];

/// `extra_args` go before the `resume` subcommand, where `codex exec` accepts its own flags.
pub fn build(program: &str, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
    ensure!(
        !req.workdir.as_os_str().is_empty(),
        "a Codex session needs a working directory"
    );
    ensure!(
        !req.prompt.trim().is_empty(),
        "a Codex session needs a prompt"
    );
    refuse_unsafe_extra_args(&req.extra_args)?;

    // Always pass --sandbox, because without it Codex picks one from the user's trust settings.
    let mut spec = CommandSpec::new(program, &req.workdir)
        .args(["exec", "--json", "--color", "never", "--cd"])
        .arg(utf8(&req.workdir)?)
        .args(["--sandbox", sandbox_mode(req), "--skip-git-repo-check"])
        .args(local_provider_args(req.launch.as_ref())?);
    if let Some(model) = session_model(req)? {
        spec = spec.args(["--model", model]);
    }

    if let Some(effort) = present(req.effort.as_deref()) {
        spec = spec.args(["--config", &reasoning_effort_override(effort)?]);
    }
    spec = spec.args(launch_tweak_args(req));
    if let Some(schema) = &req.json_schema {
        let file = schema_file(req, schema)?;
        spec = spec.args(["--output-schema", utf8(&file.path)?]);
        spec.files.push(file);
    }
    spec = spec.args(req.extra_args.iter().cloned());
    // Each picture as `codex exec` takes one, before `resume`, where its own flags go.
    for picture in &req.pictures {
        spec = spec.args(["--image", utf8(&picture.path)?]);
    }
    ensure!(
        !req.fork_session || req.resume_session.is_some(),
        "a fork needs the session to copy"
    );
    // A resume takes every flag again, since Codex applies the new call's settings. A fork
    // takes them the same way, and carries on a copy of the session under an id of its own.
    if let Some(session) = &req.resume_session {
        let how = if req.fork_session { "fork" } else { "resume" };
        spec = spec.args([how, flag_value("session id", session.trim())?]);
    }

    // `-` makes Codex read the prompt from stdin, which keeps it out of the process list.
    spec = spec.arg("-");
    spec.stdin = Some(req.prompt.clone());
    spec.env_remove = env_remove_except(kept_key_vars(req.auth));
    spec.env_set = req
        .env_set
        .iter()
        .filter(|(name, _)| !spec.env_remove.contains(name))
        .cloned()
        .collect();
    Ok(spec)
}

/// Inside our sandbox Codex's own can't start (macOS won't nest one in another), so it is off and
/// ours keeps a read-only session from writing. `sandbox.mode = "none"` turns it off too, with
/// nothing in its place. `codex exec` never asks before a command.
///
/// Work outside the project needs nothing more inside ours, which holds the never-list: in auto
/// and autopilot, and on the turn you allow to write there by answering. Codex can't ask in a
/// headless run, so in supervised ours keeps a direct turn's writes in its project until you
/// allow them, and a run's session reads outside and writes only its worktree. Codex 0.163's own
/// sandbox reads anywhere, but a writable root it adds with `--add-dir` is open whole, `~/.ssh`
/// included, so under its own sandbox it writes only its workspace.
pub fn sandbox_mode(req: &SessionRequest) -> &'static str {
    match req.role {
        _ if req.tool_sandbox_off => "danger-full-access",
        SessionRole::ReadOnly => "read-only",
        // A direct turn runs as a build session in your checkout: Codex asks only to leave its
        // sandbox, and inside ours there's nothing to leave to.
        SessionRole::Build | SessionRole::Direct => "workspace-write",
    }
}

pub fn kept_key_vars(auth: AuthMode) -> &'static [&'static str] {
    match auth {
        AuthMode::Subscription => SUBSCRIPTION_ENV_VARS,
        AuthMode::ApiKey => API_KEY_ENV_VARS,
    }
}

/// Codex's stream never names the model that ran, so the adapter passes the one your settings
/// chose. With none chosen, Codex runs its own default, since no model id is invented here. A
/// local model is named by the registry's provider model when it has one.
fn session_model(req: &SessionRequest) -> anyhow::Result<Option<&str>> {
    let local_model = req
        .launch
        .as_ref()
        .filter(|launch| launch.oss)
        .and_then(|launch| present(launch.provider_model.as_deref()));
    local_model
        .or_else(|| present(req.model.as_deref()))
        .map(|model| flag_value("model", model))
        .transpose()
}

/// `--oss --local-provider <provider> --model <model>` for a session on a local model, and
/// nothing for a hosted one, as the interactive `codex resume` takes them too.
pub fn local_mode_args(req: &SessionRequest) -> anyhow::Result<Vec<String>> {
    let mut args = local_provider_args(req.launch.as_ref())?;
    if args.is_empty() {
        return Ok(args);
    }
    if let Some(model) = session_model(req)? {
        args.extend(["--model".to_owned(), model.to_owned()]);
    }
    Ok(args)
}

/// `codex exec --oss` fails without a local provider, so one is always named.
fn local_provider_args(launch: Option<&LaunchHints>) -> anyhow::Result<Vec<String>> {
    let Some(launch) = launch else {
        return Ok(Vec::new());
    };
    let provider = present(launch.local_provider.as_deref());
    if !launch.oss {
        ensure!(
            provider.is_none(),
            "the Codex local provider {provider:?} needs oss = true"
        );
        return Ok(Vec::new());
    }
    let provider =
        provider.context("a local Codex model needs local_provider \"ollama\" or \"lmstudio\"")?;
    ensure!(
        runtime(provider).is_some_and(|runtime| runtime.codex_local),
        "Codex can't use the local provider {provider:?}; use ollama or lmstudio"
    );
    Ok(vec![
        "--oss".to_owned(),
        "--local-provider".to_owned(),
        provider.to_owned(),
    ])
}

fn launch_tweak_args(req: &SessionRequest) -> Vec<String> {
    let mut args = tweaks::context_window(req.context);
    if let Some(setting) = req.compaction {
        args.extend(tweaks::compaction(setting).args);
    }
    if let Some(setting) = req.native_fanout {
        args.extend(tweaks::native_fanout(setting).args);
        if is_judge_like(req.role, setting) {
            args.extend(tweaks::read_only_no_subagents());
        }
    }
    if req.user_hooks_off {
        args.extend(tweaks::user_hooks_off().args);
    }
    // The session you talk to keeps your memory.
    if req.role != SessionRole::Direct {
        args.extend(tweaks::memories_off());
    }
    args
}

fn is_judge_like(role: SessionRole, fanout: NativeFanout) -> bool {
    role == SessionRole::ReadOnly && tweaks::subagents_off(fanout)
}

fn refuse_unsafe_extra_args(extra_args: &[String]) -> anyhow::Result<()> {
    // Also catches attached values, such as `--sandbox=x` or `-sx`.
    let refused = extra_args.iter().find(|arg| {
        REFUSED_EXTRA_FLAGS
            .iter()
            .any(|flag| match arg.strip_prefix(flag) {
                Some(rest) if flag.starts_with("--") => rest.is_empty() || rest.starts_with('='),
                Some(_) => true,
                None => false,
            })
    });
    match refused {
        Some(arg) => anyhow::bail!("Argonauts Code never passes {arg} to Codex"),
        None => Ok(()),
    }
}

pub fn present(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

pub fn flag_value<'a>(what: &str, value: &'a str) -> anyhow::Result<&'a str> {
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

/// Kept in the scratch directory so it never ends up in the worktree, where Argonauts Code
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

pub fn utf8(path: &Path) -> anyhow::Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

pub fn output_schema_path(scratch_dir: &Path) -> PathBuf {
    scratch_dir.join(OUTPUT_SCHEMA_FILE)
}

#[cfg(test)]
#[path = "command_pictures_tests.rs"]
mod pictures_tests;
