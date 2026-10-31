//! `models.toml`: `[[model]]`, `[[effort]]`, and `[[cli]]` entries.

use std::io::ErrorKind;
use std::path::Path;

use argonauts_core::base::names::ModelRef;
use argonauts_core::models::model_catalog::{CatalogEntry, EffortNote, ModelCatalog};
use argonauts_core::models::registry::{CliManifest, ModelEntry, RegistryFile};
use serde::Deserialize;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use super::catalog;
use crate::SettingsError;
use crate::settings::write::{lock_folder, real_path, write_replacing};

const TIERS: std::ops::RangeInclusive<u8> = 1..=5;

/// One `models.toml`. A `[[model]]` table holding only `id` and `tier` rates a model described
/// elsewhere (built in, discovered, or in another file) and changes nothing else about it.
/// One holding only `id` and the catalog's keys (`name`, `description`, `efforts`, and the
/// rest) changes what `/model` says of it, and `[[effort]]` what a level means.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelsFile {
    pub models: Vec<ModelEntry>,
    pub tiers: Vec<ModelTier>,
    pub clis: Vec<CliManifest>,
    pub catalog: ModelCatalog,
}

/// The keys a `[[model]]` table may hold to describe a model without being a whole entry.
const CATALOG_KEYS: &[&str] = &[
    "name",
    "description",
    "recommended",
    "efforts",
    "default_effort",
    "aliases",
    "rank",
    "runs",
    "effort_notes",
];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelTier {
    pub id: ModelRef,
    pub tier: u8,
}

/// Reads one `models.toml`. A missing file is an empty registry; one that doesn't parse is an
/// error naming the file.
pub fn read_registry_file(path: &Path) -> Result<ModelsFile, SettingsError> {
    let Some(text) = read_if_present(path)? else {
        return Ok(ModelsFile::default());
    };
    parse_models_text(path, &text)
}

pub(crate) fn parse_models_text(path: &Path, text: &str) -> Result<ModelsFile, SettingsError> {
    parse_registry(text).map_err(|message| invalid(path, message))
}

/// Every `[[model]]` is checked, so a typo fails loudly instead of quietly meaning something
/// else.
fn parse_registry(text: &str) -> Result<ModelsFile, String> {
    let mut table: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let items = match table.remove("model") {
        None => Vec::new(),
        Some(toml::Value::Array(items)) => items,
        Some(_) => return Err("`model` must be a list of [[model]] tables".to_owned()),
    };
    let efforts: Vec<EffortNote> = match table.remove("effort") {
        None => Vec::new(),
        Some(notes) => notes
            .try_into()
            .map_err(|e: toml::de::Error| format!("[[effort]]: {}", e.message()))?,
    };
    for note in &efforts {
        catalog::check_note(note).map_err(|e| format!("[[effort]] {}: {e}", note.level))?;
    }
    let rest: RegistryFile = toml::Value::Table(table)
        .try_into()
        .map_err(|e: toml::de::Error| e.message().to_owned())?;
    let mut file = ModelsFile {
        clis: rest.clis,
        catalog: ModelCatalog {
            efforts,
            ..ModelCatalog::default()
        },
        ..ModelsFile::default()
    };
    for (index, item) in items.into_iter().enumerate() {
        let name = item
            .get("id")
            .and_then(toml::Value::as_str)
            .map_or_else(|| format!("#{}", index + 1), str::to_owned);
        let at = |message: &str| format!("[[model]] {name}: {message}");
        if sets_only_tier(&item) {
            let tier: ModelTier = item
                .try_into()
                .map_err(|e: toml::de::Error| at(e.message()))?;
            check_tier(tier.tier).map_err(|e| at(&e))?;
            file.tiers.push(tier);
        } else if describes_only(&item) {
            let (entry, tier) = description(item).map_err(|e| at(&e))?;
            file.tiers.extend(tier);
            file.catalog.models.push(entry);
        } else {
            let entry: ModelEntry = item
                .try_into()
                .map_err(|e: toml::de::Error| at(e.message()))?;
            check_entry(&entry).map_err(|e| at(&e))?;
            file.models.push(entry);
        }
    }
    Ok(file)
}

