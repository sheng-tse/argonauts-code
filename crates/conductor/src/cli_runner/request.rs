//! A session spec as the adapter's request: the role from the access, every launch flag again on
//! a resume, open input where steering needs it, the session id chosen up front where the CLI
//! allows it, a per-run project or a per-task home folder, compaction and fan-out, the status
//! tool's MCP config, launch hints from the registry, user hooks off where asked, and the CLI's own
//! memory off. `ARGONAUTS_SESSION=<key>` is set here, over anything in `spec.env`, and credentials
//! reach the CLI only where its adapter keeps them for its auth mode. The launch's own marker,
//! `ARGONAUTS_LAUNCH`, is set at spawn, so neither the spec nor the adapter may set it.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use argonauts_core::adapter::Transport;
use argonauts_core::answer_format;
use argonauts_core::driver::McpServerSpec;
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::layout::{self, session_files};
use argonauts_core::names::{ModelRef, SessionKey};
use argonauts_core::pictures::Picture;
use argonauts_core::registry::listed::nearest_listed;
use argonauts_core::registry::{
    LaunchHints, ModelAccess, ModelRegistry, RegistryKind, effort_to_run,
};
use argonauts_core::session_runner::{Access, Asking, RunnerError, SessionSpec, SteerSupport};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::UserHooks;
use argonauts_core::settings::fallback::AppliedFallback;
use argonauts_core::settings::sessions::{Compaction, FanoutSwitch, NativeFanout};
use argonauts_core::steps::StepName;
use argonauts_core::{
    Adapter, Asks, CommandSpec, PARENT_SESSION_ENV_VARS, PROVIDER_KEY_ENV_VARS, SessionRequest,
    SessionRole, create_private_dir_all, write_private_file,
};
use serde_json::{Map, Value, json};

use super::capabilities::{
    ANTIGRAVITY, CLAUDE_CODE, steer_support, takes_a_standing_note, takes_session_tools,
};
use super::launch::prompt_text;
use super::report_tool;
use crate::checks::env::looks_secret;
use crate::meter;
use crate::reaper::LAUNCH_MARKER;

/// Set on every CLI process. Approval paths refuse any caller that has it.
pub const SESSION_ENV: &str = "ARGONAUTS_SESSION";

/// Credentials that don't follow the `*_KEY` and `*_TOKEN` naming.
const CREDENTIAL_VARS: &[&str] = &["SSH_AUTH_SOCK", "DATABASE_URL"];

/// The folder in a task's own folder that holds the homes its ACP sessions keep for the task.
const TASK_HOMES: &str = "homes";

/// Where the session's MCP server finds the token it sends with each question.
pub const APPROVE_TOKEN_ENV: &str = "ARGONAUTS_APPROVE_TOKEN";

/// The name the status server has in every MCP config, which Claude Code lists in
/// `mcp_server_errors` when it fails to start.
pub const STATUS_SERVER: &str = "argonauts_status";

/// Only build, experiment, and direct sessions may change files; everything else gets the read-only
/// flags.
pub fn session_role(access: Access) -> SessionRole {
    match access {
        Access::Build | Access::Experiment => SessionRole::Build,
        Access::Direct => SessionRole::Direct,
        Access::NoTools | Access::ReadOnly => SessionRole::ReadOnly,
    }
}

/// What the session spends can go on after its turn where the meter doesn't see it: it works
/// as its CLI's own workflow, or it builds and may start the CLI's own workflows or agents,
/// which run on in the background and send no bill until a later result.
pub fn spends_unseen(spec: &SessionSpec) -> bool {
    let fans_out = match spec.native_fanout {
        NativeFanout::Switch(switch) => switch == FanoutSwitch::On,
        NativeFanout::Limit { max } => max > 0,
    };
    report_tool::reports_by_tool(spec)
        || (session_role(spec.access) == SessionRole::Build
            && fans_out
            && !spec.cli_workflow.background_off)
}

/// Writes the MCP config where the CLI takes one per run (Claude Code), so the command is
/// pure after this.
pub fn session_request(
    spec: &SessionSpec,
    adapter: &dyn Adapter,
    settings: &Settings,
    registry: Option<&dyn ModelRegistry>,
) -> Result<SessionRequest, RunnerError> {
    let agent = settings.agents.get(&spec.model.agent);
    let id = adapter.id();
    let by_tool = id == CLAUDE_CODE && report_tool::reports_by_tool(spec);
    let mcp_config = if id == CLAUDE_CODE {
        let io_error = |err: io::Error| RunnerError::Io(format!("the status tool's config: {err}"));
        let argonauts = std::env::current_exe().map_err(io_error)?;
        let (schema, report) = (
            report_tool::schema_file(spec),
            report_tool::report_file(spec),
        );
        if by_tool {
            report_tool::prepare(spec).map_err(io_error)?;
        }
        let report = by_tool.then_some(ReportFiles {
            schema: &schema,
            report: &report,
        });
        Some(
            write_session_mcp_config(
                &spec.session_dir,
                spec.status_file.as_deref(),
                report,
                spec.asking.as_ref(),
                &argonauts,
                ToolLoading::for_role(session_role(spec.access)),
            )
            .map_err(io_error)?,
        )
    } else {
        None
    };
    let launch = launch_hints(&spec.model, registry);
    let model = model_to_run(&spec.model, launch.as_ref(), adapter, settings, registry)?;
    // A level set before a model fixed at one was picked, as agy's level-named ids are, would
    // pick another model for agy, so the model's own levels win.
    let runs = ModelRef::new(spec.model.agent.clone(), model.clone());
    let effort = match registry {
        Some(registry) => effort_to_run(registry, &runs, spec.effort.clone()),
        None => spec.effort.clone(),
    };
    // The lines about its tools go where the CLI takes such a note. One with no such place
    // reads them ahead of your message, and one that gets no tools hears nothing of them.
    let standing = spec
        .asking
        .as_ref()
        .filter(|_| takes_session_tools(adapter))
        .and_then(|asking| asking.standing.clone());
    let takes_a_note = takes_a_standing_note(adapter);
    let prompt = match &standing {
        Some(standing) if !takes_a_note => {
            format!("{standing}\n\n{}", prompt_text(&spec.prompt))
        }
        _ => prompt_text(&spec.prompt),
    };
    let mut extra_args = agent
        .map(|agent| agent.extra_args.clone())
        .unwrap_or_default();
    // A flag you set yourself wins.
    if id == CLAUDE_CODE && !extra_args.iter().any(|arg| arg.starts_with(FALLBACK_MODEL)) {
        let rules = settings
            .fallback
            .applied_to(spec.role, &spec.model.agent_id());
        let models = own_fallback_models(&spec.model.agent, model.as_deref(), &rules, registry);
        if !models.is_empty() {
            extra_args.extend([FALLBACK_MODEL.to_owned(), models.join(",")]);
        }
    }
    Ok(SessionRequest {
        workdir: spec.workdir.clone(),
        prompt,
        pictures: pictures(&spec.prompt),
        model,
        effort,
        role: session_role(spec.access),
        auth: agent.map(|agent| agent.auth).unwrap_or_default(),
        // Codex refuses any other shape, and every CLI takes this one.
        json_schema: spec
            .json_schema
            .as_ref()
            .filter(|_| !by_tool)
            .map(answer_format::strict),
        resume_session: spec.resume.clone(),
        fork_session: spec.fork,
        // Claude Code's cap ignores what earlier runs of a resumed session spent, so it gets only
        // what's left.
        spend_cap_usd: meter::native_cap_usd(&spec.limits, meter::spent_before(spec, id).cost_usd),
        extra_args,
        scratch_dir: Some(spec.session_dir.clone()),
        goal_hash: spec.goal_sha256.clone(),
        keep_input_open: steer_support(adapter) == SteerSupport::Input,
        // A resume names its session with `--resume` instead.
        session_id: (id == CLAUDE_CODE && spec.resume.is_none())
            .then(|| uuid::Uuid::new_v4().to_string()),
        project: project(spec, adapter),
        // The largest window the CLI offers for the model, held to `sessions.context`.
        context: settings.sessions.context.ask(
            registry
                .and_then(|registry| registry.get(&runs))
                .and_then(|entry| entry.context_window),
        ),
        compaction: (spec.compaction != Compaction::Inherit).then_some(spec.compaction),
        native_fanout: Some(spec.native_fanout),
        mcp_config,
        env_set: session_env_pairs(&spec.key, &spec.env),
        launch,
        user_hooks_off: settings.sandbox.user_hooks == UserHooks::Disable,
        isolated: spec.isolated,
        // The launch reads what earlier isolated sessions learned, and sets this.
        builtins_off: Vec::new(),
        // The launch knows which sandbox wraps the session, and sets this.
        tool_sandbox_off: false,
        asks: spec
            .asking
            .as_ref()
            .map(|asking| asking.asks)
            .unwrap_or_default(),
        cli_command: spec.step == Some(StepName::CliCommand),
        main_session: is_main_session(spec),
        ultracode: spec.cli_workflow.ultracode,
        typed_by_you: spec.cli_workflow.typed_keyword,
        background_off: spec.cli_workflow.background_off,
        shell: spec.shell.clone(),
        preferences: spec.preferences.clone(),
        workflow_size: Some(settings.workflow_size),
        standing_note: standing.filter(|_| takes_a_note),
        autopilot: spec.autopilot,
    })
}

