//! The `claude -p` command line for one session.

use std::path::Path;

use anyhow::{bail, ensure};
use argonauts_core::worker::SessionShell;
use argonauts_core::{
    Asks, AuthMode, CommandSpec, SessionRequest, SessionRole, SpecFile, env_remove_except,
};

use crate::settings::{
    Allowed, CommandSandbox, Commands, LeftOut, OwnWorkflow, STATUS_SERVER, Style, session_settings,
};
use crate::tweaks::{self, Fanout, SUBAGENT_TOOL, WORKFLOW_TOOL};
use crate::{control, isolation};

/// Written into the session's scratch directory, outside the worktree.
pub const SETTINGS_FILE: &str = "claude-settings.json";

const READ_ONLY_TOOLS: &[&str] = &["Read", "Glob", "Grep"];
const BASH_TOOL: &str = "Bash";
const BUILD_TOOLS: &[&str] = &["Read", "Glob", "Grep", "Edit", "Write", "Bash"];
/// Newer models only get these with `CLAUDE_CODE_ENABLE_TODO_TOOLS=1`, and `--tools` must name
/// them too. They are how an owner's to-do list reaches the agent panel.
const TODO_TOOLS: &[&str] = &["TaskCreate", "TaskGet", "TaskUpdate", "TaskList"];

/// Claude Code's own workflows, and everything it would run in the background: a subagent or
/// command sent there, and a long one moved there on its own (2.1.295).
const BACKGROUND_SWITCHES: &[&str] = &[
    "CLAUDE_CODE_DISABLE_WORKFLOWS",
    "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS",
];

/// The tool on Argonauts' session server that Claude Code sends each permission prompt to.
pub const APPROVE_TOOL: &str = "approve";

/// How long Claude Code waits on one MCP call, in milliseconds. A card waits as long as you
/// take, so a question gets a day.
const ASKING_TIMEOUT_MS: &str = "86400000";

/// Messaging between Claude Code sessions on one machine (2.1.224 and later) would let blind
/// sessions reach each other outside the blackboard.
const CROSS_SESSION_TOOLS: &[&str] = &[SUBAGENT_MESSAGE_TOOL, "ListAgents"];

/// The tool the main agent continues one of its own subagents with, by the agent id its launch
/// returned. It is also how one session writes to another, which is why the rest go without.
const SUBAGENT_MESSAGE_TOOL: &str = "SendMessage";

/// An isolated session loads only your own settings. `-p` skips the trust dialog, so a
/// repository's `.claude/settings.json` would otherwise run its hooks, including ones an agent
/// wrote, and enable its plugins.
const ISOLATED_SETTING_SOURCES: &str = "user";

/// The key a subscription login may come as in headless setups, from `claude setup-token`.
pub(crate) const SUBSCRIPTION_KEYS: &[&str] = &["CLAUDE_CODE_OAUTH_TOKEN"];

const API_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
];

/// More of a parent Claude Code session's own state (seen in 2.1.285), on top of the core list,
/// so a worker started from inside Claude Code doesn't pass for that session.
pub const PARENT_SESSION_STATE: &[&str] = &[
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ATTENDED",
];

