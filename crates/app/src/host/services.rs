//! Every worker kind joined in one `WorkerDirectory`, and the `Services` the engine gets. The
//! app builds the same directory for its chat, triage, and narrator sessions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::Context as _;
use argonauts_adapter_opencode::{LocalUrls, OpenCodeAdapter};
use argonauts_conductor::{
    HarnessWorker, boards_with, gate_runner, handoff_planner, limits_service,
};
use argonauts_core::clock::SystemClock;
use argonauts_core::engine::{RepoContext, Services};
use argonauts_core::future::BoxFuture;
use argonauts_core::limits::{CostEstimate, HealthBoard, QuotaBoard, TrackRecords};
use argonauts_core::model_catalog::ModelCatalog;
use argonauts_core::names::WorkerRef;
use argonauts_core::paths::create_private_dir_all;
use argonauts_core::registry::{ModelEntry, ModelRegistry, RegistryKind};
use argonauts_core::settings::Settings;
use argonauts_core::steps::SchemaStepRunner;
use argonauts_core::worker::{Worker, WorkerDirectory, WorkerError};
use argonauts_core::{Adapter, Paths};
use argonauts_endpoints::EndpointWorker;
use argonauts_learning::records::LiveRecords;
use argonauts_policy::{SettingsReloader, approval_policy};
use argonauts_registry::models::{builtin_endpoint, endpoint_settings};
use time::OffsetDateTime;
use tokio::sync::watch;

use super::harnesses::{self, HarnessAdapters, RepoModels};
use crate::adapters::builtin_adapters;
use crate::app_settings::AppSettings;

/// Where a directory reads its settings: fixed for a run's life, or following each change you
/// make in the app. Either way with the agents a settings layer turned on, since a built-in
/// profile that's off by default counts only then.
#[derive(Clone)]
pub(crate) enum SettingsSource {
    Fixed {
        settings: Arc<Settings>,
        turned_on: Vec<String>,
    },
    Live {
        settings: watch::Receiver<Arc<Settings>>,
        turned_on: watch::Receiver<Vec<String>>,
    },
}

impl SettingsSource {
    pub(crate) fn fixed(settings: Arc<Settings>, turned_on: Vec<String>) -> Self {
        Self::Fixed {
            settings,
            turned_on,
        }
    }

    /// The app's settings now and after each change.
    pub(crate) fn live(settings: &AppSettings) -> Self {
        Self::Live {
            settings: settings.follow(),
            turned_on: settings.follow_turned_on(),
        }
    }

    fn current(&self) -> Arc<Settings> {
        match self {
            Self::Fixed { settings, .. } => Arc::clone(settings),
            Self::Live { settings, .. } => Arc::clone(&settings.borrow()),
        }
    }

    fn turned_on(&self) -> Vec<String> {
        match self {
            Self::Fixed { turned_on, .. } => turned_on.clone(),
            Self::Live { turned_on, .. } => turned_on.borrow().clone(),
        }
    }
}

/// Harness workers for each enabled adapter (the built-ins, OpenCode, and the manifest and ACP
/// profiles), and an endpoint worker per endpoint, each with its family from the registry.
pub(crate) struct Directory {
    settings: SettingsSource,
    home: Paths,
    repo: Option<RepoModels>,
    registry: Arc<dyn ModelRegistry>,
    quota: Arc<dyn QuotaBoard>,
    health: Arc<dyn HealthBoard>,
    builtins: Vec<Arc<dyn Adapter>>,
    built: Mutex<Built>,
}

/// What the settings in `settings` give: the adapters, and the workers built on first use, one
/// per worker and model.
struct Built {
    settings: Arc<Settings>,
    adapters: Vec<Arc<dyn Adapter>>,
    harnesses: HarnessAdapters,
    workers: HashMap<WorkerRef, Arc<dyn Worker>>,
}

