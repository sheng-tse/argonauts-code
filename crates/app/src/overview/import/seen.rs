//! The answers already counted, by a hash of each one's id, so an answer Claude Code copied into
//! another file (a branch, a forked session, an agent handed the conversation so far) counts
//! once. Eight bytes an answer, kept in `answers.bin` beside the records.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

pub(super) struct Seen {
    path: PathBuf,
    ids: HashSet<u64>,
    added: Vec<u64>,
}

impl Seen {
    /// What `path` holds, or nothing when it isn't there.
    pub fn load(path: &Path) -> Self {
        let bytes = fs::read(path).unwrap_or_default();
        let ids = bytes
            .chunks_exact(8)
            .map(|chunk| u64::from_le_bytes(chunk.try_into().unwrap_or_default()))
            .collect();
        Self {
            path: path.to_owned(),
            ids,
            added: Vec::new(),
        }
    }

    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(&hash(id))
    }

    pub fn insert(&mut self, id: &str) {
        let id = hash(id);
        if self.ids.insert(id) {
            self.added.push(id);
        }
    }

    /// Adds what was counted since the last save to the file.
    pub fn save(&mut self) -> io::Result<()> {
        if self.added.is_empty() {
            return Ok(());
        }
        let bytes: Vec<u8> = self.added.iter().flat_map(|id| id.to_le_bytes()).collect();
        if let Some(dir) = self.path.parent() {
            argonauts_core::create_private_dir_all(dir)?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)?
            .write_all(&bytes)?;
        self.added.clear();
        Ok(())
    }
}

/// FNV-1a, which stays the same from one build to the next, as a file kept on disk needs.
fn hash(id: &str) -> u64 {
    id.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_was_counted_is_remembered_across_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("answers.bin");
        let mut seen = Seen::load(&path);
        assert!(!seen.contains("msg_1"));
        seen.insert("msg_1");
        seen.insert("msg_1");
        seen.save().unwrap();
        seen.insert("msg_2");
        seen.save().unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), 16, "each id once");
        let again = Seen::load(&path);
        assert!(again.contains("msg_1") && again.contains("msg_2"));
        assert!(!again.contains("msg_3"));
    }
}
