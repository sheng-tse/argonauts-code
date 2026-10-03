//! The model catalog's layers, each later one winning field by field: the catalog shipped in
//! `catalog/models.toml`, the copy of that file on Argonauts' main branch fetched once a day
//! and checked against the hash published next to it, what the CLIs say of their own models,
//! and your own `models.toml`.

use std::sync::OnceLock;
use std::time::Duration;

use argonauts_core::input::EFFORT_LEVELS;
use argonauts_core::model_catalog::{CATALOG_VERSION, CatalogEntry, EffortNote, ModelCatalog};
use argonauts_core::registry::{ModelEntry, RegistryKind};
use argonauts_core::sha256_hex;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::files::ModelsFile;

const SHIPPED: &str = include_str!("../../catalog/models.toml");

/// The shipped catalog's path on the main branch, so a fix there reaches every install.
pub const CATALOG_URL: &str = "https://raw.githubusercontent.com/sheng-tse/argonauts-code/main/crates/registry/catalog/models.toml";

/// New models don't ship by the hour, so a catalog a day old is still good.
pub const CATALOG_FRESH_FOR: time::Duration = time::Duration::days(1);

const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// The catalog is a few kilobytes, so anything near this size isn't it.
const MAX_CATALOG_BYTES: usize = 1024 * 1024;

const MAX_NAME_CHARS: usize = 60;
const MAX_LINE_CHARS: usize = 200;

/// The catalog this build shipped with.
pub fn shipped_catalog() -> &'static ModelCatalog {
    static CATALOG: OnceLock<ModelCatalog> = OnceLock::new();
    // A test keeps the shipped file valid, so the empty fallback never shows.
    CATALOG.get_or_init(|| parse_catalog(SHIPPED).unwrap_or_default())
}

/// Every layer over the shipped catalog: the fetched one, then what the CLIs listed in
/// `discovered`, then your files in the order they count.
pub fn layered(
    fetched: Option<&ModelCatalog>,
    discovered: &[ModelEntry],
    yours: &[ModelsFile],
) -> ModelCatalog {
    let mut catalog = shipped_catalog().clone();
    if let Some(fetched) = fetched {
        catalog.merge(fetched.clone());
    }
    catalog.merge(cli_layer(discovered));
    for file in yours {
        catalog.merge(file_layer(file));
    }
    catalog
}

/// A catalog file's text, checked.
pub fn parse_catalog(text: &str) -> Result<ModelCatalog, String> {
    let catalog: ModelCatalog = toml::from_str(text).map_err(|e| e.message().to_owned())?;
    check_catalog(&catalog)?;
    Ok(catalog)
}

fn check_catalog(catalog: &ModelCatalog) -> Result<(), String> {
    if catalog.version > CATALOG_VERSION {
        return Err(format!(
            "its format {} is newer than this Argonauts reads ({CATALOG_VERSION})",
            catalog.version
        ));
    }
    for entry in &catalog.models {
        check_entry(entry).map_err(|e| format!("[[model]] {}: {e}", entry.id))?;
    }
    for note in &catalog.efforts {
        check_note(note).map_err(|e| format!("[[effort]] {}: {e}", note.level))?;
    }
    Ok(())
}

/// Levels must be ones `/effort` knows, and words must be one plain line, since they land on
/// the screen as they are.
pub(crate) fn check_entry(entry: &CatalogEntry) -> Result<(), String> {
    for level in entry.efforts.iter().flatten() {
        check_level(level)?;
    }
    if let Some(level) = &entry.default_effort {
        check_level(level)?;
        if entry
            .efforts
            .as_ref()
            .is_some_and(|levels| !levels.contains(level))
        {
            return Err(format!("default_effort {level:?} isn't one of its efforts"));
        }
    }
    if let Some(name) = &entry.name {
        check_text("name", name, MAX_NAME_CHARS)?;
    }
    if let Some(description) = &entry.description {
        check_text("description", description, MAX_LINE_CHARS)?;
    }
    for alias in entry.aliases.iter().flatten() {
        check_text("an alias", alias, MAX_NAME_CHARS)?;
    }
    if let Some(runs) = &entry.runs {
        check_text("runs", runs, MAX_NAME_CHARS)?;
    }
    for note in &entry.effort_notes {
        check_note(note)?;
    }
    Ok(())
}

pub(crate) fn check_note(note: &EffortNote) -> Result<(), String> {
    check_level(&note.level)?;
    check_text("an effort note", &note.description, MAX_LINE_CHARS)
}

fn check_level(level: &str) -> Result<(), String> {
    if EFFORT_LEVELS.contains(&level) {
        Ok(())
    } else {
        Err(format!(
            "effort {level:?} isn't one of {}",
            EFFORT_LEVELS.join(", ")
        ))
    }
}

fn check_text(what: &str, text: &str, max: usize) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    if text.chars().any(char::is_control) {
        return Err(format!("{what} {text:?} holds a control character"));
    }
    if text.chars().count() > max {
        return Err(format!("{what} is longer than {max} characters"));
    }
    Ok(())
}