impl Directory {
    /// The built-ins, then every `[[harness]]` entry that counts under these settings.
    fn adapters(
        &self,
        settings: &Settings,
        harnesses: &mut HarnessAdapters,
    ) -> Vec<Arc<dyn Adapter>> {
        let manifests = harnesses::manifests(
            settings,
            &self.home,
            self.repo.as_ref(),
            &self.settings.turned_on(),
        );
        let mut adapters: Vec<Arc<dyn Adapter>> = self
            .builtins
            .iter()
            .map(|adapter| match adapter.id() {
                "opencode" => Arc::new(OpenCodeAdapter::with_local_urls(opencode_local_urls(
                    settings,
                ))) as Arc<dyn Adapter>,
                _ => adapter.clone(),
            })
            .collect();
        adapters.extend(harnesses.adapters(manifests, &self.builtins, &self.home));
        adapters
    }

    /// The adapters and workers for the settings in effect now. A worker keeps the settings it
    /// was built with, such as its binary and whether it's on, and the harness list hangs on
    /// them too, so a change means building each again.
    fn current(&self) -> MutexGuard<'_, Built> {
        let settings = self.settings.current();
        let mut built = self.built.lock().unwrap_or_else(PoisonError::into_inner);
        if !Arc::ptr_eq(&built.settings, &settings) {
            let built = &mut *built;
            built.adapters = self.adapters(&settings, &mut built.harnesses);
            built.settings = settings;
            built.workers.clear();
        }
        built
    }

    /// The registry's family, or the adapter's own when the registry doesn't know the model.
    fn family(&self, worker: &WorkerRef, adapter: Option<&Arc<dyn Adapter>>) -> String {
        let family = self.registry.family(worker);
        match adapter {
            Some(adapter) if family.is_empty() || family == "unknown" => {
                adapter.default_family().to_owned()
            }
            _ => family,
        }
    }

    fn build(
        &self,
        settings: &Arc<Settings>,
        adapters: &[Arc<dyn Adapter>],
        worker: &WorkerRef,
    ) -> Result<Arc<dyn Worker>, WorkerError> {
        if let Some(adapter) = adapters
            .iter()
            .find(|adapter| adapter.id() == worker.worker)
        {
            if !enabled(settings, &worker.worker) {
                return Err(WorkerError::Disabled {
                    worker: worker.clone(),
                    reason: format!("agents.{}.enabled is false", worker.worker),
                });
            }
            let family = self.family(worker, Some(adapter));
            let harness = HarnessWorker::new(
                adapter.clone(),
                worker.clone(),
                family,
                settings.clone(),
                self.quota.clone(),
                self.health.clone(),
            )
            .with_registry(self.registry.clone())
            // Its quota probes and concurrency slots belong in this home, not the process's.
            .with_home(self.home.clone());
            return Ok(Arc::new(harness));
        }
        // Your table over the preset of the same name, so `ollama` works with no table at all.
        if let Some(endpoint) = endpoint_settings(settings, &worker.worker) {
            let family = self.family(worker, None);
            return Ok(Arc::new(EndpointWorker::new(
                &worker.worker,
                &endpoint,
                worker.clone(),
                family,
                self.registry.clone(),
                self.quota.clone(),
                self.health.clone(),
            )));
        }
        Err(WorkerError::Unknown(worker.clone()))
    }
}

fn enabled(settings: &Settings, id: &str) -> bool {
    settings.agents.get(id).is_none_or(|agent| agent.enabled)
}

/// Found where `agents.<id>.binary` points, or on PATH. Nothing runs.
pub(crate) fn installed(settings: &Settings, adapter: &Arc<dyn Adapter>) -> bool {
    let configured = settings
        .agents
        .get(adapter.id())
        .and_then(|agent| agent.binary.clone());
    match configured {
        Some(binary) => binary.is_file(),
        None => on_path(adapter.binary()),
    }
}

