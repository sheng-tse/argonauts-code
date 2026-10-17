//! The settings in effect for this app session: the three files, their trust, your `--set`
//! flags, and the session overrides the settings commands add.

use std::sync::Arc;

use argonauts_core::app::ConfigEntry;
use argonauts_core::settings::layer;
use argonauts_core::settings::{FILE_LAYERS, Settings};
use argonauts_registry::clis::agents_turned_on;
use argonauts_registry::{ResolvedSettings, SettingsError, SettingsSources, WarningAbout, resolve};
use tokio::sync::watch;

use crate::env::AppEnv;
use crate::trust::TrustStore;

pub(crate) struct AppSettings {
    flags: Vec<String>,
    /// Key and TOML value, oldest first. A later set of the same key replaces the earlier one.
    session: Vec<(String, String)>,
    interactive: bool,
    trust: TrustStore,
    resolved: ResolvedSettings,
    /// Each re-resolve publishes here, so the services that follow it see every change.
    live: watch::Sender<Arc<Settings>>,
    /// The agents a layer turned on, published before `live` so a reader woken by a settings
    /// change finds the list that goes with it.
    turned_on: watch::Sender<Vec<String>>,
}

impl AppSettings {
    /// `interactive` says a person at a terminal gave the flags, so safety keys among them
    /// apply.
    pub(crate) fn load(
        env: &AppEnv,
        flags: Vec<String>,
        interactive: bool,
    ) -> anyhow::Result<Self> {
        let trust = TrustStore::load(&env.state_dir);
        let sources = sources(env, &trust, flags.clone(), interactive);
        let resolved = resolve(&sources)?;
        let live = watch::Sender::new(Arc::new(resolved.settings.clone()));
        let turned_on = watch::Sender::new(agents_turned_on(&resolved));
        Ok(Self {
            flags,
            session: Vec::new(),
            interactive,
            trust,
            resolved,
            live,
            turned_on,
        })
    }

    pub(crate) fn settings(&self) -> &Settings {
        &self.resolved.settings
    }

    pub(crate) fn shared(&self) -> Arc<Settings> {
        Arc::clone(&self.live.borrow())
    }

    /// The settings in effect now and after each change, for services that outlive a change.
    pub(crate) fn follow(&self) -> watch::Receiver<Arc<Settings>> {
        self.live.subscribe()
    }

    /// The agents a settings layer turned on, such as Hermes, which is off by default, now and
    /// after each change.
    pub(crate) fn follow_turned_on(&self) -> watch::Receiver<Vec<String>> {
        self.turned_on.subscribe()
    }

    pub(crate) fn resolved(&self) -> &ResolvedSettings {
        &self.resolved
    }

    pub(crate) fn sha256(&self) -> &str {
        &self.resolved.sha256
    }

    pub(crate) fn entry(&self, key: &str) -> Option<&ConfigEntry> {
        self.resolved.entries.iter().find(|entry| entry.key == key)
    }

    pub(crate) fn trust(&self) -> &TrustStore {
        &self.trust
    }

    pub(crate) fn trust_mut(&mut self) -> &mut TrustStore {
        &mut self.trust
    }

    pub(crate) fn session_overrides(&self) -> &[(String, String)] {
        &self.session
    }

    /// The flags, then the session overrides, as `StartRun.overrides` for a run started now.
    pub(crate) fn run_overrides(&self) -> Vec<String> {
        let mut overrides = self.flags.clone();
        overrides.extend(
            self.session
                .iter()
                .map(|(key, value)| format!("{key}={value}")),
        );
        overrides
    }

    /// Whether the settings would still resolve with `file` trusted, so its safety keys apply,
    /// and if not, why. Nothing is trusted.
    pub(crate) fn check_trust(
        &self,
        env: &AppEnv,
        file: &std::path::Path,
    ) -> Result<(), SettingsError> {
        let mut sources = sources(env, &self.trust, self.run_overrides(), self.interactive);
        // Trusting a file asks whether it reads as written, so a key in it that can't be used
        // is the answer rather than a warning.
        sources.strict = true;
        if *file == env.project_config() {
            sources.project_trusted = true;
        } else if *file == env.project_local_config() {
            sources.project_local_trusted = true;
        }
        resolve(&sources).map(drop)
    }

    /// Re-reads the files and `trusted.json`, keeping the flags and session overrides.
    pub(crate) fn reload(&mut self, env: &AppEnv) -> anyhow::Result<()> {
        self.trust = TrustStore::load(&env.state_dir);
        let sources = sources(env, &self.trust, self.run_overrides(), self.interactive);
        self.resolved = resolve(&sources)?;
        self.turned_on
            .send_replace(agents_turned_on(&self.resolved));
        self.live
            .send_replace(Arc::new(self.resolved.settings.clone()));
        Ok(())
    }

    /// Holds `key = value` for this app session only, then re-resolves. On failure nothing
    /// changes.
    pub(crate) fn set_session(
        &mut self,
        env: &AppEnv,
        key: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        self.set_session_all(env, &[(key.to_owned(), value.to_owned())])
    }

