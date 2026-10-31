//! The app side of the CLI's other subcommands, so the binary stays thin.

pub(crate) mod run_command;

use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, anyhow, bail};
use argonauts_conductor::DoctorDetails;
use argonauts_core::agents::session_runner::{RunnerDirectory, RunnerError, SessionRunner};
use argonauts_core::base::layout::user_files;
use argonauts_core::base::names::ModelRef;
use argonauts_core::base::paths::{create_private_dir_all, write_private_file};
use argonauts_core::models::cheapest::cheapest_listed;
use argonauts_core::models::registry::{ModelEntry, ModelRegistry as _, RegistryKind};
use argonauts_core::runs::api::{RunState, RunSummary};
use argonauts_core::safety::policy::{ApprovalPolicy, ApprovalsView};
use argonauts_core::safety::sanitize::sanitize_for_terminal;
use argonauts_core::settings::layer;
use argonauts_core::settings::{SettingScope, Settings};
use argonauts_core::workflows::steps::SchemaStepRunner;
use argonauts_core::{Adapter, Paths, RunId, TaskId};
use argonauts_registry::clis::agents_turned_on;
use argonauts_registry::{
    ResolvedSettings, SettingsSources, resolve, safety_keys_in, setting_meta, write_setting,
};
use serde::Serialize;
use time::OffsetDateTime;

use crate::agents::adapters::{builtin_adapters, builtin_adapters_for};
use crate::host::clis::{self, CliAdapters, RepoModels};
use crate::host::live_registry::{LiveRunHost, live_run_hosts};
use crate::startup::app_settings::AppSettings;
use crate::startup::env::{AppEnv, SESSION_ENV};
use crate::startup::trust::TrustStore;

/// `! run-03 has been waiting for 9 days, at the plan approval. Waiting runs never expire.`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitingReminder {
    pub run: RunId,
    pub days: u64,
    /// Such as "the plan approval".
    pub stopped_at: String,
    pub worktrees: usize,
    pub worktree_bytes: u64,
}

/// What `argonauts doctor` adds to Argonauts Code's report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoctorExtras {
    pub waiting: Vec<WaitingReminder>,
}

/// Outside a git repository, with no repo named, there are no runs to remind you of.
pub async fn doctor_extras(repo: Option<&Path>) -> anyhow::Result<DoctorExtras> {
    let Some(env) = here(repo)? else {
        return Ok(DoctorExtras::default());
    };
    let settings = AppSettings::load(&env, Vec::new(), false)?;
    let runs = crate::conductor(&env.repo_root)?.list().await?;
    let threshold = settings.settings().runs.remind_waiting_days;
    let waiting = waiting_reminders(&runs, threshold, OffsetDateTime::now_utc(), |run| {
        worktree_usage(&run_worktrees(&env, run))
    });
    Ok(DoctorExtras { waiting })
}

/// Every adapter doctor checks: the built-in four, then each `[[cli]]` entry that counts
/// here, a built-in profile that's off by default only once a settings layer turns it on.
pub fn doctor_adapters(
    repo: Option<&Path>,
    overrides: Vec<String>,
) -> anyhow::Result<Vec<Arc<dyn Adapter>>> {
    let (env, resolved) = resolve_here(repo, overrides)?;
    adapters_here(env.as_ref(), &resolved)
}

/// How each installed CLI's version stands and what there is to update to, by id, as the app's
/// Doctor and Status tab say it: from `report`, and from what sessions and the install
/// sources have shown, kept under `home`.
pub fn version_checks(
    home: &Paths,
    adapters: &[Arc<dyn Adapter>],
    report: &argonauts_conductor::DoctorReport,
) -> std::collections::BTreeMap<String, argonauts_core::app::status_tab::CliVersionCheck> {
    use argonauts_conductor::boards::cli_versions::{CliVersions, Installed, version_check};

    let seen = CliVersions::load(&home.cli_versions());
    report
        .agents
        .iter()
        .filter(|check| check.status != argonauts_conductor::AgentStatus::NotInstalled)
        .filter_map(|check| {
            let adapter = adapters.iter().find(|adapter| adapter.id() == check.id)?;
            let found = version_check(adapter.as_ref(), Installed::of(check), &seen);
            Some((check.id.clone(), found))
        })
        .collect()
}

/// [`version_checks`] for the shell's `argonauts doctor`, with your home found from the
/// environment. Nothing is said of versions when it can't be found.
pub fn doctor_version_checks(
    adapters: &[Arc<dyn Adapter>],
    report: &argonauts_conductor::DoctorReport,
) -> std::collections::BTreeMap<String, argonauts_core::app::status_tab::CliVersionCheck> {
    match home() {
        Ok(home) => version_checks(&home, adapters, report),
        Err(_) => Default::default(),
    }
}

