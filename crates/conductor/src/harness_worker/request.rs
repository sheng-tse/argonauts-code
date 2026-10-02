//! A session spec as the adapter's request: the role from the access, every launch flag again on
//! a resume, open input where steering needs it, the session id chosen up front where the harness
//! allows it, a per-run project or a per-card home folder, compaction and fan-out, the status tool's MCP config, launch
//! hints from the registry, user hooks off where asked, and the vendor's own memory off.
//! `ARGONAUTS_SESSION=<key>` is set here, over anything in `spec.env`, and credentials reach the
//! harness only where its adapter keeps them for its auth mode. The launch's own marker,
//! `ARGONAUTS_LAUNCH`, is set at spawn, so neither the spec nor the adapter may set it.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use argonauts_core::adapter::Transport;
use argonauts_core::driver::McpServerSpec;
use argonauts_core::layout::{self, session_files};
use argonauts_core::names::SessionKey;
use argonauts_core::registry::{ModelAccess, ModelRegistry, RegistryKind};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::UserHooks;
use argonauts_core::settings::sessions::Compaction;
use argonauts_core::worker::{Access, SessionSpec, SteerSupport, WorkerError};
use argonauts_core::{
    Adapter, CommandSpec, PARENT_SESSION_ENV_VARS, SessionRequest, SessionRole,
    VENDOR_KEY_ENV_VARS, create_private_dir_all, write_private_file,
};
use serde_json::{Map, Value, json};

use super::capabilities::{ANTIGRAVITY, CLAUDE_CODE, steer_support};
use super::launch::prompt_text;
use crate::gates::env::looks_secret;
use crate::meter;
use crate::reaper::LAUNCH_MARKER;

/// Set on every harness process. Approval paths refuse any caller that has it.
pub const SESSION_ENV: &str = "ARGONAUTS_SESSION";

/// Credentials that don't follow the `*_KEY` and `*_TOKEN` naming.
const CREDENTIAL_VARS: &[&str] = &["SSH_AUTH_SOCK", "DATABASE_URL"];

/// The folder in a card's own folder that holds the homes its ACP sessions keep for the card.
const CARD_HOMES: &str = "homes";

/// The name the status server has in every MCP config, which Claude Code lists in
/// `mcp_server_errors` when it fails to start.
pub const STATUS_SERVER: &str = "argonauts_status";

/// Only build and spike sessions may change files; everything else gets the read-only flags.
pub fn session_role(access: Access) -> SessionRole {
    match access {
        Access::Build | Access::Spike => SessionRole::Build,
        Access::NoTools | Access::ReadOnly => SessionRole::ReadOnly,
    }
}

