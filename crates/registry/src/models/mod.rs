//! The model registry: built-in entries, `~/.argonauts/models.toml`, then the project's
//! `.argonauts/models.toml` once you trust it, later wins by `id`, plus what discovery finds.
//! Models are data, so a new one needs no code, and the model catalog says what `/model` shows
//! of each.

mod builtin;
mod cache;
pub mod catalog;
pub mod discovery;
mod estimate;
mod files;
mod provider;
pub mod reported;
pub mod sources;
pub mod suggested;
mod trust;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

use argonauts_core::ReportedModel;
use argonauts_core::agents::confine::Confine;
use argonauts_core::base::clock;
use argonauts_core::base::future::BoxFuture;
use argonauts_core::base::names::ModelRef;
use argonauts_core::base::paths::Paths;
use argonauts_core::models::limits::CostEstimate;
use argonauts_core::models::model_catalog::ModelCatalog;
use argonauts_core::models::registry::{
    CostUnit, ModelAccess, ModelEntry, ModelRegistry, RegistryKind,
};
use argonauts_core::settings::Settings;
use time::OffsetDateTime;

use crate::clis::every_builtin_cli;
use discovery::{CLAUDE_LIST_FRESH_FOR, ClaudeSource, Sources, agent_enabled};
use sources::ListSource;

pub use builtin::{
    CLAUDE_CODE_ALIASES, builtin_endpoint, builtin_models, cli_entry, endpoint_settings,
    usual_efforts,
};
pub use cache::{ModelCache, SourceListed};
pub use catalog::{CatalogSource, FetchedCatalog, shipped_catalog};
pub use files::{ModelTier, ModelsFile, read_registry_file, set_model_tier};
pub(crate) use provider::resolve_provider;
pub use provider::{provider_from_model_id, provider_problems};
pub use trust::ProjectModels;

/// Leaves out the repo's `.argonauts/models.toml`, since this takes no trust input; use
/// [`registry_with_trust`] with the hashes in `trusted.json` for that.
pub fn registry(settings: &Settings, repo_root: Option<&Path>) -> Arc<dyn ModelRegistry> {
    Arc::new(FileRegistry::load(settings, repo_root, &[]))
}

/// How OpenCode is asked for its models: inside `confine`, and in the repo only once you
/// trusted it, since OpenCode started there loads the repo's plugins.
#[derive(Debug, Clone)]
pub struct AskOpenCode {
    pub confine: Arc<dyn Confine>,
    pub in_repo: bool,
}

/// The registry with the repo's `.argonauts/models.toml` counting when its hash is in
/// `trusted_hashes`, starting from the model list `cache` keeps, when given, and reading
/// Claude Code's models from `claude` and the model catalog from `catalog` once a day, and
/// asking OpenCode for its models as `opencode` says. It comes back whole, so its warnings
/// and discovery's skipped sources can be shown.
pub fn registry_with_trust(
    settings: &Settings,
    repo_root: Option<&Path>,
    trusted_hashes: &[String],
    cache: Option<&Path>,
    claude: Option<ClaudeSource>,
    catalog: Option<CatalogSource>,
    opencode: Option<AskOpenCode>,
) -> Arc<FileRegistry> {
    let mut registry = FileRegistry::load(settings, repo_root, trusted_hashes);
    if let Some(opencode) = opencode {
        registry = registry.with_confine(opencode.confine);
        registry.sources.opencode_in_repo = opencode.in_repo;
    }
    if let Some(cache) = cache {
        registry = registry.with_cache(cache);
    }
    if let Some(claude) = claude {
        registry = registry.with_claude_source(claude);
    }
    if let Some(catalog) = catalog {
        registry = registry.with_catalog_source(catalog);
    }
    Arc::new(registry)
}

/// The CLIs that list their own models to a session: Claude Code in its `initialize` reply.
const REPORTING_CLIS: &[&str] = &["claude-code"];

/// `<repo>/.argonauts/models.toml`.
pub fn project_models_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".argonauts").join("models.toml")
}

/// The registry from its three layers, with what discovery adds between the built-ins and
/// your files.
#[derive(Debug)]
pub struct FileRegistry {
    settings: Settings,
    builtin: Vec<ModelEntry>,
    discovered: RwLock<Vec<ModelEntry>>,
    skipped: RwLock<Vec<String>>,
    refreshed: RwLock<Option<OffsetDateTime>>,
    /// Where discovery's list outlives this run; `None` keeps it in memory.
    cache: Option<PathBuf>,
    /// Where Claude Code's models come from; `None` lists its aliases only.
    claude: Option<ClaudeSource>,
    claude_listed: RwLock<Option<OffsetDateTime>>,
    /// Where the daily model catalog comes from; `None` keeps the one shipped.
    catalog_source: Option<CatalogSource>,
    /// The catalog fetched last, as the cache keeps it and as it reads.
    fetched: RwLock<Option<(FetchedCatalog, ModelCatalog)>>,
    yours: Vec<ModelsFile>,
    /// What each CLI that lists its own models listed last, by CLI id.
    reported: RwLock<BTreeMap<String, Vec<ReportedModel>>>,
    /// Where those lists are kept between runs; `None` keeps them in memory.
    home: Option<Paths>,
    /// (CLI id, `provider`) for the built-in profiles and every manifest in the files read,
    /// later winning by id.
    cli_providers: Vec<(String, String)>,
    warnings: Vec<String>,
    /// Where discovery asks from.
    sources: Sources,
    /// Lists given to it beside the ones discovery asks for itself.
    lists: Vec<Arc<dyn ListSource>>,
    /// When each source last listed, and whether its latest ask failed.
    listed: RwLock<Vec<SourceListed>>,
}

