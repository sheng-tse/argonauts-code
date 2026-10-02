//! Your prompts in this repo, `history.jsonl` in its state directory: read at start, one line
//! appended per submitted prompt, repeats collapsed.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

/// Entries kept in memory. The file itself only grows by appends.
pub const HISTORY_LIMIT: usize = 1000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptHistory {
    /// `None` keeps history in memory only.
    pub file: Option<PathBuf>,
    /// Oldest first, each prompt once.
    pub entries: Vec<String>,
}

impl PromptHistory {
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// A missing file is an empty history. A line that isn't a JSON string is skipped, so one
    /// torn write never costs the rest.
    pub fn load(file: &Path) -> Result<Self, HistoryError> {
        let mut history = Self {
            file: Some(file.to_owned()),
            entries: Vec::new(),
        };
        let text = match fs::read_to_string(file) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(history),
            Err(error) => return Err(error.into()),
        };
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            if let Ok(prompt) = serde_json::from_str::<String>(line) {
                history.remember(prompt);
            }
        }
        Ok(history)
    }

    /// Remembers the prompt and appends it to the file, unless it repeats the newest entry.
    pub fn append(&mut self, prompt: &str) -> Result<(), HistoryError> {
        if prompt.trim().is_empty() || self.entries.last().map(String::as_str) == Some(prompt) {
            return Ok(());
        }
        self.remember(prompt.to_owned());
        let Some(file) = &self.file else {
            return Ok(());
        };
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut line = serde_json::to_string(prompt)?;
        line.push('\n');
        let mut out = OpenOptions::new().create(true).append(true).open(file)?;
        out.write_all(line.as_bytes())?;
        Ok(())
    }

    /// An older copy of the same prompt moves to the newest place.
    fn remember(&mut self, prompt: String) {
        if prompt.trim().is_empty() {
            return;
        }
        if let Some(at) = self.entries.iter().position(|entry| *entry == prompt) {
            self.entries.remove(at);
        }
        self.entries.push(prompt);
        if self.entries.len() > HISTORY_LIMIT {
            let extra = self.entries.len() - HISTORY_LIMIT;
            self.entries.drain(..extra);
        }
    }
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

    #[test]
    fn history_file_appends_and_collapses_repeats() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state").join("history.jsonl");
        let mut history = PromptHistory::load(&file).unwrap();
        assert!(history.entries.is_empty());

        history.append("fix the tests").unwrap();
        history.append("fix the tests").unwrap();
        history.append("two\nlines").unwrap();
        history.append("  ").unwrap();
        history.append("fix the tests").unwrap();
        assert_eq!(history.entries, ["two\nlines", "fix the tests"]);

        let text = fs::read_to_string(&file).unwrap();
        assert_eq!(
            text,
            "\"fix the tests\"\n\"two\\nlines\"\n\"fix the tests\"\n"
        );

        fs::write(&file, format!("{text}not json\n\"last\"\n")).unwrap();
        let again = PromptHistory::load(&file).unwrap();
        assert_eq!(again.entries, ["two\nlines", "fix the tests", "last"]);
        assert_eq!(again.file.as_deref(), Some(file.as_path()));
    }

    #[test]
    fn memory_only_history_writes_nothing() {
        let mut history = PromptHistory::in_memory();
        history.append("hello").unwrap();
        assert_eq!(history.entries, ["hello"]);
    }

    #[test]
    fn an_unreadable_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let error = PromptHistory::load(dir.path()).unwrap_err();
        assert!(error.to_string().starts_with("prompt history"), "{error}");
    }
}
