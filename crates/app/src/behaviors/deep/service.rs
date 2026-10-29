//! Summaries as the Behaviors tab and `argonauts behaviors --summaries` use them: what `d` asks,
//! the kept map, and an update with the answers. Only an update calls a model.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::anyhow;
use argonauts_core::Paths;
use argonauts_core::deep_map::{
    DeepDone, DeepOffer, DeepProgress, DeepScope, DeepStart, EmbedderNote, MadeBy, ScopeOffer,
};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::ModelRef;
use argonauts_core::registry::ModelRegistry;
use argonauts_core::session_runner::RunnerDirectory;
use argonauts_core::settings::Settings;
use argonauts_core::settings::behaviors::WORD_VECTORS;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use super::action::DeepAction;
use super::choose::{self, Candidate, HostedModel, Place};
use super::embed::{Embedder, EmbeddingEndpoint, KeyedEndpoint, NOMIC, pull_nomic};
use super::gather::{Gathered, cheapest_effort, gather, project_of};
use super::offer::{covers, embedder_note, keyed_embedder, offer, scope_offer};
use super::prompt::PROMPT_VERSION;
use super::records::recorded;
use super::session::SessionSummarizer;
use super::store::Store;
use super::summarize::{LocalServer, Summarizer};
use super::update::{Before, Deep, EmbedWith, before};
use crate::host::clis::RepoModels;
use crate::host::services::{SettingsSource, directory};

/// Summaries for your settings, over every project's records.
pub struct DeepMaps {
    paths: Paths,
    settings: Settings,
    registry: Arc<dyn ModelRegistry>,
    repo_models: Option<RepoModels>,
    /// The project you're in, by its folder's name.
    project: Option<String>,
    store: Store,
    /// The records as they were first read, which every question and the update count.
    recorded: OnceLock<Vec<DeepAction>>,
    /// The app's own sessions, which a CLI's model summarizes through. Without them, the
    /// settings' CLIs are started as `argonauts behaviors` starts them.
    workers: Option<Arc<dyn RunnerDirectory>>,
}

impl DeepMaps {
    /// With the settings of the project you're in, and `overrides`.
    pub fn here(overrides: Vec<String>) -> anyhow::Result<DeepMaps> {
        let (env, resolved) = crate::commands::resolve_here(None, overrides)?;
        let paths = crate::commands::home()?;
        let repo_models = env
            .as_ref()
            .map(|env| RepoModels::new(&env.repo_root, &env.state_dir));
        let registry = crate::host::clis::registry(
            &resolved.settings,
            repo_models.as_ref(),
            Some(&paths.models_cache()),
        );
        let project = env.as_ref().and_then(|env| project_of(&env.state_dir));
        Ok(DeepMaps::new(
            paths,
            resolved.settings,
            registry,
            repo_models,
            project,
        ))
    }

    pub(crate) fn new(
        paths: Paths,
        settings: Settings,
        registry: Arc<dyn ModelRegistry>,
        repo_models: Option<RepoModels>,
        project: Option<String>,
    ) -> DeepMaps {
        DeepMaps {
            store: Store::new(&paths),
            recorded: OnceLock::new(),
            workers: None,
            paths,
            settings,
            registry,
            repo_models,
            project,
        }
    }