impl WorkerDirectory for Directory {
    fn get(&self, worker: &WorkerRef) -> Result<Arc<dyn Worker>, WorkerError> {
        let mut built = self.current();
        if let Some(found) = built.workers.get(worker) {
            return Ok(found.clone());
        }
        let made = self.build(&built.settings, &built.adapters, worker)?;
        built.workers.insert(worker.clone(), made.clone());
        Ok(made)
    }

    fn available(&self) -> Vec<WorkerRef> {
        let (settings, adapters) = {
            let built = self.current();
            (Arc::clone(&built.settings), built.adapters.clone())
        };
        let harnesses = adapters
            .iter()
            .filter(|adapter| enabled(&settings, adapter.id()) && installed(&settings, adapter))
            .map(|adapter| WorkerRef::new(adapter.id(), None));
        let known = self.registry.list();
        // Yours, then a preset such as `ollama` once the registry knows a model of it.
        let mut endpoint_ids: Vec<&str> = settings.endpoints.keys().map(String::as_str).collect();
        for entry in &known {
            let id = entry.id.worker.as_str();
            if entry.kind == RegistryKind::Endpoint
                && builtin_endpoint(id).is_some()
                && !endpoint_ids.contains(&id)
            {
                endpoint_ids.push(id);
            }
        }
        let endpoints = endpoint_ids.into_iter().flat_map(|id| {
            let models: Vec<WorkerRef> = known
                .iter()
                .filter(|entry| entry.kind == RegistryKind::Endpoint && entry.id.worker == id)
                .map(|entry| entry.id.clone())
                .collect();
            if models.is_empty() {
                vec![WorkerRef::new(id, None)]
            } else {
                models
            }
        });
        harnesses.chain(endpoints).collect()
    }

    fn enforces_read_only(&self, worker: &WorkerRef) -> bool {
        self.current()
            .adapters
            .iter()
            .find(|adapter| adapter.id() == worker.worker)
            .is_some_and(|adapter| adapter.capabilities().enforces_read_only)
    }

    fn isolates(&self, worker: &WorkerRef) -> bool {
        let built = self.current();
        match built
            .adapters
            .iter()
            .find(|adapter| adapter.id() == worker.worker)
        {
            Some(adapter) => adapter.capabilities().isolates,
            // An endpoint answers with no tools, so nothing of yours reaches it.
            None => endpoint_settings(&built.settings, &worker.worker).is_some(),
        }
    }
}

/// The model registry for the settings in effect, built again when they change, since the
/// endpoints and their models come from the settings. What discovery found carries over
/// through `cache`.
pub(crate) struct LiveRegistry {
    settings: watch::Receiver<Arc<Settings>>,
    repo: Option<RepoModels>,
    cache: Option<PathBuf>,
    current: Mutex<(Arc<Settings>, Arc<dyn ModelRegistry>)>,
}

impl LiveRegistry {
    pub(crate) fn new(
        settings: watch::Receiver<Arc<Settings>>,
        repo: Option<RepoModels>,
        cache: Option<PathBuf>,
    ) -> Self {
        let now = Arc::clone(&settings.borrow());
        let registry = harnesses::registry(&now, repo.as_ref(), cache.as_deref());
        Self {
            settings,
            repo,
            cache,
            current: Mutex::new((now, registry)),
        }
    }

    /// Trusting the repo's `models.toml` reloads the settings, so it reaches here too.
    fn registry(&self) -> Arc<dyn ModelRegistry> {
        let settings = Arc::clone(&self.settings.borrow());
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if !Arc::ptr_eq(&current.0, &settings) {
            let registry =
                harnesses::registry(&settings, self.repo.as_ref(), self.cache.as_deref());
            *current = (settings, registry);
        }
        Arc::clone(&current.1)
    }
}

impl ModelRegistry for LiveRegistry {
    fn get(&self, worker: &WorkerRef) -> Option<ModelEntry> {
        self.registry().get(worker)
    }

