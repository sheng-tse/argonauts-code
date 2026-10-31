//! `~/.argonauts/models-cache.json`: what discovery last found, so `/model` lists the CLIs'
//! models at once and the next refresh only brings it up to date.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use argonauts_core::base::paths::{create_private_dir_all, write_private_file};
use argonauts_core::models::registry::ModelEntry;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::catalog::FetchedCatalog;

static PARTIAL_COUNTER: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCache {
    #[serde(with = "time::serde::rfc3339")]
    pub refreshed: OffsetDateTime,
    pub models: Vec<ModelEntry>,
    /// Why each source found nothing that time.
    #[serde(default)]
    pub skipped: Vec<String>,
    /// When Claude Code's models were last read from Anthropic, which happens once a day.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub claude_listed: Option<OffsetDateTime>,
    /// The model catalog last fetched from Argonauts Code's repository, which happens once a day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<FetchedCatalog>,
    /// When each source last listed its models, since `/model` asks them one by one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceListed>,
}

/// When one source, by CLI or endpoint id, last listed its models, and whether it failed
/// the latest time it was asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceListed {
    pub cli: String,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub at: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
}

impl ModelCache {
    /// A missing, torn, or older-shaped file reads as no cache, and the next refresh replaces it.
    pub fn read(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Two apps refreshing at once each rename a whole file into place, so the last one wins
    /// and neither leaves half a file.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            create_private_dir_all(dir)?;
        }
        let mut json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        json.push(b'\n');
        let partial = path.with_extension(format!(
            "json.{}.{}.partial",
            std::process::id(),
            PARTIAL_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        write_private_file(&partial, &json)?;
        std::fs::rename(&partial, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&partial);
        })
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::Settings;

    use super::*;
    use crate::models::builtin::cli_entry;

    #[test]
    fn a_cache_reads_back_and_a_bad_one_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home").join("models-cache.json");
        assert_eq!(ModelCache::read(&path), None);
        let cache = ModelCache {
            refreshed: time::macros::datetime!(2026-10-01 09:30 UTC),
            models: vec![cli_entry(&Settings::default(), "codex", Some("gpt-6-luna"))],
            skipped: vec!["`agy models` failed".to_owned()],
            claude_listed: Some(time::macros::datetime!(2026-10-01 09:00 UTC)),
            catalog: Some(FetchedCatalog {
                fetched: time::macros::datetime!(2026-10-01 09:00 UTC),
                sha256: "0".repeat(64),
                signature: String::new(),
                text: "version = 1\n".to_owned(),
            }),
            sources: Vec::new(),
        };
        cache.write(&path).unwrap();
        assert_eq!(ModelCache::read(&path), Some(cache));
        let leftovers = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(leftovers, 1, "no partial file is left behind");

        std::fs::write(&path, "{\"refreshed\": ").unwrap();
        assert_eq!(ModelCache::read(&path), None);
    }
}