pub fn build_command(program: &str, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
    ensure!(
        !req.workdir.as_os_str().is_empty(),
        "the session needs a working directory"
    );
    ensure!(!req.prompt.trim().is_empty(), "the prompt is empty");
    if let Some(dir) = &req.scratch_dir {
        check_private_path("scratch directory", dir, &req.workdir)?;
    }
    if let Some(config) = &req.mcp_config {
        check_private_path("MCP config", config, &req.workdir)?;
    }

    let role = effective_role(req);
    let fanout = Fanout::from_setting(req.native_fanout);

    let mut spec = CommandSpec::new(program, &req.workdir).arg("-p");
    if req.keep_input_open {
        spec = spec.args(["--input-format", "stream-json"]);
    }
    spec = spec
        // stream-json needs --verbose under -p, or Claude Code exits 1.
        .args(["--output-format", "stream-json", "--verbose"])
        .arg("--include-partial-messages");
    spec = with_session_flag(spec, req)?;
    spec = with_permission_flags(spec, req, role, fanout);
    if let Some(config) = &req.mcp_config {
        spec = spec.args(["--mcp-config".to_owned(), path_arg(config)?]);
    }
    // Otherwise Claude Code loads its own agents, skills, plugins, and MCP servers, as it does
    // when you run it. An isolated session gets none: --tools doesn't cover MCP tools, so every
    // server but Argonauts' own is left out.
    if req.isolated {
        spec = spec
            .arg("--strict-mcp-config")
            .args(["--setting-sources", ISOLATED_SETTING_SOURCES]);
    }
    spec = with_settings(spec, req)?;
    // Added to Claude Code's own prompt, which stays whole. It is kept from a conversation's
    // first request on, so only lines that hold for the whole conversation go here.
    if let Some(note) = req.standing_note.as_deref().map(str::trim)
        && !note.is_empty()
    {
        spec = spec.args(["--append-system-prompt".to_owned(), note.to_owned()]);
    }
    if let Some(compaction) = req.compaction {
        spec = spec.args(tweaks::compaction(compaction).args);
    }
    spec = with_model_flags(spec, req)?.args(req.extra_args.iter().cloned());

    spec.env_remove = removed_env_vars(req);
    spec.env_set = env_vars(req, role, fanout)?;
    if req.keep_input_open {
        // Your conversation's session opens with `initialize`, whose reply lists the commands
        // and models, as Claude Code's own screen does.
        let opening = if req.main_session {
            control::initialize_line()
        } else {
            String::new()
        };
        let first = if !req.pictures.is_empty() {
            picture_message_line(&req.prompt, &req.pictures, req.typed_by_you)
        } else if req.typed_by_you {
            typed_message_line(&req.prompt)
        } else {
            user_message_line(&req.prompt)
        };
        spec.stdin = Some(opening + &first);
        spec.keep_stdin_open = true;
    } else {
        // On stdin, so a prompt never looks like a flag or shows in the process list. Plain
        // text can't hold a picture, so it names each one's file.
        let pictures: Vec<_> = req.pictures.iter().collect();
        spec.stdin = Some(argonauts_core::pictures::with_paths(&req.prompt, &pictures));
    }
    Ok(spec)
}

/// The generated settings go in a private file in the scratch directory, which also names the
/// session. Without one they are passed inline.
fn with_settings(mut spec: CommandSpec, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
    let commands = if req.tool_sandbox_off {
        CommandSandbox::Off
    } else {
        CommandSandbox::Own
    };
    let allowed = match (effective_role(req), req.asks) {
        (SessionRole::Direct, Asks::Never) => Allowed::Everything,
        (SessionRole::Direct, Asks::LikeCli | Asks::EveryChange) => Allowed::AsTheCliAsks,
        (SessionRole::ReadOnly | SessionRole::Build, _) => Allowed::Listed,
    };
    let own = own_workflow(
        req,
        effective_role(req),
        Fanout::from_setting(req.native_fanout),
    );
    let style = if req.autopilot {
        Style::Proactive
    } else {
        Style::Yours
    };
    let settings = session_settings(
        req.mcp_config.is_some(),
        commands,
        allowed,
        &left_out(req),
        &own,
        style,
        reader_commands(req),
    )?;
    let Some(dir) = &req.scratch_dir else {
        return Ok(spec.args(["--settings".to_owned(), settings]));
    };
    let path = dir.join(SETTINGS_FILE);
    let spec_path = path_arg(&path)?;
    spec.files.push(SpecFile {
        path,
        contents: settings,
    });
    spec = spec.args(["--settings".to_owned(), spec_path]);
    Ok(match session_name(dir) {
        Some(name) => spec.args(["--name".to_owned(), name]),
        None => spec,
    })
}

/// The commands a read-only session's Bash may run. A session with no Bash of its own has
/// nothing to limit.
fn reader_commands(req: &SessionRequest) -> Commands<'_> {
    match (&req.shell, effective_role(req)) {
        (SessionShell::Only(lines), SessionRole::ReadOnly) => Commands::Only(lines),
        _ => Commands::Any,
    }
}

/// Claude Code's own workflow, where the session may run one: a direct turn has the workflow
/// tool unless fan-out is off, and a build session when it may fan out as it sees fit. Where no
/// one is asked, a workflow runs without the review Claude Code otherwise asks for (2.1.293).
fn own_workflow(req: &SessionRequest, role: SessionRole, fanout: Fanout) -> OwnWorkflow {
    let (may_run, asks_you) = match role {
        SessionRole::Direct => (fanout != Fanout::Off, req.asks != Asks::Never),
        SessionRole::Build => (fanout.offers_workflows(), false),
        SessionRole::ReadOnly => (false, false),
    };
    if !may_run || req.background_off {
        return OwnWorkflow::default();
    }
    OwnWorkflow {
        ultracode: req.ultracode,
        size: req.workflow_size,
        runs_unasked: !asks_you,
    }
}