/// A turn of your conversation: no run or task owns it, and the conversation's steps run it.
/// Claude Code's own flag for the models it moves to inside a turn when its model is overloaded
/// or gone, tried in order, without the session ending.
const FALLBACK_MODEL: &str = "--fallback-model";

/// The models the CLI of `agent` may move to by itself, for a role whose fallbacks stay on one
/// CLI (`fallback.same_cli`): the role's listed models on that CLI, else the registry's other
/// ones, never the model the session runs. Empty for a role free to move to another CLI, which
/// falls back between sessions instead.
fn own_fallback_models(
    agent: &str,
    running: Option<&str>,
    rules: &AppliedFallback,
    registry: Option<&dyn ModelRegistry>,
) -> Vec<String> {
    if !rules.same_cli {
        return Vec::new();
    }
    let listed = rules.candidates.iter().map(|entry| entry.model().clone());
    let mut models: Vec<String> = listed
        .filter(|model| model.agent == agent)
        .filter_map(|model| model.model)
        .collect();
    if models.is_empty() {
        let known = registry.map(|registry| registry.list()).unwrap_or_default();
        models = known
            .into_iter()
            .filter(|entry| !entry.retired && entry.id.agent == agent)
            .filter_map(|entry| entry.id.model)
            .collect();
    }
    let mut kept: Vec<String> = Vec::new();
    for model in models {
        if Some(model.as_str()) != running && !kept.contains(&model) {
            kept.push(model);
        }
    }
    kept
}

fn is_main_session(spec: &SessionSpec) -> bool {
    spec.run.is_none()
        && spec.task.is_none()
        && !spec.isolated
        && matches!(
            spec.step,
            Some(
                StepName::Estimate
                    | StepName::Direct
                    | StepName::ConversationAnswer
                    | StepName::CliCommand
            )
        )
}

fn launch_hints(worker: &ModelRef, registry: Option<&dyn ModelRegistry>) -> Option<LaunchHints> {
    registry
        .and_then(|registry| registry.get(worker))
        .and_then(|entry| entry.launch)
}

/// The model a session of `worker` names: its own, else `agents.<id>.model`. A CLI whose own
/// default runs on a hosted service gets a local one rather than none, and is refused when
/// there is none, unless a launch hint names the model for it.
fn model_to_run(
    worker: &ModelRef,
    launch: Option<&LaunchHints>,
    adapter: &dyn Adapter,
    settings: &Settings,
    registry: Option<&dyn ModelRegistry>,
) -> Result<Option<String>, RunnerError> {
    let id = adapter.id();
    let model = worker.model.clone().or_else(|| {
        let agent = settings.agents.get(&worker.agent)?;
        agent.model.clone()
    });
    // A model its CLI no longer lists would only be refused, so the nearest it lists runs.
    let model = match (registry, &model) {
        (Some(registry), Some(_)) => {
            let pinned = ModelRef::new(worker.agent.clone(), model.clone());
            nearest_listed(registry, &pinned)
                .and_then(|nearest| nearest.model)
                .or(model)
        }
        _ => model,
    };
    let hinted = launch.is_some_and(|launch| launch.provider_model.is_some());
    if model.is_some() || hinted || !runs_hosted_default(adapter) {
        return Ok(model);
    }
    let local = local_model(id, registry).ok_or_else(|| {
        RunnerError::Refused(format!(
            "{} has no model set, and its own default runs on a hosted service, so the code \
             would leave this machine. Set agents.{id}.model, such as ollama/<model>, or add a \
             local model to models.toml.",
            adapter.display_name()
        ))
    })?;
    Ok(Some(local))
}

/// Why a session of `worker` would be refused before it starts, as [`session_request`]
/// refuses it, without writing anything: OpenCode with no model set and no local one known.
pub fn start_refusal(
    worker: &ModelRef,
    adapter: &dyn Adapter,
    settings: &Settings,
    registry: Option<&dyn ModelRegistry>,
) -> Option<String> {
    let launch = launch_hints(worker, registry);
    match model_to_run(worker, launch.as_ref(), adapter, settings, registry) {
        Err(RunnerError::Refused(why)) => Some(why),
        _ => None,
    }
}

