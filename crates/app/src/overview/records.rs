//! What the import keeps, beside the usage ledger in `~/.argonauts/usage/imported/`:
//! `records.jsonl`, one line for each session's answers in one hour with one model (when, which
//! CLI, model and folder, and the token counts), and `state.json`, the answer to the question
//! and how far into each session file the import has read. Nothing of what was said is kept.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use argonauts_core::{Paths, create_private_dir_all};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// One session's answers in one hour with one model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    /// The start of the hour the answers came in, as seconds since 1970.
    #[serde(with = "time::serde::timestamp")]
    pub at: OffsetDateTime,
    /// Such as `claude-code`.
    pub cli: String,
    pub model: String,
    /// The folder the session worked in.
    pub folder: String,
    /// New input only, never what came from the cache.
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Which session, numbered by the import, so sessions can be counted and timed.
    pub session: u32,
    /// A subagent gave these answers.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub subagent: bool,
}

/// What the import remembers between runs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ImportState {
    /// Your answer to the question: `None` until you give one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import: Option<bool>,
    /// The last session number given out.
    #[serde(default)]
    pub sessions: u32,
    /// Each session file read so far, by its path.
    #[serde(default)]
    pub files: BTreeMap<String, FileRead>,
    /// Compressed session files left out because nothing here could open them.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skipped: u32,
}

/// How far into one session file the import has read.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct FileRead {
    pub session: u32,
    /// Bytes read, up to the end of the last whole line.
    pub read: u64,
    /// The CLI's own running total so far, where it keeps one, so only what's new counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totals: Option<[u64; 4]>,
    /// The last answer counted, so one written over several lines counts once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_answer: Option<String>,
    /// The folder and model the file named last, for answers that don't repeat them.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// The session is a subagent's, as its start said.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub subagent: bool,
}

/// The records file as last read: its path, length and time, and what it held.
type Cached = (PathBuf, u64, SystemTime, Arc<Vec<Record>>);

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// The import's folder.
pub(crate) struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(paths: &Paths) -> Self {
        Self {
            dir: paths.usage_dir().join("imported"),
        }
    }

    #[cfg(test)]
    pub fn at(dir: &std::path::Path) -> Self {
        Self {
            dir: dir.to_owned(),
        }
    }

    fn records_file(&self) -> PathBuf {
        self.dir.join("records.jsonl")
    }

    fn state_file(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    pub fn state(&self) -> ImportState {
        fs::read_to_string(self.state_file())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Written whole to a file beside it, then moved over it, so a crash never leaves half.
    pub fn save_state(&self, state: &ImportState) -> io::Result<()> {
        create_private_dir_all(&self.dir)?;
        let text = serde_json::to_string_pretty(state).map_err(io::Error::other)?;
        let partial = self.dir.join("state.json.partial");
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&partial)?
            .write_all(text.as_bytes())?;
        fs::rename(partial, self.state_file())
    }

    pub fn append(&self, records: &[Record]) -> io::Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        create_private_dir_all(&self.dir)?;
        let mut text = String::new();
        for record in records {
            text.push_str(&serde_json::to_string(record).map_err(io::Error::other)?);
            text.push('\n');
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.records_file())?
            .write_all(text.as_bytes())
    }

    /// Every record. The file is read again only when it changed since the last time.
    pub fn records(&self) -> Arc<Vec<Record>> {
        static CACHE: Mutex<Option<Cached>> = Mutex::new(None);
        let path = self.records_file();
        let Ok(meta) = fs::metadata(&path) else {
            return Arc::new(Vec::new());
        };
        let stamp = (
            meta.len(),
            meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        );
        let mut cache = CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((cached, len, modified, records)) = cache.as_ref()
            && *cached == path
            && (*len, *modified) == stamp
        {
            return records.clone();
        }
        let records: Arc<Vec<Record>> = Arc::new(
            fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect(),
        );
        *cache = Some((path, stamp.0, stamp.1, records.clone()));
        records
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn records_and_state_read_back_and_a_change_is_seen() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        assert!(store.records().is_empty());
        assert_eq!(store.state(), ImportState::default());
        let record = Record {
            at: datetime!(2026-10-04 13:00 UTC),
            cli: "codex".into(),
            model: "gpt-5.6".into(),
            folder: "/work/app".into(),
            input: 10,
            output: 5,
            cache_read: 90,
            cache_write: 0,
            session: 1,
            subagent: false,
        };
        store.append(std::slice::from_ref(&record)).unwrap();
        assert_eq!(*store.records(), vec![record.clone()]);
        store.append(std::slice::from_ref(&record)).unwrap();
        assert_eq!(store.records().len(), 2);
        let state = ImportState {
            import: Some(true),
            sessions: 1,
            ..ImportState::default()
        };
        store.save_state(&state).unwrap();
        assert_eq!(store.state(), state);
    }
}