impl FileRegistry {
    /// `~/.argonauts/models.toml` (or `$ARGONAUTS_HOME/models.toml`) and, with a repo, its
    /// `.argonauts/models.toml` when the hash of its text is in `trusted_hashes`.
    pub fn load(settings: &Settings, repo_root: Option<&Path>, trusted_hashes: &[String]) -> Self {
        let mut registry = Self::empty(settings);
        registry.sources.repo = repo_root.map(Path::to_owned);
        match Paths::from_env() {
            Ok(paths) => {
                registry.add_global(&paths.global_models());
                registry.read_reported(paths);
            }
            Err(e) => registry
                .warnings
                .push(format!("{e}. Your own models.toml is left out.")),
        }
        if let Some(root) = repo_root {
            match ProjectModels::read(&project_models_path(root), trusted_hashes) {
                Ok(project) => registry.add_project(&project),
                Err(e) => registry
                    .warnings
                    .push(format!("{e}. Its models are left out.")),
            }
        }
        registry
    }

    /// Your own `global` file counts as it is; `project` counts only when it's trusted.
    pub fn from_files(
        settings: &Settings,
        global: Option<&Path>,
        project: Option<&ProjectModels>,
    ) -> Self {
        let mut registry = Self::empty(settings);
        if let Some(global) = global {
            registry.add_global(global);
        }
        if let Some(project) = project {
            registry.add_project(project);
        }
        registry
    }

    fn empty(settings: &Settings) -> Self {
        let cli_providers = every_builtin_cli()
            .into_iter()
            .map(|manifest| (manifest.id, manifest.provider))
            .collect();
        Self {
            settings: settings.clone(),
            builtin: builtin_models(settings),
            discovered: RwLock::new(Vec::new()),
            skipped: RwLock::new(Vec::new()),
            refreshed: RwLock::new(None),
            cache: None,
            claude: None,
            claude_listed: RwLock::new(None),
            catalog_source: None,
            fetched: RwLock::new(None),
            yours: Vec::new(),
            reported: RwLock::new(BTreeMap::new()),
            home: None,
            cli_providers,
            warnings: Vec::new(),
            sources: Sources::default(),
            lists: Vec::new(),
            listed: RwLock::new(Vec::new()),
        }
    }

    /// With the sandbox OpenCode is asked for its models in. Without one, OpenCode isn't asked.
    pub fn with_confine(mut self, confine: Arc<dyn Confine>) -> Self {
        self.sources.confine = Some(confine);
        self
    }

    /// Asking OpenCode in the repo, which you trusted, so its config there may add models.
    pub fn asking_opencode_in_the_repo(mut self) -> Self {
        self.sources.opencode_in_repo = true;
        self
    }