/// OpenCode, over its CLI or ACP, runs OpenCode Zen's hosted `opencode/big-pickle` when it's
/// given no model (1.18.34), whatever providers you set up.
fn runs_hosted_default(adapter: &dyn Adapter) -> bool {
    Path::new(adapter.binary()).file_name() == Some(OsStr::new("opencode"))
}

/// The first local model OpenCode can run, as its `provider/model`: one of its own entries
/// marked local, else a model on your Ollama or LM Studio endpoint. Models with tools come
/// first, since most sessions edit files.
fn local_model(cli: &str, registry: Option<&dyn ModelRegistry>) -> Option<String> {
    const OPENCODE_LOCAL_PROVIDERS: &[&str] = &["ollama", "lmstudio"];
    let mut found: Vec<(bool, bool, String)> = registry?
        .list()
        .into_iter()
        .filter(|entry| entry.access == ModelAccess::Local && !entry.retired)
        .filter_map(|entry| {
            let model = entry.id.model.as_deref()?;
            let route = if entry.id.agent == cli {
                entry
                    .launch
                    .as_ref()
                    .and_then(|launch| launch.provider_model.clone())
                    .unwrap_or_else(|| model.to_owned())
            } else if entry.kind == RegistryKind::Endpoint
                && OPENCODE_LOCAL_PROVIDERS.contains(&entry.id.agent.as_str())
            {
                format!("{}/{model}", entry.id.agent)
            } else {
                return None;
            };
            Some((entry.id.agent != cli, !entry.tools, route))
        })
        .collect();
    // Its own entries before the endpoints', then by tools, each in the registry's order.
    found.sort_by_key(|(endpoint, no_tools, _)| (*endpoint, *no_tools));
    found.into_iter().map(|(.., route)| route).next()
}

/// agy shares one project per run. An ACP agent gets the folder where a home kept for the task
/// lives, so a later session of the task, such as a resume, finds the same home. Its adapter
/// uses it only for a `keep = "task"` home.
fn project(spec: &SessionSpec, adapter: &dyn Adapter) -> Option<String> {
    if adapter.id() == ANTIGRAVITY {
        return spec.run.as_ref().map(|run| run.as_str().to_owned());
    }
    if adapter.transport() == Transport::Acp {
        return task_home(spec, adapter.id()).map(|home| home.display().to_string());
    }
    None
}

/// `<run>/tasks/<task>/homes/<role>/<cli>`, for a session on a task whose folder sits in its
/// run's sessions. Each role gets its own, since the home's config is different for each role, and
/// every session of a role on the task gets the same one.
pub fn task_home(spec: &SessionSpec, cli: &str) -> Option<PathBuf> {
    let task = spec.task.as_ref()?;
    let run_dir = spec.session_dir.parent()?.parent()?;
    if layout::session_dir(run_dir, &spec.key) != spec.session_dir {
        return None;
    }
    Some(
        layout::task_dir(run_dir, task)
            .join(TASK_HOMES)
            .join(spec.role.as_str())
            .join(cli),
    )
}

/// The task's home folder from [`project`], when the launch points the agent at it: the adapter
/// keeps that home for the task.
pub fn task_home_in_use(request: &SessionRequest, command: &CommandSpec) -> Option<PathBuf> {
    let home = Path::new(request.project.as_deref()?);
    let named = command
        .env_set
        .iter()
        .map(|(_, value)| value)
        .chain(&command.args)
        .any(|value| Path::new(value) == home);
    (home.is_absolute() && named).then(|| home.to_path_buf())
}

/// Writes `<session_dir>/mcp.json`, registering only `argonauts mcp session` over the status
/// file, and returns its path.
pub fn write_mcp_config(
    session_dir: &Path,
    status_file: &Path,
    argonauts: &Path,
) -> io::Result<PathBuf> {
    write_session_mcp_config(
        session_dir,
        Some(status_file),
        None,
        None,
        argonauts,
        ToolLoading::Deferred,
    )
}

/// Whether Claude Code may hold the status server's tools back behind ToolSearch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolLoading {
    /// Loaded in full with every request. A run's session names its tools, ToolSearch among
    /// them, and Claude Code then defers every MCP tool, while the run's own report, status and
    /// asks must be in front of the model from its first turn.
    Always,
    /// As Claude Code chooses, as in the conversation's own session.
    Deferred,
}

impl ToolLoading {
    pub fn for_role(role: SessionRole) -> Self {
        match role {
            SessionRole::ReadOnly | SessionRole::Build => Self::Always,
            SessionRole::Direct => Self::Deferred,
        }
    }
}

/// Like [`write_mcp_config`], with no servers at all when there is no status file, so a strict
/// config still leaves out your own MCP servers.
pub fn write_session_mcp_config(
    session_dir: &Path,
    status_file: Option<&Path>,
    report: Option<ReportFiles<'_>>,
    asking: Option<&Asking>,
    argonauts: &Path,
    loading: ToolLoading,
) -> io::Result<PathBuf> {
    let mut servers = Map::new();
    if let Some(server) = session_server(argonauts, status_file, report, asking) {
        let mut entry = json!({
            "type": "stdio",
            "command": server.command,
            "args": server.args,
        });
        if let Some(asking) = asking {
            entry["env"] = json!({ APPROVE_TOKEN_ENV: asking.token });
        }
        // Claude Code also waits for such a server to start, at most five seconds, so its
        // tools are there for the first turn.
        if loading == ToolLoading::Always {
            entry["alwaysLoad"] = json!(true);
        }
        servers.insert(server.name, entry);
    }
    let config = json!({ "mcpServers": Value::Object(servers) });
    create_private_dir_all(session_dir)?;
    let path = session_dir.join(session_files::MCP_CONFIG);
    let text = serde_json::to_string_pretty(&config).map_err(io::Error::other)?;
    write_private_file(&path, text.as_bytes())?;
    Ok(path)
}

/// Where a report tool writes: the step's schema and `report.json`.
#[derive(Debug, Clone, Copy)]
pub struct ReportFiles<'a> {
    pub schema: &'a Path,
    pub report: &'a Path,
}

