//! Claude Code's session files: `projects/<folder>/<session>.jsonl` under its folder, with
//! each subagent's in `projects/<folder>/<session>/subagents/agent-<id>.jsonl`. Every
//! assistant line carries the answer's `message.usage` (input, output, cache read and cache
//! write tokens), its `message.model`, a `timestamp` and the `cwd`. One answer can be written
//! over several lines that repeat its usage, so each `message.id` counts once. As Claude
//! Code's own stats do, a main file's side-chain lines are left to the subagents' files.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::Answer;
use super::files::walk;
use crate::overview::records::FileRead;

/// The model Claude Code names for lines it wrote itself rather than a model's answer.
const NOT_A_MODEL: &str = "<synthetic>";

/// Every session file, main and subagent.
pub(super) fn session_files(dir: &Path) -> Vec<PathBuf> {
    walk(&dir.join("projects"), 3, &|path| {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        if !name.ends_with(".jsonl") {
            return false;
        }
        // Either straight in a project's folder, or a subagent's under its session.
        let depth = path
            .strip_prefix(dir.join("projects"))
            .map(|rest| rest.components().count())
            .unwrap_or(0);
        depth == 2 || (is_subagent(path) && name.starts_with("agent-"))
    })
}

/// A subagent's file sits in a `subagents` folder under its session.
pub(super) fn is_subagent(path: &Path) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "subagents")
}

/// For a subagent's file, its session's own file: `<folder>/<session>.jsonl`.
pub(super) fn parent_session(path: &Path) -> Option<PathBuf> {
    if !is_subagent(path) {
        return None;
    }
    let session_dir = path.parent()?.parent()?;
    let name = session_dir.file_name()?.to_str()?;
    Some(session_dir.with_file_name(format!("{name}.jsonl")))
}

/// Only the fields the import keeps; everything else in a line is skipped as it's read.
#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "isSidechain", default)]
    sidechain: bool,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

/// The answers in `lines`, in order. `file` carries the folder and the last answer across
/// passes.
pub(super) fn answers(lines: &[String], file: &mut FileRead, subagent: bool) -> Vec<Answer> {
    let mut out = Vec::new();
    for text in lines {
        let Ok(line) = serde_json::from_str::<Line>(text) else {
            continue;
        };
        if let Some(cwd) = line.cwd.filter(|cwd| !cwd.is_empty()) {
            file.folder = cwd;
        }
        if line.kind.as_deref() != Some("assistant") || (line.sidechain && !subagent) {
            continue;
        }
        let Some(message) = line.message else {
            continue;
        };
        let (Some(usage), Some(model)) = (message.usage, message.model) else {
            continue;
        };
        if model == NOT_A_MODEL {
            continue;
        }
        if let Some(id) = message.id {
            if file.last_answer.as_deref() == Some(id.as_str()) {
                continue;
            }
            file.last_answer = Some(id);
        }
        let Some(at) = line
            .timestamp
            .and_then(|at| OffsetDateTime::parse(&at, &Rfc3339).ok())
        else {
            continue;
        };
        file.model.clone_from(&model);
        out.push(Answer {
            at,
            model,
            folder: file.folder.clone(),
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_read: usage.cache_read_input_tokens,
            cache_write: usage.cache_creation_input_tokens,
            subagent,
        });
    }
    out
}