/// Writes the MCP config where the harness takes one per run (Claude Code), so the command is
/// pure after this.
pub fn session_request(
    spec: &SessionSpec,
    adapter: &dyn Adapter,
    settings: &Settings,
    registry: Option<&dyn ModelRegistry>,
) -> Result<SessionRequest, WorkerError> {
    let agent = settings.agents.get(&spec.worker.worker);
    let id = adapter.id();
    let mcp_config = if id == CLAUDE_CODE {
        let io_error = |err: io::Error| WorkerError::Io(format!("the status tool's config: {err}"));
        let argonauts = std::env::current_exe().map_err(io_error)?;
        Some(
            write_session_mcp_config(&spec.session_dir, spec.status_file.as_deref(), &argonauts)
                .map_err(io_error)?,
        )
    } else {
        None
    };
    let launch = registry
        .and_then(|registry| registry.get(&spec.worker))
        .and_then(|entry| entry.launch);
    let mut model = spec
        .worker
        .model
        .clone()
        .or_else(|| agent.and_then(|agent| agent.model.clone()));
    let hinted = launch
        .as_ref()
        .is_some_and(|launch| launch.provider_model.is_some());
    if model.is_none() && !hinted && runs_hosted_default(adapter) {
        model = Some(local_model(id, registry).ok_or_else(|| {
            WorkerError::Refused(format!(
                "{} has no model set, and its own default runs on a hosted service, so the \
                 code would leave this machine. Set agents.{id}.model, such as \
                 ollama/<model>, or add a local model to models.toml.",
                adapter.display_name()
            ))
        })?);
    }
    Ok(SessionRequest {
        workdir: spec.workdir.clone(),
        prompt: prompt_text(&spec.prompt),
        model,
        effort: spec.effort.clone(),
        role: session_role(spec.access),
        auth: agent.map(|agent| agent.auth).unwrap_or_default(),
        json_schema: spec.json_schema.clone(),
        resume_session: spec.resume.clone(),
        // Claude Code's cap ignores what earlier runs of a resumed session spent, so it gets only
        // what's left.
        max_budget_usd: meter::native_cap_usd(&spec.limits, meter::spent_before(spec, id).cost_usd),
        extra_args: agent
            .map(|agent| agent.extra_args.clone())
            .unwrap_or_default(),
        scratch_dir: Some(spec.session_dir.clone()),
        goal_hash: spec.goal_sha256.clone(),
        keep_input_open: steer_support(adapter) == SteerSupport::Input,
        // A resume names its session with `--resume` instead.
        session_id: (id == CLAUDE_CODE && spec.resume.is_none())
            .then(|| uuid::Uuid::new_v4().to_string()),
        project: project(spec, adapter),
        compaction: (spec.compaction != Compaction::Inherit).then_some(spec.compaction),
        native_fanout: Some(spec.native_fanout),
        mcp_config,
        env_set: session_env_pairs(&spec.key, &spec.env),
        launch,
        user_hooks_off: settings.sandbox.user_hooks == UserHooks::Disable,
        isolated: spec.isolated,
        // The launch knows which sandbox wraps the session, and sets this.
        tool_sandbox_off: false,
    })
}

/// OpenCode, over its CLI or ACP, runs OpenCode Zen's hosted `opencode/big-pickle` when it's
/// given no model (1.18.34), whatever providers you set up.
fn runs_hosted_default(adapter: &dyn Adapter) -> bool {
    Path::new(adapter.binary()).file_name() == Some(OsStr::new("opencode"))
}

/// The first local model OpenCode can run, as its `provider/model`: one of its own entries
/// marked local, else a model on your Ollama or LM Studio endpoint. Models with tools come
/// first, since most sessions edit files.
fn local_model(harness: &str, registry: Option<&dyn ModelRegistry>) -> Option<String> {
    const OPENCODE_LOCAL_PROVIDERS: &[&str] = &["ollama", "lmstudio"];
    let mut found: Vec<(bool, bool, String)> = registry?
        .list()
        .into_iter()
        .filter(|entry| entry.access == ModelAccess::Local && !entry.retired)
        .filter_map(|entry| {
            let model = entry.id.model.as_deref()?;
            let route = if entry.id.worker == harness {
                entry
                    .launch
                    .as_ref()
                    .and_then(|launch| launch.provider_model.clone())
                    .unwrap_or_else(|| model.to_owned())
            } else if entry.kind == RegistryKind::Endpoint
                && OPENCODE_LOCAL_PROVIDERS.contains(&entry.id.worker.as_str())
            {
                format!("{}/{model}", entry.id.worker)
            } else {
                return None;
            };
            Some((entry.id.worker != harness, !entry.tools, route))
        })
        .collect();
    // Its own entries before the endpoints', then by tools, each in the registry's order.
    found.sort_by_key(|(endpoint, no_tools, _)| (*endpoint, *no_tools));
    found.into_iter().map(|(.., route)| route).next()
}

/// agy shares one project per run. An ACP agent gets the folder where a home kept for the card
/// lives, so a later session of the card, such as a resume, finds the same home. Its adapter
/// uses it only for a `keep = "card"` home.
fn project(spec: &SessionSpec, adapter: &dyn Adapter) -> Option<String> {
    if adapter.id() == ANTIGRAVITY {
        return spec.run.as_ref().map(|run| run.as_str().to_owned());
    }
    if adapter.transport() == Transport::Acp {
        return card_home(spec, adapter.id()).map(|home| home.display().to_string());
    }
    None
}