/// `<argonauts> mcp session`, with `--status-file <path>` for the status tool and the report
/// tool's files when a step has an output schema. `None` when it would serve nothing.
pub fn session_server(
    argonauts: &Path,
    status_file: Option<&Path>,
    report: Option<ReportFiles<'_>>,
    asking: Option<&Asking>,
) -> Option<McpServerSpec> {
    if status_file.is_none() && report.is_none() && asking.is_none() {
        return None;
    }
    let mut args = vec!["mcp".to_owned(), "session".to_owned()];
    if let Some(status_file) = status_file {
        args.extend([
            "--status-file".to_owned(),
            status_file.display().to_string(),
        ]);
    }
    if let Some(report) = report {
        args.extend([
            "--report-schema".to_owned(),
            report.schema.display().to_string(),
            "--report-file".to_owned(),
            report.report.display().to_string(),
        ]);
    }
    if let Some(asking) = asking {
        // The token goes in the environment, so it isn't in the process list.
        args.extend(["--ask".to_owned(), asking.endpoint.clone()]);
        if asking.asks != Asks::Never {
            args.push("--approve-tool".to_owned());
        }
        for tool in &asking.tools {
            args.extend(["--tool".to_owned(), tool.clone()]);
        }
        for role in &asking.roles {
            args.extend(["--ask-role".to_owned(), role.as_str().to_owned()]);
        }
    }
    Some(McpServerSpec {
        name: STATUS_SERVER.to_owned(),
        command: argonauts.to_path_buf(),
        args,
    })
}

fn is_reserved(name: &str) -> bool {
    name == SESSION_ENV || name == LAUNCH_MARKER || PARENT_SESSION_ENV_VARS.contains(&name)
}

/// Names that hold credentials: the agent keys, anything named like a key, token, secret, or
/// password, AWS's variables, the SSH agent, and a database URL. An agent can print its
/// environment into a transcript the agent keeps, so a CLI gets only the ones its adapter
/// keeps for its auth mode, as a check gets none.
fn is_credential(name: &str) -> bool {
    PROVIDER_KEY_ENV_VARS.contains(&name)
        || looks_secret(name)
        || name.starts_with("AWS_")
        || CREDENTIAL_VARS.contains(&name)
}

/// `spec.env` without anything the runner owns and without credentials other than agent keys,
/// which the adapter keeps or drops by its auth mode, then `ARGONAUTS_SESSION=<key>`.
fn session_env_pairs(key: &SessionKey, env: &[(String, String)]) -> Vec<(String, String)> {
    env.iter()
        .filter(|(name, _)| {
            !is_reserved(name)
                && (!is_credential(name) || PROVIDER_KEY_ENV_VARS.contains(&name.as_str()))
        })
        .cloned()
        .chain([(SESSION_ENV.to_owned(), key.to_string())])
        .collect()
}

/// Applied to the adapter's final command, so no adapter can forget it: `spec.env` where the
/// adapter set nothing, `ARGONAUTS_SESSION=<key>` over anything else, every parent-session
/// variable removed, and every credential this process has removed unless the adapter keeps it.
pub fn apply_session_env(command: &mut CommandSpec, key: &SessionKey, env: &[(String, String)]) {
    let inherited: Vec<String> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .collect();
    apply_env(command, key, env, &inherited);
}

/// [`apply_session_env`] with the names this process inherited.
fn apply_env(
    command: &mut CommandSpec,
    key: &SessionKey,
    env: &[(String, String)],
    inherited: &[String],
) {
    command.env_set.retain(|(name, _)| !is_reserved(name));
    // What the adapter set itself, and the agent keys it didn't remove, are what it keeps.
    let set_by_adapter: Vec<String> = command.env_set.iter().map(|(n, _)| n.clone()).collect();
    let removed_by_adapter = command.env_remove.clone();
    let kept = |name: &str| {
        set_by_adapter.iter().any(|set| set == name)
            || (PROVIDER_KEY_ENV_VARS.contains(&name)
                && !removed_by_adapter.iter().any(|removed| removed == name))
    };
    for (name, value) in env {
        let refused = is_reserved(name)
            || removed_by_adapter.contains(name)
            || (is_credential(name) && !kept(name));
        if !refused && !set_by_adapter.contains(name) {
            command.env_set.push((name.clone(), value.clone()));
        }
    }
    command
        .env_set
        .push((SESSION_ENV.to_owned(), key.to_string()));
    command.env_remove.retain(|name| name != SESSION_ENV);
    let dropped = inherited
        .iter()
        .map(String::as_str)
        .filter(|name| is_credential(name) && !kept(name))
        .chain(PARENT_SESSION_ENV_VARS.iter().copied());
    for name in dropped {
        if !command.env_remove.iter().any(|removed| removed == name) {
            command.env_remove.push(name.to_owned());
        }
    }
}