    fn family(&self, worker: &WorkerRef) -> String {
        self.registry().family(worker)
    }

    fn list(&self) -> Vec<ModelEntry> {
        self.registry().list()
    }

    fn estimate(&self, worker: &WorkerRef, input_tokens: u64, output_tokens: u64) -> CostEstimate {
        self.registry()
            .estimate(worker, input_tokens, output_tokens)
    }

    fn discover(&self) -> BoxFuture<'_, Vec<ModelEntry>> {
        let registry = self.registry();
        Box::pin(async move {
            let found = registry.discover().await;
            // Settings that changed meanwhile built their registry from the older cache.
            let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
            if !Arc::ptr_eq(&current.1, &registry) {
                current.1 =
                    harnesses::registry(&current.0, self.repo.as_ref(), self.cache.as_deref());
            }
            found
        })
    }

    fn refreshed(&self) -> Option<OffsetDateTime> {
        self.registry().refreshed()
    }

    fn catalog(&self) -> ModelCatalog {
        self.registry().catalog()
    }

    fn models_reported(&self, harness: &str, models: &[argonauts_core::ReportedModel]) {
        self.registry().models_reported(harness, models);
    }
}

/// Whether `binary` names a file here or on `PATH`.
fn on_path(binary: &str) -> bool {
    if binary.contains('/') {
        return std::path::Path::new(binary).is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(binary).is_file()))
}

/// Your `[endpoints.ollama]` and `[endpoints.lmstudio]` addresses, which OpenCode is told for a
/// local model in place of its own defaults.
fn opencode_local_urls(settings: &Settings) -> LocalUrls {
    ["ollama", "lmstudio"]
        .into_iter()
        .fold(LocalUrls::new(), |urls, id| {
            match endpoint_settings(settings, id).and_then(|endpoint| endpoint.base_url) {
                Some(base_url) => urls.with(id, base_url),
                None => urls,
            }
        })
}

/// The directory for these settings. `repo` adds the repo's `models.toml` harness entries once
/// you trust that file.
pub(crate) fn directory(
    settings: SettingsSource,
    home: &Paths,
    repo: Option<&RepoModels>,
    registry: Arc<dyn ModelRegistry>,
    quota: Arc<dyn QuotaBoard>,
    health: Arc<dyn HealthBoard>,
) -> Arc<dyn WorkerDirectory> {
    Arc::new(Directory {
        settings,
        home: home.clone(),
        repo: repo.cloned(),
        registry,
        quota,
        health,
        builtins: builtin_adapters(),
        // Settings no source hands out, so the first use builds from the ones in effect then.
        built: Mutex::new(Built {
            settings: Arc::new(Settings::default()),
            adapters: Vec::new(),
            harnesses: HarnessAdapters::default(),
            workers: HashMap::new(),
        }),
    })
}

/// Everything the engine gets for one run. `turned_on` is what `agents_turned_on` gave when the
/// run started.
pub(crate) fn services(
    settings: Arc<Settings>,
    turned_on: Vec<String>,
    home: &Paths,
    repo: &RepoContext,
    reload: SettingsReloader,
) -> anyhow::Result<Services> {
    // The policy keeps its shadow records and spot checks here, and asks at every point when
    // the folder is missing, since a record it couldn't keep could raise autonomy.
    let state_dir = home.state_dir(&repo.id);
    create_private_dir_all(&state_dir)
        .with_context(|| format!("creating {}", state_dir.display()))?;
    let repo_models = RepoModels::new(&repo.root, &state_dir);
    let registry = harnesses::registry(&settings, Some(&repo_models), Some(&home.models_cache()));
    let (quota, health) = boards_with(home, &settings.health);
    let workers = directory(
        SettingsSource::fixed(settings.clone(), turned_on),
        home,
        Some(&repo_models),
        registry.clone(),
        quota.clone(),
        health.clone(),
    );
    let steps = Arc::new(SchemaStepRunner);
    let policy = approval_policy(
        settings.clone(),
        reload,
        &state_dir,
        workers.clone(),
        steps.clone(),
    );
    let track_records: Arc<dyn TrackRecords> = Arc::new(LiveRecords::new(
        state_dir.clone(),
        repo.root.clone(),
        settings.clone(),
    ));
    let handoff = handoff_planner(
        settings.clone(),
        registry.clone(),
        quota.clone(),
        health.clone(),
        track_records.clone(),
        Some(repo.run_dir.clone()),
    );
    let limits = limits_service(
        settings.clone(),
        registry.clone(),
        workers.clone(),
        quota.clone(),
        health.clone(),
        handoff.clone(),
    );
    Ok(Services {
        workers,
        steps,
        policy,
        limits,
        handoff,
        quota,
        health,
        gates: gate_runner(settings),
        registry,
        track_records,
        clock: Arc::new(SystemClock),
    })
}

