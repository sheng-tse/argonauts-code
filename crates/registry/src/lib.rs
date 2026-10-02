//! Settings resolution, presets, workflow files, harness profiles, the model registry, and
//! the plan-terms note.

mod catalog;
pub mod harnesses;
pub mod models;
mod presets;
mod resolve;
mod settings;
mod terms;
pub mod workflows;
mod write;

use std::path::PathBuf;

use argonauts_core::app::ConfigEntry;
use argonauts_core::events::run::OverrideRecord;
use argonauts_core::settings::Settings;

pub use catalog::{setting_meta, settings_catalog};
pub use harnesses::{HarnessEntries, builtin_harnesses, harness_manifests};
pub use models::{family_from_model_id, family_problems, registry};
pub use presets::{builtin_preset, presets, presets_in, upgrade_presets, write_preset};
pub use resolve::resolve;
pub use terms::TERMS_NOTE;
pub use workflows::{WorkflowError, WorkflowInfo, find_workflow, workflows_in};
pub use write::{remove_setting, safety_keys_in, write_setting};

/// Later layers win key by key: built-in, `global`, `project`, `project_local`, the preset with
/// the run's recipe over it, then `overrides`. Preset files are looked up in
/// `.argonauts/presets/`, then `~/.argonauts/presets/`, then the built-in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SettingsSources {
    /// `~/.argonauts/config.toml`. Any of the three files may be missing.
    pub global: Option<PathBuf>,
    /// `<repo>/.argonauts/config.toml`, shared with the team.
    pub project: Option<PathBuf>,
    /// `<repo>/.argonauts/config.local.toml`, just for you.
    pub project_local: Option<PathBuf>,
    /// `--set key=value` flags, in order.
    pub overrides: Vec<String>,
    /// Whether a person at a terminal gave the flags. Safety keys need it.
    pub interactive: bool,
    /// Until the shared project file is trusted (its hash is in `trusted.json`), its safety
    /// keys are ignored with a warning, so a cloned repo can't loosen them.
    pub project_trusted: bool,
    /// The same for `config.local.toml`, which a repo can also carry committed.
    pub project_local_trusted: bool,
    /// The recipe triage matched for this run, applied in the preset layer.
    pub recipe: Option<RecipeOverlay>,
}

/// A recipe version's protocol settings, as learning's `recipes::settings_overlay` gives them.
/// A recipe that sets a safety key or a fixed-core key is left out whole, with a warning, never
/// half-applied.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecipeOverlay {
    /// Such as `recipe api-feature v4`, which `/config` shows as the keys' source.
    pub label: String,
    /// The settings, as TOML text.
    pub settings: String,
    /// One of the version's canary runs: with `evolution.canary_pull_requests`, its merge opens
    /// a pull request (`merge.mode = "pull_request"`).
    pub canary: bool,
    /// A replay of this version in an isolated repo, which never merges (`merge.mode =
    /// "leave"`) and spends at most this much (`budget.run_usd`).
    pub replay_budget_usd: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSettings {
    pub settings: Settings,
    /// SHA-256 of the canonical JSON.
    pub sha256: String,
    /// Every leaf key, its value, and the layer that set it.
    pub entries: Vec<ConfigEntry>,
    /// Keys a layer set to something other than the built-in default.
    pub overrides: Vec<OverrideRecord>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("invalid config in {layer}: {message}")]
    Invalid { layer: String, message: String },
    #[error("{path}: {message}")]
    Io { path: PathBuf, message: String },
    /// Such as a safety key from a non-interactive `--set`.
    #[error("refused: {0}")]
    Refused(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetUpgrade {
    pub name: String,
    pub path: PathBuf,
    pub outcome: PresetUpgradeOutcome,
}

/// Judged by the built-in hash the file records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetUpgradeOutcome {
    Current,
    /// You hadn't edited it, so it became the new built-in.
    Updated,
    /// You edited it, so it was kept. `tell` is true only the first time, so the app says so
    /// once and points at `/preset diff <name>`.
    KeptEdited {
        tell: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetInfo {
    pub name: String,
    /// `built-in`, or the file it came from.
    pub source: String,
    pub description: String,
    /// The dotted keys it sets.
    pub keys: Vec<String>,
}