fn sets_only_tier(item: &toml::Value) -> bool {
    item.as_table().is_some_and(|table| {
        table.contains_key("tier") && table.keys().all(|key| key == "id" || key == "tier")
    })
}

fn describes_only(item: &toml::Value) -> bool {
    item.as_table().is_some_and(|table| {
        table.keys().any(|key| CATALOG_KEYS.contains(&key.as_str()))
            && table
                .keys()
                .all(|key| key == "id" || key == "tier" || CATALOG_KEYS.contains(&key.as_str()))
    })
}

/// A `[[model]]` that only describes a model, with the tier it sets too.
fn description(item: toml::Value) -> Result<(CatalogEntry, Option<ModelTier>), String> {
    let toml::Value::Table(mut table) = item else {
        return Err("isn't a table".to_owned());
    };
    let tier = match (table.remove("tier"), table.get("id")) {
        (Some(tier), Some(id)) => {
            let mut rating = toml::Table::new();
            rating.insert("id".to_owned(), id.clone());
            rating.insert("tier".to_owned(), tier);
            let rating: ModelTier = toml::Value::Table(rating)
                .try_into()
                .map_err(|e: toml::de::Error| e.message().to_owned())?;
            check_tier(rating.tier)?;
            Some(rating)
        }
        _ => None,
    };
    let entry: CatalogEntry = toml::Value::Table(table)
        .try_into()
        .map_err(|e: toml::de::Error| e.message().to_owned())?;
    catalog::check_entry(&entry)?;
    Ok((entry, tier))
}

fn only_tier(table: &Table) -> bool {
    table.iter().all(|(key, _)| key == "id" || key == "tier")
}

fn check_entry(entry: &ModelEntry) -> Result<(), String> {
    if let Some(tier) = entry.tier {
        check_tier(tier)?;
    }
    catalog::check_entry(&catalog::described(entry))?;
    // A price that's NaN or below zero would make every estimate pass a dollar cap.
    let cost = &entry.cost;
    let prices = [
        ("input_per_mtok", cost.input_per_mtok),
        ("output_per_mtok", cost.output_per_mtok),
        ("cached_input_per_mtok", cost.cached_input_per_mtok),
    ];
    for (key, price) in prices {
        if let Some(price) = price.filter(|p| !p.is_finite() || *p < 0.0) {
            return Err(format!(
                "cost.{key} must be dollars per million tokens, 0 or more (it's {price})"
            ));
        }
    }
    Ok(())
}

fn check_tier(tier: u8) -> Result<(), String> {
    if TIERS.contains(&tier) {
        Ok(())
    } else {
        Err(format!("tier must be 1 to 5, not {tier}"))
    }
}

