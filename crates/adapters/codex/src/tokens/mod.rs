//! Codex's tokens as its own rollouts hold them, counted by the plainest rules and apart from
//! the Overview's import on purpose, so a rule the import gets wrong shows up as a difference:
//! the growth of each rollout's running total, a forked one's from where it started.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime, UtcOffset};

type Days = BTreeMap<Date, u64>;

/// Every token of the rollouts in Codex's folder `dir`, by UTC day, reading compressed ones
/// through the `zstd` program. A rollout that can't be opened, such as a compressed one without
/// `zstd`, and a folder or folder entry that can't be read each add one to `unopened`; bytes
/// that aren't text spoil only their line.
pub fn by_day(dir: &Path, zstd: Option<&Path>, unopened: &mut u32) -> Days {
    let mut paths = rollouts(&dir.join("sessions"), unopened);
    paths.extend(rollouts(&dir.join("archived_sessions"), unopened));
    // A rollout keeps its name when Codex moves or compresses it; where two copies are kept,
    // the one that grew further counts.
    let mut sessions: BTreeMap<String, Days> = BTreeMap::new();
    for path in paths {
        let Some(text) = open(&path, zstd) else {
            *unopened += 1;
            continue;
        };
        let name = path.file_name().and_then(|name| name.to_str());
        let name = name.unwrap_or("").trim_end_matches(".zst").to_owned();
        let session = growth(&text);
        let more = |kept: &Days| kept.values().sum::<u64>() < session.values().sum::<u64>();
        if sessions.get(&name).is_none_or(more) {
            sessions.insert(name, session);
        }
    }
    let mut days = Days::new();
    for (day, tokens) in sessions.into_values().flatten() {
        *days.entry(day).or_default() += tokens;
    }
    days.retain(|_, tokens| *tokens > 0);
    days
}

/// What a rollout's running total grew by, on the day of each count. Input counts the cached
/// part already. A total that fell started again, so all of it is new.
fn growth(text: &str) -> Days {
    let mut days = Days::new();
    let mut forked = false;
    let mut before: Option<[u64; 2]> = None;
    for line in text.lines() {
        let Ok(line) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let payload = &line["payload"];
        if line["type"] == "session_meta" {
            forked = !payload["forked_from_id"].is_null();
            continue;
        }
        if line["type"] != "event_msg" || payload["type"] != "token_count" {
            continue;
        }
        let info = &payload["info"];
        let total = &info["total_token_usage"];
        if !total.is_object() {
            continue;
        }
        let now = tokens(total);
        let start = before.unwrap_or_else(|| {
            if forked {
                let last = tokens(&info["last_token_usage"]);
                [0, 1].map(|k| now[k].saturating_sub(last[k]))
            } else {
                [0, 0]
            }
        });
        let grown = if now[0] >= start[0] && now[1] >= start[1] {
            (now[0] - start[0]) + (now[1] - start[1])
        } else {
            now[0] + now[1]
        };
        before = Some(now);
        if let Some(day) = utc_day(&line["timestamp"]) {
            *days.entry(day).or_default() += grown;
        }
    }
    days
}

/// Input and output of a usage record, each a whole number or none.
fn tokens(usage: &Value) -> [u64; 2] {
    ["input_tokens", "output_tokens"].map(|key| usage[key].as_u64().unwrap_or(0))
}

/// A plain rollout's text, or a compressed one's through `zstd`.
fn open(path: &Path, zstd: Option<&Path>) -> Option<String> {
    if path.extension().is_none_or(|ext| ext != "zst") {
        let bytes = std::fs::read(path).ok()?;
        return Some(String::from_utf8_lossy(&bytes).into_owned());
    }
    let output = Command::new(zstd?)
        .args(["-dc", "--"])
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Every rollout under `dir`, plain or compressed, in order, without following links. A folder
/// or entry that can't be read adds one to `unopened`; a folder that isn't there adds nothing.
fn rollouts(dir: &Path, unopened: &mut u32) -> Vec<PathBuf> {
    let rollout = |name: &str| {
        name.starts_with("rollout-") && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zst"))
    };
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
            } else if kind.is_file() && entry.file_name().to_str().is_some_and(rollout) {
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

    const ROLLOUT: &str = r#"{"type":"event_msg","timestamp":"2026-10-01T12:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":3,"output_tokens":4}}}}"#;

    #[test]
    fn a_folder_that_cannot_be_listed_counts_as_unopened() {
        let dir = tempfile::tempdir().unwrap();
        let readable = dir.path().join("sessions/2026/10/01");
        let locked = dir.path().join("sessions/2026/10/02");
        for (folder, name) in [(&readable, "rollout-a.jsonl"), (&locked, "rollout-b.jsonl")] {
            std::fs::create_dir_all(folder).unwrap();
            std::fs::write(folder.join(name), ROLLOUT).unwrap();
        }
        std::fs::set_permissions(&locked, Permissions::from_mode(0o000)).unwrap();
        // Root lists any folder, so there the lock proves nothing.
        let listable = std::fs::read_dir(&locked).is_ok();
        let mut unopened = 0;
        let days = by_day(dir.path(), None, &mut unopened);
        std::fs::set_permissions(&locked, Permissions::from_mode(0o755)).unwrap();
        if listable {
            return;
        }
        assert_eq!(unopened, 1);
        assert_eq!(days.values().sum::<u64>(), 7);
    }

    #[test]
    fn a_missing_archive_folder_is_not_unopened() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("sessions");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("rollout-a.jsonl"), ROLLOUT).unwrap();
        let mut unopened = 0;
        assert_eq!(
            by_day(dir.path(), None, &mut unopened)
                .values()
                .sum::<u64>(),
            7
        );
        assert_eq!(unopened, 0);
    }
}
