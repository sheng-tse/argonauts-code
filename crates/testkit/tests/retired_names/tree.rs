//! The tracked files of the workspace, read once for every check.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use super::table::{self, Table};

pub struct File {
    /// From the workspace's top, with forward slashes.
    pub path: String,
    /// `None` for a file that isn't text.
    pub text: Option<String>,
}

pub fn files() -> &'static [File] {
    static FILES: OnceLock<Vec<File>> = OnceLock::new();
    FILES.get_or_init(|| {
        let root = table::workspace();
        tracked(&root)
            .into_iter()
            .map(|path| {
                let text = std::fs::read(root.join(&path))
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok());
                File { path, text }
            })
            .collect()
    })
}

fn paths() -> &'static HashSet<&'static str> {
    static PATHS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    PATHS.get_or_init(|| files().iter().map(|file| file.path.as_str()).collect())
}

/// What git tracks. Outside a git checkout, every file but build output and hidden folders.
fn tracked(root: &Path) -> Vec<String> {
    let listed = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output();
    if let Ok(output) = listed
        && output.status.success()
        && !output.stdout.is_empty()
    {
        return String::from_utf8_lossy(&output.stdout)
            .split('\0')
            .filter(|path| !path.is_empty() && root.join(path).is_file())
            .map(str::to_owned)
            .collect();
    }
    let mut found = Vec::new();
    walk(root, root, &mut found);
    found.sort();
    found
}

fn walk(root: &Path, dir: &Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "target" {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, found);
        } else if let Ok(inside) = path.strip_prefix(root) {
            found.push(inside.to_string_lossy().replace('\\', "/"));
        }
    }
}

impl Table {
    /// A CLI's own output: its words are the CLI's, so only its name is checked.
    pub fn is_recording(&self, path: &str) -> bool {
        let under_fixtures = path.contains("/fixtures/")
            && self
                .recordings
                .endings_under_fixtures
                .iter()
                .any(|ending| path.ends_with(ending.as_str()));
        under_fixtures
            || table::fits_any(&self.recordings.also, path)
            || has_a_meta_beside(path, |meta| paths().contains(meta))
    }

    /// Whether a file's words are read: not a recording, not the table, not a listed file.
    pub fn reads_words_of(&self, path: &str) -> bool {
        path != table::TABLE
            && !self.is_recording(path)
            && !self
                .unread
                .iter()
                .any(|unread| table::fits(&unread.path, path))
    }
}

/// Whether a file under fixtures/ has a meta file beside it: its folder's `meta.json`, or a
/// scenario's, named by the file's name up to a dot or an underscore, as
/// `providers.meta.json` beside `providers_models.stdout.txt`.
pub fn has_a_meta_beside(path: &str, tracked: impl Fn(&str) -> bool) -> bool {
    let Some((folder, name)) = path.rsplit_once('/') else {
        return false;
    };
    if !path.contains("/fixtures/") || name.ends_with("meta.json") {
        return false;
    }
    tracked(&format!("{folder}/meta.json"))
        || name
            .match_indices(['.', '_'])
            .any(|(at, _)| tracked(&format!("{folder}/{}.meta.json", &name[..at])))
}