    /// Summarizes through `workers` where a CLI's model summarizes.
    pub(crate) fn with_workers(mut self, workers: Arc<dyn RunnerDirectory>) -> DeepMaps {
        self.workers = Some(workers);
        self
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    fn recorded(&self) -> &[DeepAction] {
        self.recorded.get_or_init(|| recorded(&self.paths))
    }

    /// What `d` asks: what to summarize and who summarizes, from this machine's servers and
    /// CLIs and the recorded actions. No model is called.
    pub async fn offer(&self) -> DeepOffer {
        let gathered = gather(&self.settings, &self.registry).await;
        let candidates = self.candidates(&gathered);
        let mut offer = offer(
            self.recorded(),
            self.project.as_deref(),
            &candidates,
            self.registry.as_ref(),
            &gathered.names,
            embedder_note(
                gathered.ollama.as_ref(),
                gathered.keyed.as_ref(),
                &gathered.names,
            ),
            OffsetDateTime::now_utc(),
        );
        offer.named = choose::named(&self.settings.behaviors.summarizer, &candidates);
        offer.keyed_embedder = keyed_embedder(gathered.keyed.as_ref());
        let everything = offer.scopes.first().map_or(0, |scope| scope.actions);
        offer.made_by = self.store.layout().map(|layout| {
            let model = layout
                .maker
                .summarizer
                .parse()
                .unwrap_or_else(|_| ModelRef::new(layout.maker.summarizer.clone(), None));
            MadeBy {
                new_actions: everything
                    .saturating_sub(self.already(&DeepScope::Everything, &model)),
                name: candidates
                    .iter()
                    .find(|candidate| candidate.model == model)
                    .map_or_else(|| gathered.names.worker(&model), |c| c.name.clone()),
                model,
                actions: layout.points.len(),
                prompt_changed: layout.maker.prompt != PROMPT_VERSION,
            }
        });
        offer
    }

    /// The summarizers offered, the one the settings name first, else auto's order. A model
    /// the settings name that a server here serves is this machine's, even where the list
    /// offers its twin, such as `qwen3-coder:30b` for `qwen3-coder:30b-64k`, which then goes.
    fn candidates(&self, gathered: &Gathered) -> Vec<Candidate> {
        let mut candidates =
            choose::candidates(&gathered.locals, &gathered.hosted, &gathered.names);
        if let Some(named) = choose::named(&self.settings.behaviors.summarizer, &candidates) {
            let at = candidates
                .iter()
                .position(|candidate| candidate.model == named);
            let served = gathered.locals.iter().find(|local| {
                local.runtime == named.agent && named.model.as_deref() == Some(local.model.as_str())
            });
            let chosen = match (at, served) {
                (Some(at), _) => candidates.remove(at),
                (None, Some(local)) => choose::local_candidate(local, &gathered.names),
                (None, None) => named_candidate(&named, &gathered.names),
            };
            candidates.retain(|candidate| {
                !(chosen.local()
                    && candidate.local()
                    && candidate.model.agent == chosen.model.agent
                    && candidate.short == chosen.short)
            });
            candidates.insert(0, chosen);
        }
        candidates
    }

    /// The counts of one scope, for a question about it.
    pub fn scope(&self, scope: DeepScope, summarizer_local: bool) -> ScopeOffer {
        let recorded = self.recorded();
        let now = OffsetDateTime::now_utc();
        let covered: Vec<&DeepAction> =
            recorded.iter().filter(|a| covers(&scope, a, now)).collect();
        scope_offer(scope, &covered, summarizer_local, &DisplayNames::default())
    }

    /// How many of `scope`'s actions `summarizer` summarized already.
    pub fn already(&self, scope: &DeepScope, summarizer: &ModelRef) -> usize {
        let recorded = self.recorded();
        let now = OffsetDateTime::now_utc();
        let covered: Vec<DeepAction> = recorded
            .iter()
            .filter(|action| covers(scope, action, now))
            .cloned()
            .collect();
        super::summaries::needed(&covered, &self.store, &summarizer.to_string()).already
    }

    /// The map's own summarizer and its size, when `summarizer` would summarize it all again.
    pub fn redo(&self, summarizer: &ModelRef) -> Option<(ModelRef, usize)> {
        match before(&self.store, &summarizer.to_string()) {
            Before::Redo { made_by, actions } => Some((
                made_by
                    .parse()
                    .unwrap_or_else(|_| ModelRef::new(made_by.clone(), None)),
                actions,
            )),
            Before::Go => None,
        }
    }

    /// Updates the map as `start` says, telling `progress` after each request and `placed`
    /// each time more dots were placed while it goes. Stopping keeps every summary written.
    pub async fn update(
        &self,
        start: &DeepStart,
        stop: &CancellationToken,
        progress: &mut (dyn FnMut(DeepProgress) + Send),
        placed: &mut (dyn FnMut() + Send),
    ) -> anyhow::Result<DeepDone> {
        let gathered = gather(&self.settings, &self.registry).await;
        let candidates = self.candidates(&gathered);
        let chosen = candidates
            .iter()
            .find(|candidate| candidate.model == start.summarizer)
            .cloned()
            .unwrap_or_else(|| named_candidate(&start.summarizer, &gathered.names));
        if let Before::Redo { made_by, actions } = before(&self.store, &chosen.model.to_string())
            && !start.redo
        {
            anyhow::bail!(
                "Summaries of {actions} actions were written by {made_by}; summarizing \
                 them again with {} asks first",
                chosen.name
            );
        }
        let summarizer = self.summarizer_for(&chosen)?;
        let embedding = self.embedder_for(&gathered, start.download_embedder).await;
        let recorded = self.recorded();
        let now = OffsetDateTime::now_utc();
        let deep = Deep {
            store: &self.store,
            summarizer: summarizer.as_ref(),
            embed_with: match &embedding {
                Embedding::Model(embedder) => EmbedWith::Model(embedder.as_ref()),
                Embedding::Words => EmbedWith::Words,
                Embedding::Missing(why) => EmbedWith::Missing(why.clone()),
            },
        };
        let name = chosen.name.clone();
        let updated = deep
            .update(
                recorded,
                |a| covers(&start.scope, a, now),
                stop,
                &mut |at| {
                    progress(DeepProgress {
                        done: at.done,
                        total: at.total,
                        seconds_left: at.left().map(|left| left.as_secs()),
                        summarizer: name.clone(),
                    });
                },
                placed,
            )
            .await?;
        let done = &updated.summarized;
        Ok(DeepDone {
            written: done.written,
            reused: done.reused,
            already: done.already,
            placed: updated.placed,
            new_places: updated.new_places,
            stopped: done.stopped,
            failed: done.failed.clone(),
            by_words: self
                .store
                .layout()
                .is_some_and(|layout| layout.maker.embedder == WORD_VECTORS),
            embedder_failed: updated.embedder_failed,
        })
    }

    /// After a run, with `behaviors.update = "after each run"`, the update that adds that run's
    /// actions to a map that's there, with the summarizer that made it, when that one runs on
    /// this machine. A hosted one asked you when the map was made, and isn't sent more without
    /// asking again. A map that waits to be made again, by a newer prompt or the summarizer the
    /// settings now name, isn't added to: only `d` asks about that. No model is called.
    pub(crate) async fn after_run(&self) -> anyhow::Result<AfterRun> {
        use argonauts_core::settings::behaviors::BehaviorUpdate;
        if self.settings.behaviors.update != BehaviorUpdate::AfterEachRun {
            return Ok(AfterRun::Nothing);
        }
        let Some(layout) = self.store.layout() else {
            return Ok(AfterRun::Nothing);
        };
        let summarizer: ModelRef = layout.maker.summarizer.parse().map_err(|_| {
            anyhow!(
                "the map's summarizer {} isn't a model",
                layout.maker.summarizer
            )
        })?;
        let gathered = gather(&self.settings, &self.registry).await;
        let named = choose::named(&self.settings.behaviors.summarizer, &[]);
        let another = named.is_some_and(|named| named != summarizer);
        if layout.maker.prompt != PROMPT_VERSION || another {
            return Ok(AfterRun::RedoWaits);
        }
        let local = choose::candidates(&gathered.locals, &[], &gathered.names)
            .into_iter()
            .any(|candidate| candidate.model == summarizer);
        if !local {
            return Ok(AfterRun::Nothing);
        }
        Ok(AfterRun::Update(DeepStart {
            scope: DeepScope::Everything,
            summarizer,
            download_embedder: false,
            redo: false,
        }))
    }

    fn summarizer_for(&self, chosen: &Candidate) -> anyhow::Result<Box<dyn Summarizer>> {
        if let Place::ThisMachine(local) = &chosen.place {
            return LocalServer::new(&local.runtime, &local.base_url, &local.model)
                .map(|server| Box::new(server) as Box<dyn Summarizer>)
                .ok_or_else(|| anyhow!("no client for this machine's servers"));
        }
        let workers = self.workers.clone().unwrap_or_else(|| {
            let (plan_usage, health) = argonauts_conductor::boards_with(
                &self.paths,
                &self.settings.health,
                self.registry.clone(),
            );
            directory(
                SettingsSource::fixed(Arc::new(self.settings.clone()), Vec::new()),
                &self.paths,
                self.repo_models.as_ref(),
                self.registry.clone(),
                plan_usage,
                health,
            )
        });
        let runner = workers
            .get(&chosen.model)
            .map_err(|err| anyhow!("{} can't summarize: {err}", chosen.name))?;
        let effort = crate::adapters::builtin_adapters()
            .iter()
            .find(|adapter| adapter.id() == chosen.model.agent)
            .and_then(|adapter| cheapest_effort(self.registry.as_ref(), adapter.as_ref()));
        // Each summarizer's folders are its own, so two updates in one process, as tests run
        // them, never take each other's away.
        static SUMMARIZERS: AtomicU32 = AtomicU32::new(0);
        let n = SUMMARIZERS.fetch_add(1, Ordering::Relaxed);
        let scratch: PathBuf =
            std::env::temp_dir().join(format!("argonauts-summaries-{}-{n}", std::process::id()));
        Ok(Box::new(SessionSummarizer::new(runner, effort, scratch)))
    }

    /// The embedder the settings give: word vectors, a model they name, or auto's
    /// nomic-embed-text on Ollama, downloaded first when you said yes, else an embedding
    /// endpoint with a key.
    async fn embedder_for(&self, gathered: &Gathered, download: bool) -> Embedding {
        let setting = self.settings.behaviors.embedder.trim();
        if setting == WORD_VECTORS {
            return Embedding::Words;
        }
        let model = |endpoint: EmbeddingEndpoint| Embedding::Model(Box::new(endpoint));
        if !self.settings.behaviors.embedder_is_auto() {
            // nomic-embed-text named on Ollama is downloaded first when you said yes, as
            // auto's is.
            let named_nomic = setting.parse::<ModelRef>().is_ok_and(|named| {
                named.agent == "ollama" && named.model.as_deref() == Some(NOMIC)
            });
            if download
                && named_nomic
                && let Some((base_url, false)) = &gathered.ollama
            {
                let _ = pull_nomic(base_url).await;
            }
            return match self.named_embedder(setting, gathered) {
                Some(embedder) => Embedding::Model(embedder),
                None => Embedding::Missing(format!("{setting} isn't set up here")),
            };
        }
        let Some((base_url, has_nomic)) = gathered.ollama.clone() else {
            return match gathered.keyed.as_ref().and_then(KeyedEndpoint::embedder) {
                Some(endpoint) => model(endpoint),
                None => Embedding::Missing("no embedding model answers on this machine".to_owned()),
            };
        };
        if !has_nomic && (!download || pull_nomic(&base_url).await.is_err()) {
            return Embedding::Missing(format!("{NOMIC} isn't on Ollama"));
        }
        match EmbeddingEndpoint::new("ollama", &base_url, NOMIC, None, true) {
            Some(endpoint) => model(endpoint),
            None => Embedding::Missing(format!("no client for {NOMIC} on Ollama")),
        }
    }

    /// `ollama/<model>`, or `<endpoint>/<model>` for an `[endpoints.<endpoint>]` table.
    fn named_embedder(&self, setting: &str, gathered: &Gathered) -> Option<Box<dyn Embedder>> {
        let wanted: ModelRef = setting.parse().ok()?;
        let model = wanted.model.clone()?;
        if wanted.agent == "ollama"
            && let Some((base_url, _)) = &gathered.ollama
        {
            return EmbeddingEndpoint::new("ollama", base_url, &model, None, true)
                .map(|endpoint| Box::new(endpoint) as Box<dyn Embedder>);
        }
        let table = self.settings.endpoints.get(&wanted.agent)?;
        let token = table
            .api_key_env
            .as_deref()
            .and_then(|variable| std::env::var(variable).ok());
        EmbeddingEndpoint::new(
            &wanted.agent,
            table.base_url.as_deref()?,
            &model,
            token,
            table.local,
        )
        .map(|endpoint| Box::new(endpoint) as Box<dyn Embedder>)
    }

    /// The embedder note the questions show.
    pub async fn embedder(&self) -> EmbedderNote {
        let gathered = gather(&self.settings, &self.registry).await;
        embedder_note(
            gathered.ollama.as_ref(),
            gathered.keyed.as_ref(),
            &gathered.names,
        )
    }
}

/// What happened to Summaries after a run.
#[derive(Debug)]
pub(crate) enum AfterRun {
    /// Nothing: the setting is off, there's no map, or its summarizer isn't on this machine.
    Nothing,
    /// The map waits to be made again, by a newer prompt or the summarizer the settings name.
    RedoWaits,
    /// This update adds the new actions.
    Update(DeepStart),
}

/// How an update would turn its summaries into vectors, before the map has its say.
enum Embedding {
    Model(Box<dyn Embedder>),
    /// The settings ask for word vectors.
    Words,
    /// The embedding model the settings would use isn't there now, for this reason.
    Missing(String),
}

/// A summarizer the settings name that nothing offered: a CLI's model, run with no tools.
fn named_candidate(model: &ModelRef, names: &DisplayNames) -> Candidate {
    Candidate {
        model: model.clone(),
        name: names.worker(model),
        short: names
            .model(&model.agent, model.model.as_deref())
            .unwrap_or_else(|| names.cli(&model.agent)),
        place: Place::Hosted(HostedModel {
            worker: model.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use argonauts_core::settings::agents::EndpointSettings;
    use argonauts_core::settings::behaviors::BehaviorUpdate;
    use argonauts_testkit::services::StaticRegistry;
    use argonauts_testkit::{FakeKind, FakeModelServer, FakeOpenAi};
    use serde_json::json;

    use super::super::summarize::fakes::Echo;
    use super::super::update::EmbedWith;
    use super::super::update::update;
    use super::*;

    /// A home with one run of six actions, and a server on this machine that answers nothing,
    /// so any call to it fails and is recorded.
    fn home_and_server() -> (tempfile::TempDir, Paths, FakeOpenAi) {
        let home = tempfile::tempdir().unwrap();
        let run = home.path().join("state/council-3f9a1c2b4d5e/runs/run-01");
        fs::create_dir_all(&run).unwrap();
        let mut lines = String::new();
        for at in 0..6 {
            let line = json!({"seq": at, "ts": "2026-10-17T07:17:01Z", "run_id": "run-01",
                "source": "claude-code", "session": "s-001",
                "kind": {"type": "agent", "data": {"type": "tool_started", "id": format!("t{at}"),
                    "name": "Bash", "input": {"command": format!("cargo test {at}")}, "parent": null}}});
            lines.push_str(&format!("{line}\n"));
        }
        fs::write(run.join("events.jsonl"), lines).unwrap();
        let paths = Paths::new(home.path());
        (home, paths, FakeOpenAi::start().unwrap())
    }

    #[tokio::test]
    async fn an_update_another_summarizer_would_redo_needs_your_yes() {
        let (_home, paths, _server) = home_and_server();
        let store = Store::new(&paths);
        let recorded = recorded(&paths);
        update(
            &recorded,
            |_| true,
            &store,
            &Echo::new("ollama/qwen3-coder:30b"),
            EmbedWith::Words,
            &CancellationToken::new(),
            &mut |_| {},
            &mut || {},
        )
        .await
        .unwrap();
        let mut settings = Settings::default();
        settings.registry.local_servers = false;
        let maps = DeepMaps::new(
            paths,
            settings,
            Arc::new(StaticRegistry::default()),
            None,
            None,
        );
        let (made_by, actions) = maps.redo(&"claude-code/haiku".parse().unwrap()).unwrap();
        assert_eq!(
            (made_by.to_string().as_str(), actions),
            ("ollama/qwen3-coder:30b", 6)
        );
        assert!(
            maps.redo(&"ollama/qwen3-coder:30b".parse().unwrap())
                .is_none()
        );
        assert_eq!(
            maps.already(
                &DeepScope::Everything,
                &"ollama/qwen3-coder:30b".parse().unwrap()
            ),
            6
        );
        let start = DeepStart {
            scope: DeepScope::Everything,
            summarizer: "claude-code/haiku".parse().unwrap(),
            download_embedder: false,
            redo: false,
        };
        let refused = maps
            .update(&start, &CancellationToken::new(), &mut |_| {}, &mut || {})
            .await;
        let why = refused.err().map(|err| err.to_string()).unwrap_or_default();
        assert!(why.contains("asks first"), "{why}");
    }

    /// A map `made_by` made of the six actions, then a seventh recorded, with the setting
    /// that updates after each run and a vLLM on this machine serving `qwen-test`.
    async fn after_a_run(made_by: &str) -> (DeepMaps, FakeModelServer, tempfile::TempDir) {
        after_a_run_with(made_by, |_| {}).await
    }

    async fn after_a_run_with(
        made_by: &str,
        change: impl FnOnce(&mut Settings),
    ) -> (DeepMaps, FakeModelServer, tempfile::TempDir) {
        let (home, paths, _server) = home_and_server();
        update(
            &recorded(&paths),
            |_| true,
            &Store::new(&paths),
            &Echo::new(made_by),
            EmbedWith::Words,
            &CancellationToken::new(),
            &mut |_| {},
            &mut || {},
        )
        .await
        .unwrap();
        let log = home
            .path()
            .join("state/council-3f9a1c2b4d5e/runs/run-02/events.jsonl");
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        let line = json!({"seq": 0, "ts": "2026-10-18T07:17:01Z", "run_id": "run-02",
            "source": "claude-code", "session": "s-001",
            "kind": {"type": "agent", "data": {"type": "tool_started", "id": "t0",
                "name": "Bash", "input": {"command": "git log"}, "parent": null}}});
        fs::write(&log, format!("{line}\n")).unwrap();
        let server = FakeModelServer::start(
            FakeKind::Vllm {
                max_model_len: 32_768,
            },
            &["qwen-test"],
        )
        .unwrap();
        let mut settings = Settings::default();
        settings.registry.local_servers = false;
        settings.behaviors.update = BehaviorUpdate::AfterEachRun;
        settings.endpoints.insert(
            "vllm".to_owned(),
            EndpointSettings {
                base_url: Some(server.base_url()),
                ..EndpointSettings::default()
            },
        );
        change(&mut settings);
        let maps = DeepMaps::new(
            paths,
            settings,
            Arc::new(StaticRegistry::default()),
            None,
            None,
        );
        (maps, server, home)
    }

    fn asked_to_summarize(server: &FakeModelServer) -> usize {
        server
            .requests()
            .iter()
            .filter(|request| request.method == "POST")
            .count()
    }

    /// Ollama serves `qwen3-coder:30b` and `qwen3-coder:30b-64k`, which the list offers once;
    /// the settings name the one with the longer window.
    #[tokio::test]
    async fn a_model_the_settings_name_on_this_machine_is_offered_as_this_machines() {
        let (_home, paths, _server) = home_and_server();
        let ollama = FakeModelServer::start(
            FakeKind::Ollama,
            &["qwen3-coder:30b", "qwen3-coder:30b-64k"],
        )
        .unwrap();
        let mut settings = Settings::default();
        settings.registry.local_servers = false;
        settings.behaviors.summarizer = "ollama/qwen3-coder:30b-64k".to_owned();
        settings.endpoints.insert(
            "ollama".to_owned(),
            EndpointSettings {
                base_url: Some(ollama.base_url()),
                ..EndpointSettings::default()
            },
        );
        let maps = DeepMaps::new(
            paths,
            settings,
            Arc::new(StaticRegistry::default()),
            None,
            None,
        );
        let offer = maps.offer().await;
        let first = &offer.summarizers[0];
        assert_eq!(first.model.to_string(), "ollama/qwen3-coder:30b-64k");
        assert!(first.local(), "{first:?}");
        let qwens = offer
            .summarizers
            .iter()
            .filter(|offer| offer.short == first.short)
            .count();
        assert_eq!(qwens, 1, "its twin goes: {:?}", offer.summarizers);
    }

    #[tokio::test]
    async fn after_a_run_a_map_made_on_this_machine_takes_the_new_actions() {
        let (maps, server, _home) = after_a_run("vllm/qwen-test").await;
        let after = maps.after_run().await.unwrap();
        let AfterRun::Update(start) = after else {
            panic!("{after:?}")
        };
        assert_eq!(asked_to_summarize(&server), 0, "deciding calls no model");
        let mut seen = Vec::new();
        let done = maps
            .update(
                &start,
                &CancellationToken::new(),
                &mut |progress| seen.push(progress),
                &mut || {},
            )
            .await
            .unwrap();
        // The fake server can't summarize, so the one new action's request fails; it was sent.
        assert_eq!(asked_to_summarize(&server), 1);
        assert!(done.failed.is_some());
        assert_eq!(done.already, 6);
    }

    #[tokio::test]
    async fn after_a_run_a_map_a_cli_made_waits_for_you() {
        let (maps, server, _home) = after_a_run("claude-code/haiku").await;
        let done = maps.after_run().await.unwrap();
        assert!(matches!(done, AfterRun::Nothing), "{done:?}");
        assert_eq!(asked_to_summarize(&server), 0);
    }

    #[tokio::test]
    async fn after_a_run_a_map_that_waits_to_be_made_again_is_left_alone() {
        let (maps, server, home) = after_a_run_with("vllm/qwen-test", |settings| {
            settings.behaviors.summarizer = "claude-code/haiku".to_owned();
        })
        .await;
        let done = maps.after_run().await.unwrap();
        assert!(matches!(done, AfterRun::RedoWaits), "{done:?}");
        let (maps, _, _) = after_a_run("vllm/qwen-test").await;
        let store = Store::new(&Paths::new(home.path()));
        let mut layout = store.layout().unwrap();
        layout.maker.prompt = "0".to_owned();
        store.save_layout(&layout).unwrap();
        let older = DeepMaps::new(
            Paths::new(home.path()),
            maps.settings().clone(),
            Arc::new(StaticRegistry::default()),
            None,
            None,
        );
        let done = older.after_run().await.unwrap();
        assert!(matches!(done, AfterRun::RedoWaits), "{done:?}");
        assert_eq!(asked_to_summarize(&server), 0);
    }
}
