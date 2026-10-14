//! Claude Code's session files: `projects/<folder>/<session>.jsonl` under its folder, with each
//! subagent's under its session's `subagents` folder, its workflows' agents a few folders deeper
//! (`subagents/workflows/<run>/agent-<id>.jsonl`). Every assistant line carries the answer's
//! `message.usage` (input, output, cache read and cache write tokens), its `message.model`, a
//! `timestamp` and the `cwd`.
//!
//! One answer is written over several lines as it streams, each repeating its usage with the
//! output counted so far, so an answer counts once, by its `message.id`, with the largest
//! counts any of its lines gave. Lines a branch copied from another session say where from
//! (`forkedFrom`) and are left out, and an answer whose id was already counted in another file
//! is a copy too. As Claude Code's own stats do, a main file's side-chain lines are left to
//! the subagents' files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::IgnoredAny;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::files::walk;
use super::seen::Seen;
use super::{Answer, count};
use crate::overview::records::FileRead;

/// The model Claude Code names for lines it wrote itself rather than a model's answer.
const NOT_A_MODEL: &str = "<synthetic>";

/// How deep under `projects` a session file can sit: a workflow's agent is five folders down.
const DEPTH: usize = 8;

/// Answers of a file remembered for its next pass, in case one was still being written.
const KEEP_OPEN: usize = 4;

/// Every session file, main and subagent.
pub(super) fn session_files(dir: &Path) -> Vec<PathBuf> {
    let projects = dir.join("projects");
    walk(&projects, DEPTH, &|path| {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        if !name.ends_with(".jsonl") {
            return false;
        }
        let depth = path
            .strip_prefix(&projects)
            .map(|rest| rest.components().count())
            .unwrap_or(0);
        depth == 2 || (name.starts_with("agent-") && is_subagent(path))
    })
}

/// A subagent's file sits somewhere under a `subagents` folder of its session.
pub(super) fn is_subagent(path: &Path) -> bool {
    subagents_folder(path).is_some()
}

/// For a subagent's file, its session's own file: `<folder>/<session>.jsonl`.
pub(super) fn parent_session(path: &Path) -> Option<PathBuf> {
    let session_dir = subagents_folder(path)?.parent()?;
    let name = session_dir.file_name()?.to_str()?;
    Some(session_dir.with_file_name(format!("{name}.jsonl")))
}

fn subagents_folder(path: &Path) -> Option<&Path> {
    path.ancestors()
        .skip(1)
        .find(|dir| dir.file_name().is_some_and(|name| name == "subagents"))
}

/// Only the fields the import keeps; everything else in a line is skipped as it's read.
#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "isSidechain")]
    sidechain: Option<bool>,
    #[serde(rename = "forkedFrom")]
    forked_from: Option<IgnoredAny>,
    #[serde(rename = "requestId")]
    request: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

/// A count that is missing or `null` is zero; the line still counts.
#[derive(Deserialize)]
struct Usage {
    #[serde(default, deserialize_with = "count")]
    input_tokens: u64,
    #[serde(default, deserialize_with = "count")]
    output_tokens: u64,
    #[serde(default, deserialize_with = "count")]
    cache_read_input_tokens: u64,
    #[serde(default, deserialize_with = "count")]
    cache_creation_input_tokens: u64,
}

impl Usage {
    fn counts(&self) -> [u64; 4] {
        [
            self.input_tokens,
            self.output_tokens,
            self.cache_read_input_tokens,
            self.cache_creation_input_tokens,
        ]
    }
}

/// An answer met in this pass: the largest counts its lines gave, and what an earlier pass
/// already counted of it.
struct Open {
    id: Option<String>,
    answer: Answer,
    counts: [u64; 4],
    counted: [u64; 4],
}

/// The answers in `lines`, in order, each once. `file` carries the folder and the answers
/// still open across passes, `seen` the answers counted in any file, and `unreadable` gains
/// each line that couldn't be read.
pub(super) fn answers(
    lines: &[String],
    file: &mut FileRead,
    subagent: bool,
    seen: &mut Seen,
    unreadable: &mut u32,
) -> Vec<Answer> {
    let mut open: Vec<Open> = Vec::new();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    let mut last_at: Option<OffsetDateTime> = None;
    for text in lines {
        let Ok(line) = serde_json::from_str::<Line>(text) else {
            *unreadable += 1;
            continue;
        };
        if let Some(cwd) = line.cwd.filter(|cwd| !cwd.is_empty()) {
            file.folder = cwd;
        }
        let at = line
            .timestamp
            .and_then(|at| OffsetDateTime::parse(&at, &Rfc3339).ok());
        if at.is_some() {
            last_at = at;
        }
        let sidechain = line.sidechain.unwrap_or(false);
        if line.kind.as_deref() != Some("assistant")
            || (sidechain && !subagent)
            || line.forked_from.is_some()
        {
            continue;
        }
        let Some(message) = line.message else {
            continue;
        };
        let Some(usage) = message.usage else {
            continue;
        };
        let model = message.model.unwrap_or_default();
        if model == NOT_A_MODEL {
            continue;
        }
        let counts = usage.counts();
        let id = message.id.or(line.request);
        if let Some(&index) = id.as_ref().and_then(|id| by_id.get(id)) {
            let answer = &mut open[index];
            answer.counts = larger(answer.counts, counts);
            continue;
        }
        let Some(at) = at.or(last_at) else {
            *unreadable += 1;
            continue;
        };
        let counted = match &id {
            Some(id) => match file.open.iter().find(|(open, _)| open == id) {
                Some((_, counted)) => *counted,
                None if seen.contains(id) => continue,
                None => {
                    seen.insert(id);
                    [0; 4]
                }
            },
            None => [0; 4],
        };
        if let Some(id) = &id {
            by_id.insert(id.clone(), open.len());
        }
        file.model.clone_from(&model);
        open.push(Open {
            id,
            answer: Answer {
                at,
                model,
                folder: file.folder.clone(),
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                subagent,
            },
            counts,
            counted,
        });
    }
    remember_open(file, &open);
    open.into_iter()
        .map(|open| {
            let counts = larger(open.counts, open.counted);
            let new = [0, 1, 2, 3].map(|k| counts[k] - open.counted[k]);
            Answer {
                input: new[0],
                output: new[1],
                cache_read: new[2],
                cache_write: new[3],
                ..open.answer
            }
        })
        .collect()
}

/// The last few answers of the file with what's counted of them, so a line of one of them
/// that a later pass reads adds only what grew.
fn remember_open(file: &mut FileRead, open: &[Open]) {
    for answer in open {
        let Some(id) = &answer.id else {
            continue;
        };
        let counts = larger(answer.counts, answer.counted);
        file.open.retain(|(open, _)| open != id);
        file.open.push((id.clone(), counts));
    }
    let extra = file.open.len().saturating_sub(KEEP_OPEN);
    file.open.drain(..extra);
}

fn larger(a: [u64; 4], b: [u64; 4]) -> [u64; 4] {
    [0, 1, 2, 3].map(|k| a[k].max(b[k]))
}