/// What doctor reports beyond versions for each adapter here: login, flags, roles, hooks, the
/// status tool and the plan usage probe, then the sandbox self-test and any always-on agents. It
/// never makes a model call.
pub async fn doctor_details(
    repo: Option<&Path>,
    overrides: Vec<String>,
) -> anyhow::Result<DoctorDetails> {
    let (env, resolved) = resolve_here(repo, overrides)?;
    let adapters = adapters_here(env.as_ref(), &resolved)?;
    Ok(argonauts_conductor::doctor_details(&adapters, &resolved.settings).await)
}

pub(crate) fn adapters_here(
    env: Option<&AppEnv>,
    resolved: &ResolvedSettings,
) -> anyhow::Result<Vec<Arc<dyn Adapter>>> {
    adapters_turning_on(env, resolved, &[])
}

/// [`adapters_here`], as if your settings also turned on `also_on`, so `/agents add` can check
/// a built-in profile that's still off.
pub(crate) fn adapters_turning_on(
    env: Option<&AppEnv>,
    resolved: &ResolvedSettings,
    also_on: &[String],
) -> anyhow::Result<Vec<Arc<dyn Adapter>>> {
    let paths = match env {
        Some(env) => env.paths.clone(),
        None => home()?,
    };
    let repo = env.map(|env| RepoModels::new(&env.repo_root, &env.state_dir));
    let mut turned_on = agents_turned_on(resolved);
    turned_on.extend(also_on.iter().cloned());
    let manifests = clis::manifests(&resolved.settings, &paths, repo.as_ref(), &turned_on);
    let builtins = builtin_adapters();
    let mut adapters = builtin_adapters_for(&resolved.settings, repo.as_ref());
    adapters.extend(CliAdapters::default().adapters(manifests, &builtins, &paths));
    Ok(adapters)
}

/// Waiting runs you were reminded of, or waiting longer than `threshold_days`, oldest first.
fn waiting_reminders(
    runs: &[RunSummary],
    threshold_days: u32,
    now: OffsetDateTime,
    usage: impl Fn(&RunId) -> (usize, u64),
) -> Vec<WaitingReminder> {
    let threshold = time::Duration::days(i64::from(threshold_days));
    let mut reminders: Vec<WaitingReminder> = runs
        .iter()
        .filter(|run| run.state == RunState::Waiting)
        .filter_map(|run| {
            let waiting_for = now - run.updated;
            if !run.remind && waiting_for <= threshold {
                return None;
            }
            let (worktrees, worktree_bytes) = usage(&run.run);
            Some(WaitingReminder {
                run: run.run.clone(),
                days: u64::try_from(waiting_for.whole_days()).unwrap_or(0),
                stopped_at: run.stopped_at.clone(),
                worktrees,
                worktree_bytes,
            })
        })
        .collect();
    reminders.sort_by_key(|reminder| std::cmp::Reverse(reminder.days));
    reminders
}

/// `~/.argonauts/worktrees/<repo>/<run>/`, which holds one worktree per task.
fn run_worktrees(env: &AppEnv, run: &RunId) -> PathBuf {
    let any_task = TaskId::new("task").ok();
    any_task
        .map(|task| env.paths.worktree_dir(&env.repo_id, run, &task))
        .and_then(|dir| dir.parent().map(Path::to_owned))
        .unwrap_or_default()
}

/// How many worktrees the folder holds, and their size on disk. Links are counted, never
/// followed.
fn worktree_usage(dir: &Path) -> (usize, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let worktrees: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    let bytes = worktrees.iter().map(|path| size_of(path)).sum();
    (worktrees.len(), bytes)
}

fn size_of(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() {
        return metadata.len();
    }
    std::fs::read_dir(path)
        .map(|entries| entries.flatten().map(|entry| size_of(&entry.path())).sum())
        .unwrap_or(0)
}

/// Runs in this repo that wait for you, newest first.
pub async fn waiting_runs(repo: Option<&Path>) -> anyhow::Result<Vec<RunSummary>> {
    let Some(env) = here(repo)? else {
        return Ok(Vec::new());
    };
    let runs = crate::conductor(&env.repo_root)?.list().await?;
    Ok(waiting_for_you(runs))
}

fn waiting_for_you(runs: Vec<RunSummary>) -> Vec<RunSummary> {
    let mut waiting: Vec<RunSummary> = runs
        .into_iter()
        .filter(|run| !run.waiting.is_empty())
        .collect();
    waiting.sort_by_key(|run| std::cmp::Reverse(run.updated));
    waiting
}

/// `argonauts status`: every live run host across your repos, and this repo's runs that
/// haven't ended.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StatusReport {
    pub live: Vec<LiveRunHost>,
    pub runs: Vec<RunSummary>,
}