/// An isolated session gets none of your plugins, mods, hooks, or synced extras. Your hooks
/// alone go when `sandbox.user_hooks` says so.
fn left_out(req: &SessionRequest) -> LeftOut {
    LeftOut {
        hooks: req.isolated || req.user_hooks_off,
        plugins: req.isolated.then(|| {
            let dir = isolation::config_dir(req);
            isolation::plugins_to_turn_off(dir.as_deref(), &req.builtins_off)
        }),
    }
}

/// A build session needs the approved goal. Without one it gets the read-only flags, so nothing
/// is written before you approve the goal, even if the conductor's own check were missed.
pub fn effective_role(req: &SessionRequest) -> SessionRole {
    let approved = req
        .goal_hash
        .as_deref()
        .is_some_and(|h| !h.trim().is_empty());
    match req.role {
        SessionRole::Build if approved => SessionRole::Build,
        SessionRole::Direct if approved => SessionRole::Direct,
        _ => SessionRole::ReadOnly,
    }
}

/// A run's session gets a fixed tool list and asks nothing. A direct turn gets Claude Code's
/// default tools, and asks as you run it: your own default mode and rules decide what asks
/// (`LikeCli`), or every edit and command does (`EveryChange`), each question going to the
/// approval tool. With no tool to ask through, anything that would ask is refused.
fn with_permission_flags(
    spec: CommandSpec,
    req: &SessionRequest,
    role: SessionRole,
    fanout: Fanout,
) -> CommandSpec {
    // A project's settings can set another default mode, and a resume forgets the flag, so it
    // goes on every launch.
    let spec = match role {
        SessionRole::ReadOnly | SessionRole::Build => spec
            .args(["--permission-mode", permission_mode(role)])
            .args(["--permission-prompts", "none"])
            .args(["--tools".to_owned(), tool_list(role, fanout, req)]),
        SessionRole::Direct => {
            let asking = req.mcp_config.is_some() && req.asks != Asks::Never;
            let spec = match req.asks {
                Asks::Never => spec.args(["--permission-mode", "acceptEdits"]),
                Asks::LikeCli => spec,
                // 2.1.289 names the mode that asks about edits and commands `manual`.
                Asks::EveryChange => spec.args(["--permission-mode", "manual"]),
            };
            if asking {
                spec.args([
                    "--permission-prompt-tool".to_owned(),
                    format!("mcp__{STATUS_SERVER}__{APPROVE_TOOL}"),
                ])
            } else {
                spec.args(["--permission-prompts", "none"])
            }
        }
    };
    spec.arg("--disallowedTools").args(disallowed_tools(
        fanout,
        req.main_session,
        req.background_off,
    ))
}

/// One stream-json user message, the way `--input-format stream-json` reads them.
pub fn user_message_line(text: &str) -> String {
    let message = serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": text},
    });
    format!("{message}\n")
}

/// A user line marked as typed by a person, which Claude Code's `ultracode` keyword needs: the
/// same line without the mark doesn't trigger it (checked live on 2.1.293).
pub fn typed_message_line(text: &str) -> String {
    let message = serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": text},
        "origin": {"kind": "human"},
    });
    format!("{message}\n")
}

/// A user line holding your words and your pictures, each as an image block, as Claude Code's
/// own input sends a pasted picture. One that can't be sent goes as its file's path.
pub fn picture_message_line(
    text: &str,
    pictures: &[argonauts_core::pictures::Picture],
    typed: bool,
) -> String {
    let mut message = picture_message(text, pictures);
    if typed {
        message["origin"] = serde_json::json!({"kind": "human"});
    }
    format!("{message}\n")
}

/// The same with the uuid Claude Code's `command_lifecycle` lines name it by, for a message
/// that joins a running turn.
pub fn picture_message_line_with_uuid(
    text: &str,
    pictures: &[argonauts_core::pictures::Picture],
    uuid: &str,
) -> String {
    let mut message = picture_message(text, pictures);
    message["uuid"] = serde_json::json!(uuid);
    format!("{message}\n")
}

fn picture_message(
    text: &str,
    pictures: &[argonauts_core::pictures::Picture],
) -> serde_json::Value {
    let (blocks, unsent): (Vec<_>, Vec<_>) = pictures
        .iter()
        .map(|picture| (picture, picture.image_block()))
        .partition(|(_, block)| block.is_some());
    let unsent: Vec<_> = unsent.into_iter().map(|(picture, _)| picture).collect();
    let mut content = vec![serde_json::json!({
        "type": "text",
        "text": argonauts_core::pictures::with_paths(text, &unsent),
    })];
    content.extend(blocks.into_iter().filter_map(|(_, block)| block));
    serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": content},
    })
}

