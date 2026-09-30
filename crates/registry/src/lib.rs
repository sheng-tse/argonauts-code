//! Settings resolution, presets, the model registry, and the plan-terms note. Only the default
//! `peer-crew` preset resolves so far, and the registry knows no models.

mod catalog;
mod resolve;
mod write;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use argonauts_core::app::ConfigEntry;
use argonauts_core::events::run::OverrideRecord;
use argonauts_core::future::BoxFuture;
use argonauts_core::limits::CostEstimate;
use argonauts_core::names::WorkerRef;
use argonauts_core::registry::{ModelEntry, ModelRegistry};
use argonauts_core::settings::Settings;

pub use catalog::{setting_meta, settings_catalog};
pub use resolve::resolve;
pub use write::{remove_setting, safety_keys_in, write_setting};

/// Shown once in `argonauts init`. It names no vendor on purpose.
pub const TERMS_NOTE: &str =
    "Automated use may be subject to your plan's terms; API keys work with every adapter.";

/// Later layers win key by key: built-in, `global`, `project`, `project_local`, the preset,
/// then `overrides`. Preset files are looked up in `.argonauts/presets/`, then
/// `~/.argonauts/presets/`, then the built-in.
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
    #[error("{0} is not implemented yet")]
    NotImplemented(String),
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

/// Includes the repo's `.argonauts/models.toml`. For now it knows no models.
pub fn registry(settings: &Settings, repo_root: Option<&Path>) -> Arc<dyn ModelRegistry> {
    let _ = (settings, repo_root);
    Arc::new(EmptyRegistry)
}

/// The built-in preset `name` as an editable TOML file. It sets only the keys the preset
/// changes, since the preset layer hides your settings files, and records the built-in's hash
/// so an upgrade can tell whether you edited it. Not written yet, so it returns an error.
pub fn builtin_preset(name: &str) -> Result<String, SettingsError> {
    Err(SettingsError::NotImplemented(format!(
        "the built-in preset file for {name:?}"
    )))
}

/// Writes the built-in preset into `dir` and returns the file. A file there that you edited is
/// never replaced (`SettingsError::Refused`). Not implemented yet.
pub fn write_preset(name: &str, dir: &Path) -> Result<PathBuf, SettingsError> {
    let _ = (name, dir);
    Err(SettingsError::NotImplemented(
        "writing preset files".to_owned(),
    ))
}

/// Replaces each preset file in `dir` that you didn't edit with the new built-in and keeps the
/// edited ones. Only for `~/.argonauts/presets/`: a project's presets are reviewed and
/// committed by the team. Not implemented yet.
pub fn upgrade_presets(dir: &Path) -> Result<Vec<PresetUpgrade>, SettingsError> {
    let _ = dir;
    Err(SettingsError::NotImplemented(
        "upgrading preset files".to_owned(),
    ))
}

/// Every preset you can pick: the built-ins, then the `*.toml` files in `global`, then in
/// `project`. A file shadows an earlier preset of the same name. File descriptions aren't read
/// yet.
pub fn presets_in(global: &Path, project: Option<&Path>) -> Vec<PresetInfo> {
    let mut all = presets();
    for dir in std::iter::once(global).chain(project) {
        for (name, path) in preset_files(dir) {
            let keys = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| text.parse::<toml::Table>().ok())
                .map(|table| {
                    resolve::leaves(&table)
                        .into_iter()
                        .map(|(key, _)| key)
                        .collect()
                })
                .unwrap_or_default();
            let info = PresetInfo {
                source: path.display().to_string(),
                description: String::new(),
                keys,
                name,
            };
            match all.iter_mut().find(|known| known.name == info.name) {
                Some(known) => {
                    known.source = info.source;
                    known.keys = info.keys;
                }
                None => all.push(info),
            }
        }
    }
    all
}

fn preset_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "toml"))
        .filter_map(|path| {
            let name = path.file_stem()?.to_str()?.to_owned();
            Some((name, path))
        })
        .collect();
    files.sort();
    files
}

/// The built-in presets, in the order the picker shows them.
pub fn presets() -> Vec<PresetInfo> {
    let built_in = |name: &str, description: &str, keys: &[&str]| PresetInfo {
        name: name.to_owned(),
        source: "built-in".to_owned(),
        description: description.to_owned(),
        keys: keys.iter().map(|key| (*key).to_owned()).collect(),
    };
    vec![
        built_in(
            "peer-crew",
            "Peers, blind drafts, cross-family review (the default)",
            &[],
        ),
        built_in(
            "coordinated-crew",
            "One coordinator plans and delegates; blind reviewers and an adjudicator",
            &[
                "drafting.by",
                "judging.method",
                "routing.algorithm",
                "review.assign",
                "review.per_card",
                "review.blind",
                "roles.adjudicator.when",
                "roles.adjudicator.fresh_session",
            ],
        ),
        built_in("solo", "One agent does every task", &["triage.force"]),
        built_in(
            "solo-reviewed",
            "One agent, reviewed by another model family",
            &["triage.force", "solo.review"],
        ),
        built_in(
            "best-of-n",
            "Several solo attempts; keeps one that passes the visible tests",
            &["triage.force", "solo.attempts"],
        ),
        built_in(
            "draft-and-vote",
            "Drafts, then a vote; no critique or spikes",
            &[
                "judging.method",
                "critique.rounds",
                "spikes.enabled",
                "divide.single_owner",
            ],
        ),
        built_in(
            "ablate-no-blind",
            "Drafts see each other",
            &["drafting.blind"],
        ),
        built_in(
            "ablate-no-anonymize",
            "Judges see authors",
            &["judging.anonymize"],
        ),
        built_in(
            "ablate-no-critique",
            "No critique round",
            &["critique.rounds"],
        ),
        built_in(
            "ablate-fixed-roles",
            "Fixed owners, no router",
            &["routing.algorithm"],
        ),
        built_in(
            "ablate-same-family",
            "Reviewers may share the owner's family",
            &["review.other_family"],
        ),
        built_in("ablate-no-spikes", "No spikes", &["spikes.enabled"]),
    ]
}