pub async fn status(repo: Option<&Path>) -> anyhow::Result<StatusReport> {
    let paths = home()?;
    let live = live_run_hosts(&paths);
    let runs = match here(repo)? {
        Some(env) => crate::conductor(&env.repo_root)?.list().await?,
        None => Vec::new(),
    };
    let runs = runs
        .into_iter()
        .filter(|run| {
            matches!(
                run.state,
                RunState::Running | RunState::Pausing | RunState::Waiting | RunState::Crashed
            )
        })
        .collect();
    Ok(StatusReport { live, runs })
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalsReport {
    pub view: ApprovalsView,
    /// Such as "CI failures wait for a CI run."
    pub notes: Vec<String>,
}

pub async fn approvals_report(repo: Option<&Path>) -> anyhow::Result<ApprovalsReport> {
    let env = required(repo)?;
    let settings = AppSettings::load(&env, Vec::new(), false)?;
    let view = policy_for(&env, settings.shared()).view();
    let mut notes = vec!["CI failures wait for a CI run.".to_owned()];
    if env.paths.home().join(user_files::AUTO_OFF).exists() {
        notes.push(
            "Auto is off everywhere since `argonauts auto off`: every approval waits for you."
                .to_owned(),
        );
    }
    Ok(ApprovalsReport { view, notes })
}

/// `argonauts auto off`: every approval is supervised until you turn it back on.
pub fn auto_off() -> anyhow::Result<()> {
    let paths = home()?;
    // Turning autonomy off must work even where the settings don't resolve.
    if let Ok(Some(env)) = here(None)
        && let Ok(settings) = AppSettings::load(&env, Vec::new(), false)
    {
        policy_for(&env, settings.shared()).auto_off();
    }
    // The marker is what every run checks, so it's there even when no repo is at hand.
    let marker = paths.home().join(user_files::AUTO_OFF);
    if !marker.exists() {
        create_private_dir_all(paths.home())?;
        let note = format!("auto off since {}\n", OffsetDateTime::now_utc());
        write_private_file(&marker, note.as_bytes())
            .with_context(|| format!("writing {}", marker.display()))?;
    }
    Ok(())
}

/// The repo's policy, for what needs no session: its view, `auto_off`, and spot checks.
pub(crate) fn policy_for(env: &AppEnv, settings: Arc<Settings>) -> Arc<dyn ApprovalPolicy> {
    policy_for_state(&env.state_dir, settings)
}

/// [`policy_for`] over a repo's state folder.
pub(crate) fn policy_for_state(
    state_dir: &Path,
    settings: Arc<Settings>,
) -> Arc<dyn ApprovalPolicy> {
    argonauts_policy::approval_policy(
        settings,
        Arc::new(|| None),
        state_dir,
        Arc::new(NoRunners),
        Arc::new(SchemaStepRunner),
    )
}

/// The policy's view and `auto_off` start no session, so they get no workers.
pub(crate) struct NoRunners;

impl RunnerDirectory for NoRunners {
    fn get(&self, worker: &ModelRef) -> Result<Arc<dyn SessionRunner>, RunnerError> {
        Err(RunnerError::Unknown(worker.clone()))
    }

    fn available(&self) -> Vec<ModelRef> {
        Vec::new()
    }
}

/// What `argonauts models` lists.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelList {
    pub models: Vec<ModelEntry>,
    /// Each source the latest discovery skipped, with why, such as "LM Studio: not running".
    pub skipped: Vec<String>,
}

/// The registry's models with what discovery last found, after a fresh discovery when asked,
/// which also updates the list `/model` starts from, and the sources discovery skipped.
/// Discovery never calls a model.
pub async fn models(
    repo: Option<&Path>,
    overrides: Vec<String>,
    discover: bool,
) -> anyhow::Result<ModelList> {
    let (env, resolved) = resolve_here(repo, overrides)?;
    let repo = env
        .as_ref()
        .map(|env| RepoModels::new(&env.repo_root, &env.state_dir));
    let cache = home()?.models_cache();
    let settings = &resolved.settings;
    let registry = crate::host::clis::file_registry(settings, repo.as_ref(), Some(&cache));
    let models = if discover {
        registry.discover().await
    } else {
        registry.list()
    };
    Ok(ModelList {
        models: of_clis_on(settings, models),
        skipped: registry.discovery_skipped(),
    })
}

/// `models` but those of a CLI that is off, which the skipped sources name instead.
fn of_clis_on(settings: &Settings, models: Vec<ModelEntry>) -> Vec<ModelEntry> {
    models
        .into_iter()
        .filter(|entry| {
            entry.kind != RegistryKind::Cli
                || settings
                    .agents
                    .get(&entry.id.agent)
                    .is_none_or(|agent| agent.enabled)
        })
        .collect()
}

/// The cheapest model and effort `agent` lists, from the list discovery last kept, else the
/// adapter's own pick while no list is known. `None` for a CLI Argonauts Code has no adapter
/// for and whose list prices nothing. Nothing is fetched.
pub fn cheapest_model(
    repo: Option<&Path>,
    overrides: Vec<String>,
    agent: &str,
) -> anyhow::Result<Option<(String, Option<String>)>> {
    let (env, resolved) = resolve_here(repo, overrides)?;
    let repo = env
        .as_ref()
        .map(|env| RepoModels::new(&env.repo_root, &env.state_dir));
    let cache = home()?.models_cache();
    let registry = crate::host::clis::registry(&resolved.settings, repo.as_ref(), Some(&cache));
    let listed =
        cheapest_listed(registry.as_ref(), agent).map(|cheapest| (cheapest.model, cheapest.effort));
    Ok(listed.or_else(|| {
        builtin_adapters()
            .iter()
            .find(|adapter| adapter.id() == agent)
            .map(|adapter| {
                let (model, effort) = adapter.cheapest_model();
                (model.to_owned(), effort.map(str::to_owned))
            })
    }))
}