/// Sets or clears your `tier` (1 to 5) for the model `entry` names, keeping the file's
/// comments. A whole entry the file holds gets the tier; any other model gets a `[[model]]`
/// with only `id` and `tier`, so the rest of its entry still comes from where it did (the
/// built-ins follow `agents.<id>.auth`, and discovery keeps its efforts and prices current).
/// Clearing takes that table out again.
pub fn set_model_tier(
    file: &Path,
    entry: &ModelEntry,
    tier: Option<u8>,
) -> Result<(), SettingsError> {
    let id = entry.id.to_string();
    if let Some(tier) = tier {
        check_tier(tier).map_err(|e| invalid(file, format!("[[model]] {id}: {e}")))?;
    }
    let target = real_path(file)?;
    if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| io_error(parent, &e))?;
    }
    let _turn = lock_folder(&target)?;
    let text = read_if_present(&target)?.unwrap_or_default();
    let mut doc: DocumentMut = text.parse().map_err(|e| invalid(file, format!("{e}")))?;

    if tier.is_none() && !doc.contains_key("model") {
        return Ok(());
    }
    let models = doc
        .entry("model")
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()));
    let Some(models) = models.as_array_of_tables_mut() else {
        return Err(invalid(
            file,
            "`model` isn't a list of [[model]] tables".to_owned(),
        ));
    };
    let at = models
        .iter()
        .position(|table| table.get("id").and_then(Item::as_str) == Some(id.as_str()));
    match (at, tier) {
        (Some(at), Some(tier)) => {
            if let Some(table) = models.get_mut(at) {
                table.insert("tier", value(i64::from(tier)));
            }
        }
        (Some(at), None) => {
            let Some(table) = models.get_mut(at) else {
                return Ok(());
            };
            if only_tier(table) {
                models.remove(at);
            } else {
                table.remove("tier");
            }
        }
        (None, Some(tier)) => {
            let mut table = Table::new();
            table.insert("id", value(id));
            table.insert("tier", value(i64::from(tier)));
            models.push(table);
        }
        (None, None) => return Ok(()),
    }
    write_replacing(&target, &doc.to_string())
}

fn read_if_present(path: &Path) -> Result<Option<String>, SettingsError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(path, &e)),
    }
}

fn invalid(path: &Path, message: String) -> SettingsError {
    SettingsError::Invalid {
        layer: path.display().to_string(),
        message,
    }
}

