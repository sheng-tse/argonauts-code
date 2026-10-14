//! Codex's session logs: `sessions/<yyyy>/<mm>/<dd>/rollout-<time>-<id>.jsonl` under its
//! folder, moved to `archived_sessions/` when archived, and compressed to `.jsonl.zst` by newer
//! Codex once finished. A rollout is known by its id wherever it is and however it's kept, so
//! one that moved or was compressed after a pass carries on where that pass left it.
//!
//! Each line is `{"timestamp", "type", "payload"}`, as its open source rollout format has it:
//! `session_meta` names the folder (and a subagent's source, and the session it was forked
//! from), `turn_context` the model and folder of the turns after it, and an `event_msg` of type
//! `token_count` the session's running `total_token_usage` with the last request's
//! `last_token_usage`. An answer's tokens are the growth of that total. A forked session's
//! total starts from the other session's, so its first count adds only the last request.
//! Codex counts cached input inside its input, so the cached part is taken out.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::files::{is_compressed, walk};
use super::{Answer, count};
use crate::overview::records::FileRead;

/// Every rollout by its id, plain or compressed. Where one rollout is kept twice, as while
/// Codex compresses it or opens it again to add to it, the copy changed last is read.
pub(super) fn rollouts(dir: &Path) -> Vec<(String, PathBuf)> {
    let rollout = |path: &Path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with("rollout-")
                    && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zst"))
            })
    };
    let mut files = walk(&dir.join("sessions"), 3, &rollout);
    files.extend(walk(&dir.join("archived_sessions"), 3, &rollout));
    let mut by_id: BTreeMap<String, (PathBuf, SystemTime)> = BTreeMap::new();
    for path in files {
        let Some(id) = rollout_id(&path) else {
            continue;
        };
        let changed = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let newer = by_id.get(&id).is_none_or(|(kept, kept_changed)| {
            changed > *kept_changed || (changed == *kept_changed && is_compressed(kept))
        });
        if newer {
            by_id.insert(id, (path, changed));
        }
    }
    by_id
        .into_iter()
        .map(|(id, (path, _))| (id, path))
        .collect()
}

/// The id at the end of a rollout's name: `rollout-2026-02-03T10-00-00-<id>.jsonl`.
fn rollout_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name
        .strip_suffix(".jsonl.zst")
        .or_else(|| name.strip_suffix(".jsonl"))?;
    let rest = stem.strip_prefix("rollout-")?;
    // The time is `yyyy-mm-ddThh-mm-ss`, then a dash.
    let id = rest.get(20..).filter(|id| !id.is_empty()).unwrap_or(rest);
    Some(id.to_owned())
}

#[derive(Deserialize)]
struct Line {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<Payload>,
}

/// The payload's fields the import reads, whichever kind of line it is.
#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "type")]
    kind: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    source: Option<Value>,
    forked_from_id: Option<Value>,
    info: Option<Info>,
}

#[derive(Deserialize)]
struct Info {
    total_token_usage: Option<TokenUsage>,
    last_token_usage: Option<TokenUsage>,
}

/// A count that is missing or `null` is zero; the line still counts.
#[derive(Deserialize, Clone, Copy)]
struct TokenUsage {
    #[serde(default, deserialize_with = "count")]
    input_tokens: u64,
    #[serde(default, deserialize_with = "count")]
    cached_input_tokens: u64,
    #[serde(default, deserialize_with = "count")]
    output_tokens: u64,
}

impl TokenUsage {
    fn as_totals(self) -> [u64; 4] {
        [
            self.input_tokens,
            self.cached_input_tokens,
            self.output_tokens,
            0,
        ]
    }
}

/// The answers in `lines`, in order. `file` carries the folder, the model, whether it's a
/// subagent's or a forked session and the running total across passes, and `unreadable` gains
/// each line that couldn't be read.
pub(super) fn answers(lines: &[String], file: &mut FileRead, unreadable: &mut u32) -> Vec<Answer> {
    let mut out = Vec::new();
    for text in lines {
        let Ok(line) = serde_json::from_str::<Line>(text) else {
            *unreadable += 1;
            continue;
        };
        let Some(payload) = line.payload else {
            continue;
        };
        match line.kind.as_deref() {
            Some("session_meta") => {
                if let Some(cwd) = payload.cwd {
                    file.folder = cwd;
                }
                file.subagent = payload.source.as_ref().is_some_and(names_subagent);
                file.forked = payload
                    .forked_from_id
                    .as_ref()
                    .is_some_and(|id| !id.is_null());
            }
            Some("turn_context") => {
                if let Some(cwd) = payload.cwd {
                    file.folder = cwd;
                }
                if let Some(model) = payload.model {
                    file.model = model;
                }
            }
            Some("event_msg") if payload.kind.as_deref() == Some("token_count") => {
                let Some(info) = payload.info else {
                    continue;
                };
                let Some(total) = info.total_token_usage else {
                    continue;
                };
                let now = total.as_totals();
                let before = match file.totals {
                    Some(before) => before,
                    // What the forked session started from isn't this one's work.
                    None if file.forked => {
                        let last = info.last_token_usage.map(TokenUsage::as_totals);
                        let last = last.unwrap_or_default();
                        [0, 1, 2, 3].map(|k| now[k].saturating_sub(last[k]))
                    }
                    None => [0; 4],
                };
                // A total that fell means the count started again, so all of it is new.
                let grew = now.iter().zip(before).all(|(now, before)| *now >= before);
                let new = if grew {
                    [0, 1, 2].map(|k| now[k] - before[k])
                } else {
                    [now[0], now[1], now[2]]
                };
                file.totals = Some(now);
                if new.iter().all(|n| *n == 0) {
                    continue;
                }
                let Some(at) = line
                    .timestamp
                    .and_then(|at| OffsetDateTime::parse(&at, &Rfc3339).ok())
                else {
                    continue;
                };
                let [input, cached, output] = new;
                out.push(Answer {
                    at,
                    model: file.model.clone(),
                    folder: file.folder.clone(),
                    input: input.saturating_sub(cached),
                    output,
                    cache_read: cached.min(input),
                    cache_write: 0,
                    subagent: file.subagent,
                });
            }
            _ => {}
        }
    }
    out
}

/// A session's source names a subagent, as `"subagent"` or `{"subagent": …}`.
fn names_subagent(source: &Value) -> bool {
    match source {
        Value::String(source) => source.contains("subagent"),
        Value::Object(fields) => fields.keys().any(|key| key.contains("subagent")),
        _ => false,
    }
}