/// `argonauts config set`: where your changes go, as the app saves them, unless `scope` picks the
/// file. A safety key needs a terminal, and this refuses inside an agent. Returns the file
/// written.
pub fn config_set(
    repo: Option<&Path>,
    key: &str,
    value: &str,
    scope: Option<SettingScope>,
) -> anyhow::Result<PathBuf> {
    // Only a project file needs a repository.
    let env = match scope_for(&home()?, key, scope)? {
        SettingScope::Project | SettingScope::ProjectLocal => Some(required(repo)?),
        SettingScope::Global | SettingScope::Session => None,
    };
    safety_needs_a_terminal(env.as_ref(), key, value)?;
    // An agent could unset `ARGONAUTS_SESSION` or fake a terminal, so its ancestry decides too.
    crate::refuse_inside_agent()?;
    let saved = config_set_in(env.as_ref(), &home()?, key, value, scope)?;
    if let Some(note) = &saved.note {
        eprintln!("{note}");
    }
    Ok(saved.file)
}

/// What `argonauts config set` wrote.
#[derive(Debug)]
struct Saved {
    file: PathBuf,
    /// Says a project file stays untrusted, and how to trust it.
    note: Option<String>,
}

/// `env` is `None` when no project is at hand, where only the global file can be set.
fn config_set_in(
    env: Option<&AppEnv>,
    paths: &Paths,
    key: &str,
    value: &str,
    scope: Option<SettingScope>,
) -> anyhow::Result<Saved> {
    let scope = scope_for(paths, key, scope)?;
    safety_needs_a_terminal(env, key, value)?;
    let (env, file) = match (scope, env) {
        (SettingScope::Global, _) => {
            let file = paths.global_config();
            write_setting(&file, key, value)?;
            return Ok(Saved { file, note: None });
        }
        (SettingScope::Session, _) => bail!(
            "save_changes_to keeps your changes for one app session, which this isn't; pick a \
             file with --local, --project, or --global"
        ),
        (_, None) => bail!("{key} goes in a project file, so run this in the project's folder"),
        (SettingScope::Project, Some(env)) => (env, env.project_config()),
        (SettingScope::ProjectLocal, Some(env)) => (env, env.project_local_config()),
    };
    write_project_file(env, &file, key, value)
}

/// The scope `scope` picks, or else where your changes go: `save_changes_to` as your global file
/// in `paths` sets it, a key for one repository kept in this project just for you, as the app
/// saves a change.
fn scope_for(
    paths: &Paths,
    key: &str,
    scope: Option<SettingScope>,
) -> anyhow::Result<SettingScope> {
    setting_meta(key)
        .ok_or_else(|| anyhow!("there's no setting {key}; `argonauts config show` lists them"))?;
    if let Some(asked) = scope {
        return crate::controller::settings::save::asked_place(key, asked);
    }
    let sources = SettingsSources {
        global: Some(paths.global_config()),
        ..SettingsSources::default()
    };
    let save_changes_to = resolve(&sources)?.settings.save_changes_to;
    Ok(crate::controller::settings::save::usual_scope(
        save_changes_to,
        key,
    ))
}

/// Writes into a project file and carries its trust forward, as the app's saves do. You trust
/// what it holds after when you trusted it before, it didn't exist, or it set no safety key,
/// since then nothing in it is something you didn't write. Otherwise it stays untrusted.
fn write_project_file(env: &AppEnv, file: &Path, key: &str, value: &str) -> anyhow::Result<Saved> {
    let mut trust = TrustStore::load(&env.state_dir);
    let original = std::fs::read(file).ok();
    let carry = original.is_none() || trust.was_trusted(file) || !sets_a_safety_key(file);
    write_setting(file, key, value)?;
    if !carry {
        let note = format!(
            "{} stays untrusted, so its safety settings stay ignored until you trust it with \
             `argonauts setup`.",
            env.shown(file)
        );
        return Ok(Saved {
            file: file.to_owned(),
            note: Some(note),
        });
    }
    if let Err(err) = trust.trust(file) {
        // Put the file back, so its safety keys don't quietly stop applying.
        let restored = match &original {
            Some(bytes) => std::fs::write(file, bytes),
            None => std::fs::remove_file(file),
        };
        return Err(match restored {
            Ok(()) => err,
            Err(restore) => err.context(format!(
                "and putting {} back failed: {restore}",
                file.display()
            )),
        });
    }
    Ok(Saved {
        file: file.to_owned(),
        note: None,
    })
}

