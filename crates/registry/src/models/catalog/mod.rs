//! The model catalog's layers, each later one winning field by field: the catalog shipped in
//! `catalog/models.toml`, the copy served beside the update manifests, fetched once a day and
//! signed with the same minisign keys, what the CLIs say of their own models, and your own
//! `models.toml`.

use std::sync::OnceLock;
use std::time::Duration;

use argonauts_core::app::model_picker::LocalModel;
use argonauts_core::input::EFFORT_LEVELS;
use argonauts_core::models::model_catalog::{
    CATALOG_VERSION, CatalogEntry, EffortNote, ModelCatalog,
};
use argonauts_core::models::registry::{ModelEntry, RegistryKind};
use argonauts_core::sha256_hex;
use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::files::ModelsFile;

const SHIPPED: &str = include_str!("../../../catalog/models.toml");

/// Where the catalog is served, beside the update manifests, so a model that ships after a
/// release reaches every install. Its signature is beside it as `models.toml.minisig`.
pub const CATALOG_URL: &str = "https://argonauts-code.github.io/updates/v1/models.toml";

/// New models don't ship by the hour, so a catalog a day old is still good.
pub const CATALOG_FRESH_FOR: time::Duration = time::Duration::days(1);

const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// The catalog is a few kilobytes, so anything near this size isn't it.
const MAX_CATALOG_BYTES: usize = 1024 * 1024;

const MAX_NAME_CHARS: usize = 60;
/// Past any window a model takes today.
const MAX_WINDOW: u64 = 16 * 1024 * 1024;
const MAX_LINE_CHARS: usize = 200;

/// The catalog this build shipped with.
pub fn shipped_catalog() -> &'static ModelCatalog {
    static CATALOG: OnceLock<ModelCatalog> = OnceLock::new();
    // A test keeps the shipped file valid, so the empty fallback never shows.
    CATALOG.get_or_init(|| parse_catalog(SHIPPED).unwrap_or_default())
}

/// Every layer over the shipped catalog: the fetched one, then what discovery found in
/// `discovered`, then the lists the CLIs `reported` themselves, then your files in the order
/// they count.
pub fn layered(
    fetched: Option<&ModelCatalog>,
    discovered: &[ModelEntry],
    reported: &[ModelCatalog],
    yours: &[ModelsFile],
) -> ModelCatalog {
    let mut catalog = shipped_catalog().clone();
    // A copy published before the one this build ships would put older facts over newer ones.
    if let Some(fetched) = fetched.filter(|fetched| !published_before(fetched, &catalog)) {
        catalog.merge(fetched.clone());
    }
    catalog.merge(cli_layer(discovered));
    for layer in reported {
        catalog.merge(keeping_aliases(&catalog, layer.clone()));
    }
    for file in yours {
        catalog.merge(file_layer(file));
    }
    catalog
}

/// `layer` with each model's short names added to those the catalog already knows, so a CLI's
/// list doesn't drop one such as `claude-haiku-4-5`.
fn keeping_aliases(catalog: &ModelCatalog, mut layer: ModelCatalog) -> ModelCatalog {
    for entry in &mut layer.models {
        let Some(aliases) = entry.aliases.as_mut() else {
            continue;
        };
        let known = catalog
            .models
            .iter()
            .find(|known| known.id == entry.id)
            .and_then(|known| known.aliases.as_ref());
        for alias in known.into_iter().flatten() {
            if !aliases.contains(alias) {
                aliases.push(alias.clone());
            }
        }
    }
    layer
}

/// A catalog file's text, checked.
pub fn parse_catalog(text: &str) -> Result<ModelCatalog, String> {
    let catalog: ModelCatalog = toml::from_str(text).map_err(|e| e.message().to_owned())?;
    check_catalog(&catalog)?;
    Ok(catalog)
}