/// A user line with the uuid Claude Code's `command_lifecycle` lines name it by.
pub fn user_message_line_with_uuid(text: &str, uuid: &str) -> String {
    let message = serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": text},
        "uuid": uuid,
    });
    format!("{message}\n")
}

fn permission_mode(role: SessionRole) -> &'static str {
    match role {
        // Denies anything that would prompt, and the tool list has no way to write.
        SessionRole::ReadOnly => "dontAsk",
        SessionRole::Build | SessionRole::Direct => "acceptEdits",
    }
}

fn tool_list(role: SessionRole, fanout: Fanout, req: &SessionRequest) -> String {
    let background_off = req.background_off;
    let mut tools: Vec<&str> = match role {
        SessionRole::ReadOnly => READ_ONLY_TOOLS.to_vec(),
        SessionRole::Build | SessionRole::Direct => [BUILD_TOOLS, TODO_TOOLS].concat(),
    };
    // A reviewer that may run the checks, its settings holding Bash to them.
    if role == SessionRole::ReadOnly && !req.shell.is_none() {
        tools.push(BASH_TOOL);
    }
    if role == SessionRole::Build && fanout.offers_subagents() {
        tools.push(SUBAGENT_TOOL);
    }
    if role == SessionRole::Build && fanout.offers_workflows() && !background_off {
        tools.push(WORKFLOW_TOOL);
    }
    tools.join(",")
}

/// Your conversation's own session keeps `SendMessage` while it may start subagents: it's how
/// the main agent passes your message on to one of them, and resumes one that finished
/// (checked live on 2.1.292). It still can't list other sessions to write to,
/// and every session Argonauts starts refuses messages from outside.
fn disallowed_tools(fanout: Fanout, main_session: bool, background_off: bool) -> Vec<&'static str> {
    let mut tools = CROSS_SESSION_TOOLS.to_vec();
    if background_off {
        tools.push(WORKFLOW_TOOL);
    }
    if fanout == Fanout::Off {
        tools.push(SUBAGENT_TOOL);
    } else if main_session {
        tools.retain(|tool| *tool != SUBAGENT_MESSAGE_TOOL);
    }
    tools
}

fn with_session_flag(spec: CommandSpec, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
    match (&req.resume_session, &req.session_id) {
        (Some(resume), chosen) => {
            let resume = flag_value("session id", resume)?;
            if let Some(chosen) = chosen {
                // A different id would fork the session, which the adapter doesn't do.
                ensure!(
                    chosen == resume,
                    "can't resume session {resume} under a new id {chosen}"
                );
            }
            // Never --continue, which picks the latest session in the folder.
            Ok(spec.args(["--resume", resume]))
        }
        (None, Some(chosen)) => {
            ensure!(
                is_uuid(chosen),
                "Claude Code session ids are UUIDs, not {chosen:?}"
            );
            Ok(spec.args(["--session-id", chosen.as_str()]))
        }
        (None, None) => Ok(spec),
    }
}

fn with_model_flags(mut spec: CommandSpec, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
    if let Some(model) = &req.model {
        let model = tweaks::model_for(model, req.context, req.auth);
        spec = spec.args(["--model", flag_value("model", &model)?]);
    }
    if let Some(effort) = &req.effort {
        spec = spec.args(["--effort", flag_value("effort", effort)?]);
    }
    if let Some(schema) = &req.json_schema {
        ensure!(schema.is_object(), "the JSON schema must be an object");
        spec = spec.args(["--json-schema".to_owned(), schema.to_string()]);
    }
    if let Some(budget) = req.max_budget_usd {
        spec = spec.args(native_cap_args(budget)?);
    }
    Ok(spec)
}

/// `--max-budget-usd` is a backstop for the conductor's own meter. Subagent spend counts toward
/// it, and totals restored by `--resume` don't, so the caller passes what is left.
pub fn native_cap_args(budget_usd: f64) -> anyhow::Result<[String; 2]> {
    ensure!(
        budget_usd.is_finite() && budget_usd > 0.0,
        "Claude Code's spend limit must be a positive number of dollars, not {budget_usd}"
    );
    Ok(["--max-budget-usd".to_owned(), budget_usd.to_string()])
}

