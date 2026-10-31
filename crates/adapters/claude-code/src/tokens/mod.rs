//! Claude Code's tokens as its own files hold them: a count of its session files by the
//! plainest rules, and the totals its /stats keeps in `stats-cache.json`.
//!
//! The count is written apart from the Overview's import on purpose, so a rule the import gets
//! wrong shows up as a difference: every assistant line under `projects/` with usage counts,
//! one answer per message id at the largest counts any of its lines gave.

pub mod stats_cache;

use std::collections::{BTreeMap, HashMap};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime, UtcOffset};

/// The model Claude Code names for lines it wrote itself rather than a model's answer.
const NOT_A_MODEL: &str = "<synthetic>";

/// Every token of the session files in Claude Code's folder `dir`, by UTC day. A file, folder or
/// folder entry that can't be read adds one to `unopened`; bytes that aren't text spoil only
/// their line.
pub fn by_day(dir: &Path, unopened: &mut u32) -> BTreeMap<Date, u64> {
    let mut answers: HashMap<String, (Date, [u64; 4])> = HashMap::new();
    let mut days = BTreeMap::new();
    for path in session_files(&dir.join("projects"), unopened) {
        let Ok(bytes) = std::fs::read(&path) else {
            *unopened += 1;
            continue;
        };
        let mut last_day = None;
        for line in String::from_utf8_lossy(&bytes).lines() {
            let Ok(line) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let day = utc_day(&line["timestamp"]).or(last_day);
            last_day = day;
            let message = &line["message"];
            let usage = &message["usage"];
            if line["type"] != "assistant" || !usage.is_object() || message["model"] == NOT_A_MODEL
            {
                continue;
            }
            let Some(day) = day else {
                continue;
            };
            let counts = [
                "input_tokens",
                "output_tokens",
                "cache_read_input_tokens",
                "cache_creation_input_tokens",
            ]
            .map(|key| usage[key].as_u64().unwrap_or(0));
            match message["id"].as_str().or(line["requestId"].as_str()) {
                Some(id) => {
                    let answer = answers.entry(id.to_owned()).or_insert((day, [0; 4]));
                    answer.1 = [0, 1, 2, 3].map(|k| answer.1[k].max(counts[k]));
                }
                None => *days.entry(day).or_default() += counts.iter().sum::<u64>(),
            }
        }
    }
    for (day, counts) in answers.into_values() {
        *days.entry(day).or_default() += counts.iter().sum::<u64>();
    }
    days.retain(|_, tokens| *tokens > 0);
    days
}

/// Every `.jsonl` file under `dir`, in order, without following links. A folder or entry that
/// can't be read adds one to `unopened`; a folder that isn't there adds nothing.
fn session_files(dir: &Path, unopened: &mut u32) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let entries = match std::fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(_) => {
                *unopened += 1;
                continue;
            }
        };
        for entry in entries {
            let Ok((entry, kind)) =
                entry.and_then(|entry| entry.file_type().map(|kind| (entry, kind)))
            else {
                *unopened += 1;
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                folders.push(path);
            } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "jsonl") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn utc_day(at: &Value) -> Option<Date> {
    let at = OffsetDateTime::parse(at.as_str()?, &Rfc3339).ok()?;
    Some(at.to_offset(UtcOffset::UTC).date())
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn answer(id: &str) -> String {
        let usage = r#"{"input_tokens":3,"output_tokens":4}"#;
        format!(
            r#"{{"type":"assistant","timestamp":"2026-10-01T12:00:00Z","message":{{"id":"{id}","model":"claude-haiku","usage":{usage}}}}}"#
        )
    }

    #[test]
    fn a_folder_that_cannot_be_listed_counts_as_unopened() {
        let dir = tempfile::tempdir().unwrap();
        let readable = dir.path().join("projects/readable");
        let locked = dir.path().join("projects/locked");
        for (folder, id) in [(&readable, "a"), (&locked, "b")] {
            std::fs::create_dir_all(folder).unwrap();
            std::fs::write(folder.join("session.jsonl"), answer(id)).unwrap();
        }
        std::fs::set_permissions(&locked, Permissions::from_mode(0o000)).unwrap();
        // Root lists any folder, so there the lock proves nothing.
        let listable = std::fs::read_dir(&locked).is_ok();
        let mut unopened = 0;
        let days = by_day(dir.path(), &mut unopened);
        std::fs::set_permissions(&locked, Permissions::from_mode(0o755)).unwrap();
        if listable {
            return;
        }
        assert_eq!(unopened, 1);
        assert_eq!(days.values().sum::<u64>(), 7);
    }

    #[test]
    fn a_missing_projects_folder_is_not_unopened() {
        let dir = tempfile::tempdir().unwrap();
        let mut unopened = 0;
        assert!(by_day(dir.path(), &mut unopened).is_empty());
        assert_eq!(unopened, 0);
    }
}
