//! The app side of the CLI's other subcommands, so the binary stays thin.

use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, anyhow, bail};
use argonauts_conductor::DoctorDetails;
use argonauts_core::api::{RunState, RunSummary};
use argonauts_core::layout::user_files;
use argonauts_core::names::WorkerRef;
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::policy::{ApprovalPolicy, ApprovalsView};
use argonauts_core::registry::ModelEntry;
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::settings::{SettingScope, Settings};
use argonauts_core::steps::SchemaStepRunner;
use argonauts_core::worker::{Worker, WorkerDirectory, WorkerError};
use argonauts_core::{Adapter, CardId, Paths, RunId};
use argonauts_registry::harnesses::agents_turned_on;
use argonauts_registry::{
    ResolvedSettings, SettingsSources, resolve, safety_keys_in, setting_meta, write_setting,
};
use serde::Serialize;
use time::OffsetDateTime;

use crate::adapters::builtin_adapters;
use crate::app_settings::AppSettings;
use crate::env::{AppEnv, SESSION_ENV};
use crate::host::harnesses::{self, HarnessAdapters, RepoModels};
use crate::live_registry::{LiveSupervisor, live_supervisors};
use crate::trust::TrustStore;

/// `! run-03 has been parked for 9 days, at the plan checkpoint. Parked runs never expire.`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedReminder {
    pub run: RunId,
    pub days: u64,
    /// Such as "the plan checkpoint".
    pub stopped_at: String,
    pub worktrees: usize,
    pub worktree_bytes: u64,
}

/// What `argonauts doctor` adds to the conductor's report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoctorExtras {
    pub parked: Vec<ParkedReminder>,
}

/// Outside a git repository, with no repo named, there are no runs to remind you of.
pub async fn doctor_extras(repo: Option<&Path>) -> anyhow::Result<DoctorExtras> {
    let Some(env) = here(repo)? else {
        return Ok(DoctorExtras::default());
    };
    let settings = AppSettings::load(&env, Vec::new(), false)?;
    let runs = crate::conductor(&env.repo_root)?.list().await?;
    let threshold = settings.settings().runs.remind_parked_days;
    let parked = parked_reminders(&runs, threshold, OffsetDateTime::now_utc(), |run| {
        worktree_usage(&run_worktrees(&env, run))
    });
    Ok(DoctorExtras { parked })
}

/// Every adapter doctor checks: the built-in four, then each `[[harness]]` entry that counts
/// here, a built-in profile that's off by default only once a settings layer turns it on.
pub fn doctor_adapters(
    repo: Option<&Path>,
    overrides: Vec<String>,
) -> anyhow::Result<Vec<Arc<dyn Adapter>>> {
    let (env, resolved) = resolve_here(repo, overrides)?;
    adapters_here(env.as_ref(), &resolved)
}

/// What doctor reports beyond versions for each adapter here: login, flags, roles, hooks, the
/// status tool and the quota probe, then the sandbox self-test and any always-on agents. It
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
    let manifests = harnesses::manifests(&resolved.settings, &paths, repo.as_ref(), &turned_on);
    let builtins = builtin_adapters();
    let mut adapters = builtins.clone();
    adapters.extend(HarnessAdapters::default().adapters(manifests, &builtins, &paths));
    Ok(adapters)
}

/// Parked runs you were reminded of, or parked longer than `threshold_days`, oldest first.
fn parked_reminders(
    runs: &[RunSummary],
    threshold_days: u32,
    now: OffsetDateTime,
    usage: impl Fn(&RunId) -> (usize, u64),
) -> Vec<ParkedReminder> {
    let threshold = time::Duration::days(i64::from(threshold_days));
    let mut reminders: Vec<ParkedReminder> = runs
        .iter()
        .filter(|run| run.state == RunState::Parked)
        .filter_map(|run| {
            let parked_for = now - run.updated;
            if !run.remind && parked_for <= threshold {
                return None;
            }
            let (worktrees, worktree_bytes) = usage(&run.run);
            Some(ParkedReminder {
                run: run.run.clone(),
                days: u64::try_from(parked_for.whole_days()).unwrap_or(0),
                stopped_at: run.stopped_at.clone(),
                worktrees,
                worktree_bytes,
            })
        })
        .collect();
    reminders.sort_by_key(|reminder| std::cmp::Reverse(reminder.days));
    reminders
}

