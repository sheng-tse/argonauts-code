//! `trusted.json`: the project settings files you trusted, by the hash of what they held then.
//! Until a file's current contents are trusted, its safety keys don't apply.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::layout::repo_state;
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::sha256_hex;
use argonauts_registry::SettingsError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct TrustFile {
    /// The file's path, and the SHA-256 of the contents you trusted.
    files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrustStore {
    path: PathBuf,
    files: BTreeMap<String, String>,
}

impl TrustStore {
    /// An unreadable or missing file trusts nothing.
    pub(crate) fn load(state_dir: &Path) -> Self {
        let path = state_dir.join(repo_state::TRUSTED);
        let files = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<TrustFile>(&text).ok())
            .map(|file| file.files)
            .unwrap_or_default();
        Self { path, files }
    }

    /// A file that doesn't exist sets nothing, so there's nothing to distrust.
    pub(crate) fn is_trusted(&self, file: &Path) -> bool {
        match hash_of(file) {
            None => true,
            Some(hash) => self.files.get(&key(file)) == Some(&hash),
        }
    }

    /// Whether the file exists with contents you trusted before. Unlike [`Self::is_trusted`],
    /// false for a missing file.
    pub(crate) fn was_trusted(&self, file: &Path) -> bool {
        hash_of(file).is_some_and(|hash| self.files.get(&key(file)) == Some(&hash))
    }

    /// Trusts the file as it is now and saves.
    pub(crate) fn trust(&mut self, file: &Path) -> anyhow::Result<()> {
        match hash_of(file) {
            Some(hash) => {
                self.files.insert(key(file), hash);
            }
            None => {
                self.files.remove(&key(file));
            }
        }
        self.save()
    }

    /// The hash recorded for the file, trusted or since changed.
    pub(crate) fn recorded(&self, file: &Path) -> Option<String> {
        self.files.get(&key(file)).cloned()
    }

    /// Puts back what [`Self::recorded`] returned, such as after a write that had to be undone.
    pub(crate) fn put_back(&mut self, file: &Path, recorded: Option<String>) -> anyhow::Result<()> {
        match recorded {
            Some(hash) => self.files.insert(key(file), hash),
            None => self.files.remove(&key(file)),
        };
        self.save()
    }

    fn save(&self) -> anyhow::Result<()> {
        let file = TrustFile {
            files: self.files.clone(),
        };
        let mut text = serde_json::to_vec_pretty(&file)?;
        text.push(b'\n');
        if let Some(dir) = self.path.parent() {
            create_private_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        write_private_file(&self.path, &text)
            .with_context(|| format!("writing {}", self.path.display()))
    }
}

/// Why a file can't be trusted: its safety settings, once they apply, don't resolve. Said in
/// place of the trust question, and never alongside "stays untrusted".
pub(crate) fn cant_trust(shown: &str, err: &SettingsError) -> String {
    match err {
        SettingsError::Invalid { layer, message } if layer == shown => {
            format!("{shown} needs a fix before you can trust it: {message}")
        }
        SettingsError::Invalid { layer, message } => {
            format!("{shown} can't be trusted until {layer} is fixed: {message}")
        }
        other => format!("{shown} can't be trusted: {other}"),
    }
}

/// `None` when the file can't be read.
pub(crate) fn hash_of(file: &Path) -> Option<String> {
    std::fs::read(file).ok().map(|bytes| sha256_hex(&bytes))
}

fn key(file: &Path) -> String {
    file.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_to_a_trusted_file_needs_trust_again() {
        let dir = tempfile::tempdir().unwrap();
        // The repo's state directory may not exist yet.
        let state = &dir.path().join("state");
        let repo = tempfile::tempdir().unwrap();
        let config = repo.path().join("config.toml");
        let mut store = TrustStore::load(state);
        assert!(store.is_trusted(&config));
        std::fs::write(&config, "[sandbox]\nnetwork = true\n").unwrap();
        assert!(!store.is_trusted(&config));
        store.trust(&config).unwrap();
        assert!(TrustStore::load(state).is_trusted(&config));
        std::fs::write(&config, "[sandbox]\nmode = \"off\"\n").unwrap();
        assert!(!TrustStore::load(state).is_trusted(&config));
    }

    #[test]
    fn a_recorded_hash_can_be_put_back() {
        let state = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let config = repo.path().join("config.toml");
        std::fs::write(&config, "[ui]\ntheme = \"dark\"\n").unwrap();
        let mut store = TrustStore::load(state.path());
        let before = store.recorded(&config);
        store.trust(&config).unwrap();
        assert!(store.was_trusted(&config));
        store.put_back(&config, before).unwrap();
        assert!(!TrustStore::load(state.path()).was_trusted(&config));
        assert_eq!(TrustStore::load(state.path()).recorded(&config), None);
    }
}