/// Whether a settings file sets any safety key. One that doesn't parse sets none.
fn sets_a_safety_key(file: &Path) -> bool {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .is_some_and(|table| table_sets_a_safety_key("", &table))
}

fn table_sets_a_safety_key(prefix: &str, table: &toml::Table) -> bool {
    table.iter().any(|(name, value)| {
        let key = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        match value {
            toml::Value::Table(inner) => table_sets_a_safety_key(&key, inner),
            value => !safety_keys_in(&key, &value.to_string()).is_empty(),
        }
    })
}

fn safety_needs_a_terminal(env: Option<&AppEnv>, key: &str, value: &str) -> anyhow::Result<()> {
    let safety_keys = safety_keys_in(key, value);
    match env {
        Some(env) => refuse_safety_change(env, &safety_keys),
        // Outside a repository there is no AppEnv, so judge the terminal as it would.
        None => refuse_unless_person(
            &safety_keys,
            std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
            std::env::var_os(SESSION_ENV).is_some(),
        ),
    }
}

/// Safety settings change only with you at a terminal, never from an agent session. The app
/// and `argonauts config set` both refuse through this, with the same words.
pub(crate) fn refuse_safety_change(env: &AppEnv, safety_keys: &[String]) -> anyhow::Result<()> {
    refuse_unless_person(safety_keys, env.interactive, env.agent_session)
}

fn refuse_unless_person(
    safety_keys: &[String],
    interactive: bool,
    agent_session: bool,
) -> anyhow::Result<()> {
    if safety_keys.is_empty() || (interactive && !agent_session) {
        return Ok(());
    }
    let not_an_agent = if agent_session {
        ", not an agent session"
    } else {
        ""
    };
    bail!(
        "Changing safety settings needs you at a terminal{not_an_agent} ({}).",
        safety_keys.join(", ")
    )
}

/// `argonauts config show`: the resolved settings as TOML, headed by the files they came from,
/// your `--set` flags, the keys set above the built-in defaults, and any warnings.
pub fn config_show(repo: Option<&Path>, overrides: Vec<String>) -> anyhow::Result<String> {
    let (env, resolved) = resolve_here(repo, overrides.clone())?;
    show(env.as_ref(), &home()?, &resolved, &overrides)
}

fn show(
    env: Option<&AppEnv>,
    paths: &Paths,
    resolved: &ResolvedSettings,
    overrides: &[String],
) -> anyhow::Result<String> {
    let mut files = vec![paths.global_config()];
    if let Some(env) = env {
        files.push(env.project_config());
        files.push(env.project_local_config());
    }
    let shown = |path: &Path| match env {
        Some(env) => env.shown(path),
        None => path.display().to_string(),
    };
    let mut out = String::new();
    writeln!(out, "# Resolved settings, sha256 {}", resolved.sha256)?;
    writeln!(out, "# built-in defaults")?;
    for file in &files {
        let state = if file.exists() { "read" } else { "not found" };
        writeln!(out, "# {} ({state})", shown(file))?;
    }
    for flag in overrides {
        writeln!(out, "# --set {}", sanitize_for_terminal(flag))?;
    }
    let changed: Vec<_> = resolved
        .entries
        .iter()
        .filter(|entry| entry.layer != layer::BUILT_IN)
        .collect();
    if !changed.is_empty() {
        writeln!(out, "#")?;
        writeln!(out, "# Set above the built-in defaults:")?;
        for entry in changed {
            let line = format!("{} = {} ({})", entry.key, entry.value, entry.layer);
            writeln!(out, "#   {}", sanitize_for_terminal(&line))?;
        }
    }
    // Warnings can quote key names from a repo's file.
    for warning in &resolved.warnings {
        writeln!(out, "# warning: {}", sanitize_for_terminal(warning))?;
    }
    writeln!(out)?;
    let body = toml::to_string(&resolved.settings).context("writing the settings as TOML")?;
    out.push_str(&sanitize_for_terminal(&body));
    Ok(out)
}

/// The settings in effect here, with each project file's trust and these `--set` flags.
pub fn current_settings(repo: Option<&Path>, overrides: Vec<String>) -> anyhow::Result<Settings> {
    resolve_here(repo, overrides).map(|(_, resolved)| resolved.settings)
}

/// What reading the settings here warned of, such as a key that can't be used and so has its
/// default, for Doctor to name.
pub fn settings_warnings(repo: Option<&Path>, overrides: Vec<String>) -> Vec<String> {
    resolve_here(repo, overrides)
        .map(|(_, resolved)| {
            let warnings = resolved.warnings.into_iter();
            warnings.map(|warning| warning.text).collect()
        })
        .unwrap_or_default()
}

/// The project holding `dir`, or the current directory: the top of its git repository, or the
/// folder itself outside one.
pub fn repo_root(dir: Option<&Path>) -> anyhow::Result<PathBuf> {
    let start = match dir {
        Some(dir) => dir.to_owned(),
        None => std::env::current_dir().context("reading the current directory")?,
    };
    Ok(crate::git::project_root(&start))
}