/// `~/.argonauts/worktrees/<repo>/<run>/`, which holds one worktree per card.
fn run_worktrees(env: &AppEnv, run: &RunId) -> PathBuf {
    let any_card = CardId::new("card").ok();
    any_card
        .map(|card| env.paths.worktree_dir(&env.repo_id, run, &card))
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
pub async fn inbox(repo: Option<&Path>) -> anyhow::Result<Vec<RunSummary>> {
    let env = required(repo)?;
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

/// `argonauts status`: every live supervisor across your repos, and this repo's runs that
/// haven't ended.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StatusReport {
    pub live: Vec<LiveSupervisor>,
    pub runs: Vec<RunSummary>,
}

pub async fn status(repo: Option<&Path>) -> anyhow::Result<StatusReport> {
    let paths = home()?;
    let live = live_supervisors(&paths);
    let runs = match here(repo)? {
        Some(env) => crate::conductor(&env.repo_root)?.list().await?,
        None => Vec::new(),
    };
    let runs = runs
        .into_iter()
        .filter(|run| {
            matches!(
                run.state,
                RunState::Running | RunState::Pausing | RunState::Parked | RunState::Crashed
            )
        })
        .collect();
    Ok(StatusReport { live, runs })
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalsReport {
    pub view: ApprovalsView,
    /// Such as "CI failures wait for a CI integration."
    pub notes: Vec<String>,
}

pub async fn approvals_report(repo: Option<&Path>) -> anyhow::Result<ApprovalsReport> {
    let env = required(repo)?;
    let settings = AppSettings::load(&env, Vec::new(), false)?;
    let view = policy_for(&env, settings.shared()).view();
    let mut notes = vec!["CI failures wait for a CI integration.".to_owned()];
    if env.paths.home().join(user_files::AUTO_OFF).exists() {
        notes.push(
            "Auto is off everywhere since `argonauts auto off`: every checkpoint waits for you."
                .to_owned(),
        );
    }
    Ok(ApprovalsReport { view, notes })
}

/// `argonauts auto off`: every checkpoint is supervised until you turn it back on.
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
        Arc::new(NoWorkers),
        Arc::new(SchemaStepRunner),
    )
}

/// The policy's view and `auto_off` start no session, so they get no workers.
pub(crate) struct NoWorkers;

impl WorkerDirectory for NoWorkers {
    fn get(&self, worker: &WorkerRef) -> Result<Arc<dyn Worker>, WorkerError> {
        Err(WorkerError::Unknown(worker.clone()))
    }

    fn available(&self) -> Vec<WorkerRef> {
        Vec::new()
    }
}

/// The registry's models with what discovery last found, after a fresh discovery when asked,
/// which also updates the list `/model` starts from. Discovery never calls a model.
pub async fn models(repo: Option<&Path>, discover: bool) -> anyhow::Result<Vec<ModelEntry>> {
    let (env, resolved) = resolve_here(repo, Vec::new())?;
    let repo = env
        .as_ref()
        .map(|env| RepoModels::new(&env.repo_root, &env.state_dir));
    let cache = home()?.models_cache();
    let registry =
        crate::host::harnesses::registry(&resolved.settings, repo.as_ref(), Some(&cache));
    Ok(if discover {
        registry.discover().await
    } else {
        registry.list()
    })
}