fn env_vars(
    req: &SessionRequest,
    role: SessionRole,
    fanout: Fanout,
) -> anyhow::Result<Vec<(String, String)>> {
    let mut vars = Vec::new();
    // Auto memory is shared by every worktree of a repository, so it would leak between cards.
    // A direct turn is Claude Code in your checkout, memory and all.
    if role != SessionRole::Direct {
        vars.push(("CLAUDE_CODE_DISABLE_AUTO_MEMORY".to_owned(), "1".to_owned()));
    }
    if role == SessionRole::Build {
        vars.push(("CLAUDE_CODE_ENABLE_TODO_TOOLS".to_owned(), "1".to_owned()));
    }
    if role == SessionRole::Direct && req.asks != Asks::Never && req.mcp_config.is_some() {
        // Yours would cut a card short.
        vars.push(("MCP_TOOL_TIMEOUT".to_owned(), ASKING_TIMEOUT_MS.to_owned()));
        // Since 2.1.281 a flagged rm is denied after two minutes in auto or bypass mode, which
        // your own default mode may be. Its card waits as long as you take, as every card does.
        vars.push((
            "CLAUDE_CODE_DISABLE_DANGEROUS_RM_TIMEOUT".to_owned(),
            "1".to_owned(),
        ));
    }
    if let Some(compaction) = req.compaction {
        vars.extend(tweaks::compaction(compaction).env_set);
    }
    if let Some(setting) = req.native_fanout
        && fanout != Fanout::Unset
    {
        vars.extend(tweaks::native_fanout(setting).env_set);
    }
    if req.background_off {
        // The session's answer is taken when its turn ends, and Claude Code stops what still
        // runs once its input closes, so a workflow or background agent would be cut short.
        for name in BACKGROUND_SWITCHES {
            if !vars.iter().any(|(set, _)| set == name) {
                vars.push(((*name).to_owned(), "1".to_owned()));
            }
        }
    }
    let removed = removed_env_vars(req);
    for (name, value) in &req.env_set {
        ensure!(
            !name.is_empty() && !name.contains('=') && !name.contains('\0'),
            "{name:?} isn't a valid environment variable name"
        );
        ensure!(
            !value.contains('\0'),
            "the value of {name} holds a NUL byte"
        );
        // Setting one of these would undo the auth mode or reconnect a parent session.
        ensure!(
            !removed.iter().any(|r| r == name),
            "{name} can't be set for a Claude Code session with this auth mode"
        );
        vars.push((name.clone(), value.clone()));
    }
    Ok(vars)
}

fn removed_env_vars(req: &SessionRequest) -> Vec<String> {
    // With -p an API key or auth token wins over the login, so under a subscription both go.
    let mut vars = match req.auth {
        AuthMode::Subscription => env_remove_except(SUBSCRIPTION_KEYS),
        AuthMode::ApiKey => env_remove_except(API_KEYS),
    };
    vars.extend(PARENT_SESSION_STATE.iter().map(|v| (*v).to_owned()));
    if req.effort.is_some() {
        // The environment variable outranks --effort.
        vars.push("CLAUDE_CODE_EFFORT_LEVEL".to_owned());
    }
    if let Some(compaction) = req.compaction {
        vars.extend(
            tweaks::compaction_env_remove(compaction)
                .iter()
                .map(|v| (*v).to_owned()),
        );
    }
    vars
}

/// The worker sets the scratch directory to the session folder, so its name tells you which
/// session a row in your own Claude Code agent list belongs to.
fn session_name(scratch_dir: &Path) -> Option<String> {
    let folder = scratch_dir.file_name()?.to_string_lossy();
    let folder: String = folder.chars().filter(|c| !c.is_control()).collect();
    let folder = folder.trim();
    (!folder.is_empty()).then(|| format!("argonauts {folder}"))
}

/// Files Claude Code reads at start must be absolute, or they'd resolve against the worktree,
/// and outside it, or the agent could rewrite its own settings.
fn check_private_path(what: &str, path: &Path, workdir: &Path) -> anyhow::Result<()> {
    ensure!(
        path.is_absolute(),
        "the {what} must be an absolute path, not {}",
        path.display()
    );
    ensure!(
        !path.starts_with(workdir),
        "the {what} {} is inside the worktree",
        path.display()
    );
    Ok(())
}

fn path_arg(path: &Path) -> anyhow::Result<String> {
    match path.to_str() {
        Some(text) => Ok(text.to_owned()),
        None => bail!("{} isn't valid UTF-8", path.display()),
    }
}

fn flag_value<'a>(what: &str, value: &'a str) -> anyhow::Result<&'a str> {
    ensure!(!value.trim().is_empty(), "the {what} is empty");
    ensure!(
        !value.starts_with('-'),
        "the {what} can't start with '-': {value}"
    );
    Ok(value)
}

/// `--session-id` takes only a UUID in the 8-4-4-4-12 hex form.
fn is_uuid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.chars().all(|c| c.is_ascii_hexdigit()))
}

#[cfg(test)]
#[path = "command_pictures_tests.rs"]
mod pictures_tests;