    /// Starts from the list `path` keeps, for the sources these settings still use, and keeps
    /// each later discovery there. With discovery off the file is neither read nor written.
    pub fn with_cache(mut self, path: &Path) -> Self {
        if !self.settings.registry.discover {
            return self;
        }
        if let Some(cache) = ModelCache::read(path) {
            let models = still_listed(&self.settings, cache.models);
            *self
                .discovered
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner) = models;
            *self
                .skipped
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner) = cache.skipped;
            *self
                .refreshed
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner) = Some(cache.refreshed);
            *self
                .claude_listed
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner) = cache.claude_listed;
            *self
                .listed
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner) = cache.sources;
            *self
                .fetched
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner) = cache
                .catalog
                .and_then(|file| file.catalog().ok().map(|catalog| (file, catalog)));
        }
        self.cache = Some(path.to_owned());
        self
    }

    /// Claude Code can't list its models, so discovery reads them from `source`, at most once
    /// a day, keeping what it read last in between.
    pub fn with_claude_source(mut self, source: ClaudeSource) -> Self {
        self.claude = Some(source);
        self
    }

    /// Discovery asks `source` too, beside the CLIs it runs itself.
    pub fn with_source(mut self, source: Arc<dyn ListSource>) -> Self {
        self.lists.push(source);
        self
    }

    /// What discovery found for `cli` last, which a source that can't answer keeps.
    fn discovered_for(&self, cli: &str) -> Vec<ModelEntry> {
        self.discovered
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|entry| entry.id.agent == cli)
            .cloned()
            .collect()
    }

    /// Every source discovery asks: the CLIs it runs, the endpoints, and the lists it was given.
    fn source_ids(&self) -> Vec<String> {
        let mut ids = discovery::source_ids(&self.settings);
        if self.settings.registry.discover {
            ids.extend(
                self.lists
                    .iter()
                    .filter(|source| agent_enabled(&self.settings, source.cli()))
                    .map(|source| source.cli().to_owned()),
            );
        }
        ids
    }

    /// `cli` listed at `at`, or failed with its last list kept.
    fn mark_listed(&self, cli: &str, at: Option<OffsetDateTime>, failed: bool) {
        let mut listed = self.listed.write().unwrap_or_else(PoisonError::into_inner);
        match listed.iter_mut().find(|known| known.cli == cli) {
            Some(known) => {
                known.at = at.or(known.at);
                known.failed = failed;
            }
            None => listed.push(SourceListed {
                cli: cli.to_owned(),
                at,
                failed,
            }),
        }
    }

    fn listed_now(&self) -> Vec<SourceListed> {
        self.listed
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// What one source answered, in place of what it listed before and where that stood, so
    /// the other lists keep their order. OpenCode's models on a server here are held to that
    /// server's window again, whichever of the two answered.
    fn replace_listed(&self, cli: &str, models: Vec<ModelEntry>) {
        let mut discovered = self
            .discovered
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let at = discovered
            .iter()
            .position(|entry| entry.id.agent == cli)
            .unwrap_or(discovered.len());
        discovered.retain(|entry| entry.id.agent != cli);
        let at = at.min(discovered.len());
        discovered.splice(at..at, models);
        discovery::hold_to_their_servers(&mut discovered);
    }

    /// The cache file as it stands now, after one source answered.
    fn keep_in_cache(&self) {
        let Some(path) = &self.cache else {
            return;
        };
        let refreshed = self
            .refreshed
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .unwrap_or_else(OffsetDateTime::now_utc);
        let cache = ModelCache {
            refreshed,
            models: self
                .discovered
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            skipped: self.discovery_skipped(),
            claude_listed: *self
                .claude_listed
                .read()
                .unwrap_or_else(PoisonError::into_inner),
            catalog: self
                .fetched
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .map(|(file, _)| file.clone()),
            sources: self.listed_now(),
        };
        if let Err(e) = cache.write(path) {
            self.note_skipped(format!(
                "couldn't keep the model list in {}: {e}",
                path.display()
            ));
        }
    }

    fn note_skipped(&self, why: String) {
        let mut skipped = self.skipped.write().unwrap_or_else(PoisonError::into_inner);
        if !skipped.contains(&why) {
            skipped.push(why);
        }
    }

    /// Asks one source again, keeping its last list when it fails.
    async fn discover_one(&self, cli: &str) {
        let answer = match self.lists.iter().find(|source| source.cli() == cli) {
            Some(source) if self.source_ids().iter().any(|id| id == cli) => {
                let last = self.discovered_for(cli);
                let answer = source.list(&self.settings, last).await;
                Some(match answer.failed {
                    Some(why) => Err(why),
                    None => Ok(answer.models),
                })
            }
            Some(_) => None,
            None => discovery::discover_source(&self.settings, &self.sources, cli).await,
        };
        match answer {
            None => return,
            Some(Ok(models)) => {
                self.replace_listed(cli, models);
                self.mark_listed(cli, Some(clock::now_utc()), false);
            }
            Some(Err(why)) => {
                self.note_skipped(discovery::listing_failed(cli, &why));
                self.mark_listed(cli, None, true);
            }
        }
        self.keep_in_cache();
    }

    /// Asks each source the app gave, one after another, each with what it listed before.
    async fn ask_sources(&self) -> Vec<(String, sources::Answer)> {
        let mut answers = Vec::new();
        for source in &self.lists {
            if !agent_enabled(&self.settings, source.cli()) {
                continue;
            }
            let last = self.discovered_for(source.cli());
            let answer = source.list(&self.settings, last).await;
            answers.push((source.cli().to_owned(), answer));
        }
        answers
    }

    /// The model catalog comes from `source` at most once a day, the one fetched last staying
    /// in between.
    pub fn with_catalog_source(mut self, source: CatalogSource) -> Self {
        self.catalog_source = Some(source);
        self
    }

    /// The source to fetch the catalog from now, when one is set and the last catalog is a day
    /// old, with that catalog.
    fn catalog_due(&self, now: OffsetDateTime) -> Option<(&CatalogSource, Option<FetchedCatalog>)> {
        let source = self.catalog_source.as_ref()?;
        let known = self
            .fetched
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|(file, _)| file.clone());
        known
            .as_ref()
            .is_none_or(|file| now - file.fetched >= catalog::CATALOG_FRESH_FOR)
            .then_some((source, known))
    }

    /// The source to read Claude Code's models from now, when one is set, Claude Code is on,
    /// and the last list is a day old.
    fn claude_due(&self, now: OffsetDateTime) -> Option<&ClaudeSource> {
        let source = self.claude.as_ref()?;
        if !agent_enabled(&self.settings, "claude-code") {
            return None;
        }
        let listed = *self
            .claude_listed
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        listed
            .is_none_or(|at| now - at >= CLAUDE_LIST_FRESH_FOR)
            .then_some(source)
    }

    /// The lists the CLIs reported last, kept in `home`'s state folder, where new ones go too.
    fn read_reported(&mut self, home: Paths) {
        let reported = self
            .reported
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        for cli in REPORTING_CLIS {
            let models = reported::read(&home.reported_models(cli));
            if !models.is_empty() {
                reported.insert((*cli).to_owned(), models);
            }
        }
        self.home = Some(home);
    }

    fn add_global(&mut self, path: &Path) {
        match read_registry_file(path) {
            Ok(file) => self.add_file(file),
            Err(e) => self.warnings.push(format!("{e}. Its models are left out.")),
        }
    }

    fn add_project(&mut self, project: &ProjectModels) {
        let Some(text) = project.text() else {
            return;
        };
        let path = project.path();
        let file = match files::parse_models_text(path, text) {
            Ok(file) => file,
            Err(e) => {
                self.warnings.push(format!("{e}. Its models are left out."));
                return;
            }
        };
        if project.trusted() {
            self.add_file(file);
            return;
        }
        let count = match file.models.len() + file.tiers.len() {
            0 => return,
            1 => "its model entry is".to_owned(),
            n => format!("its {n} model entries are"),
        };
        self.warnings.push(format!(
            "{}: {count} ignored until you trust the file (/setup, or the trust question when \
             it changes)",
            path.display()
        ));
    }

    fn add_file(&mut self, mut file: ModelsFile) {
        for manifest in std::mem::take(&mut file.clis) {
            match self
                .cli_providers
                .iter_mut()
                .find(|(id, _)| *id == manifest.id)
            {
                Some(slot) => slot.1 = manifest.provider,
                None => self.cli_providers.push((manifest.id, manifest.provider)),
            }
        }
        self.yours.push(file);
    }

    /// Files that couldn't be read or wait for your trust, each naming the file and why.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Why each source the latest discovery skipped found nothing, such as a CLI that isn't
    /// installed or an endpoint that didn't answer.
    pub fn discovery_skipped(&self) -> Vec<String> {
        self.skipped
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// A tier set on its own lands on whatever entry the layers below give that model.
    fn entries(&self) -> Vec<ModelEntry> {
        let mut all = self.builtin.clone();
        let discovered = self
            .discovered
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        merge(&mut all, discovered);
        let reported = self.reported.read().unwrap_or_else(PoisonError::into_inner);
        for (cli, models) in reported.iter() {
            let added = reported::new_entries(&self.settings, cli, models, &all);
            all.extend(added);
        }
        for file in &self.yours {
            merge(&mut all, file.models.clone());
            for rating in &file.tiers {
                if let Some(at) = position(&all, &rating.id) {
                    all[at].tier = Some(rating.tier);
                }
            }
        }
        all
    }

    fn manifest_provider(&self, worker: &ModelRef) -> Option<&str> {
        self.cli_providers
            .iter()
            .find(|(id, _)| *id == worker.agent)
            .map(|(_, provider)| provider.as_str())
    }

    /// A model the registry has no entry for still costs what its CLI or endpoint does:
    /// the CLI's default entry, or an entry made from `[endpoints.<id>]`.
    fn estimate_entry(&self, worker: &ModelRef) -> Option<ModelEntry> {
        if let Some(entry) = self.get(worker) {
            return Some(entry);
        }
        if let Some(entry) = self.get(&ModelRef::new(worker.agent.clone(), None)) {
            return Some(entry);
        }
        let endpoint = endpoint_settings(&self.settings, &worker.agent)?;
        let mut entry = builtin::cli_entry(&self.settings, &worker.agent, None);
        entry.id = worker.clone();
        entry.kind = RegistryKind::Endpoint;
        entry.tools = false;
        (entry.access, entry.cost) = if endpoint.local {
            (ModelAccess::Local, builtin::cost(CostUnit::None))
        } else {
            (ModelAccess::ApiKey, builtin::cost(CostUnit::Usd))
        };
        Some(entry)
    }
}