/// The pictures sent with your message, numbered as their chips were.
fn pictures(parts: &[InputPart]) -> Vec<Picture> {
    parts
        .iter()
        .filter(|part| part.kind == InputPartKind::Picture)
        .filter_map(|part| {
            let number = part
                .label
                .trim_start_matches("[Image #")
                .trim_end_matches(']')
                .parse()
                .ok()?;
            let path = part.source.as_ref()?.into();
            Some(Picture { number, path })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_runner::testing::{TestAdapter, spec_in};
    use argonauts_core::names::{ModelRef, RoleName};
    use argonauts_core::registry::{CostModel, CostUnit, ModelEntry};
    use argonauts_core::settings::agents::AgentSettings;
    use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
    use argonauts_core::{RunId, Spend};
    use argonauts_testkit::services::StaticRegistry;

    fn request_for(adapter: &TestAdapter, spec: &SessionSpec) -> SessionRequest {
        session_request(spec, adapter, &Settings::default(), None).unwrap()
    }

    #[test]
    fn read_only_flags_until_approved() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = TestAdapter::new("true");
        let mut spec = spec_in(dir.path(), dir.path(), "look");
        for (access, role) in [
            (Access::NoTools, SessionRole::ReadOnly),
            (Access::ReadOnly, SessionRole::ReadOnly),
            (Access::Build, SessionRole::Build),
            (Access::Experiment, SessionRole::Build),
        ] {
            spec.access = access;
            assert_eq!(request_for(&adapter, &spec).role, role, "{access:?}");
        }
    }

    /// A format that leaves a field optional goes out with every field required and the
    /// optional one allowed to be null, which Codex's strict mode needs.
    #[test]
    fn the_answer_format_goes_out_strict() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = TestAdapter::new("true");
        let mut spec = spec_in(dir.path(), dir.path(), "review it");
        spec.json_schema = Some(json!({
            "type": "object",
            "properties": {"accept": {"type": "boolean"}, "summary": {"type": "string"}},
            "required": ["accept"],
        }));
        let sent = request_for(&adapter, &spec).json_schema.unwrap();
        assert_eq!(answer_format::codex_problems(&sent), Vec::<String>::new());
        assert_eq!(sent["required"], json!(["accept", "summary"]));
        assert_eq!(
            sent["properties"]["summary"]["type"],
            json!(["string", "null"])
        );
    }

    /// The main session's tools go to its server by name, and the standing lines go where the
    /// CLI takes such a note. A CLI that gets no tools hears nothing of them.
    #[test]
    fn the_main_session_s_tools_and_standing_lines_reach_a_cli_that_takes_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec_in(dir.path(), dir.path(), "add due dates");
        spec.access = Access::Direct;
        let asking = Asking {
            asks: Asks::Never,
            endpoint: "127.0.0.1:41234".to_owned(),
            token: "t".to_owned(),
            tools: vec!["argonauts_start_workflow".to_owned()],
            standing: Some("Hand real work to the crew.".to_owned()),
            roles: Vec::new(),
        };
        let server = session_server(Path::new("/bin/argonauts"), None, None, Some(&asking))
            .expect("a server for the tools");
        assert_eq!(
            server.args,
            [
                "mcp",
                "session",
                "--ask",
                "127.0.0.1:41234",
                "--tool",
                "argonauts_start_workflow"
            ]
        );
        spec.asking = Some(asking);

        let claude = TestAdapter::new("true").with_id(CLAUDE_CODE);
        let request = request_for(&claude, &spec);
        assert_eq!(
            request.standing_note.as_deref(),
            Some("Hand real work to the crew.")
        );
        assert_eq!(request.prompt, "add due dates");

        let other = TestAdapter::new("true").with_id("codex");
        let request = request_for(&other, &spec);
        assert_eq!(request.standing_note, None);
        assert_eq!(request.prompt, "add due dates");
    }

    /// The adapter hears that the session's run is on autopilot, and picks its CLI's own way
    /// of working without stopping.
    #[test]
    fn a_session_on_autopilot_says_so_to_its_adapter() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec_in(dir.path(), dir.path(), "add due dates");
        let claude = TestAdapter::new("true").with_id(CLAUDE_CODE);
        assert!(!request_for(&claude, &spec).autopilot);
        spec.autopilot = true;
        assert!(request_for(&claude, &spec).autopilot);
    }

    #[test]
    fn a_blind_or_isolated_session_tells_the_adapter() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = TestAdapter::new("true");
        let mut spec = spec_in(dir.path(), dir.path(), "look");
        assert!(!request_for(&adapter, &spec).isolated);
        spec.isolated = true;
        assert!(request_for(&adapter, &spec).isolated);
    }

    fn local_entry(worker: &str, model: &str, kind: RegistryKind, tools: bool) -> ModelEntry {
        ModelEntry {
            id: ModelRef::new(worker, Some(model.to_owned())),
            provider: "meta".into(),
            kind,
            access: ModelAccess::Local,
            tools,
            cost: CostModel {
                unit: CostUnit::None,
                input_per_mtok: None,
                output_per_mtok: None,
                cached_input_per_mtok: None,
            },
            strengths: Vec::new(),
            good_for: Vec::new(),
            context_window: None,
            efforts: Vec::new(),
            aliases: Vec::new(),
            retired: false,
            successor: None,
            discovered: true,
            cli_default: false,
            default_effort: None,
            launch: None,
            tier: None,
            name: None,
            description: None,
        }
    }

    /// A model your settings pin that its CLI no longer lists runs as the nearest one it
    /// does, in a session of its own as in a run's.
    #[test]
    fn a_pinned_model_its_cli_no_longer_lists_runs_the_nearest_listed_one() {
        let dir = tempfile::tempdir().unwrap();
        let agy = TestAdapter::new("true").with_id("antigravity-cli");
        let mut spec = spec_in(dir.path(), dir.path(), "fix it");
        spec.model = "antigravity-cli/gemini-3.6-flash-low".parse().unwrap();
        let registry = StaticRegistry::default();
        for model in ["gemini-3.8-flash-high", "gemini-3.8-flash-low"] {
            registry.add(local_entry(
                "antigravity-cli",
                model,
                RegistryKind::Cli,
                true,
            ));
        }
        let settings = Settings::default();
        let request = session_request(&spec, &agy, &settings, Some(&registry)).unwrap();
        assert_eq!(request.model.as_deref(), Some("gemini-3.8-flash-low"));

        // One it lists runs as it is.
        spec.model = "antigravity-cli/gemini-3.8-flash-high".parse().unwrap();
        let request = session_request(&spec, &agy, &settings, Some(&registry)).unwrap();
        assert_eq!(request.model.as_deref(), Some("gemini-3.8-flash-high"));
    }

    #[test]
    fn opencode_with_no_model_runs_a_local_one_or_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let spec = |worker: &str| {
            let mut spec = spec_in(dir.path(), dir.path(), "fix it");
            spec.model = worker.parse().unwrap();
            spec
        };
        let opencode = TestAdapter::new("true")
            .with_id("opencode-acp")
            .with_binary("opencode");
        let settings = Settings::default();

        // Nothing local known: it asks for a model instead of running OpenCode's hosted one.
        let empty = StaticRegistry::default();
        let err = session_request(&spec("opencode-acp"), &opencode, &settings, Some(&empty))
            .unwrap_err()
            .to_string();
        assert!(err.contains("agents.opencode-acp.model"), "{err}");
        assert!(err.contains("hosted"), "{err}");
        let err = session_request(&spec("opencode-acp"), &opencode, &settings, None).unwrap_err();
        assert!(matches!(err, RunnerError::Refused(_)), "{err}");

        // A local endpoint's model, preferring one with tools; hosted entries never count.
        let registry = StaticRegistry::default();
        let mut hosted = local_entry("openrouter", "z-ai/glm-5", RegistryKind::Endpoint, true);
        hosted.access = ModelAccess::ApiKey;
        registry.add(hosted);
        registry.add(local_entry("vllm", "qwen3", RegistryKind::Endpoint, true));
        registry.add(local_entry(
            "ollama",
            "gemma4:e2b",
            RegistryKind::Endpoint,
            false,
        ));
        registry.add(local_entry(
            "ollama",
            "llama3.2",
            RegistryKind::Endpoint,
            true,
        ));
        let request =
            session_request(&spec("opencode-acp"), &opencode, &settings, Some(&registry)).unwrap();
        assert_eq!(request.model.as_deref(), Some("ollama/llama3.2"));

        // Its own local entry comes first.
        registry.add(local_entry(
            "opencode-acp",
            "lmstudio/qwen3-8b",
            RegistryKind::Cli,
            false,
        ));
        let request =
            session_request(&spec("opencode-acp"), &opencode, &settings, Some(&registry)).unwrap();
        assert_eq!(request.model.as_deref(), Some("lmstudio/qwen3-8b"));

        // A model you set is kept, hosted or not, and other CLIs are left alone.
        let request = session_request(
            &spec("opencode-acp/opencode/big-pickle"),
            &opencode,
            &settings,
            Some(&empty),
        )
        .unwrap();
        assert_eq!(request.model.as_deref(), Some("opencode/big-pickle"));
        let mut chosen = Settings::default();
        chosen.agents.insert(
            "opencode-acp".into(),
            AgentSettings {
                model: Some("anthropic/claude-sonnet-4-6".into()),
                ..AgentSettings::default()
            },
        );
        let request =
            session_request(&spec("opencode-acp"), &opencode, &chosen, Some(&empty)).unwrap();
        assert_eq!(
            request.model.as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );
        let other = TestAdapter::new("true");
        let request = session_request(&spec("fake"), &other, &settings, Some(&empty)).unwrap();
        assert_eq!(request.model, None);
    }

    /// A fallback reads the refusal before starting anything, in the words a start would give.
    #[test]
    fn opencode_with_no_model_is_known_to_refuse_before_it_starts() {
        let dir = tempfile::tempdir().unwrap();
        let opencode = TestAdapter::new("true")
            .with_id("opencode-acp")
            .with_binary("opencode");
        let unset: ModelRef = "opencode-acp".parse().unwrap();
        let settings = Settings::default();
        let empty = StaticRegistry::default();

        let why = start_refusal(&unset, &opencode, &settings, Some(&empty)).expect("a refusal");
        let mut spec = spec_in(dir.path(), dir.path(), "fix it");
        spec.model = unset.clone();
        let refused = session_request(&spec, &opencode, &settings, Some(&empty)).unwrap_err();
        assert_eq!(refused, RunnerError::Refused(why));

        // A model set, a local one to run, or a CLI whose default stays here starts.
        let set: ModelRef = "opencode-acp/ollama/llama3.2".parse().unwrap();
        assert_eq!(
            start_refusal(&set, &opencode, &settings, Some(&empty)),
            None
        );
        let mut chosen = Settings::default();
        chosen.agents.insert(
            "opencode-acp".into(),
            AgentSettings {
                model: Some("ollama/llama3.2".into()),
                ..AgentSettings::default()
            },
        );
        assert_eq!(
            start_refusal(&unset, &opencode, &chosen, Some(&empty)),
            None
        );
        let local = StaticRegistry::default();
        local.add(local_entry(
            "ollama",
            "llama3.2",
            RegistryKind::Endpoint,
            true,
        ));
        assert_eq!(
            start_refusal(&unset, &opencode, &settings, Some(&local)),
            None
        );
        let other = TestAdapter::new("true");
        let fake: ModelRef = "fake".parse().unwrap();
        assert_eq!(start_refusal(&fake, &other, &settings, Some(&empty)), None);
    }

    /// A level set before a model fixed at one was picked never reaches the CLI: agy would run
    /// the sibling model named for it. A model with levels to pick from runs at the one set.
    #[test]
    fn a_model_fixed_at_one_level_runs_at_it_whatever_is_set() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = TestAdapter::new("true");
        let registry = StaticRegistry::default();
        let mut fixed = local_entry("fake", "flash-high", RegistryKind::Cli, true);
        fixed.efforts = vec!["high".into()];
        registry.add(fixed);
        let mut picks = local_entry("fake", "pro", RegistryKind::Cli, true);
        picks.efforts = vec!["low".into(), "high".into()];
        registry.add(picks);
        let settings = Settings::default();
        let mut spec = spec_in(dir.path(), dir.path(), "go");
        spec.effort = Some("low".into());
        for (worker, effort) in [
            ("fake/flash-high", Some("high")),
            ("fake/pro", Some("low")),
            ("fake/unlisted", Some("low")),
            ("fake", Some("low")),
        ] {
            spec.model = worker.parse().unwrap();
            let request = session_request(&spec, &adapter, &settings, Some(&registry)).unwrap();
            assert_eq!(request.effort.as_deref(), effort, "{worker}");
        }
    }

    #[test]
    fn a_role_that_stays_on_its_cli_names_the_models_the_cli_may_move_to() {
        use argonauts_core::settings::fallback::FallbackEntry;
        let entry = |model: &str| FallbackEntry::Model(model.parse().unwrap());
        let registry = StaticRegistry::default();
        for model in ["opus", "sonnet", "haiku"] {
            registry.add(local_entry("claude-code", model, RegistryKind::Cli, true));
        }
        let models = |rules: &AppliedFallback| {
            own_fallback_models("claude-code", Some("opus"), rules, Some(&registry))
        };

        // Free to move to another CLI, it falls back between sessions instead.
        let mut rules = AppliedFallback::default();
        assert!(models(&rules).is_empty());

        // Staying on its CLI with no list, the CLI's other models in the registry's order.
        rules.same_cli = true;
        assert_eq!(models(&rules), ["sonnet", "haiku"]);

        // A list names them in your order, and another CLI's models are passed over.
        rules.candidates = vec![
            entry("codex/gpt-5.6"),
            entry("claude-code/haiku"),
            entry("claude-code/opus"),
            entry("claude-code/haiku"),
        ];
        assert_eq!(models(&rules), ["haiku"]);

        // Without a registry and with nothing listed, there is nothing to name.
        rules.candidates.clear();
        assert!(own_fallback_models("claude-code", None, &rules, None).is_empty());
    }

    #[test]
    fn resume_passes_every_flag_again() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = TestAdapter::new("true");
        let mut settings = Settings::default();
        let agent = AgentSettings {
            model: Some("big".into()),
            extra_args: vec!["--quiet".into()],
            ..AgentSettings::default()
        };
        settings.agents.insert("fake".into(), agent);
        let mut spec = spec_in(dir.path(), dir.path(), "go on");
        spec.effort = Some("low".into());
        let fresh = session_request(&spec, &adapter, &settings, None).unwrap();
        spec.resume = Some("cli-1".into());
        let resumed = session_request(&spec, &adapter, &settings, None).unwrap();
        assert_eq!(resumed.resume_session.as_deref(), Some("cli-1"));
        assert_eq!(
            SessionRequest {
                resume_session: None,
                ..resumed.clone()
            },
            fresh
        );
        assert_eq!(resumed.model.as_deref(), Some("big"));
        assert_eq!(resumed.extra_args, ["--quiet"]);
        assert_eq!(resumed.effort.as_deref(), Some("low"));
        assert_eq!(resumed.scratch_dir.as_deref(), Some(dir.path()));
    }

    #[test]
    fn a_resume_caps_only_whats_left() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = TestAdapter::new("true");
        let mut spec = spec_in(dir.path(), dir.path(), "go on");
        spec.limits.limit = Some(LimitAmount {
            unit: LimitUnit::Usd,
            amount: 2.0,
        });
        assert_eq!(request_for(&adapter, &spec).spend_cap_usd, Some(2.2));
        let spent = Spend {
            cost_usd: 0.6,
            ..Spend::default()
        };
        meter::record_cli_session_spend(dir.path(), adapter.id(), "cli-2", spent);
        spec.resume = Some("cli-2".into());
        assert_eq!(request_for(&adapter, &spec).spend_cap_usd, Some(1.6));
    }

    #[test]
    fn claude_gets_a_session_id_and_a_strict_mcp_config() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = TestAdapter::new("true")
            .with_id(CLAUDE_CODE)
            .with_input_lines();
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        let request = request_for(&adapter, &spec);
        assert!(uuid::Uuid::parse_str(request.session_id.as_deref().unwrap()).is_ok());
        assert!(request.keep_input_open);
        let config = request.mcp_config.unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(written, json!({ "mcpServers": {} }));

        spec.status_file = Some(dir.path().join("status.json"));
        spec.resume = Some("abc".into());
        let request = request_for(&adapter, &spec);
        assert!(request.session_id.is_none());
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(request.mcp_config.unwrap()).unwrap())
                .unwrap();
        let server = &written["mcpServers"][STATUS_SERVER];
        let status = dir.path().join("status.json").display().to_string();
        assert_eq!(
            server["args"],
            json!(["mcp", "session", "--status-file", status])
        );
        assert!(Path::new(server["command"].as_str().unwrap()).is_absolute());
    }

    /// Its own workflow runs on after the turn, so the report comes through the tool when the
    /// workflow is done, and the CLI isn't made to answer in the format at every turn's end.
    #[test]
    fn a_session_in_its_own_workflow_reports_through_the_tool() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = TestAdapter::new("true")
            .with_id(CLAUDE_CODE)
            .with_input_lines();
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        spec.session_dir = dir.path().join("session");
        spec.json_schema = Some(json!({"type": "object", "properties": {}}));
        assert!(request_for(&adapter, &spec).json_schema.is_some());
        spec.cli_workflow.ultracode = true;
        let request = request_for(&adapter, &spec);
        assert!(request.json_schema.is_none());
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(request.mcp_config.unwrap()).unwrap())
                .unwrap();
        let args = written["mcpServers"][STATUS_SERVER]["args"].to_string();
        assert!(args.contains("--report-schema"), "{args}");
        assert!(args.contains("--report-file"), "{args}");
        assert!(spec.session_dir.join(session_files::SCHEMA).exists());
    }

    /// A run's session names its tools, ToolSearch among them, and Claude Code then holds every
    /// MCP tool back behind it. The status server's tools stay loaded there, while the
    /// conversation's own session leaves that to Claude Code.
    #[test]
    fn a_run_session_keeps_the_status_tools_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = TestAdapter::new("true")
            .with_id(CLAUDE_CODE)
            .with_input_lines();
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        spec.status_file = Some(dir.path().join("status.json"));
        for (access, always) in [
            (Access::ReadOnly, Some(true)),
            (Access::Build, Some(true)),
            (Access::Direct, None),
        ] {
            spec.access = access;
            let request = request_for(&adapter, &spec);
            let written: Value = serde_json::from_str(
                &std::fs::read_to_string(request.mcp_config.unwrap()).unwrap(),
            )
            .unwrap();
            let server = &written["mcpServers"][STATUS_SERVER];
            assert_eq!(server["alwaysLoad"].as_bool(), always, "{access:?}");
        }
    }

    #[test]
    fn an_acp_session_on_a_task_gets_the_tasks_home_folder() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("runs/run-7");
        let adapter = TestAdapter::new("true")
            .with_id("hermes")
            .with_driver(crate::cli_runner::testing::FakeDriver::default());
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        spec.session_dir = layout::session_dir(&run_dir, &spec.key);
        // Not on a task: conversation and estimate sessions keep their home in the session folder.
        assert!(request_for(&adapter, &spec).project.is_none());

        let task = argonauts_core::TaskId::new("c-001").unwrap();
        spec.run = Some(RunId::new("run-7").unwrap());
        spec.task = Some(task.clone());
        let want = layout::task_dir(&run_dir, &task).join("homes/main/hermes");
        assert_eq!(task_home(&spec, "hermes"), Some(want.clone()));
        assert_eq!(
            request_for(&adapter, &spec).project,
            Some(want.display().to_string())
        );
        // The session folder still changes with every session.
        assert_eq!(
            request_for(&adapter, &spec).scratch_dir,
            Some(spec.session_dir.clone())
        );

        // Another session of the task shares it, and another role doesn't.
        let mut later = spec.clone();
        later.key = SessionKey::numbered(99);
        later.session_dir = layout::session_dir(&run_dir, &later.key);
        assert_eq!(task_home(&later, "hermes"), Some(want.clone()));
        later.role = RoleName::Reviewer;
        assert_ne!(task_home(&later, "hermes"), Some(want));

        // A session folder outside its run's sessions names no task folder.
        spec.session_dir = dir.path().join("elsewhere");
        assert!(task_home(&spec, "hermes").is_none());

        // A CLI that reads its pipes line by line gets none.
        spec.session_dir = layout::session_dir(&run_dir, &spec.key);
        let lines = TestAdapter::new("true").with_id("fake");
        assert!(request_for(&lines, &spec).project.is_none());
    }

    #[test]
    fn agy_gets_the_run_as_its_project() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = TestAdapter::new("true").with_id(ANTIGRAVITY);
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        assert!(request_for(&adapter, &spec).project.is_none());
        spec.run = Some(RunId::new("run-7").unwrap());
        assert_eq!(
            request_for(&adapter, &spec).project.as_deref(),
            Some("run-7")
        );
        assert!(request_for(&adapter, &spec).mcp_config.is_none());
    }

    #[test]
    fn compaction_hooks_and_fanout_come_through() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = TestAdapter::new("true");
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        spec.compaction = Compaction::Inherit;
        assert!(request_for(&adapter, &spec).compaction.is_none());
        spec.compaction = Compaction::Off;
        assert_eq!(
            request_for(&adapter, &spec).compaction,
            Some(Compaction::Off)
        );
        assert_eq!(
            request_for(&adapter, &spec).native_fanout,
            Some(spec.native_fanout)
        );
        let mut settings = Settings::default();
        settings.sandbox.user_hooks = UserHooks::Disable;
        assert!(
            session_request(&spec, &adapter, &settings, None)
                .unwrap()
                .user_hooks_off
        );
    }

    /// Only a session whose CLI may work on in the background spends where the meter can't
    /// see it: one in its own workflow, or a build session offered the CLI's own workflows or
    /// agents with its background on.
    #[test]
    fn a_session_spends_unseen_only_where_its_cli_works_in_the_background() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        assert!(
            !spends_unseen(&spec),
            "a reader has no background of its own"
        );
        spec.access = Access::Build;
        assert!(spends_unseen(&spec));
        spec.cli_workflow.background_off = true;
        assert!(!spends_unseen(&spec));
        spec.cli_workflow.background_off = false;
        spec.native_fanout = NativeFanout::Switch(FanoutSwitch::Off);
        assert!(!spends_unseen(&spec));
        spec.native_fanout = NativeFanout::Limit { max: 0 };
        assert!(!spends_unseen(&spec));
        spec.native_fanout = NativeFanout::Switch(FanoutSwitch::On);
        assert!(spends_unseen(&spec));
        let mut own = spec_in(dir.path(), dir.path(), "hi");
        own.json_schema = Some(json!({"type": "object"}));
        own.cli_workflow.ultracode = true;
        assert!(spends_unseen(&own));
    }

    #[test]
    fn env_scrubbed_per_agent() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        spec.model = ModelRef::new("fake", None);
        spec.env = vec![
            (SESSION_ENV.into(), "forged".into()),
            (LAUNCH_MARKER.into(), "forged".into()),
            ("CLAUDECODE".into(), "1".into()),
            ("CLAUDE_CODE_ENABLE_TODO_TOOLS".into(), "1".into()),
            ("SHARED".into(), "from-spec".into()),
            ("OPENAI_API_KEY".into(), "sk-foreign".into()),
            ("NPM_TOKEN".into(), "npm".into()),
        ];
        let request = request_for(&TestAdapter::new("true"), &spec);
        assert_eq!(
            request.env_set,
            [
                ("CLAUDE_CODE_ENABLE_TODO_TOOLS".to_owned(), "1".to_owned()),
                ("SHARED".to_owned(), "from-spec".to_owned()),
                ("OPENAI_API_KEY".to_owned(), "sk-foreign".to_owned()),
                (SESSION_ENV.to_owned(), spec.key.to_string()),
            ],
            "an agent key is the adapter's to keep or drop, any other credential goes"
        );

        let mut command = CommandSpec::new("agent", dir.path());
        command.env_set = vec![
            (SESSION_ENV.into(), "adapter-forged".into()),
            (LAUNCH_MARKER.into(), "adapter-forged".into()),
            ("SHARED".into(), "from-adapter".into()),
            ("CLAUDE_CODE_SESSION_ID".into(), "parent".into()),
        ];
        command.env_remove = vec![SESSION_ENV.into(), "OPENAI_API_KEY".into()];
        let inherited = [
            "PATH",
            "HOME",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_PROFILE",
            "NPM_TOKEN",
            "HF_TOKEN",
            "VAULT_TOKEN",
            "DB_PASSWORD",
            "STRIPE_SECRET",
            "DATABASE_URL",
            "SSH_AUTH_SOCK",
            "OPENAI_API_KEY",
            "GEMINI_API_KEY",
        ]
        .map(str::to_owned);
        apply_env(&mut command, &spec.key, &spec.env, &inherited);
        assert_eq!(
            command.env_set,
            [
                ("SHARED".to_owned(), "from-adapter".to_owned()),
                ("CLAUDE_CODE_ENABLE_TODO_TOOLS".to_owned(), "1".to_owned()),
                (SESSION_ENV.to_owned(), spec.key.to_string()),
            ],
            "a key the adapter removed never comes back from spec.env"
        );
        for name in &inherited[2..13] {
            assert!(command.env_remove.contains(name), "{name}");
        }
        for kept in ["PATH", "HOME", "GEMINI_API_KEY"] {
            assert!(!command.env_remove.contains(&kept.to_owned()), "{kept}");
        }
        assert!(!command.env_remove.contains(&SESSION_ENV.to_owned()));
        for name in PARENT_SESSION_ENV_VARS {
            assert!(command.env_remove.contains(&(*name).to_owned()), "{name}");
        }

        // A key the adapter sets for its auth mode stays, whatever this process has.
        let mut command = CommandSpec::new("agent", dir.path());
        command.env_set = vec![("AWS_BEARER_TOKEN_BEDROCK".into(), "bedrock".into())];
        apply_env(&mut command, &spec.key, &[], &inherited);
        assert!(
            !command
                .env_remove
                .contains(&"AWS_BEARER_TOKEN_BEDROCK".to_owned())
        );
        assert!(
            command
                .env_remove
                .contains(&"AWS_SECRET_ACCESS_KEY".to_owned())
        );
    }

    /// A sandboxed CLI reaches its model API only through the run host's proxy, so the
    /// proxy variables pass through like any other setting, from this process or the spec.
    #[test]
    fn proxy_variables_reach_the_cli() {
        const PROXY_VARS: [&str; 7] = [
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
            "ALL_PROXY",
            "NO_PROXY",
            "no_proxy",
        ];
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec_in(dir.path(), dir.path(), "go");
        spec.env = PROXY_VARS
            .iter()
            .map(|name| ((*name).to_owned(), "http://127.0.0.1:9".to_owned()))
            .collect();
        let mut command = CommandSpec::new("agent", dir.path());
        apply_env(
            &mut command,
            &spec.key,
            &spec.env,
            &PROXY_VARS.map(str::to_owned),
        );
        for name in PROXY_VARS {
            assert!(!command.env_remove.iter().any(|n| n == name), "{name}");
            assert!(command.env_set.iter().any(|(n, _)| n == name), "{name}");
        }
    }

    #[test]
    fn report_files_join_the_status_server() {
        let report = ReportFiles {
            schema: Path::new("/s/schema.json"),
            report: Path::new("/s/report.json"),
        };
        let argonauts = Path::new("/bin/argonauts");
        assert!(session_server(argonauts, None, None, None).is_none());
        let server = session_server(
            argonauts,
            Some(Path::new("/s/status.json")),
            Some(report),
            None,
        )
        .unwrap();
        assert_eq!(server.name, STATUS_SERVER);
        assert_eq!(
            server.args,
            [
                "mcp",
                "session",
                "--status-file",
                "/s/status.json",
                "--report-schema",
                "/s/schema.json",
                "--report-file",
                "/s/report.json"
            ]
        );
    }
}

#[cfg(test)]
#[path = "request_pictures_tests.rs"]
mod pictures_tests;

#[cfg(test)]
#[path = "request_own_workflow_tests.rs"]
mod own_workflow_tests;