/// `<run>/cards/<card>/homes/<role>/<harness>`, for a session on a card whose folder sits in its
/// run's sessions. Each role gets its own, since the home's config differs by role, and every
/// session of a role on the card gets the same one.
pub fn card_home(spec: &SessionSpec, harness: &str) -> Option<PathBuf> {
    let card = spec.card.as_ref()?;
    let run_dir = spec.session_dir.parent()?.parent()?;
    if layout::session_dir(run_dir, &spec.key) != spec.session_dir {
        return None;
    }
    Some(
        layout::card_dir(run_dir, card)
            .join(CARD_HOMES)
            .join(spec.role.as_str())
            .join(harness),
    )
}

/// The card's home folder from [`project`], when the launch points the agent at it: the adapter
/// keeps that home for the card.
pub fn card_home_in_use(request: &SessionRequest, command: &CommandSpec) -> Option<PathBuf> {
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
    write_session_mcp_config(session_dir, Some(status_file), argonauts)
}

/// Like [`write_mcp_config`], with no servers at all when there is no status file, so a strict
/// config still leaves out your own MCP servers.
pub fn write_session_mcp_config(
    session_dir: &Path,
    status_file: Option<&Path>,
    argonauts: &Path,
) -> io::Result<PathBuf> {
    let mut servers = Map::new();
    if let Some(server) = session_server(argonauts, status_file, None) {
        servers.insert(
            server.name,
            json!({
                "type": "stdio",
                "command": server.command,
                "args": server.args,
            }),
        );
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
) -> Option<McpServerSpec> {
    if status_file.is_none() && report.is_none() {
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
    Some(McpServerSpec {
        name: STATUS_SERVER.to_owned(),
        command: argonauts.to_path_buf(),
        args,
    })
}

fn is_reserved(name: &str) -> bool {
    name == SESSION_ENV || name == LAUNCH_MARKER || PARENT_SESSION_ENV_VARS.contains(&name)
}

/// Names that hold credentials: the vendor keys, anything named like a key, token, secret, or
/// password, AWS's variables, the SSH agent, and a database URL. An agent can print its
/// environment into a transcript the vendor keeps, so a harness gets only the ones its adapter
/// keeps for its auth mode, as a gate gets none.
fn is_credential(name: &str) -> bool {
    VENDOR_KEY_ENV_VARS.contains(&name)
        || looks_secret(name)
        || name.starts_with("AWS_")
        || CREDENTIAL_VARS.contains(&name)
}

/// `spec.env` without anything the worker owns and without credentials other than vendor keys,
/// which the adapter keeps or drops by its auth mode, then `ARGONAUTS_SESSION=<key>`.
fn session_env_pairs(key: &SessionKey, env: &[(String, String)]) -> Vec<(String, String)> {
    env.iter()
        .filter(|(name, _)| {
            !is_reserved(name)
                && (!is_credential(name) || VENDOR_KEY_ENV_VARS.contains(&name.as_str()))
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
    // What the adapter set itself, and the vendor keys it didn't remove, are what it keeps.
    let set_by_adapter: Vec<String> = command.env_set.iter().map(|(n, _)| n.clone()).collect();
    let removed_by_adapter = command.env_remove.clone();
    let kept = |name: &str| {
        set_by_adapter.iter().any(|set| set == name)
            || (VENDOR_KEY_ENV_VARS.contains(&name)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness_worker::testing::{TestAdapter, spec_in};
    use argonauts_core::names::{RoleName, WorkerRef};
    use argonauts_core::registry::{CostModel, CostUnit, ModelEntry};
    use argonauts_core::settings::agents::AgentSettings;
    use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
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
            (Access::Spike, SessionRole::Build),
        ] {
            spec.access = access;
            assert_eq!(request_for(&adapter, &spec).role, role, "{access:?}");
        }
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
            id: WorkerRef::new(worker, Some(model.to_owned())),
            family: "llama".into(),
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

    #[test]
    fn opencode_with_no_model_runs_a_local_one_or_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let spec = |worker: &str| {
            let mut spec = spec_in(dir.path(), dir.path(), "fix it");
            spec.worker = worker.parse().unwrap();
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
        assert!(matches!(err, WorkerError::Refused(_)), "{err}");

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
            RegistryKind::Harness,
            false,
        ));
        let request =
            session_request(&spec("opencode-acp"), &opencode, &settings, Some(&registry)).unwrap();
        assert_eq!(request.model.as_deref(), Some("lmstudio/qwen3-8b"));

        // A model you set is kept, hosted or not, and other harnesses are left alone.
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
        spec.resume = Some("vendor-1".into());
        let resumed = session_request(&spec, &adapter, &settings, None).unwrap();
        assert_eq!(resumed.resume_session.as_deref(), Some("vendor-1"));
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
        spec.limits.budget = Some(BudgetAmount {
            unit: BudgetUnit::Usd,
            amount: 2.0,
        });
        assert_eq!(request_for(&adapter, &spec).max_budget_usd, Some(2.2));
        let spent = Spend {
            cost_usd: 0.6,
            ..Spend::default()
        };
        meter::record_vendor_session_spend(dir.path(), adapter.id(), "vendor-2", spent);
        spec.resume = Some("vendor-2".into());
        assert_eq!(request_for(&adapter, &spec).max_budget_usd, Some(1.6));
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

    #[test]
    fn an_acp_session_on_a_card_gets_the_cards_home_folder() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("runs/run-7");
        let adapter = TestAdapter::new("true")
            .with_id("hermes")
            .with_driver(crate::harness_worker::testing::FakeDriver::default());
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        spec.session_dir = layout::session_dir(&run_dir, &spec.key);
        // Not on a card: chat and triage sessions keep their home in the session folder.
        assert!(request_for(&adapter, &spec).project.is_none());

        let card = argonauts_core::CardId::new("c-001").unwrap();
        spec.run = Some(RunId::new("run-7").unwrap());
        spec.card = Some(card.clone());
        let want = layout::card_dir(&run_dir, &card).join("homes/default/hermes");
        assert_eq!(card_home(&spec, "hermes"), Some(want.clone()));
        assert_eq!(
            request_for(&adapter, &spec).project,
            Some(want.display().to_string())
        );
        // The session folder still changes with every session.
        assert_eq!(
            request_for(&adapter, &spec).scratch_dir,
            Some(spec.session_dir.clone())
        );

        // Another session of the card shares it, and another role doesn't.
        let mut later = spec.clone();
        later.key = SessionKey::numbered(99);
        later.session_dir = layout::session_dir(&run_dir, &later.key);
        assert_eq!(card_home(&later, "hermes"), Some(want.clone()));
        later.role = RoleName::Reviewer;
        assert_ne!(card_home(&later, "hermes"), Some(want));

        // A session folder outside its run's sessions names no card folder.
        spec.session_dir = dir.path().join("elsewhere");
        assert!(card_home(&spec, "hermes").is_none());

        // A harness that reads its pipes line by line gets none.
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

    #[test]
    fn env_scrubbed_per_vendor() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        spec.worker = WorkerRef::new("fake", None);
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
            "a vendor key is the adapter's to keep or drop, any other credential goes"
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

    /// A sandboxed harness reaches its model API only through the supervisor's proxy, so the
    /// proxy variables pass through like any other setting, from this process or the spec.
    #[test]
    fn proxy_variables_reach_the_harness() {
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
        assert!(session_server(argonauts, None, None).is_none());
        let server =
            session_server(argonauts, Some(Path::new("/s/status.json")), Some(report)).unwrap();
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