/// The cached entries whose CLI is still on or whose endpoint is still asked. A CLI's model
/// takes its access and cost from the settings now, since you may have changed its auth,
/// except what the CLI's own listing decided: a model on this computer stays local, and a
/// cost the listing gave stays. An endpoint is asked while you have a table for it, and a
/// known server on this computer while `registry.local_servers` is on.
fn still_listed(settings: &Settings, cached: Vec<ModelEntry>) -> Vec<ModelEntry> {
    let asked = |id: &str| {
        settings.endpoints.contains_key(id)
            || (settings.registry.local_servers
                && builtin_endpoint(id).is_some_and(|endpoint| endpoint.local))
    };
    cached
        .into_iter()
        .filter(|entry| match entry.kind {
            RegistryKind::Cli => settings
                .agents
                .get(&entry.id.agent)
                .is_none_or(|agent| agent.enabled),
            RegistryKind::Endpoint => asked(&entry.id.agent),
        })
        .map(|mut entry| {
            if entry.kind == RegistryKind::Cli && entry.access != ModelAccess::Local {
                let (access, cost) = builtin::cli_access(settings, &entry.id.agent);
                entry.access = access;
                if !entry.listed_cost {
                    entry.cost = cost;
                }
            }
            entry
        })
        .collect()
}

/// A Claude Code model read from Anthropic, rather than one of the aliases Claude Code takes.
fn from_anthropic(entry: &ModelEntry) -> bool {
    entry.id.agent == "claude-code"
        && entry.discovered
        && entry
            .id
            .model
            .as_deref()
            .is_some_and(|model| !CLAUDE_CODE_ALIASES.contains(&model))
}

/// Later wins by `id`, keeping the place of the first.
fn merge(into: &mut Vec<ModelEntry>, later: Vec<ModelEntry>) {
    for entry in later {
        match into.iter_mut().find(|e| e.id == entry.id) {
            Some(slot) => *slot = entry,
            None => into.push(entry),
        }
    }
}

/// The entry with this exact id, else the one that lists the model as an alias.
fn position(entries: &[ModelEntry], worker: &ModelRef) -> Option<usize> {
    let exact = entries.iter().position(|e| e.id == *worker);
    let alias = || {
        let model = worker.model.as_deref()?;
        entries
            .iter()
            .position(|e| e.id.agent == worker.agent && e.aliases.iter().any(|a| a == model))
    };
    exact.or_else(alias)
}

impl ModelRegistry for FileRegistry {
    fn get(&self, worker: &ModelRef) -> Option<ModelEntry> {
        let entries = self.entries();
        position(&entries, worker).map(|at| entries[at].clone())
    }

    fn provider(&self, worker: &ModelRef) -> String {
        provider::resolve_provider(
            worker,
            self.get(worker).as_ref(),
            self.manifest_provider(worker),
        )
    }

    fn list(&self) -> Vec<ModelEntry> {
        self.entries()
    }

    fn estimate(&self, worker: &ModelRef, input_tokens: u64, output_tokens: u64) -> CostEstimate {
        let entry = self.estimate_entry(worker);
        estimate::estimate(entry.as_ref(), worker, input_tokens, output_tokens)
    }