struct EmptyRegistry;

impl ModelRegistry for EmptyRegistry {
    fn get(&self, worker: &WorkerRef) -> Option<ModelEntry> {
        let _ = worker;
        None
    }

    fn family(&self, worker: &WorkerRef) -> String {
        let _ = worker;
        "unknown".to_owned()
    }

    fn list(&self) -> Vec<ModelEntry> {
        Vec::new()
    }

    fn estimate(&self, worker: &WorkerRef, input_tokens: u64, output_tokens: u64) -> CostEstimate {
        let _ = (worker, input_tokens, output_tokens);
        CostEstimate::Unknown
    }

    fn discover(&self) -> BoxFuture<'_, Vec<ModelEntry>> {
        Box::pin(async {
            unimplemented!("model discovery is not implemented yet");
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_terms_note_is_one_neutral_sentence() {
        assert_eq!(TERMS_NOTE.matches('.').count(), 1);
        for vendor in [
            "Claude",
            "Anthropic",
            "OpenAI",
            "Codex",
            "Google",
            "Gemini",
            "agy",
        ] {
            assert!(!TERMS_NOTE.contains(vendor), "{vendor}");
        }
    }

    #[test]
    fn the_default_preset_comes_first_and_every_contract_preset_is_listed() {
        let names: Vec<String> = presets().into_iter().map(|p| p.name).collect();
        assert_eq!(names[0], "peer-crew");
        assert_eq!(names.len(), 12);
        assert!(names.iter().any(|n| n == "coordinated-crew"));
    }

    #[test]
    fn your_own_presets_are_listed_and_files_shadow_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("home/presets");
        let project = dir.path().join("repo/.argonauts/presets");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(global.join("peer-crew.toml"), "[critique]\nrounds = 2\n").unwrap();
        std::fs::write(global.join("my-team.toml"), "[review]\nper_card = 2\n").unwrap();
        std::fs::write(project.join("my-team.toml"), "preset_note = 1\n").unwrap();
        std::fs::write(project.join("notes.md"), "not a preset").unwrap();
        let all = presets_in(&global, Some(&project));
        assert_eq!(all.len(), presets().len() + 1);
        let find = |name: &str| all.iter().find(|preset| preset.name == name).unwrap();
        let peer = find("peer-crew");
        assert!(
            peer.source.ends_with("home/presets/peer-crew.toml"),
            "{peer:?}"
        );
        assert_eq!(peer.keys, ["critique.rounds"]);
        assert!(!peer.description.is_empty());
        let mine = find("my-team");
        assert!(
            mine.source
                .ends_with("repo/.argonauts/presets/my-team.toml")
        );
        assert_eq!(find("solo").source, "built-in");
        assert_eq!(presets_in(&dir.path().join("none"), None), presets());
    }

    #[test]
    fn the_catalog_lists_every_default_key() {
        let catalog = settings_catalog();
        let find = |key: &str| catalog.iter().find(|meta| meta.key == key);
        assert!(find("ui.keys").is_some_and(|meta| !meta.safety));
        assert!(find("approvals.merge.mode").is_some_and(|meta| meta.safety));
        assert!(find("runs.remind_parked_days").is_some());
        assert!(find("limits.parked_run_expires_days").is_none());
    }

    #[test]
    fn the_placeholder_registry_knows_nothing() {
        let registry = registry(&Settings::default(), None);
        let worker: WorkerRef = "claude-code/opus".parse().unwrap();
        assert!(registry.get(&worker).is_none());
        assert!(registry.list().is_empty());
        assert_eq!(registry.estimate(&worker, 10, 10), CostEstimate::Unknown);
        let dir = Path::new("/nowhere");
        assert!(matches!(
            write_preset("peer-crew", dir),
            Err(SettingsError::NotImplemented(_))
        ));
        assert!(matches!(
            upgrade_presets(dir),
            Err(SettingsError::NotImplemented(_))
        ));
        assert!(matches!(
            builtin_preset("peer-crew"),
            Err(SettingsError::NotImplemented(_))
        ));
    }
}