/// The JSON Schema of every settings file.
pub fn config_schema() -> serde_json::Value {
    schemars::schema_for!(Settings).to_value()
}

pub(crate) fn home() -> anyhow::Result<Paths> {
    Paths::from_env().map_err(|_| anyhow!("can't find your home directory; set ARGONAUTS_HOME"))
}

/// This project's environment. `None` when no folder was named and the current one is neither
/// a git repository nor a folder Argonauts Code has worked in: nothing of a project's is there to
/// read, and a command that only looks must not start keeping state for it.
fn here(repo: Option<&Path>) -> anyhow::Result<Option<AppEnv>> {
    let env = AppEnv::discover(repo)?;
    let a_project = repo.is_some()
        || env.git().has_history()
        || env.state_dir.is_dir()
        || env.repo_root.join(".argonauts").is_dir();
    Ok(a_project.then_some(env))
}

/// For the commands that act on this folder as a project, whatever it has been so far.
fn required(repo: Option<&Path>) -> anyhow::Result<AppEnv> {
    AppEnv::discover(repo)
}

/// The settings for this repo with each project file's trust, or the global file alone outside
/// one.
pub(crate) fn resolve_here(
    repo: Option<&Path>,
    overrides: Vec<String>,
) -> anyhow::Result<(Option<AppEnv>, ResolvedSettings)> {
    if let Some(env) = here(repo)? {
        let settings = AppSettings::load(&env, overrides, env.interactive)?;
        let resolved = settings.resolved().clone();
        return Ok((Some(env), resolved));
    }
    let paths = home()?;
    let sources = SettingsSources {
        global: Some(paths.global_config()),
        overrides,
        interactive: false,
        home: std::env::var_os("HOME").map(PathBuf::from),
        ..SettingsSources::default()
    };
    Ok((None, resolve(&sources)?))
}

#[cfg(test)]
mod tests {
    use argonauts_core::Spend;
    use argonauts_core::base::names::ApprovalId;
    use argonauts_core::runs::events::run::RunMode;
    use argonauts_testkit::TempRepo;

    use super::*;

    #[test]
    fn a_cli_that_is_off_lists_no_models() {
        let mut settings = Settings::default();
        settings
            .agents
            .entry("codex".to_owned())
            .or_default()
            .enabled = false;
        let entry = |id: &str| {
            let worker: argonauts_core::base::names::ModelRef = id.parse().unwrap();
            argonauts_registry::models::cli_entry(&settings, &worker.agent, worker.model.as_deref())
        };
        let mut endpoint = entry("ollama/llama3.2");
        endpoint.kind = RegistryKind::Endpoint;
        let models = vec![
            entry("codex/gpt-6-luna"),
            entry("claude-code/haiku"),
            endpoint,
        ];
        let kept: Vec<String> = of_clis_on(&settings, models)
            .iter()
            .map(|entry| entry.id.to_string())
            .collect();
        assert_eq!(kept, ["claude-code/haiku", "ollama/llama3.2"]);
    }

    fn env_for(repo: &TempRepo, interactive: bool) -> AppEnv {
        let mut env = AppEnv::for_repo(Paths::new(repo.home()), repo.root(), interactive).unwrap();
        env.agent_session = false;
        env
    }

    fn set(
        env: &AppEnv,
        key: &str,
        value: &str,
        scope: Option<SettingScope>,
    ) -> anyhow::Result<Saved> {
        config_set_in(Some(env), &env.paths, key, value, scope)
    }

    #[test]
    fn doctor_checks_hermes_once_your_settings_turn_it_on() {
        let repo = TempRepo::new().unwrap();
        let env = env_for(&repo, false);
        let ids = |flags: Vec<String>| -> Vec<String> {
            let settings = AppSettings::load(&env, flags, false).unwrap();
            adapters_here(Some(&env), settings.resolved())
                .unwrap()
                .iter()
                .map(|adapter| adapter.id().to_owned())
                .collect()
        };
        let off = ids(Vec::new());
        assert!(off.iter().any(|id| id == "claude-code"), "{off:?}");
        assert!(off.iter().any(|id| id == "opencode-acp"), "{off:?}");
        assert!(!off.iter().any(|id| id == "hermes"), "{off:?}");

        let on = ids(vec!["agents.hermes.enabled=true".to_owned()]);
        assert!(on.iter().any(|id| id == "hermes"), "{on:?}");
    }

