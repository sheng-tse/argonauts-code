//! Codex's session logs: `sessions/<yyyy>/<mm>/<dd>/rollout-<time>-<id>.jsonl` under its
//! folder, older ones in `archived_sessions/`, and newer Codex compresses finished ones to
//! `.jsonl.zst`. Each line is `{"timestamp", "type", "payload"}`, as its open source rollout
//! format has it: `session_meta` names the folder (and a subagent's source), `turn_context`
//! the model and folder of the turns after it, and an `event_msg` of type `token_count` the
//! session's running `total_token_usage`. An answer's tokens are the growth of that total.
//! Codex counts cached input inside its input, so the cached part is taken out.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::Answer;
use super::files::walk;
use crate::overview::records::FileRead;

/// Every session log, plain or compressed.
pub(super) fn session_files(dir: &Path) -> Vec<PathBuf> {
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
    files
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
    info: Option<Info>,
}

#[derive(Deserialize)]
struct Info {
    total_token_usage: Option<TokenUsage>,
}

#[derive(Deserialize, Clone, Copy)]
struct TokenUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
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
/// subagent's session and the running total across passes.
pub(super) fn answers(lines: &[String], file: &mut FileRead) -> Vec<Answer> {
    let mut out = Vec::new();
    for text in lines {
        let Ok(line) = serde_json::from_str::<Line>(text) else {
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
                let Some(total) = payload.info.and_then(|info| info.total_token_usage) else {
                    continue;
                };
                let now = total.as_totals();
                let before = file.totals.unwrap_or_default();
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
