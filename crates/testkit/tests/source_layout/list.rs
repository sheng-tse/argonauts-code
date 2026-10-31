//! `source-layout.toml`: core's ceiling, the crates allowed loose files, and the files and
//! crates over the limits today, each with its ceiling and planned split.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::sources::workspace;

pub const LIST: &str = "crates/testkit/source-layout.toml";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct List {
    pub core: Core,
    /// Each crate whose `src/` may hold loose files, by its path, with why.
    #[serde(default)]
    pub exceptions: BTreeMap<String, String>,
    #[serde(default, rename = "file")]
    pub files: Vec<Listed>,
    #[serde(default, rename = "crate")]
    pub crates: Vec<Listed>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Core {
    /// In production lines.
    pub ceiling: usize,
}

/// A file or crate over its limit today.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Listed {
    /// From the workspace's top: a file's path, or a crate's as the root manifest names it.
    pub path: String,
    /// Its production lines when it was listed.
    pub ceiling: usize,
    pub split: String,
}

impl List {
    pub fn file(&self, path: &str) -> Option<&Listed> {
        self.files.iter().find(|listed| listed.path == path)
    }

    pub fn crate_at(&self, path: &str) -> Option<&Listed> {
        self.crates.iter().find(|listed| listed.path == path)
    }
}

pub fn load() -> List {
    let path = workspace().join(LIST);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{LIST}: {e}"));
    toml::from_str(&text).unwrap_or_else(|e| panic!("{LIST}: {e}"))
}
