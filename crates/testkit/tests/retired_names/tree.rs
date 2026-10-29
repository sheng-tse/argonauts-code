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

/// What git tracks. Outside a git checkout, as in a copy of the source, every file the ignore
/// files leave in.
fn tracked(root: &Path) -> Vec<String> {
    listed_by_git(root, &["ls-files", "-z"]).unwrap_or_else(|| walked(root))
}

/// The files git lists under `root` with `args`, those still on disk, or `None` outside a git
/// checkout.
pub fn listed_by_git(root: &Path, args: &[&str]) -> Option<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    let listed = String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|path| !path.is_empty() && root.join(path).is_file())
        .map(str::to_owned)
        .collect();
    Some(listed)
}

/// Every file under `root` that no `.gitignore` on its way names, sorted: what git would list,
/// tracked or not, in a checkout of the same files.
pub fn walked(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    walk(root, "", &mut Vec::new(), &mut found);
    found.retain(|path| root.join(path).is_file());
    found.sort();
    found
}

/// Walks `dir`, `from_top` below the workspace's top, with the ignore lines read on the way.
fn walk(dir: &Path, from_top: &str, ignores: &mut Vec<Ignore>, found: &mut Vec<String>) {
    let own = std::fs::read_to_string(dir.join(".gitignore"))
        .map(|text| Ignore::read(from_top, &text))
        .unwrap_or_default();
    let before = ignores.len();
    ignores.extend(own);
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // A link to a folder is listed as itself, as git keeps it.
            let is_folder = entry.file_type().is_ok_and(|kind| kind.is_dir());
            let path = format!("{from_top}{name}");
            if name == ".git" || ignores.iter().any(|ignore| ignore.skips(&path, is_folder)) {
                continue;
            }
            match is_folder {
                true => walk(&entry.path(), &format!("{path}/"), ignores, found),
                false => found.push(path),
            }
        }
    }
    ignores.truncate(before);
}

/// One line of a `.gitignore`, in the forms the workspace's own use: a name or a pattern with
/// `*` matched against every name below the file's folder, or against the path from that
/// folder when it holds a slash; a folder only when it ends in one.
struct Ignore {
    /// The ignore file's folder, from the workspace's top: empty, or ending in a slash.
    base: String,
    pattern: String,
    anchored: bool,
    folders_only: bool,
}

impl Ignore {
    fn read(base: &str, text: &str) -> Vec<Self> {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| {
                let folders_only = line.ends_with('/');
                let line = line.trim_end_matches('/');
                Self {
                    base: base.to_owned(),
                    anchored: line.contains('/'),
                    pattern: line.trim_start_matches('/').to_owned(),
                    folders_only,
                }
            })
            .collect()
    }

    fn skips(&self, path: &str, is_folder: bool) -> bool {
        let Some(below) = path.strip_prefix(&self.base) else {
            return false;
        };
        if self.folders_only && !is_folder {
            return false;
        }
        let name = below.rsplit('/').next().unwrap_or(below);
        table::fits(&self.pattern, if self.anchored { below } else { name })
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