/// What the CLIs say of their own models: the names and descriptions Codex and agy give, the
/// levels they list, and their order, as each one's own picker shows it.
pub fn cli_layer(discovered: &[ModelEntry]) -> ModelCatalog {
    let mut catalog = ModelCatalog::default();
    let mut counts: Vec<(&str, u32)> = Vec::new();
    for entry in discovered
        .iter()
        .filter(|entry| entry.kind == RegistryKind::Harness)
    {
        let Some(mut layer) = CatalogEntry::from_cli(entry) else {
            continue;
        };
        let harness = entry.id.worker.as_str();
        let rank = match counts.iter_mut().find(|(id, _)| *id == harness) {
            Some((_, count)) => {
                *count += 1;
                *count
            }
            None => {
                counts.push((harness, 1));
                1
            }
        };
        layer.rank = Some(rank);
        layer.efforts = if entry.efforts.is_empty() && names_every_level(harness) {
            Some(Vec::new())
        } else {
            (!entry.efforts.is_empty()).then(|| entry.efforts.clone())
        };
        layer.default_effort.clone_from(&entry.default_effort);
        catalog.models.push(layer);
    }
    catalog
}

/// agy names each level it runs as a model of its own, so a model it lists without one takes
/// none, until a recording shows its `--effort` works for one. Its known "--effort is not
/// supported for the current model" error says as much.
fn names_every_level(harness: &str) -> bool {
    harness == "antigravity-cli"
}

/// One `models.toml`: what each whole `[[model]]` says of names, descriptions, levels, and
/// aliases, then the `[[model]]` tables that only describe a model, and `[[effort]]`.
pub fn file_layer(file: &ModelsFile) -> ModelCatalog {
    let mut catalog = ModelCatalog {
        models: file.models.iter().map(described).collect(),
        ..ModelCatalog::default()
    };
    catalog.merge(file.catalog.clone());
    catalog
}

/// The catalog fields of a registry entry, leaving unset what it leaves empty.
pub(crate) fn described(entry: &ModelEntry) -> CatalogEntry {
    CatalogEntry {
        name: entry.name.clone(),
        description: entry.description.clone(),
        efforts: (!entry.efforts.is_empty()).then(|| entry.efforts.clone()),
        default_effort: entry.default_effort.clone(),
        aliases: (!entry.aliases.is_empty()).then(|| entry.aliases.clone()),
        ..CatalogEntry::new(entry.id.clone())
    }
}

/// The catalog last fetched, as the model cache keeps it: the text, the hash the repository
/// published with it, and when it was fetched or last found unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchedCatalog {
    #[serde(with = "time::serde::rfc3339")]
    pub fetched: OffsetDateTime,
    pub sha256: String,
    pub text: String,
}

impl FetchedCatalog {
    /// Checked on every read, so a cache cut short or edited by hand counts for nothing.
    pub fn catalog(&self) -> Result<ModelCatalog, String> {
        if sha256_hex(self.text.as_bytes()) != self.sha256 {
            return Err("the catalog doesn't match its hash".to_owned());
        }
        parse_catalog(&self.text)
    }
}

/// Where the daily catalog comes from: `url`, with its SHA-256 at `url` plus `.sha256`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSource {
    pub url: String,
}

impl CatalogSource {
    pub fn argonauts() -> Self {
        Self {
            url: CATALOG_URL.to_owned(),
        }
    }

    fn hash_url(&self) -> String {
        format!("{}.sha256", self.url)
    }

    /// Reads the published hash first, and the catalog itself only when that hash isn't the
    /// one `known` holds. What it fetches must match the hash and parse; otherwise the error
    /// says why, and the catalog fetched before stays.
    pub async fn fetch(
        &self,
        known: Option<&FetchedCatalog>,
        now: OffsetDateTime,
    ) -> Result<(FetchedCatalog, ModelCatalog), String> {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .map_err(|e| format!("couldn't start an HTTP client: {e}"))?;
        let hash_url = self.hash_url();
        let published = get(&client, &hash_url).await?;
        let sha256 = published
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("{hash_url} holds no SHA-256"));
        }
        if let Some(known) = known.filter(|known| known.sha256 == sha256)
            && let Ok(catalog) = known.catalog()
        {
            let fetched = FetchedCatalog {
                fetched: now,
                ..known.clone()
            };
            return Ok((fetched, catalog));
        }
        let text = get(&client, &self.url).await?;
        if sha256_hex(text.as_bytes()) != sha256 {
            return Err(format!(
                "{} doesn't match the hash published with it",
                self.url
            ));
        }
        let fetched = FetchedCatalog {
            fetched: now,
            sha256,
            text,
        };
        let catalog = fetched
            .catalog()
            .map_err(|e| format!("{}: {e}", self.url))?;
        Ok((fetched, catalog))
    }
}

async fn get(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let mut reply = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("GET {url} failed: {}", e.without_url()))?;
    let status = reply.status();
    if !status.is_success() {
        return Err(format!("GET {url} answered {status}"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = reply
        .chunk()
        .await
        .map_err(|e| format!("reading {url} failed: {}", e.without_url()))?
    {
        if body.len() + chunk.len() > MAX_CATALOG_BYTES {
            return Err(format!("GET {url} sent more than a catalog's worth"));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| format!("GET {url} sent text that isn't UTF-8"))
}

#[cfg(test)]
mod tests;