    #[test]
    fn config_set_carries_a_project_file_trust_forward() {
        let repo = TempRepo::new().unwrap();
        let env = env_for(&repo, false);
        let trusted = || TrustStore::load(&env.state_dir);
        let project = env.project_config();
        repo.write(".argonauts/config.toml", "[sandbox]\nnetwork = true\n")
            .unwrap();
        trusted().trust(&project).unwrap();
        let saved = set(&env, "ui.bell", "false", Some(SettingScope::Project)).unwrap();
        assert_eq!(saved.note, None);
        assert!(trusted().was_trusted(&project));
        let settings = AppSettings::load(&env, Vec::new(), false).unwrap();
        assert!(settings.settings().sandbox.network);
        assert!(!settings.settings().ui.bell);
        assert!(
            settings.resolved().warnings.is_empty(),
            "{:?}",
            settings.resolved().warnings
        );

        // A file you never trusted, which sets a safety key, stays untrusted and says so.
        let local = env.project_local_config();
        repo.write(
            ".argonauts/config.local.toml",
            "[sandbox]\nnetwork = true\n",
        )
        .unwrap();
        let saved = set(&env, "crew.main", "codex", Some(SettingScope::ProjectLocal)).unwrap();
        assert_eq!(saved.file, local);
        let note = saved.note.unwrap();
        assert!(note.contains("stays untrusted"), "{note}");
        assert!(note.contains("argonauts setup"), "{note}");
        assert!(!trusted().is_trusted(&local));

        // A new file, or one that sets no safety key, holds only what you wrote.
        std::fs::remove_file(&project).unwrap();
        std::fs::remove_file(&local).unwrap();
        trusted().trust(&project).unwrap();
        trusted().trust(&local).unwrap();
        repo.write(".argonauts/config.local.toml", "[ui]\nbell = true\n")
            .unwrap();
        for (key, value, scope, file) in [
            (
                "captain_or_crew.captain_max_files",
                "5",
                SettingScope::Project,
                &project,
            ),
            ("crew.main", "codex", SettingScope::ProjectLocal, &local),
        ] {
            let saved = set(&env, key, value, Some(scope)).unwrap();
            assert_eq!((&saved.file, saved.note), (file, None), "{key}");
            assert!(trusted().was_trusted(file), "{key}");
        }
    }