/// Whether `catalog` was published before `than`: a day earlier, or no day where `than` says
/// one, as every copy from before catalogs carried their day.
pub fn published_before(catalog: &ModelCatalog, than: &ModelCatalog) -> bool {
    match (&catalog.published, &than.published) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(day), Some(than)) => day < than,
    }
}

fn check_catalog(catalog: &ModelCatalog) -> Result<(), String> {
    if let Some(day) = &catalog.published {
        let format = time::macros::format_description!("[year]-[month]-[day]");
        time::Date::parse(day, format)
            .map_err(|_| format!("its published day {day:?} isn't a YYYY-MM-DD date"))?;
    }
    if catalog.version > CATALOG_VERSION {
        return Err(format!(
            "its format {} is newer than this Argonauts Code reads ({CATALOG_VERSION})",
            catalog.version
        ));
    }
    for entry in &catalog.models {
        check_entry(entry).map_err(|e| format!("[[model]] {}: {e}", entry.id))?;
    }
    for note in &catalog.efforts {
        check_note(note).map_err(|e| format!("[[effort]] {}: {e}", note.level))?;
    }
    for model in &catalog.local {
        check_local(model).map_err(|e| format!("[[local]] {}: {e}", model.tag))?;
    }
    for price in &catalog.prices {
        check_text("a price's model", &price.model, MAX_NAME_CHARS)
            .map_err(|e| format!("[[price]]: {e}"))?;
        if !price.list_price().is_valid() {
            return Err(format!(
                "[[price]] {}: prices are dollars per million tokens, 0 or more",
                price.model
            ));
        }
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

/// A local model's facts must be ones a computer can rank by: a tag a shell reads as one word,
/// sizes that are amounts, and a score out of 100 with where it came from.
fn check_local(model: &LocalModel) -> Result<(), String> {
    check_text("its tag", &model.tag, MAX_NAME_CHARS)?;
    if model.tag.contains(char::is_whitespace) || model.tag.starts_with('-') {
        return Err("its tag must be one word".to_owned());
    }
    check_text("name", &model.name, MAX_NAME_CHARS)?;
    if model.window == 0 || model.window > MAX_WINDOW {
        return Err(format!("window must be 1 to {MAX_WINDOW} tokens"));
    }
    for (what, figure) in [
        ("weights_gb", model.weights_gb),
        ("download_gb", model.download_gb),
        ("context_kib_per_token", model.context_kib_per_token),
    ] {
        if !figure.0.is_finite() || figure.0 < 0.0 {
            return Err(format!("{what} must be 0 or more"));
        }
    }
    if let Some(month) = &model.released
        && !is_month(month)
    {
        return Err(format!("released {month:?} isn't a month such as 2025-07"));
    }
    if let Some(coding) = &model.coding {
        if !(0.0..=100.0).contains(&coding.score.0) {
            return Err("a coding score is out of 100".to_owned());
        }
        check_text("a coding score's source", &coding.source, MAX_LINE_CHARS)?;
        check_text("a coding score's date", &coding.checked, MAX_NAME_CHARS)?;
    }
    if let Some(next) = &model.replaced_by {
        check_text("replaced_by", next, MAX_NAME_CHARS)?;
    }
    Ok(())
}

/// `2025-07`.
fn is_month(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 7
        && bytes[4] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(at, b)| at == 4 || b.is_ascii_digit())
        && matches!(text[5..].parse::<u8>(), Ok(1..=12))
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
        .filter(|entry| entry.kind == RegistryKind::Cli)
    {
        let Some(mut layer) = CatalogEntry::from_cli(entry) else {
            continue;
        };
        let cli = entry.id.agent.as_str();
        let rank = match counts.iter_mut().find(|(id, _)| *id == cli) {
            Some((_, count)) => {
                *count += 1;
                *count
            }
            None => {
                counts.push((cli, 1));
                1
            }
        };
        layer.rank = Some(rank);
        layer.efforts = if entry.efforts.is_empty() && names_every_level(cli) {
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
fn names_every_level(cli: &str) -> bool {
    cli == "antigravity-cli"
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

/// The catalog last fetched, as the model cache keeps it: the text and its hash, the signature
/// it was served with, and when it was fetched or last found unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchedCatalog {
    #[serde(with = "time::serde::rfc3339")]
    pub fetched: OffsetDateTime,
    pub sha256: String,
    /// The `.minisig` file, so a catalog signed the same way isn't downloaded again. A cache
    /// from before catalogs were signed has none, and fetches the signed one.
    #[serde(default)]
    pub signature: String,
    pub text: String,
}

impl FetchedCatalog {
    /// Checked against its hash on every read, so a cache cut short or garbled counts for
    /// nothing. The hash proves no signer: one who edits the text and its hash together edits
    /// your own files, where the signature was checked when it was fetched.
    pub fn catalog(&self) -> Result<ModelCatalog, String> {
        if sha256_hex(self.text.as_bytes()) != self.sha256 {
            return Err("the catalog doesn't match its hash".to_owned());
        }
        parse_catalog(&self.text)
    }
}

/// Where the daily catalog comes from: `url`, signed at `url` plus `.minisig` by one of `keys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSource {
    pub url: String,
    /// Minisign public keys, as a `.pub` file's second line holds one.
    pub keys: Vec<String>,
}

impl CatalogSource {
    /// The catalog served beside the update manifests, signed by one of the keys that sign
    /// them.
    pub fn argonauts(keys: &[&str]) -> Self {
        Self {
            url: CATALOG_URL.to_owned(),
            keys: keys.iter().map(|key| (*key).to_owned()).collect(),
        }
    }

    fn signature_url(&self) -> String {
        format!("{}.minisig", self.url)
    }

    /// Reads the signature first, and the catalog itself only when that signature isn't the
    /// one `known` came with. What it fetches must be signed by one of the keys and parse;
    /// otherwise the error says why, and the catalog fetched before stays.
    pub async fn fetch(
        &self,
        known: Option<&FetchedCatalog>,
        now: OffsetDateTime,
    ) -> Result<(FetchedCatalog, ModelCatalog), String> {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .user_agent(concat!("argonauts/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("couldn't start an HTTP client: {e}"))?;
        let signature = get(&client, &self.signature_url()).await?;
        if let Some(known) = known.filter(|known| known.signature == signature)
            && let Ok(catalog) = known.catalog()
        {
            let fetched = FetchedCatalog {
                fetched: now,
                ..known.clone()
            };
            return Ok((fetched, catalog));
        }
        let text = get(&client, &self.url).await?;
        if !signed_by(text.as_bytes(), &signature, &self.keys) {
            return Err(format!(
                "{} isn't signed by a key this Argonauts Code trusts",
                self.url
            ));
        }
        let fetched = FetchedCatalog {
            fetched: now,
            sha256: sha256_hex(text.as_bytes()),
            signature,
            text,
        };
        let catalog = fetched
            .catalog()
            .map_err(|e| format!("{}: {e}", self.url))?;
        // The signature proves who signed it, not when: a copy older than what this build
        // ships, or than the one fetched before, is refused, so none rolls facts back.
        let cached = known.and_then(|known| known.catalog().ok());
        let newest = [Some(shipped_catalog()), cached.as_ref()];
        if newest
            .into_iter()
            .flatten()
            .any(|than| published_before(&catalog, than))
        {
            return Err(format!(
                "{} was published before the catalog this Argonauts Code already has",
                self.url
            ));
        }
        Ok((fetched, catalog))
    }
}

/// Whether `signature`, a `.minisig` file, signs `data` with one of `keys`. A key that isn't a
/// minisign public key signs nothing.
fn signed_by(data: &[u8], signature: &str, keys: &[String]) -> bool {
    let Ok(signature) = Signature::decode(signature) else {
        return false;
    };
    keys.iter()
        .filter_map(|key| PublicKey::from_base64(key.trim()).ok())
        .any(|key| key.verify(data, &signature, false).is_ok())
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
