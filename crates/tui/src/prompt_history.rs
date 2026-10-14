//! Your prompts in this repo, `history.jsonl` in its state directory: read at start, one line
//! appended per submitted prompt, repeats collapsed. Each line is `{"prompt": …, "at": …}` with
//! the Unix time it was sent; a bare JSON string from before times were kept still reads, with
//! no time.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Entries kept in memory. The file grows by appends, and past twice this many lines it is
/// rewritten at start with only these.
pub const HISTORY_LIMIT: usize = 1000;

/// Lines past which the file is rewritten at start.
const COMPACT_AT: usize = 2 * HISTORY_LIMIT;

/// The file in each repo's state directory.
const FILE_NAME: &str = argonauts_core::layout::repo_state::HISTORY;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptHistory {
    /// `None` keeps history in memory only.
    pub file: Option<PathBuf>,
    /// Oldest first, each prompt once.
    pub entries: Vec<PromptEntry>,
    /// History files read for the search, each kept until the file changes, so opening it
    /// again reads only what changed.
    read: HashMap<PathBuf, ReadFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadFile {
    stamp: Stamp,
    entries: Vec<PromptEntry>,
}

/// History files change only by appends and rewrites, so the length and time tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn of(file: &Path) -> Option<Self> {
        let meta = fs::metadata(file).ok()?;
        Some(Self {
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptEntry {
    pub prompt: String,
    /// Unix seconds when it was sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
}

impl PromptEntry {
    pub fn new(text: impl Into<String>, at: Option<u64>) -> Self {
        Self {
            prompt: text.into(),
            at,
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Line {
    Entry(PromptEntry),
    Bare(String),
}

impl PromptHistory {
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// An empty history that appends to `file`, for when reading it failed.
    pub fn unread(file: &Path) -> Self {
        Self {
            file: Some(file.to_owned()),
            ..Self::default()
        }
    }

    /// A missing file is an empty history. A line that isn't a prompt is skipped, so one torn
    /// write never costs the rest. A file grown past [`COMPACT_AT`] lines is rewritten with
    /// only the entries kept.
    pub fn load(file: &Path) -> Result<Self, HistoryError> {
        let mut history = Self::unread(file);
        let text = match fs::read_to_string(file) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(history),
            Err(error) => return Err(error.into()),
        };
        let (entries, lines) = parse(&text);
        if lines > COMPACT_AT {
            // Housekeeping only: the history read fine either way.
            let _ = compact(file, text.len(), &entries);
        }
        history.entries = entries;
        Ok(history)
    }

    /// Oldest first.
    pub fn texts(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.prompt.as_str())
    }

    /// Remembers the prompt, sent now, and appends it to the file, unless it repeats the
    /// newest entry.
    pub fn append(&mut self, prompt: &str) -> Result<(), HistoryError> {
        self.append_at(prompt, unix_now())
    }

    pub fn append_at(&mut self, prompt: &str, at: u64) -> Result<(), HistoryError> {
        let newest = self.entries.last().map(|entry| entry.prompt.as_str());
        if prompt.trim().is_empty() || newest == Some(prompt) {
            return Ok(());
        }
        let entry = PromptEntry::new(prompt, Some(at));
        let mut line = serde_json::to_string(&entry)?;
        self.remember(entry);
        let Some(file) = &self.file else {
            return Ok(());
        };
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)?;
        }
        line.push('\n');
        let mut out = OpenOptions::new().create(true).append(true).open(file)?;
        out.write_all(line.as_bytes())?;
        Ok(())
    }

    /// This repo's prompts as its file has them now, with this window's, oldest first and each
    /// prompt once at its newest. Another window in this repo appends to the same file, so its
    /// prompts since start are here too.
    pub fn this_project(&mut self) -> Vec<PromptEntry> {
        let mut all = match self.file.clone() {
            Some(file) => self.read_cached(&file),
            None => Vec::new(),
        };
        all.extend(self.entries.iter().cloned());
        merged(all, HISTORY_LIMIT)
    }

    /// This repo's prompts and every other repo's, from the `history.jsonl` beside each other
    /// state directory, oldest first and each prompt once at its newest. Files that can't be
    /// read are left out.
    pub fn everywhere(&mut self) -> Vec<PromptEntry> {
        let files = self.other_files();
        // A repo whose history went away leaves nothing behind.
        let own = self.file.clone();
        self.read
            .retain(|file, _| files.contains(file) || own.as_ref() == Some(file));
        let mut all = Vec::new();
        for file in &files {
            all.extend(self.read_cached(file));
        }
        all.extend(self.this_project());
        merged(all, usize::MAX)
    }

    /// A file's entries, read again only when it changed since the last time.
    fn read_cached(&mut self, file: &Path) -> Vec<PromptEntry> {
        let Some(stamp) = Stamp::of(file) else {
            self.read.remove(file);
            return Vec::new();
        };
        if let Some(read) = self.read.get(file).filter(|read| read.stamp == stamp) {
            return read.entries.clone();
        }
        let entries = fs::read_to_string(file).map_or_else(|_| Vec::new(), |text| parse(&text).0);
        let read = ReadFile {
            stamp,
            entries: entries.clone(),
        };
        self.read.insert(file.to_owned(), read);
        entries
    }

    /// `state/<repo>/history.jsonl` for every repo but this one.
    fn other_files(&self) -> Vec<PathBuf> {
        let Some(file) = &self.file else {
            return Vec::new();
        };
        let Some(own) = file.parent() else {
            return Vec::new();
        };
        let Some(Ok(dirs)) = own.parent().map(fs::read_dir) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = dirs
            .flatten()
            .map(|dir| dir.path())
            .filter(|dir| dir != own)
            .map(|dir| dir.join(FILE_NAME))
            .filter(|file| file.is_file())
            .collect();
        files.sort();
        files
    }

    /// An older copy of the same prompt moves to the newest place.
    fn remember(&mut self, entry: PromptEntry) {
        if entry.prompt.trim().is_empty() {
            return;
        }
        if let Some(at) = self
            .entries
            .iter()
            .position(|old| old.prompt == entry.prompt)
        {
            self.entries.remove(at);
        }
        self.entries.push(entry);
        if self.entries.len() > HISTORY_LIMIT {
            let extra = self.entries.len() - HISTORY_LIMIT;
            self.entries.drain(..extra);
        }
    }
}

/// The file's entries, each prompt once where it last came and only the newest
/// [`HISTORY_LIMIT`], and how many lines it has.
fn parse(text: &str) -> (Vec<PromptEntry>, usize) {
    let mut entries = Vec::new();
    let mut lines = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        lines += 1;
        match serde_json::from_str::<Line>(line) {
            Ok(Line::Entry(entry)) => entries.push(entry),
            Ok(Line::Bare(text)) => entries.push(PromptEntry::new(text, None)),
            Err(_) => {}
        }
    }
    (newest_once(entries, HISTORY_LIMIT), lines)
}

/// Oldest first and each prompt once at its newest. Untimed entries count as the oldest, and
/// the sort is stable, so each list keeps its own order.
fn merged(mut all: Vec<PromptEntry>, limit: usize) -> Vec<PromptEntry> {
    all.sort_by_key(|entry| entry.at.unwrap_or(0));
    newest_once(all, limit)
}

/// Each prompt once, in the place it last came, and only the last `limit` of them.
fn newest_once(entries: Vec<PromptEntry>, limit: usize) -> Vec<PromptEntry> {
    let mut keep = vec![false; entries.len()];
    let mut seen = HashSet::new();
    let mut kept = 0;
    for (at, entry) in entries.iter().enumerate().rev() {
        if kept == limit {
            break;
        }
        if !entry.prompt.trim().is_empty() && seen.insert(entry.prompt.as_str()) {
            keep[at] = true;
            kept += 1;
        }
    }
    entries
        .into_iter()
        .zip(keep)
        .filter_map(|(entry, keep)| keep.then_some(entry))
        .collect()
}

/// Rewrites the file with only `entries`, unless another window appended to it since it was
/// read, `read_len` bytes long.
fn compact(file: &Path, read_len: usize, entries: &[PromptEntry]) -> Result<(), HistoryError> {
    let mut text = String::new();
    for entry in entries {
        text.push_str(&serde_json::to_string(entry)?);
        text.push('\n');
    }
    let spare = file.with_extension(format!("jsonl.{}", std::process::id()));
    fs::write(&spare, text)?;
    let unchanged = fs::metadata(file).is_ok_and(|meta| meta.len() == read_len as u64);
    if unchanged && fs::rename(&spare, file).is_ok() {
        return Ok(());
    }
    fs::remove_file(&spare)?;
    Ok(())
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("prompt history: {0}")]
    Io(#[from] std::io::Error),
    #[error("prompt history: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(entries: &[PromptEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.prompt.as_str()).collect()
    }

    #[test]
    fn history_file_appends_and_collapses_repeats() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state").join("history.jsonl");
        let mut history = PromptHistory::load(&file).unwrap();
        assert!(history.entries.is_empty());

        history.append_at("fix the tests", 100).unwrap();
        history.append_at("fix the tests", 101).unwrap();
        history.append_at("two\nlines", 200).unwrap();
        history.append_at("  ", 250).unwrap();
        history.append_at("fix the tests", 300).unwrap();
        assert_eq!(texts(&history.entries), ["two\nlines", "fix the tests"]);
        assert_eq!(history.entries[1].at, Some(300));

        let text = fs::read_to_string(&file).unwrap();
        assert_eq!(
            text,
            concat!(
                "{\"prompt\":\"fix the tests\",\"at\":100}\n",
                "{\"prompt\":\"two\\nlines\",\"at\":200}\n",
                "{\"prompt\":\"fix the tests\",\"at\":300}\n",
            )
        );

        fs::write(&file, format!("{text}not json\n\"last\"\n")).unwrap();
        let again = PromptHistory::load(&file).unwrap();
        assert_eq!(
            texts(&again.entries),
            ["two\nlines", "fix the tests", "last"]
        );
        assert_eq!(
            again.entries[2].at, None,
            "a line from before times has none"
        );
        assert_eq!(again.file.as_deref(), Some(file.as_path()));
    }

    #[test]
    fn memory_only_history_writes_nothing() {
        let mut history = PromptHistory::in_memory();
        history.append("hello").unwrap();
        assert_eq!(history.texts().collect::<Vec<_>>(), ["hello"]);
        assert!(history.entries[0].at.is_some());
    }

    #[test]
    fn an_unreadable_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let error = PromptHistory::load(dir.path()).unwrap_err();
        assert!(error.to_string().starts_with("prompt history"), "{error}");
    }

    #[test]
    fn everywhere_merges_every_repo_by_time() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let write = |repo: &str, lines: &str| {
            fs::create_dir_all(state.join(repo)).unwrap();
            fs::write(state.join(repo).join("history.jsonl"), lines).unwrap();
        };
        write(
            "web-111",
            "\"untimed\"\n{\"prompt\":\"deploy\",\"at\":150}\n{\"prompt\":\"shared\",\"at\":50}\n",
        );
        write("api-222", "{\"prompt\":\"migrate\",\"at\":250}\n");
        // A folder without history, and a file where a folder would be, are both skipped.
        fs::create_dir_all(state.join("empty-333")).unwrap();
        fs::write(state.join("stray"), "x").unwrap();

        let mut here = PromptHistory::load(&state.join("here-444").join("history.jsonl")).unwrap();
        here.append_at("fix lint", 100).unwrap();
        here.append_at("shared", 300).unwrap();

        let all = here.everywhere();
        assert_eq!(
            texts(&all),
            ["untimed", "fix lint", "deploy", "migrate", "shared"]
        );
        assert_eq!(
            all.last().unwrap().at,
            Some(300),
            "a repeat keeps its newest time"
        );
        assert_eq!(texts(&here.entries), ["fix lint", "shared"]);
    }

    #[test]
    fn a_prompt_from_another_window_in_this_repo_shows_in_both_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let file = state.join("here-444").join("history.jsonl");
        let mut here = PromptHistory::load(&file).unwrap();
        here.append_at("fix lint", 100).unwrap();
        assert_eq!(texts(&here.this_project()), ["fix lint"]);

        // The other window appends to the same file after this one started.
        let mut out = OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(out, "{{\"prompt\":\"from the other window\",\"at\":200}}").unwrap();
        drop(out);

        assert_eq!(
            texts(&here.this_project()),
            ["fix lint", "from the other window"]
        );
        assert_eq!(
            texts(&here.everywhere()),
            ["fix lint", "from the other window"]
        );
        // This window's own list is what it sent.
        assert_eq!(texts(&here.entries), ["fix lint"]);

        // A prompt this window sends again moves to the newest place.
        here.append_at("from the other window", 300).unwrap();
        here.append_at("fix lint", 400).unwrap();
        assert_eq!(
            texts(&here.this_project()),
            ["from the other window", "fix lint"]
        );
    }

    #[test]
    fn a_file_read_once_is_read_again_only_when_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        fs::create_dir_all(state.join("web-111")).unwrap();
        let other = state.join("web-111").join("history.jsonl");
        fs::write(&other, "{\"prompt\":\"deploy\",\"at\":150}\n").unwrap();
        let mut here = PromptHistory::load(&state.join("here-444").join("history.jsonl")).unwrap();
        assert_eq!(texts(&here.everywhere()), ["deploy"]);
        assert_eq!(here.read[&other].entries.len(), 1);

        fs::write(
            &other,
            "{\"prompt\":\"deploy\",\"at\":150}\n{\"prompt\":\"roll back\",\"at\":160}\n",
        )
        .unwrap();
        assert_eq!(texts(&here.everywhere()), ["deploy", "roll back"]);

        fs::remove_file(&other).unwrap();
        assert!(here.everywhere().is_empty());
        assert!(!here.read.contains_key(&other));
    }

    #[test]
    fn a_long_file_keeps_the_newest_of_each_and_is_compacted_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("history.jsonl");
        let mut text = String::new();
        for n in 0..50_000u64 {
            let line = PromptEntry::new(format!("prompt {}", n % 1_500), Some(n));
            text.push_str(&serde_json::to_string(&line).unwrap());
            text.push('\n');
        }
        fs::write(&file, &text).unwrap();

        let history = PromptHistory::load(&file).unwrap();
        assert_eq!(history.entries.len(), HISTORY_LIMIT);
        assert_eq!(history.entries[0].prompt, "prompt 1000");
        assert_eq!(history.entries[0].at, Some(49_000));
        assert_eq!(history.entries.last().unwrap().prompt, "prompt 499");

        let rewritten = fs::read_to_string(&file).unwrap();
        assert_eq!(rewritten.lines().count(), HISTORY_LIMIT);
        assert_eq!(PromptHistory::load(&file).unwrap().entries, history.entries);
        let spares = fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(spares, 1, "only history.jsonl is left");
    }

    #[test]
    fn a_file_another_window_appended_to_is_left_whole() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("history.jsonl");
        fs::write(&file, "\"one\"\n").unwrap();
        compact(&file, 1, &[PromptEntry::new("kept", Some(1))]).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "\"one\"\n");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn everywhere_without_a_file_is_this_repo() {
        let mut history = PromptHistory::in_memory();
        history.append_at("only here", 5).unwrap();
        assert_eq!(texts(&history.everywhere()), ["only here"]);
    }
}