fn io_error(path: &Path, e: &std::io::Error) -> SettingsError {
    SettingsError::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::models::registry::{CliManifest, CostUnit, ModelAccess, RegistryKind};

    use super::*;
    use crate::models::{FileRegistry, ProjectModels};
    use argonauts_core::models::registry::ModelRegistry;
    use argonauts_core::settings::Settings;
    use argonauts_core::sha256_hex;

    const CONTRACT_EXAMPLE: &str = include_str!("../../tests/fixtures/models.toml");

    #[test]
    fn registry_file_parses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.toml");
        std::fs::write(&path, CONTRACT_EXAMPLE).unwrap();
        let file = read_registry_file(&path).unwrap();

        let [opus, codex] = file.models.as_slice() else {
            panic!("{:#?}", file.models);
        };
        assert_eq!(opus.id.to_string(), "claude-code/opus");
        assert_eq!(opus.provider, "anthropic");
        assert_eq!(opus.kind, RegistryKind::Cli);
        assert_eq!(opus.access, ModelAccess::Subscription);
        assert!(opus.tools);
        assert_eq!(opus.cost.unit, CostUnit::PlanUsage);
        assert_eq!(opus.strengths, ["refactor"]);
        assert_eq!(opus.efforts, ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(opus.tier, Some(5));
        assert!(!opus.discovered);

        assert_eq!(codex.id, ModelRef::new("codex", Some("<model>".to_owned())));
        assert_eq!(codex.access, ModelAccess::Local);
        assert_eq!(codex.cost.unit, CostUnit::None);
        let launch = codex.launch.as_ref().unwrap();
        assert!(launch.oss);
        assert_eq!(launch.local_provider.as_deref(), Some("ollama"));
        assert_eq!(codex.tier, None);

        let [plain, acp]: &[CliManifest; 2] = file.clis.as_slice().try_into().unwrap();
        assert_eq!(plain.id, "my-agent");
        assert_eq!(plain.build_args, ["--model", "{model}"]);
        assert_eq!(acp.provider, "from-model");
        assert!(acp.acp.is_some());

        assert_eq!(
            read_registry_file(&dir.path().join("absent.toml")).unwrap(),
            ModelsFile::default()
        );
        std::fs::write(&path, "[[model]]\nid = 3\n").unwrap();
        let err = read_registry_file(&path).unwrap_err().to_string();
        assert!(err.contains(&path.display().to_string()), "{err}");
        std::fs::write(
            &path,
            "[[model]]\nid = \"codex/x\"\nprovider = \"openai\"\nkind = \"cli\"\n\
             access = \"subscription\"\ncost = { unit = \"plan_usage\" }\nefforts = [\"hgih\"]\n",
        )
        .unwrap();
        let err = read_registry_file(&path).unwrap_err().to_string();
        assert!(err.contains("\"hgih\""), "{err}");

        // Later wins by id: the project over yours over the built-in.
        let global = dir.path().join("global.toml");
        let project = dir.path().join("project.toml");
        std::fs::write(
            &global,
            "[[model]]\nid = \"claude-code/opus\"\nprovider = \"anthropic\"\nkind = \"cli\"\n\
             access = \"subscription\"\ncost = { unit = \"plan_usage\" }\nstrengths = [\"mine\"]\n\
             [[model]]\nid = \"ollama/qwen3:8b\"\nprovider = \"alibaba\"\nkind = \"endpoint\"\n\
             access = \"local\"\ncost = { unit = \"none\" }\n",
        )
        .unwrap();
        std::fs::write(
            &project,
            "[[model]]\nid = \"ollama/qwen3:8b\"\nprovider = \"alibaba\"\nkind = \"endpoint\"\n\
             access = \"local\"\ncost = { unit = \"none\" }\ngood_for = [\"reviewer\"]\n",
        )
        .unwrap();
        let trusted = |path: &Path| {
            let hash = sha256_hex(&std::fs::read(path).unwrap());
            ProjectModels::read(path, &[hash]).unwrap()
        };
        let registry = FileRegistry::from_files(
            &Settings::default(),
            Some(&global),
            Some(&trusted(&project)),
        );
        assert!(registry.warnings().is_empty(), "{:?}", registry.warnings());
        let get = |w: &str| registry.get(&w.parse().unwrap()).unwrap();
        assert_eq!(get("claude-code/opus").strengths, ["mine"]);
        assert_eq!(get("ollama/qwen3:8b").good_for.len(), 1);
        assert_eq!(
            registry.provider(&"ollama/qwen3:8b".parse().unwrap()),
            "alibaba"
        );
        let ids: Vec<String> = registry.list().iter().map(|e| e.id.to_string()).collect();
        assert_eq!(ids.iter().filter(|id| *id == "claude-code/opus").count(), 1);
        assert!(ids.contains(&"ollama/qwen3:8b".to_owned()));

        std::fs::write(&project, "not toml [").unwrap();
        let registry = FileRegistry::from_files(
            &Settings::default(),
            Some(&global),
            Some(&trusted(&project)),
        );
        assert_eq!(registry.warnings().len(), 1);
        assert!(registry.warnings()[0].contains("project.toml"));
        assert!(registry.get(&"ollama/qwen3:8b".parse().unwrap()).is_some());
    }

    #[test]
    fn tier_is_read_and_checked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.toml");
        let entry = |tier: &str| {
            format!(
                "# my models\n[[model]]\nid = \"codex/gpt-6-luna\" # the cheap one\nprovider = \"openai\"\n\
                 kind = \"cli\"\naccess = \"subscription\"\ncost = {{ unit = \"plan_usage\" }}\n{tier}"
            )
        };

        std::fs::write(&path, entry("tier = 3\n")).unwrap();
        let file = read_registry_file(&path).unwrap();
        assert_eq!(file.models[0].tier, Some(3));
        std::fs::write(&path, entry("")).unwrap();
        assert_eq!(read_registry_file(&path).unwrap().models[0].tier, None);
        for bad in ["tier = 0\n", "tier = 6\n"] {
            std::fs::write(&path, entry(bad)).unwrap();
            let err = read_registry_file(&path).unwrap_err().to_string();
            assert!(err.contains("tier must be 1 to 5"), "{err}");
            assert!(err.contains("codex/gpt-6-luna"), "{err}");
            assert!(err.contains(&path.display().to_string()), "{err}");
        }

        std::fs::write(&path, entry("tier = 3\n")).unwrap();
        let luna = read_registry_file(&path).unwrap().models.remove(0);
        set_model_tier(&path, &luna, Some(2)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# my models") && text.contains("# the cheap one"),
            "{text}"
        );
        assert_eq!(read_registry_file(&path).unwrap().models[0].tier, Some(2));

        set_model_tier(&path, &luna, None).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("tier"), "{text}");
        assert!(text.contains("# the cheap one"), "{text}");

        let err = set_model_tier(&path, &luna, Some(9))
            .unwrap_err()
            .to_string();
        assert!(err.contains("tier must be 1 to 5"), "{err}");

        // A model the file doesn't have yet, such as a discovered one, gets only its tier, so
        // the rest keeps coming from discovery.
        let mut found = crate::models::builtin::cli_entry(
            &Settings::default(),
            "antigravity-cli",
            Some("gemini-3.6-flash-low"),
        );
        found.discovered = true;
        found.efforts = vec!["low".to_owned()];
        set_model_tier(&path, &found, Some(1)).unwrap();
        let file = read_registry_file(&path).unwrap();
        assert_eq!(file.models.len(), 1);
        assert_eq!(
            file.tiers,
            [ModelTier {
                id: found.id.clone(),
                tier: 1
            }]
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my models"), "{text}");
        assert!(
            text.ends_with("[[model]]\nid = \"antigravity-cli/gemini-3.6-flash-low\"\ntier = 1\n"),
            "{text}"
        );
        set_model_tier(&path, &found, Some(2)).unwrap();
        assert_eq!(read_registry_file(&path).unwrap().tiers[0].tier, 2);

        let fresh = dir.path().join("new").join("models.toml");
        set_model_tier(&fresh, &found, None).unwrap();
        assert!(!fresh.exists());
        set_model_tier(&fresh, &found, Some(4)).unwrap();
        assert_eq!(read_registry_file(&fresh).unwrap().tiers[0].tier, 4);

        // A tier alone needs a model to land on; the rest of the entry is discovery's.
        let registry = FileRegistry::from_files(&Settings::default(), Some(&path), None);
        let id = found.id.clone();
        assert!(registry.get(&id).is_none());
        registry.discovered.write().unwrap().push(found.clone());
        let tiered = registry.get(&id).unwrap();
        assert_eq!(tiered.tier, Some(2));
        assert!(tiered.discovered);
        assert_eq!(tiered.efforts, ["low"]);

        for bad in ["tier = 0\n", "tier = 6\n"] {
            std::fs::write(&fresh, format!("[[model]]\nid = \"codex/x\"\n{bad}")).unwrap();
            let err = read_registry_file(&fresh).unwrap_err().to_string();
            assert!(
                err.contains("[[model]] codex/x: tier must be 1 to 5"),
                "{err}"
            );
        }
    }

    #[test]
    fn prices_must_be_real_dollars() {
        let entry = |price: &str| {
            format!(
                "[[model]]\nid = \"openrouter/openai/gpt-5\"\nprovider = \"openai\"\nkind = \"endpoint\"\n\
                 access = \"api-key\"\ncost = {{ unit = \"usd\", input_per_mtok = 1.0, {price} }}\n"
            )
        };
        assert!(parse_registry(&entry("output_per_mtok = 0.0")).is_ok());
        for bad in [
            "output_per_mtok = nan",
            "output_per_mtok = -1.0",
            "output_per_mtok = inf",
            "output_per_mtok = 1.0, cached_input_per_mtok = -0.5",
        ] {
            let err = parse_registry(&entry(bad)).unwrap_err();
            assert!(
                err.starts_with("[[model]] openrouter/openai/gpt-5: cost."),
                "{bad}: {err}"
            );
        }
    }
}