/// `argonauts config set`: to the key's default scope unless `scope` picks the file. A safety key
/// needs a terminal, and this refuses inside an agent. Returns the file written.
pub fn config_set(
    repo: Option<&Path>,
    key: &str,
    value: &str,
    scope: Option<SettingScope>,
) -> anyhow::Result<PathBuf> {
    // Only a project file needs a repository.
    let env = match scope_for(key, scope)? {
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

/// `env` is `None` outside a git repository, where only the global file can be set.
fn config_set_in(
    env: Option<&AppEnv>,
    paths: &Paths,
    key: &str,
    value: &str,
    scope: Option<SettingScope>,
) -> anyhow::Result<Saved> {
    let scope = scope_for(key, scope)?;
    safety_needs_a_terminal(env, key, value)?;
    let (env, file) = match (scope, env) {
        (SettingScope::Global, _) => {
            let file = paths.global_config();
            write_setting(&file, key, value)?;
            return Ok(Saved { file, note: None });
        }
        (SettingScope::Session, _) => bail!(
            "{key} is saved for one app session by default; pick a file with --local, \
             --project, or --global"
        ),
        (_, None) => bail!("{key} goes in a project file, so run this inside a git repository"),
        (SettingScope::Project, Some(env)) => (env, env.project_config()),
        (SettingScope::ProjectLocal, Some(env)) => (env, env.project_local_config()),
    };
    write_project_file(env, &file, key, value)
}

/// The scope `scope` picks, or else the key's default one.
fn scope_for(key: &str, scope: Option<SettingScope>) -> anyhow::Result<SettingScope> {
    let meta = setting_meta(key)
        .ok_or_else(|| anyhow!("there's no setting {key}; `argonauts config show` lists them"))?;
    Ok(scope.unwrap_or(meta.default_scope))
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
             `argonauts init`.",
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
        .filter(|entry| entry.layer != "built-in")
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

/// The top of the git repository holding `dir`, or the current directory.
pub fn repo_root(dir: Option<&Path>) -> anyhow::Result<PathBuf> {
    let start = match dir {
        Some(dir) => dir.to_owned(),
        None => std::env::current_dir().context("reading the current directory")?,
    };
    crate::git::toplevel(&start)
}

/// The JSON Schema of every settings file.
pub fn config_schema() -> serde_json::Value {
    schemars::schema_for!(Settings).to_value()
}

fn home() -> anyhow::Result<Paths> {
    Paths::from_env().map_err(|_| anyhow!("can't find your home directory; set ARGONAUTS_HOME"))
}

/// This repo's environment, or `None` outside a git repository when no repo was named.
fn here(repo: Option<&Path>) -> anyhow::Result<Option<AppEnv>> {
    if repo.is_none() {
        let cwd = std::env::current_dir().context("reading the current directory")?;
        if crate::git::toplevel(&cwd).is_err() {
            return Ok(None);
        }
    }
    AppEnv::discover(repo).map(Some)
}

fn required(repo: Option<&Path>) -> anyhow::Result<AppEnv> {
    here(repo)?.ok_or_else(|| anyhow!("run this inside a git repository"))
}

/// The settings for this repo with each project file's trust, or the global file alone outside
/// one.
fn resolve_here(
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
        ..SettingsSources::default()
    };
    Ok((None, resolve(&sources)?))
}

#[cfg(test)]
mod tests {
    use argonauts_core::Spend;
    use argonauts_core::events::run::RunMode;
    use argonauts_core::names::CheckpointId;
    use argonauts_testkit::TempRepo;

    use super::*;

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
        let saved = set(&env, "default_agent", "codex", None).unwrap();
        assert_eq!(saved.file, local);
        let note = saved.note.unwrap();
        assert!(note.contains("stays untrusted"), "{note}");
        assert!(note.contains("argonauts init"), "{note}");
        assert!(!trusted().is_trusted(&local));

        // A new file, or one that sets no safety key, holds only what you wrote.
        std::fs::remove_file(&project).unwrap();
        std::fs::remove_file(&local).unwrap();
        trusted().trust(&project).unwrap();
        trusted().trust(&local).unwrap();
        repo.write(".argonauts/config.local.toml", "[ui]\nbell = true\n")
            .unwrap();
        for (key, value, file) in [
            ("triage.solo_max_files", "5", &project),
            ("default_agent", "codex", &local),
        ] {
            let saved = set(&env, key, value, None).unwrap();
            assert_eq!((&saved.file, saved.note), (file, None), "{key}");
            assert!(trusted().was_trusted(file), "{key}");
        }
    }

    #[test]
    fn config_set_of_the_global_file_needs_no_repository() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let saved = config_set_in(None, &paths, "ui.bell", "false", None).unwrap();
        assert_eq!(saved.file, paths.global_config());
        assert!(
            std::fs::read_to_string(&saved.file)
                .unwrap()
                .contains("bell = false")
        );
        for scope in [None, Some(SettingScope::Project)] {
            let err = config_set_in(None, &paths, "triage.solo_max_files", "5", scope)
                .unwrap_err()
                .to_string();
            assert!(err.contains("git repository"), "{err}");
        }
    }

    #[test]
    fn config_set_uses_the_key_default_scope() {
        let repo = TempRepo::new().unwrap();
        let env = env_for(&repo, false);
        for (key, value, file, scope) in [
            (
                "default_agent",
                "codex",
                env.project_local_config(),
                SettingScope::ProjectLocal,
            ),
            (
                "ui.bell",
                "false",
                env.global_config(),
                SettingScope::Global,
            ),
            (
                "triage.solo_max_files",
                "5",
                env.project_config(),
                SettingScope::Project,
            ),
        ] {
            assert_eq!(setting_meta(key).unwrap().default_scope, scope, "{key}");
            let written = set(&env, key, value, None).unwrap().file;
            assert_eq!(written, file, "{key}");
            let leaf = key.rsplit('.').next().unwrap();
            let text = std::fs::read_to_string(&written).unwrap();
            assert!(text.contains(&format!("{leaf} = ")), "{key} in {text}");
        }
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
            "\"shadow\"",
            Some(SettingScope::Global),
        )
        .unwrap();
        assert!(
            std::fs::read_to_string(env.global_config())
                .unwrap()
                .contains("shadow")
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

    fn parked(run: &str, days_ago: i64, remind: bool) -> RunSummary {
        RunSummary {
            run: RunId::new(run).unwrap(),
            title: run.to_owned(),
            mode: RunMode::Crew,
            preset: "peer-crew".to_owned(),
            state: RunState::Parked,
            stopped_at: "the plan checkpoint".to_owned(),
            updated: OffsetDateTime::now_utc() - time::Duration::days(days_ago),
            spent: Spend::default(),
            sessions_to_resume: 0,
            waiting: vec![CheckpointId::numbered(1)],
            live: false,
            remind,
        }
    }

    #[test]
    fn doctor_reminds_of_runs_parked_too_long() {
        let mut merged = parked("run-01", 30, false);
        merged.state = RunState::Merged;
        let runs = vec![
            merged,
            parked("run-02", 2, false),
            parked("run-03", 9, false),
            parked("run-04", 1, true),
        ];
        let reminders = parked_reminders(&runs, 7, OffsetDateTime::now_utc(), |_| (2, 4096));
        let names: Vec<&str> = reminders.iter().map(|r| r.run.as_str()).collect();
        assert_eq!(names, vec!["run-03", "run-04"]);
        assert_eq!(reminders[0].days, 9);
        assert_eq!(reminders[0].stopped_at, "the plan checkpoint");
        assert_eq!(
            (reminders[0].worktrees, reminders[0].worktree_bytes),
            (2, 4096)
        );
    }

    #[test]
    fn worktree_usage_counts_each_card_and_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(worktree_usage(&dir.path().join("none")), (0, 0));
        for card in ["card-1", "card-2"] {
            std::fs::create_dir_all(dir.path().join(card).join("src")).unwrap();
            std::fs::write(dir.path().join(card).join("src").join("a.rs"), "12345").unwrap();
        }
        std::os::unix::fs::symlink("/", dir.path().join("card-1").join("root")).unwrap();
        let (count, bytes) = worktree_usage(dir.path());
        assert_eq!(count, 2);
        assert!((10..10_000).contains(&bytes), "{bytes}");
    }

    #[test]
    fn the_inbox_holds_runs_that_wait_for_you() {
        let mut done = parked("run-01", 1, false);
        done.waiting.clear();
        let runs = vec![done, parked("run-02", 3, false), parked("run-03", 1, false)];
        let names: Vec<String> = waiting_for_you(runs)
            .into_iter()
            .map(|run| run.run.to_string())
            .collect();
        assert_eq!(names, vec!["run-03", "run-02"]);
    }

    #[test]
    fn config_show_is_toml_headed_by_its_files_and_flags() {
        let repo = TempRepo::new().unwrap();
        repo.write(".argonauts/config.toml", "[triage]\nsolo_max_files = 5\n")
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
        assert_eq!(reparsed.triage.solo_max_files, 5);
        assert!(!reparsed.ui.bell);
    }

    #[test]
    fn the_schema_describes_the_settings() {
        let schema = config_schema();
        let properties = schema["properties"].as_object().unwrap();
        for key in ["preset", "default_agent", "crew", "approvals"] {
            assert!(properties.contains_key(key), "{key}");
        }
    }
}
