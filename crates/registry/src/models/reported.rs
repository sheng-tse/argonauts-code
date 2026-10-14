//! The models a CLI lists itself, such as Claude Code's `initialize` reply: names,
//! descriptions, effort levels, and what `default` runs today. They're kept in
//! `~/.argonauts/state/<cli>-models.json`, so the picker mirrors the CLI from the next start
//! too, and a model the CLI adds shows up the same day. They count over the fetched catalog and
//! under your own `models.toml`.

use std::fs;
use std::io;
use std::path::Path;

use argonauts_core::ReportedModel;
use argonauts_core::model_catalog::{CatalogEntry, ModelCatalog};
use argonauts_core::names::ModelRef;
use argonauts_core::registry::ModelEntry;
use argonauts_core::settings::Settings;
use argonauts_core::{create_private_dir_all, write_private_file};
use serde::{Deserialize, Serialize};

use super::builtin::cli_entry;

/// What `default` is called in the list.
const DEFAULT: &str = "default";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ReportedFile {
    #[serde(default)]
    models: Vec<ReportedModel>,
}

/// The list kept at `path`, or none when there's no file or it doesn't read.
pub fn read(path: &Path) -> Vec<ReportedModel> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ReportedFile>(&bytes).ok())
        .map(|file| file.models)
        .unwrap_or_default()
}

pub fn write(path: &Path, models: &[ReportedModel]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        create_private_dir_all(dir)?;
    }
    let file = ReportedFile {
        models: models.to_vec(),
    };
    let json = serde_json::to_vec_pretty(&file).map_err(io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    write_private_file(&temp, &json)?;
    fs::rename(&temp, path)
}

/// The model a listed one runs: `opus` runs `claude-opus-5-5`.
fn runs(model: &ReportedModel) -> &str {
    model.resolved.as_deref().unwrap_or(&model.value)
}

/// The catalog layer the list makes: `default` says what the CLI runs, and every other
/// model its name, description, levels, short name, and place, in the CLI's order.
pub fn layer(cli: &str, models: &[ReportedModel]) -> ModelCatalog {
    let mut catalog = ModelCatalog::default();
    let mut rank = 0;
    for model in models {
        if model.value == DEFAULT {
            let mut entry = CatalogEntry::new(ModelRef::new(cli, None));
            entry.runs = model.resolved.clone();
            // `Opus 5.5 · Best for everyday, complex tasks`: the picker names the model itself.
            let description = model
                .description
                .split_once(" · ")
                .map_or(model.description.as_str(), |(_, rest)| rest);
            entry.description = (!description.is_empty()).then(|| description.to_owned());
            catalog.models.push(entry);
            continue;
        }
        rank += 1;
        let id = runs(model);
        let mut entry = CatalogEntry::new(ModelRef::new(cli, Some(id.to_owned())));
        entry.name = Some(model.display_name.clone());
        entry.description = (!model.description.is_empty()).then(|| model.description.clone());
        entry.efforts = Some(model.efforts.clone());
        entry.aliases = (model.value != id).then(|| vec![model.value.clone()]);
        entry.rank = Some(rank);
        catalog.models.push(entry);
    }
    catalog
}

/// Registry entries for the listed models `known` has no entry for, by id or short name, so a
/// model the CLI adds can be picked before any catalog knows it.
pub fn new_entries(
    settings: &Settings,
    cli: &str,
    models: &[ReportedModel],
    known: &[ModelEntry],
) -> Vec<ModelEntry> {
    let has = |name: &str| {
        known.iter().any(|entry| {
            entry.id.agent == cli
                && (entry.id.model.as_deref() == Some(name)
                    || entry.aliases.iter().any(|alias| alias == name))
        })
    };
    models
        .iter()
        .filter(|model| model.value != DEFAULT && !has(&model.value) && !has(runs(model)))
        .map(|model| {
            let mut entry = cli_entry(settings, cli, Some(runs(model)));
            entry.discovered = true;
            entry.efforts = model.efforts.clone();
            entry.name = Some(model.display_name.clone());
            entry.description = (!model.description.is_empty()).then(|| model.description.clone());
            if model.value != runs(model) {
                entry.aliases = vec![model.value.clone()];
            }
            entry
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(value: &str, resolved: &str, name: &str, description: &str) -> ReportedModel {
        ReportedModel {
            value: value.to_owned(),
            resolved: Some(resolved.to_owned()),
            display_name: name.to_owned(),
            description: description.to_owned(),
            efforts: vec!["low".to_owned(), "high".to_owned()],
        }
    }

    /// As Claude Code 2.1.289 listed them in a recorded session, with one model it might add.
    fn listed() -> Vec<ReportedModel> {
        vec![
            model(
                "default",
                "claude-opus-5-5",
                "Default (recommended)",
                "Opus 5.5 · Best for everyday, complex tasks",
            ),
            model(
                "opus",
                "claude-opus-5-5",
                "Opus 5.5",
                "For complex work and everyday tasks",
            ),
            model(
                "sonnet",
                "claude-sonnet-5-5",
                "Sonnet 5.5",
                "Most efficient for simpler tasks",
            ),
            ReportedModel {
                efforts: Vec::new(),
                ..model(
                    "haiku",
                    "claude-haiku-4-5-20251001",
                    "Haiku 4.5",
                    "Fastest for quick answers",
                )
            },
            model(
                "claude-opus-6",
                "claude-opus-6",
                "Opus 6",
                "The newest, for your hardest work",
            ),
        ]
    }

    #[test]
    fn the_list_says_what_default_runs_and_names_each_model() {
        let layer = layer("claude-code", &listed());
        let default = &layer.models[0];
        assert_eq!(default.id, ModelRef::new("claude-code", None));
        assert_eq!(default.runs.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(
            default.description.as_deref(),
            Some("Best for everyday, complex tasks")
        );
        let sonnet = layer
            .models
            .iter()
            .find(|m| m.id.model.as_deref() == Some("claude-sonnet-5-5"))
            .unwrap();
        assert_eq!(sonnet.name.as_deref(), Some("Sonnet 5.5"));
        assert_eq!(sonnet.aliases.as_deref(), Some(&["sonnet".to_owned()][..]));
        assert_eq!(sonnet.rank, Some(2));
        let haiku = layer
            .models
            .iter()
            .find(|m| m.aliases.as_deref() == Some(&["haiku".to_owned()][..]))
            .unwrap();
        assert_eq!(haiku.efforts.as_deref(), Some(&[][..]));
        let newest = layer.models.last().unwrap();
        assert_eq!(newest.aliases, None);
        assert_eq!(newest.rank, Some(4));
    }

    #[test]
    fn only_models_the_registry_lacks_get_entries() {
        let settings = Settings::default();
        let known = vec![
            cli_entry(&settings, "claude-code", Some("opus")),
            cli_entry(&settings, "claude-code", Some("sonnet")),
            cli_entry(&settings, "claude-code", Some("haiku")),
        ];
        let added = new_entries(&settings, "claude-code", &listed(), &known);
        let ids: Vec<_> = added.iter().map(|e| e.id.to_string()).collect();
        assert_eq!(ids, ["claude-code/claude-opus-6"]);
        assert_eq!(added[0].name.as_deref(), Some("Opus 6"));
        assert!(added[0].discovered);
    }

    #[test]
    fn the_list_is_kept_between_runs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/claude-code-models.json");
        assert!(read(&path).is_empty());
        write(&path, &listed()).unwrap();
        assert_eq!(read(&path), listed());
        std::fs::write(&path, "not json").unwrap();
        assert!(read(&path).is_empty());
    }
}