#[cfg(test)]
mod tests {
    use argonauts_core::RepoId;
    use argonauts_core::layout::project_files;
    use argonauts_core::settings::agents::EndpointSettings;
    use argonauts_core::worker::WorkerKind;

    use super::*;
    use crate::trust::TrustStore;

    const HARNESSES: &str = r#"
[[harness]]
id = "lines-agent"
display_name = "Lines Agent"
binary = "/opt/lines-agent/bin/lines-agent"
family = "lines"
read_only_args = ["--read-only"]
build_args = []

[[harness]]
id = "acp-agent"
display_name = "ACP Agent"
binary = "/opt/acp-agent/bin/acp-agent"
family = "acp"
read_only_args = []
build_args = []
transport = "acp"

[harness.acp]
modes = { read_only = "plan", build = "default" }
model = "legacy-set-model"
effort = "none"
report = "final-prompt"
resume = "load"
steer = "input"
usage = ["end-turn"]
quota = "none"

[[harness]]
id = "codex"
display_name = "Not Codex"
binary = "/opt/fake/codex"
family = "fake"
read_only_args = []
build_args = []
"#;

    fn directory_for(
        settings: Settings,
        home: &Paths,
        repo: Option<&RepoModels>,
    ) -> Arc<dyn WorkerDirectory> {
        directory_turned_on(settings, Vec::new(), home, repo)
    }

    fn directory_turned_on(
        settings: Settings,
        turned_on: Vec<String>,
        home: &Paths,
        repo: Option<&RepoModels>,
    ) -> Arc<dyn WorkerDirectory> {
        let settings = Arc::new(settings);
        let registry = harnesses::registry(&settings, repo, None);
        let (quota, health) = boards_with(home, &settings.health);
        directory(
            SettingsSource::fixed(settings, turned_on),
            home,
            repo,
            registry,
            quota,
            health,
        )
    }