    /// Holds every pair at once and re-resolves once, so keys that only make sense together,
    /// such as a workflow and its orchestrator, never apply alone. On failure nothing changes.
    pub(crate) fn set_session_all(
        &mut self,
        env: &AppEnv,
        pairs: &[(String, String)],
    ) -> anyhow::Result<()> {
        let before = self.session.clone();
        for (key, value) in pairs {
            self.session.retain(|(existing, _)| existing != key);
            self.session.push((key.clone(), value.clone()));
        }
        // A failed reload keeps the settings it had, so only the list needs putting back.
        self.reload(env)
            .inspect_err(|_| self.session = before.clone())?;
        let keys: Vec<String> = pairs.iter().map(|(key, _)| key.clone()).collect();
        if let Some(unknown) = self.unknown_among(&keys) {
            self.session = before;
            self.reload(env)?;
            anyhow::bail!("{unknown} isn't a setting; /config lists them");
        }
        Ok(())
    }

    /// The first key at or under one of `keys` that the last resolve ignored as unknown.
    pub(crate) fn unknown_among(&self, keys: &[String]) -> Option<String> {
        self.resolved.warnings.iter().find_map(|warning| {
            let WarningAbout::Unknown { key } = &warning.about else {
                return None;
            };
            keys.iter()
                .any(|written| {
                    key == written
                        || key
                            .strip_prefix(written.as_str())
                            .is_some_and(|rest| rest.starts_with('.'))
                })
                .then(|| key.to_owned())
        })
    }

    /// Whether the session held the key.
    pub(crate) fn remove_session(&mut self, env: &AppEnv, key: &str) -> anyhow::Result<bool> {
        let before = self.session.clone();
        self.session.retain(|(existing, _)| existing != key);
        if self.session.len() == before.len() {
            return Ok(false);
        }
        self.reload(env).inspect_err(|_| self.session = before)?;
        Ok(true)
    }

    /// Whether a session override sets this leaf key, itself or through a table value set on
    /// a key above it.
    pub(crate) fn session_sets(&self, key: &str) -> bool {
        self.session
            .iter()
            .any(|(set, value)| leaf_keys(set, value).iter().any(|leaf| leaf == key))
    }

    /// The layer the value in effect came from, `session` for a session override, and
    /// `built-in` for a key nothing sets.
    pub(crate) fn layer(&self, key: &str) -> String {
        // Session overrides reach `resolve` after the flags, so it reports them as `flag`.
        match self.entry(key) {
            Some(entry) if entry.layer == layer::FLAG && self.session_sets(key) => {
                layer::SESSION.to_owned()
            }
            Some(entry) => entry.layer.clone(),
            None => layer::BUILT_IN.to_owned(),
        }
    }

    /// For a value from `from` when that's the workflow, the settings file that picked the
    /// workflow, if one did: saving there or later wins over the workflow.
    pub(crate) fn workflow_from(&self, from: &str) -> Option<String> {
        if from != layer::WORKFLOW {
            return None;
        }
        let picked = self.layer("workflow");
        FILE_LAYERS.contains(&picked.as_str()).then_some(picked)
    }

    /// Every resolved entry, with `session` as the layer of your session overrides.
    pub(crate) fn entries(&self) -> Vec<ConfigEntry> {
        self.resolved
            .entries
            .iter()
            .map(|entry| ConfigEntry {
                layer: self.layer(&entry.key),
                ..entry.clone()
            })
            .collect()
    }

    /// The layer that would set `key` without this session's overrides, such as `project`
    /// under a session override a toggle added.
    pub(crate) fn layer_without_session(&self, env: &AppEnv, key: &str) -> anyhow::Result<String> {
        if !self.session_sets(key) {
            return Ok(self.layer(key));
        }
        let sources = sources(env, &self.trust, self.flags.clone(), self.interactive);
        let resolved = resolve(&sources)?;
        Ok(resolved
            .entries
            .iter()
            .find(|entry| entry.key == key)
            .map_or_else(|| "built-in".to_owned(), |entry| entry.layer.clone()))
    }
}

/// The dotted leaf keys that `key = value` sets: the key itself, or each leaf of a table value.
pub(crate) fn leaf_keys(key: &str, value: &str) -> Vec<String> {
    match format!("value = {value}").parse::<toml::Table>() {
        Ok(table) => match table.get("value") {
            Some(toml::Value::Table(inner)) => {
                let mut leaves = Vec::new();
                walk_leaves(key, inner, &mut leaves);
                leaves
            }
            _ => vec![key.to_owned()],
        },
        Err(_) => vec![key.to_owned()],
    }
}

fn walk_leaves(prefix: &str, table: &toml::Table, leaves: &mut Vec<String>) {
    for (name, value) in table {
        let key = format!("{prefix}.{name}");
        match value {
            toml::Value::Table(inner) => walk_leaves(&key, inner, leaves),
            _ => leaves.push(key),
        }
    }
}

/// The three files with their trust, plus `overrides`. The run host's settings reloader and
/// `start` resolve from the same sources.
pub(crate) fn sources(
    env: &AppEnv,
    trust: &TrustStore,
    overrides: Vec<String>,
    interactive: bool,
) -> SettingsSources {
    let project = env.project_config();
    let project_local = env.project_local_config();
    SettingsSources {
        global: Some(env.global_config()),
        project_trusted: trust.is_trusted(&project),
        project_local_trusted: trust.is_trusted(&project_local),
        project: Some(project),
        project_local: Some(project_local),
        overrides,
        interactive,
        learned_workflow: None,
        request: Vec::new(),
        strict: false,
        waiting: argonauts_crew::prompts::trust::waiting(&env.repo_root, &env.state_dir)
            .all()
            .collect(),
        home: env.user_home.clone(),
    }
}
