//! Settings resolution, workflows and their files, CLI profiles, the model registry, and
//! the plan-terms note.

mod always;
mod catalog;
pub mod clis;
mod form;
pub mod models;
mod resolve;
mod settings;
mod terms;
pub mod workflows;
mod write;

use std::path::PathBuf;

use argonauts_core::app::ConfigEntry;
use argonauts_core::events::run::OverrideRecord;
use argonauts_core::settings::Settings;

pub use always::save_required_steps;
pub use catalog::{setting_meta, settings_catalog};
pub use clis::{CliEntries, builtin_clis, cli_manifests};
pub use form::settings_form;
pub use models::{provider_from_model_id, provider_problems, registry};
pub use resolve::resolve;
pub use settings::crew::crew_list_problems;
pub use settings::plain::check_commands;
pub use settings::renamed::{
    Rename, Renamed, mod_renames_in, rename_in_file, rename_in_mod_file, renames_in,
};
pub use settings::tighten::tightens;
pub use terms::TERMS_NOTE;
pub use workflows::{WorkflowError, WorkflowInfo, find_workflow, workflows_in};
pub use workflows::{
    builtin_workflow, upgrade_workflows, workflow_choices, workflow_choices_in, write_workflow,
};
pub use write::{check_settings, remove_setting, safety_keys_in, write_setting};

/// Later layers win key by key: built-in, `global`, `project`, `project_local`, the workflow with
/// the run's learned workflow over it, then `overrides`. Workflow files are looked up in
/// `.argonauts/workflows/`, then `~/.argonauts/workflows/`, then the built-in.
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
    /// The learned workflow the Captain-or-crew choice matched for this run, applied in the
    /// workflow layer.
    pub learned_workflow: Option<LearnedWorkflowOverlay>,
    /// `key=value` settings a request asked for, above every other layer, for one run. A
    /// safety key among them must only tighten what the other layers say.
    pub request: Vec<String>,
    /// A key a file sets that can't be used is an error, as a check of the files wants. Off,
    /// as when the app starts, the key is left out, its default is used and a warning names
    /// it, so a file from an older Argonauts Code or with a typo never stops the app.
    pub strict: bool,
    /// The project's files that came with the repository and wait for your trust. The
    /// workflow's settings layer passes them over, so one named after a built-in leaves it in
    /// place until you trust it.
    pub waiting: Vec<PathBuf>,
    /// Your home folder, so a file under it is named from `~/` in what reading it warns of.
    pub home: Option<PathBuf>,
}

/// A learned workflow version's protocol settings, as learning's
/// `learned_workflows::settings_overlay` gives them. A learned workflow that sets a safety key or a
/// fixed-core key is left out whole, with a warning, never half-applied.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LearnedWorkflowOverlay {
    /// Such as `learned workflow api-feature v4`, which `/config` shows as the keys' source.
    pub label: String,
    /// The settings, as TOML text.
    pub settings: String,
    /// One of the version's canary runs: with `evolution.canary_pull_requests`, its merge opens
    /// a pull request (`merge.mode = "pull_request"`).
    pub canary: bool,
    /// A replay of this version in an isolated repo, which never merges (`merge.mode =
    /// "leave"`) and spends at most this much (`limits.spend_per_run`).
    pub replay_limit_usd: Option<u64>,
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
    pub warnings: Vec<SettingsWarning>,
}

/// Something reading the settings left out or put back to its default, worded for a line on
/// the screen. What it is about tells the start which lines a question there already says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsWarning {
    pub about: WarningAbout,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarningAbout {
    /// A key of a project file you haven't trusted, left out until you do.
    Untrusted {
        file: PathBuf,
        key: String,
    },
    /// A key no setting has, such as a typo.
    Unknown {
        key: String,
    },
    /// A name that changed, which the file still uses, so the line does nothing until the file
    /// says the new one.
    Renamed {
        file: PathBuf,
    },
    Other,
}

impl SettingsWarning {
    pub(crate) fn new(about: WarningAbout, text: impl Into<String>) -> Self {
        Self {
            about,
            text: text.into(),
        }
    }

    pub(crate) fn other(text: impl Into<String>) -> Self {
        Self::new(WarningAbout::Other, text)
    }
}

impl std::ops::Deref for SettingsWarning {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl PartialEq<str> for SettingsWarning {
    fn eq(&self, other: &str) -> bool {
        self.text == other
    }
}

impl PartialEq<&str> for SettingsWarning {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

impl PartialEq<String> for SettingsWarning {
    fn eq(&self, other: &String) -> bool {
        &self.text == other
    }
}

impl std::fmt::Display for SettingsWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
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
pub struct WorkflowUpgrade {
    pub name: String,
    pub path: PathBuf,
    pub outcome: WorkflowUpgradeOutcome,
}

/// Judged by the built-in hash the file records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowUpgradeOutcome {
    Current,
    /// You hadn't edited it, so it became the new built-in.
    Updated,
    /// You edited it, so it was kept. `tell` is true only the first time, so the app says so
    /// once and points at `/workflow diff <name>`.
    KeptEdited {
        tell: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowChoice {
    pub name: String,
    /// `built-in`, or the file it came from.
    pub source: String,
    pub description: String,
    /// The dotted keys it sets.
    pub keys: Vec<String>,
}