    /// Only Claude Code keeps your plugins out of a blind role's session among the CLIs, and an
    /// endpoint loads nothing of yours. The rest load their own MCP servers and extensions.
    #[test]
    fn only_claude_code_and_endpoints_take_a_blind_role() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        std::fs::create_dir_all(home.home()).unwrap();
        let mut settings = Settings::default();
        settings
            .endpoints
            .insert("local".to_owned(), EndpointSettings::default());
        let workers = directory_for(settings, &home, None);
        let isolates = |id: &str| workers.isolates(&WorkerRef::new(id, None));
        assert!(isolates("claude-code"));
        assert!(isolates("local"));
        for id in ["codex", "antigravity-cli", "opencode", "unknown-agent"] {
            assert!(!isolates(id), "{id}");
        }
    }

    #[test]
    fn directory_joins_builtins_manifests_and_endpoints() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        std::fs::create_dir_all(home.home()).unwrap();
        std::fs::write(home.global_models(), HARNESSES).unwrap();
        let mut settings = Settings::default();
        settings
            .endpoints
            .insert("local".to_owned(), EndpointSettings::default());
        settings.agents.get_mut("codex").unwrap().enabled = false;
        let workers = directory_for(settings, &home, None);

        let claude = workers
            .get(&WorkerRef::new("claude-code", Some("haiku".to_owned())))
            .unwrap();
        assert_eq!(claude.kind(), WorkerKind::Harness);
        assert_eq!(claude.worker_ref().model.as_deref(), Some("haiku"));
        assert!(Arc::ptr_eq(
            &claude,
            &workers
                .get(&WorkerRef::new("claude-code", Some("haiku".to_owned())))
                .unwrap()
        ));
        assert_eq!(
            workers
                .get(&WorkerRef::new("lines-agent", None))
                .unwrap()
                .family(),
            "lines"
        );
        assert_eq!(
            workers
                .get(&WorkerRef::new("acp-agent", None))
                .unwrap()
                .family(),
            "acp"
        );
        let endpoint = workers
            .get(&WorkerRef::new("local", Some("llama3.2".to_owned())))
            .unwrap();
        assert_eq!(endpoint.kind(), WorkerKind::Endpoint);
        assert!(matches!(
            workers.get(&WorkerRef::new("codex", None)),
            Err(WorkerError::Disabled { .. })
        ));
        assert!(matches!(
            workers.get(&WorkerRef::new("nobody", None)),
            Err(WorkerError::Unknown(_))
        ));
        let available = workers.available();
        assert!(available.contains(&WorkerRef::new("local", None)));
        assert!(!available.iter().any(|worker| worker.worker == "codex"));
        assert!(
            !available
                .iter()
                .any(|worker| worker.worker == "lines-agent"),
            "its binary isn't installed"
        );
    }

    #[test]
    fn a_live_directory_follows_each_settings_change() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        let mut before = Settings::default();
        before.agents.get_mut("codex").unwrap().enabled = false;
        let (live, follow) = watch::channel(Arc::new(before.clone()));
        let registry: Arc<dyn ModelRegistry> =
            Arc::new(LiveRegistry::new(follow.clone(), None, None));
        let (quota, health) = boards_with(&home, &before.health);
        let (turn_on, turned_on) = watch::channel(Vec::new());
        let workers = directory(
            SettingsSource::Live {
                settings: follow,
                turned_on,
            },
            &home,
            None,
            registry,
            quota,
            health,
        );
        let codex = WorkerRef::new("codex", None);
        let local = WorkerRef::new("local", None);
        let claude = WorkerRef::new("claude-code", None);
        let first_claude = workers.get(&claude).unwrap();
        assert!(matches!(
            workers.get(&codex),
            Err(WorkerError::Disabled { .. })
        ));
        assert!(matches!(workers.get(&local), Err(WorkerError::Unknown(_))));

        let mut after = before;
        after.agents.get_mut("codex").unwrap().enabled = true;
        after
            .endpoints
            .insert("local".to_owned(), EndpointSettings::default());
        let hermes = WorkerRef::new("hermes", None);
        assert!(matches!(workers.get(&hermes), Err(WorkerError::Unknown(_))));
        turn_on.send_replace(vec!["hermes".to_owned()]);
        live.send_replace(Arc::new(after));

        assert!(workers.get(&codex).is_ok());
        assert_eq!(workers.get(&hermes).unwrap().kind(), WorkerKind::Harness);
        assert_eq!(workers.get(&local).unwrap().kind(), WorkerKind::Endpoint);
        assert!(workers.available().contains(&local));
        assert!(
            !Arc::ptr_eq(&first_claude, &workers.get(&claude).unwrap()),
            "built again with the new settings"
        );
    }

    #[test]
    fn the_repos_state_folder_exists_before_the_first_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let id = RepoId::for_repo(&root).unwrap();
        let worktrees = home.home().join("worktrees");
        let repo = RepoContext {
            run_dir: home.state_dir(&id).join("runs").join("run-01"),
            base_worktree: worktrees.join("base"),
            worktrees,
            root,
            id,
        };
        assert!(!home.state_dir(&repo.id).exists());
        let reload: SettingsReloader = Arc::new(|| None);
        services(
            Arc::new(Settings::default()),
            Vec::new(),
            &home,
            &repo,
            reload,
        )
        .unwrap();
        let state = std::fs::metadata(home.state_dir(&repo.id)).unwrap();
        assert!(state.is_dir());
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&state.permissions()) & 0o077,
            0
        );
    }

    #[test]
    fn repo_harnesses_need_trust_and_an_outside_binary() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".argonauts")).unwrap();
        let inside = repo.join("tool");
        let models = format!(
            "[[harness]]\nid = \"outside\"\ndisplay_name = \"Outside\"\nbinary = \"/opt/outside\"\nfamily = \"o\"\nread_only_args = []\nbuild_args = []\n\n\
             [[harness]]\nid = \"inside\"\ndisplay_name = \"Inside\"\nbinary = \"{}\"\nfamily = \"i\"\nread_only_args = []\nbuild_args = []\n",
            inside.display()
        );
        let path = repo.join(".argonauts").join(project_files::MODELS);
        std::fs::write(&path, models).unwrap();

        let state_dir = home.state_dir(&RepoId::for_repo(&repo).unwrap());
        let models = RepoModels::new(&repo, &state_dir);
        let untrusted = directory_for(Settings::default(), &home, Some(&models));
        assert!(untrusted.get(&WorkerRef::new("outside", None)).is_err());

        std::fs::create_dir_all(&state_dir).unwrap();
        TrustStore::load(&state_dir).trust(&path).unwrap();
        let trusted = directory_for(Settings::default(), &home, Some(&models));
        assert!(trusted.get(&WorkerRef::new("outside", None)).is_ok());
        assert!(trusted.get(&WorkerRef::new("inside", None)).is_err());
    }
    #[test]
    fn a_built_in_endpoint_needs_no_table_and_yours_moves_it() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        let ollama = WorkerRef::new("ollama", Some("llama3.2".to_owned()));
        let preset = directory_for(Settings::default(), &home, None);
        let worker = preset.get(&ollama).unwrap();
        assert_eq!(worker.kind(), WorkerKind::Endpoint);

        let mut moved = Settings::default();
        moved.endpoints.insert(
            "ollama".to_owned(),
            EndpointSettings {
                base_url: Some("http://gpu-box:11434/v1".to_owned()),
                ..EndpointSettings::default()
            },
        );
        let settings = endpoint_settings(&moved, "ollama").unwrap();
        assert_eq!(
            settings.base_url.as_deref(),
            Some("http://gpu-box:11434/v1")
        );
        assert!(!settings.local, "a server elsewhere isn't on your machine");
        assert_eq!(
            opencode_local_urls(&moved),
            LocalUrls::new()
                .with("ollama", "http://gpu-box:11434/v1")
                .with("lmstudio", "http://127.0.0.1:1234/v1"),
            "OpenCode is told where your Ollama moved"
        );
        assert!(directory_for(moved, &home, None).get(&ollama).is_ok());
    }

    #[test]
    fn hermes_runs_only_once_a_layer_turns_it_on() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        let hermes = WorkerRef::new("hermes", None);
        let off = directory_for(Settings::default(), &home, None);
        assert!(matches!(off.get(&hermes), Err(WorkerError::Unknown(_))));

        let on = directory_turned_on(Settings::default(), vec!["hermes".to_owned()], &home, None);
        assert_eq!(on.get(&hermes).unwrap().kind(), WorkerKind::Harness);
        let template = home
            .home()
            .join(argonauts_core::layout::user_files::HARNESSES)
            .join("hermes");
        assert!(
            template.join("config.yaml").is_file(),
            "its home template is in place"
        );
    }
}
