//! Snapshot files no test draws any more, which a renamed test or snapshot leaves behind.
//!
//! Only a run of every test knows which snapshots are drawn. insta adds the path of each
//! one it compares to the file `INSTA_SNAPSHOT_REFERENCES_FILE` names, so the check reads
//! such a list.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::report::Place;
use super::tree::File;

/// The snapshots among `files` that `drawn` leaves out. `drawn` holds one path on each line.
pub fn undrawn(root: &Path, drawn: &str, files: &[File]) -> Vec<Place> {
    // Both sides are resolved, since the list may reach a file through a link such as `/tmp`.
    let drawn: BTreeSet<PathBuf> = drawn
        .lines()
        .filter_map(|line| std::fs::canonicalize(line.trim()).ok())
        .collect();
    files
        .iter()
        .filter(|file| file.path.ends_with(".snap"))
        .filter(|file| {
            std::fs::canonicalize(root.join(&file.path)).map_or(true, |path| !drawn.contains(&path))
        })
        .map(|file| {
            Place::new(
                &file.path,
                None,
                name(&file.path),
                "a test that draws it, or delete the file",
            )
        })
        .collect()
}

/// A snapshot's own name: its file name without the module path insta puts before it.
fn name(path: &str) -> &str {
    let file = path.rsplit('/').next().unwrap_or(path);
    let file = file.strip_suffix(".snap").unwrap_or(file);
    file.rsplit("__").next().unwrap_or(file)
}