    #[test]
    fn config_set_of_the_global_file_needs_no_repository() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        for (key, value, leaf) in [
            ("ui.bell", "false", "bell = false"),
            (
                "captain_or_crew.captain_max_files",
                "5",
                "captain_max_files = 5",
            ),
        ] {
            let saved = config_set_in(None, &paths, key, value, None).unwrap();
            assert_eq!(saved.file, paths.global_config(), "{key}");
            let text = std::fs::read_to_string(&saved.file).unwrap();
            assert!(text.contains(leaf), "{key} in {text}");
        }
        let err = config_set_in(
            None,
            &paths,
            "captain_or_crew.captain_max_files",
            "5",
            Some(SettingScope::Project),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("the project's folder"), "{err}");
    }

    /// With no flag, a setting saves where your changes go, as the app saves one: everywhere by
    /// default, a key for one repository in this project just for you, and wherever your
    /// global file's `save_changes_to` sends your changes.
    #[test]
    fn config_set_saves_where_your_changes_go() {
        let repo = TempRepo::new().unwrap();
        // merge.target and save_changes_to are safety keys, so you're at a terminal.
        let env = env_for(&repo, true);
        for (key, value, file) in [
            ("crew.main", "codex", env.global_config()),
            ("ui.bell", "false", env.global_config()),
            (
                "captain_or_crew.captain_max_files",
                "5",
                env.global_config(),
            ),
            ("merge.target", "main", env.project_local_config()),
            ("save_changes_to", "\"just_you\"", env.global_config()),
            // Your global file now sends your changes to this project, just for you.
            ("ui.bell", "true", env.project_local_config()),
            ("save_changes_to", "\"this_project\"", env.global_config()),
            ("crew.main", "claude-code", env.project_config()),
        ] {
            let written = set(&env, key, value, None).unwrap().file;
            assert_eq!(written, file, "{key}");
            let leaf = key.rsplit('.').next().unwrap();
            let text = std::fs::read_to_string(&written).unwrap();
            assert!(text.contains(&format!("{leaf} = ")), "{key} in {text}");
        }
        // A change kept for one app session has no file here, so it asks for one.
        set(&env, "save_changes_to", "\"this_session\"", None).unwrap();
        let err = set(&env, "ui.bell", "false", None).unwrap_err().to_string();
        assert!(err.contains("--local"), "{err}");
    }

    #[test]
    fn config_set_flags_pick_the_file() {
        let repo = TempRepo::new().unwrap();
        let env = env_for(&repo, false);
        let key = "ui.bell";
        for (scope, file) in [
            (SettingScope::ProjectLocal, env.project_local_config()),
            (SettingScope::Project, env.project_config()),
            (SettingScope::Global, env.global_config()),
        ] {
            let written = set(&env, key, "false", Some(scope)).unwrap().file;
            assert_eq!(written, file);
            assert!(
                std::fs::read_to_string(&file)
                    .unwrap()
                    .contains("bell = false")
            );
        }
    }

    #[test]
    fn config_set_of_a_safety_key_needs_a_terminal() {
        let repo = TempRepo::new().unwrap();
        let env = env_for(&repo, false);
        let err = set(
            &env,
            "approvals.merge.mode",
            "auto",
            Some(SettingScope::Global),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("needs you at a terminal"), "{err}");
        assert!(!env.global_config().exists());
        let at_terminal = env_for(&repo, true);
        set(
            &at_terminal,
            "approvals.merge.mode",
            "\"auto_preview\"",
            Some(SettingScope::Global),
        )
        .unwrap();
        assert!(
            std::fs::read_to_string(env.global_config())
                .unwrap()
                .contains("auto_preview")
        );
    }

    #[test]
    fn config_set_refuses_an_unknown_key_and_the_session_scope() {
        let repo = TempRepo::new().unwrap();
        let env = env_for(&repo, false);
        assert!(set(&env, "no.such.key", "1", None).is_err());
        let err = set(&env, "ui.bell", "false", Some(SettingScope::Session))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--local"), "{err}");
    }

    fn waiting(run: &str, days_ago: i64, remind: bool) -> RunSummary {
        RunSummary {
            run: RunId::new(run).unwrap(),
            title: run.to_owned(),
            mode: RunMode::Crew,
            workflow: "council".to_owned(),
            state: RunState::Waiting,
            stopped_at: "the plan approval".to_owned(),
            updated: OffsetDateTime::now_utc() - time::Duration::days(days_ago),
            spent: Spend::default(),
            sessions_to_resume: 0,
            waiting: vec![ApprovalId::numbered(1)],
            live: false,
            remind,
            name: None,
            shape_known: true,
        }
    }

    #[test]
    fn doctor_reminds_of_runs_waiting_too_long() {
        let mut merged = waiting("run-01", 30, false);
        merged.state = RunState::Merged;
        let runs = vec![
            merged,
            waiting("run-02", 2, false),
            waiting("run-03", 9, false),
            waiting("run-04", 1, true),
        ];
        let reminders = waiting_reminders(&runs, 7, OffsetDateTime::now_utc(), |_| (2, 4096));
        let names: Vec<&str> = reminders.iter().map(|r| r.run.as_str()).collect();
        assert_eq!(names, vec!["run-03", "run-04"]);
        assert_eq!(reminders[0].days, 9);
        assert_eq!(reminders[0].stopped_at, "the plan approval");
        assert_eq!(
            (reminders[0].worktrees, reminders[0].worktree_bytes),
            (2, 4096)
        );
    }

    #[test]
    fn worktree_usage_counts_each_task_and_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(worktree_usage(&dir.path().join("none")), (0, 0));
        for task in ["task-1", "task-2"] {
            std::fs::create_dir_all(dir.path().join(task).join("src")).unwrap();
            std::fs::write(dir.path().join(task).join("src").join("a.rs"), "12345").unwrap();
        }
        std::os::unix::fs::symlink("/", dir.path().join("task-1").join("root")).unwrap();
        let (count, bytes) = worktree_usage(dir.path());
        assert_eq!(count, 2);
        assert!((10..10_000).contains(&bytes), "{bytes}");
    }

    #[test]
    fn waiting_runs_are_the_runs_that_wait_for_you() {
        let mut done = waiting("run-01", 1, false);
        done.waiting.clear();
        let runs = vec![
            done,
            waiting("run-02", 3, false),
            waiting("run-03", 1, false),
        ];
        let names: Vec<String> = waiting_for_you(runs)
            .into_iter()
            .map(|run| run.run.to_string())
            .collect();
        assert_eq!(names, vec!["run-03", "run-02"]);
    }

    #[test]
    fn config_show_is_toml_headed_by_its_files_and_flags() {
        let repo = TempRepo::new().unwrap();
        repo.write(
            ".argonauts/config.toml",
            "[captain_or_crew]\ncaptain_max_files = 5\n",
        )
        .unwrap();
        let env = env_for(&repo, false);
        let flags = vec!["ui.bell=false".to_owned()];
        let settings = AppSettings::load(&env, flags.clone(), false).unwrap();
        let text = show(Some(&env), &env.paths, settings.resolved(), &flags).unwrap();
        assert!(
            text.contains(&format!("sha256 {}", settings.sha256())),
            "{text}"
        );
        assert!(text.contains("config.toml (not found)"), "{text}");
        assert!(text.contains(".argonauts/config.toml (read)"), "{text}");
        assert!(text.contains("# --set ui.bell=false"), "{text}");
        let reparsed: Settings = toml::from_str(&text).unwrap();
        assert_eq!(&reparsed, settings.settings());
        assert_eq!(reparsed.captain_or_crew.captain_max_files, 5);
        assert!(!reparsed.ui.bell);
    }

    #[test]
    fn the_schema_describes_the_settings() {
        let schema = config_schema();
        let properties = schema["properties"].as_object().unwrap();
        for key in ["workflow", "crew", "approvals"] {
            assert!(properties.contains_key(key), "{key}");
        }
    }
}