    /// What it finds replaces what discovery found before, under your own entries, except that
    /// a source that failed keeps its earlier list, and so does Claude Code's list from
    /// Anthropic until it's a day old. Why each source it skipped found nothing is kept for
    /// [`FileRegistry::discovery_skipped`], and the list goes to the cache file.
    fn discover(&self) -> BoxFuture<'_, Vec<ModelEntry>> {
        Box::pin(async move {
            if !self.settings.registry.discover {
                return Vec::new();
            }
            let now = clock::now_utc();
            let claude = async {
                match self.claude_due(now) {
                    Some(source) => Some(source.list(&self.settings).await),
                    None => None,
                }
            };
            let fetch = async {
                match self.catalog_due(now) {
                    Some((source, known)) => Some(source.fetch(known.as_ref(), now).await),
                    None => None,
                }
            };
            let (mut found, claude, fetched, answers) = tokio::join!(
                discovery::discover_with_reasons(&self.settings, &self.sources),
                claude,
                fetch,
                self.ask_sources(),
            );
            for (cli, answer) in answers {
                match answer.failed {
                    // What it listed before stays, as for a CLI discovery runs itself.
                    Some(why) => {
                        found.skipped.push(format!("{cli}: {why}"));
                        found.failed.push(cli);
                    }
                    None => found.models.extend(answer.models),
                }
            }
            match fetched {
                Some(Ok(fetched)) => {
                    *self.fetched.write().unwrap_or_else(PoisonError::into_inner) = Some(fetched);
                }
                Some(Err(reason)) => found
                    .skipped
                    .push(format!("Argonauts Code's model catalog: {reason}")),
                None => {}
            }
            let mut claude_listed = *self
                .claude_listed
                .read()
                .unwrap_or_else(PoisonError::into_inner);
            let keep_claude = match claude {
                Some(Ok(models)) => {
                    found.models.extend(models);
                    claude_listed = Some(now);
                    false
                }
                Some(Err(reason)) => {
                    found
                        .skipped
                        .push(format!("Claude Code's models: {reason}"));
                    true
                }
                None => self.claude.is_some(),
            };
            let claude_on = agent_enabled(&self.settings, "claude-code");
            let refreshed = clock::now_utc();
            let (models, kept_claude) = {
                let mut discovered = self
                    .discovered
                    .write()
                    .unwrap_or_else(PoisonError::into_inner);
                let kept_claude: Vec<ModelEntry> = discovered
                    .iter()
                    .filter(|_| keep_claude && claude_on)
                    .filter(|entry| from_anthropic(entry))
                    .cloned()
                    .collect();
                let kept = discovered
                    .iter()
                    .filter(|entry| found.failed.contains(&entry.id.agent))
                    .cloned();
                let models: Vec<ModelEntry> = found
                    .models
                    .iter()
                    .cloned()
                    .chain(kept_claude.iter().cloned())
                    .chain(kept)
                    .collect();
                discovered.clone_from(&models);
                (models, kept_claude)
            };
            let mut skipped = found.skipped;
            for cli in self.source_ids() {
                let failed = found.failed.contains(&cli);
                self.mark_listed(&cli, (!failed).then_some(refreshed), failed);
            }
            let sources = self.listed_now();
            if let Some(path) = &self.cache {
                let catalog = self
                    .fetched
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                    .map(|(file, _)| file.clone());
                let cache = ModelCache {
                    refreshed,
                    models,
                    skipped: skipped.clone(),
                    claude_listed,
                    catalog,
                    sources,
                };
                if let Err(e) = cache.write(path) {
                    skipped.push(format!(
                        "couldn't keep the model list in {}: {e}",
                        path.display()
                    ));
                }
            }
            *self.skipped.write().unwrap_or_else(PoisonError::into_inner) = skipped;
            *self
                .refreshed
                .write()
                .unwrap_or_else(PoisonError::into_inner) = Some(refreshed);
            *self
                .claude_listed
                .write()
                .unwrap_or_else(PoisonError::into_inner) = claude_listed;
            found.models.into_iter().chain(kept_claude).collect()
        })
    }

    /// An endpoint's own server, or for an OpenCode model on a server here, that server.
    fn loaded_window(&self, worker: &ModelRef) -> BoxFuture<'_, Option<u64>> {
        let served = match worker.model.as_deref() {
            Some(model) if worker.agent == "opencode" => model
                .split_once('/')
                .and_then(|(provider, model)| {
                    Some((discovery::server_for_opencode(provider)?, model))
                })
                .map(|(endpoint, model)| (endpoint.to_owned(), model.to_owned())),
            Some(model) => endpoint_settings(&self.settings, &worker.agent)
                .map(|_| (worker.agent.clone(), model.to_owned())),
            None => None,
        };
        Box::pin(async move {
            let (endpoint, model) = served?;
            discovery::loaded_window(&self.settings, &endpoint, &model).await
        })
    }

    fn refreshed(&self) -> Option<OffsetDateTime> {
        *self
            .refreshed
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn catalog(&self) -> ModelCatalog {
        let discovered = self
            .discovered
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let fetched = self.fetched.read().unwrap_or_else(PoisonError::into_inner);
        let reported: Vec<ModelCatalog> = self
            .reported
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(cli, models)| reported::layer(cli, models))
            .collect();
        catalog::layered(
            fetched.as_ref().map(|(_, catalog)| catalog),
            &discovered,
            &reported,
            &self.yours,
        )
    }

    fn sources(&self) -> Vec<String> {
        self.source_ids()
    }

    fn discover_source<'a>(&'a self, cli: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(self.discover_one(cli))
    }

    fn source_listed(&self, cli: &str) -> (Option<OffsetDateTime>, bool) {
        let known = self.listed_now().into_iter().find(|known| known.cli == cli);
        match known {
            Some(known) => (known.at.or_else(|| self.refreshed()), known.failed),
            None => (self.refreshed(), false),
        }
    }

    fn source_note(&self, cli: &str) -> Option<String> {
        (cli == "opencode")
            .then(|| discovery::opencode_note(&self.sources))
            .flatten()
    }

    fn models_reported(&self, cli: &str, models: &[ReportedModel]) {
        if models.is_empty() {
            return;
        }
        let mut reported = self
            .reported
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if reported.get(cli).is_some_and(|known| known == models) {
            return;
        }
        reported.insert(cli.to_owned(), models.to_vec());
        // A list that can't be kept is only missing until the CLI's next reply.
        if let Some(home) = &self.home {
            let _ = reported::write(&home.reported_models(cli), models);
        }
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::AuthMode;
    use argonauts_core::settings::agents::{AgentSettings, EndpointSettings};
    use argonauts_core::{TokenPrice, sha256_hex};

    use super::*;

    /// What Claude Code lists itself counts over the catalogs, a model it adds can be picked
    /// at once, and the list comes back after a restart.
    #[test]
    fn the_models_claude_code_lists_are_mirrored_and_kept() {
        use argonauts_core::ReportedModel;

        let listed = vec![
            ReportedModel {
                value: "default".into(),
                resolved: Some("claude-opus-5-5".into()),
                display_name: "Default (recommended)".into(),
                description: "Opus 5.5 · Best for everyday, complex tasks".into(),
                efforts: vec!["low".into(), "xhigh".into()],
            },
            ReportedModel {
                value: "sonnet".into(),
                resolved: Some("claude-sonnet-5-5".into()),
                display_name: "Sonnet 5.5".into(),
                description: "Lighter work, quickly".into(),
                efforts: vec!["low".into(), "high".into()],
            },
            ReportedModel {
                value: "claude-opus-6".into(),
                resolved: Some("claude-opus-6".into()),
                display_name: "Opus 6".into(),
                description: "The newest".into(),
                efforts: vec!["low".into(), "max".into()],
            },
        ];
        let home = tempfile::tempdir().unwrap();
        let mut registry = FileRegistry::from_files(&Settings::default(), None, None);
        registry.read_reported(Paths::new(home.path()));
        registry.models_reported("claude-code", &listed);

        let worker = |w: &str| -> ModelRef { w.parse().unwrap() };
        let newest = registry.get(&worker("claude-code/claude-opus-6")).unwrap();
        assert_eq!(newest.efforts, ["low", "max"]);
        let catalog = registry.catalog();
        let sonnet = catalog.label(&worker("claude-code/claude-sonnet-5-5"));
        assert_eq!(sonnet.name, "Sonnet 5.5");
        assert_eq!(sonnet.description.as_deref(), Some("Lighter work, quickly"));
        assert_eq!(
            catalog
                .entry(&worker("claude-code"))
                .and_then(|e| e.runs.clone())
                .as_deref(),
            Some("claude-opus-5-5")
        );
        // The shipped short name for Haiku survives a list that names it otherwise.
        assert!(
            catalog
                .entry(&worker("claude-code/claude-haiku-4-5"))
                .is_some()
        );

        let mut again = FileRegistry::from_files(&Settings::default(), None, None);
        again.read_reported(Paths::new(home.path()));
        assert!(again.get(&worker("claude-code/claude-opus-6")).is_some());
        assert_eq!(
            again
                .catalog()
                .label(&worker("claude-code/claude-sonnet-5-5"))
                .description
                .as_deref(),
            Some("Lighter work, quickly")
        );
    }

    #[test]
    fn built_ins_answer_before_any_file() {
        let mut settings = Settings::default();
        settings.endpoints.insert(
            "vllm".to_owned(),
            EndpointSettings {
                base_url: Some("http://localhost:8000/v1".to_owned()),
                local: true,
                ..EndpointSettings::default()
            },
        );
        let registry = FileRegistry::from_files(&settings, None, None);
        let worker = |w: &str| -> ModelRef { w.parse().unwrap() };
        assert!(registry.get(&worker("claude-code/opus")).is_some());
        assert!(registry.get(&worker("codex/gpt-6-luna")).is_none());
        assert_eq!(registry.list().len(), 7);
        assert_eq!(
            registry.estimate(&worker("codex/gpt-6-luna"), 10, 10),
            CostEstimate::Unknown
        );
        assert_eq!(
            registry.estimate(&worker("ollama/llama3.2"), 10, 10),
            CostEstimate::Free
        );
        assert_eq!(
            registry.estimate(&worker("vllm/qwen3"), 10, 10),
            CostEstimate::Free
        );
        assert_eq!(
            registry.estimate(&worker("openrouter/unpriced/model"), 10, 10),
            CostEstimate::Unknown
        );
        assert_eq!(
            registry.estimate(&worker("somewhere/else"), 10, 10),
            CostEstimate::Unknown
        );
    }

    #[test]
    fn your_entries_win_over_discovered_ones() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("models.toml");
        std::fs::write(
            &file,
            "[[model]]\nid = \"ollama/llama3.2:latest\"\nprovider = \"meta\"\nkind = \"endpoint\"\n\
             access = \"local\"\ncost = { unit = \"none\" }\ntier = 2\n",
        )
        .unwrap();
        let registry = FileRegistry::from_files(&Settings::default(), Some(&file), None);
        let found = discovery::parse_openai_models(
            "ollama",
            true,
            r#"{"data":[{"id":"llama3.2:latest"},{"id":"gemma3:12b"}]}"#,
        )
        .unwrap();
        merge(&mut registry.discovered.write().unwrap(), found);

        let llama = registry
            .get(&"ollama/llama3.2:latest".parse().unwrap())
            .unwrap();
        assert!(!llama.discovered);
        assert_eq!(llama.tier, Some(2));
        let gemma = registry.get(&"ollama/gemma3:12b".parse().unwrap()).unwrap();
        assert!(gemma.discovered);
        assert_eq!(registry.list().len(), 9);
    }

    #[tokio::test]
    async fn discovery_off_finds_nothing() {
        let mut settings = Settings::default();
        settings.registry.discover = false;
        let registry = FileRegistry::from_files(&settings, None, None);
        assert!(registry.discover().await.is_empty());
        assert_eq!(registry.list().len(), 7);
    }

    #[test]
    fn aliases_find_their_entry() {
        let registry = FileRegistry::from_files(&Settings::default(), None, None);
        let found = discovery::parse_openai_models(
            "ollama",
            true,
            r#"{"data":[{"id":"llama3.2:latest"}]}"#,
        )
        .unwrap();
        merge(&mut registry.discovered.write().unwrap(), found);
        let entry = registry.get(&"ollama/llama3.2".parse().unwrap()).unwrap();
        assert_eq!(entry.id.to_string(), "ollama/llama3.2:latest");
        assert!(registry.get(&"ollama/llama3".parse().unwrap()).is_none());
    }

    const LAUNCH_ENTRY: &str = "[[model]]\nid = \"opencode/ollama/llama3.2\"\nprovider = \"openai\"\n\
        kind = \"cli\"\naccess = \"local\"\ncost = { unit = \"none\" }\ntier = 5\n\
        launch = { provider_model = \"openrouter/openai/gpt-5\" }\n";

    #[test]
    fn an_untrusted_project_file_changes_no_model() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("models.toml");
        std::fs::write(&file, LAUNCH_ENTRY).unwrap();
        let worker: ModelRef = "opencode/ollama/llama3.2".parse().unwrap();

        let untrusted = ProjectModels::read(&file, &[]).unwrap();
        assert!(!untrusted.trusted());
        let registry = FileRegistry::from_files(&Settings::default(), None, Some(&untrusted));
        let launch = registry.get(&worker).and_then(|e| e.launch);
        assert_eq!(launch, None);
        // The id names the provider; the untrusted entry's "openai" is ignored.
        assert_eq!(registry.provider(&worker), "meta");
        assert_eq!(registry.warnings().len(), 1, "{:?}", registry.warnings());
        assert!(registry.warnings()[0].contains("its model entry is ignored until you trust"));

        let hash = sha256_hex(LAUNCH_ENTRY.as_bytes());
        let trusted = ProjectModels::read(&file, std::slice::from_ref(&hash)).unwrap();
        assert_eq!(trusted.sha256(), Some(hash.clone()));
        let registry = FileRegistry::from_files(&Settings::default(), None, Some(&trusted));
        let launch = registry.get(&worker).and_then(|e| e.launch).unwrap();
        assert_eq!(
            launch.provider_model.as_deref(),
            Some("openrouter/openai/gpt-5")
        );
        assert!(registry.warnings().is_empty(), "{:?}", registry.warnings());

        // Trust is for the text you saw: an edit after it waits for you again.
        std::fs::write(&file, format!("{LAUNCH_ENTRY}# changed\n")).unwrap();
        let changed = ProjectModels::read(&file, &[hash]).unwrap();
        assert!(!changed.trusted());
        let registry = FileRegistry::from_files(&Settings::default(), None, Some(&changed));
        assert_eq!(registry.get(&worker).and_then(|e| e.launch), None);

        // Without a trust input, `registry` leaves the project file out.
        let repo = tempfile::tempdir().unwrap();
        let path = project_models_path(repo.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, LAUNCH_ENTRY).unwrap();
        let plain = super::registry(&Settings::default(), Some(repo.path()));
        assert_eq!(plain.get(&worker).and_then(|e| e.launch), None);
        let hash = sha256_hex(LAUNCH_ENTRY.as_bytes());
        let with_trust = registry_with_trust(
            &Settings::default(),
            Some(repo.path()),
            &[hash],
            None,
            None,
            None,
            None,
        );
        assert!(with_trust.get(&worker).and_then(|e| e.launch).is_some());
    }

    #[test]
    fn a_tier_alone_keeps_the_rest_of_the_entry_current() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("models.toml");
        let worker: ModelRef = "claude-code/opus".parse().unwrap();
        let opus = FileRegistry::from_files(&Settings::default(), None, None)
            .get(&worker)
            .unwrap();
        set_model_tier(&file, &opus, Some(4)).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text, "[[model]]\nid = \"claude-code/opus\"\ntier = 4\n");

        let mut settings = Settings::default();
        settings.agents.insert(
            "claude-code".to_owned(),
            AgentSettings {
                auth: AuthMode::ApiKey,
                price: Some(TokenPrice {
                    input_per_mtok: 1.0,
                    output_per_mtok: 4.0,
                    cached_input_per_mtok: None,
                }),
                ..AgentSettings::default()
            },
        );
        let registry = FileRegistry::from_files(&settings, Some(&file), None);
        let entry = registry.get(&worker).unwrap();
        assert_eq!(entry.tier, Some(4));
        assert_eq!(entry.access, ModelAccess::ApiKey);
        assert_eq!(
            registry.estimate(&worker, 1_000_000, 500_000),
            CostEstimate::Usd(3.0)
        );

        // Clearing the tier takes its table out again.
        set_model_tier(&file, &opus, None).unwrap();
        assert!(read_registry_file(&file).unwrap().tiers.is_empty());
        let registry = FileRegistry::from_files(&settings, Some(&file), None);
        assert_eq!(registry.get(&worker).unwrap().tier, None);
    }

    #[tokio::test]
    async fn discovery_keeps_why_a_source_was_skipped() {
        let mut settings = Settings::default();
        settings.registry.local_servers = false;
        for agent in ["claude-code", "codex", "antigravity-cli", "opencode"] {
            settings.agents.insert(
                agent.to_owned(),
                AgentSettings {
                    enabled: false,
                    ..AgentSettings::default()
                },
            );
        }
        settings.endpoints.insert(
            "gone".to_owned(),
            EndpointSettings {
                base_url: Some("http://127.0.0.1:9/v1".to_owned()),
                ..EndpointSettings::default()
            },
        );
        let registry = FileRegistry::from_files(&settings, None, None);
        assert!(registry.discovery_skipped().is_empty());
        assert!(registry.discover().await.is_empty());
        let skipped = registry.discovery_skipped();
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        assert_eq!(
            skipped[0],
            "Claude Code, Codex, agy, OpenCode: off in your settings"
        );
        assert!(skipped[1].starts_with("endpoint gone: "), "{skipped:?}");
    }

    /// A fake `codex` that prints `fixture` for `codex debug models`, and fails otherwise.
    #[cfg(unix)]
    fn fake_codex(dir: &Path, name: &str, fixture: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        let path = dir.join(name);
        let script = format!(
            "#!/bin/sh\n[ \"$*\" = \"debug models\" ] || exit 2\ncat '{fixtures}/{fixture}'\n"
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Codex alone, at `binary`, so no real CLI or server runs.
    fn codex_only(binary: PathBuf) -> Settings {
        let mut settings = Settings::default();
        settings.registry.local_servers = false;
        for agent in ["claude-code", "antigravity-cli", "opencode"] {
            settings.agents.get_mut(agent).unwrap().enabled = false;
        }
        settings.agents.get_mut("codex").unwrap().binary = Some(binary);
        settings
    }

    fn codex_models(registry: &FileRegistry) -> Vec<ModelEntry> {
        registry
            .list()
            .into_iter()
            .filter(|e| e.id.agent == "codex" && e.id.model.is_some())
            .collect()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_cached_list_counts_until_a_refresh_lands() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("home").join("models-cache.json");
        let codex = fake_codex(dir.path(), "codex", "codex/debug-models.json");
        let settings = codex_only(codex);

        let first = FileRegistry::from_files(&settings, None, None).with_cache(&cache);
        assert_eq!(first.refreshed(), None);
        assert!(codex_models(&first).is_empty());
        let found = first.discover().await;
        assert_eq!(found.len(), 7);
        let refreshed = first.refreshed().expect("a refresh time");
        let cached = ModelCache::read(&cache).expect("a cache file");
        assert_eq!(cached.refreshed, refreshed);
        assert_eq!(cached.models.len(), 7);

        // The next start lists them before any refresh, efforts and all.
        let next = FileRegistry::from_files(&settings, None, None).with_cache(&cache);
        assert_eq!(next.refreshed(), Some(refreshed));
        let listed = codex_models(&next);
        assert_eq!(listed.len(), 7);
        assert!(listed.iter().all(|e| e.discovered && !e.efforts.is_empty()));

        // A refresh that fails for Codex keeps what Codex listed before.
        let failing = fake_codex(dir.path(), "codex-broken", "codex/debug-models.json");
        std::fs::write(&failing, "#!/bin/sh\necho 'not logged in' >&2\nexit 3\n").unwrap();
        let broken = FileRegistry::from_files(&codex_only(failing), None, None).with_cache(&cache);
        assert!(broken.discover().await.is_empty());
        assert_eq!(codex_models(&broken).len(), 7);
        // Codex's failure, after the CLIs turned off.
        assert_eq!(broken.discovery_skipped().len(), 2);
        assert_eq!(ModelCache::read(&cache).unwrap().models.len(), 7);

        // A CLI you turned off, or an API key you switched to, shows at once.
        let mut off = settings.clone();
        off.agents.get_mut("codex").unwrap().enabled = false;
        let off = FileRegistry::from_files(&off, None, None).with_cache(&cache);
        assert!(codex_models(&off).is_empty());
        let mut keyed = settings.clone();
        keyed.agents.get_mut("codex").unwrap().auth = argonauts_core::AuthMode::ApiKey;
        let keyed = FileRegistry::from_files(&keyed, None, None).with_cache(&cache);
        assert!(
            codex_models(&keyed)
                .iter()
                .all(|e| e.access == ModelAccess::ApiKey)
        );

        // With discovery off the cache is neither read nor written.
        let mut quiet = settings.clone();
        quiet.registry.discover = false;
        let quiet = FileRegistry::from_files(&quiet, None, None).with_cache(&cache);
        assert!(codex_models(&quiet).is_empty());
        assert!(quiet.discover().await.is_empty());
        assert_eq!(quiet.refreshed(), None);
        assert_eq!(ModelCache::read(&cache).unwrap().models.len(), 7);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_refresh_drops_models_a_cli_no_longer_lists() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("models-cache.json");
        let mut gone = cli_entry_for_test("codex", "gpt-retired");
        gone.discovered = true;
        ModelCache {
            refreshed: OffsetDateTime::UNIX_EPOCH,
            models: vec![gone],
            skipped: Vec::new(),
            claude_listed: None,
            catalog: None,
            sources: Vec::new(),
        }
        .write(&cache)
        .unwrap();
        let codex = fake_codex(dir.path(), "codex", "codex/debug-models.json");
        let registry = FileRegistry::from_files(&codex_only(codex), None, None).with_cache(&cache);
        let retired: ModelRef = "codex/gpt-retired".parse().unwrap();
        assert!(registry.get(&retired).is_some());
        registry.discover().await;
        assert!(registry.get(&retired).is_none());
        assert!(registry.refreshed().unwrap() > OffsetDateTime::UNIX_EPOCH);
    }

    fn cli_entry_for_test(adapter: &str, model: &str) -> ModelEntry {
        builtin::cli_entry(&Settings::default(), adapter, Some(model))
    }
}
