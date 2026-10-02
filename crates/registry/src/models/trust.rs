//! Trust for the project's `models.toml`. Its `[[model]]` entries carry launch hints, prices,
//! access, families, and tiers, and its `[[harness]]` entries name programs to run, so a file a
//! cloned repo carries counts only once you trust it as it is.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use argonauts_core::sha256_hex;

use crate::SettingsError;

/// The project's `models.toml`, read once. The trust decision is made on the SHA-256 of this
/// exact text, and the registry parses the same text, so a file changed after you trusted it
/// waits for you again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectModels {
    path: PathBuf,
    /// `None` when there's no file.
    text: Option<String>,
    trusted: bool,
}

impl ProjectModels {
    /// `trusted_hashes` are the SHA-256 hashes in the repo's `trusted.json`.
    pub fn read(path: &Path, trusted_hashes: &[String]) -> Result<Self, SettingsError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(err) if err.kind() == ErrorKind::NotFound => None,
            Err(err) => {
                return Err(SettingsError::Io {
                    path: path.to_owned(),
                    message: err.to_string(),
                });
            }
        };
        let trusted = text
            .as_deref()
            .is_some_and(|text| trusted_hashes.contains(&sha256_hex(text.as_bytes())));
        Ok(Self {
            path: path.to_owned(),
            text,
            trusted,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The hash `trusted.json` records for this text. `None` when there's no file.
    pub fn sha256(&self) -> Option<String> {
        self.text.as_deref().map(|text| sha256_hex(text.as_bytes()))
    }

    /// Whether the text read is one you trusted. A missing file has nothing to trust.
    pub fn trusted(&self) -> bool {
        self.trusted
    }

    pub(crate) fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }
}
