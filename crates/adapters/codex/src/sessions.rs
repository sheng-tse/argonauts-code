//! Codex's own sessions. It keeps each one as
//! `~/.codex/sessions/<yyyy>/<mm>/<dd>/rollout-<time>-<id>.jsonl`, whose first record names the
//! folder it ran in, and the names you gave them in `~/.codex/session_index.jsonl`. Only these
//! are read, never credentials.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Context as _;
use argonauts_core::names::VendorId;
use argonauts_core::vendor_sessions::{
    Speaker, VendorScan, VendorSession, VendorTurn, is_session_id,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// How long a title or a last message may be in the list.
const LINE_CHARS: usize = 200;

/// The sessions that ran in `scan.folder`, newest first, at most `limit`. Like `codex resume`,
/// it lists the ones you started in the CLI or an editor, and leaves out `codex exec` runs, such
/// as Argonauts' own chat turns, and subagents.
pub(crate) fn list(scan: &VendorScan, limit: usize) -> anyhow::Result<Vec<VendorSession>> {
    let mut files = rollouts(&scan.home);
    // Newest written first, so a short list reads only the files it shows.
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    let names = thread_names(&scan.home);
    let mut sessions = Vec::new();
    for (modified, path) in files {
        if sessions.len() >= limit {
            break;
        }
        let Some(meta) = first_record(&path) else {
            continue;
        };
        if meta.cwd.as_deref() != Some(scan.folder.as_path()) || !meta.interactive() {
            continue;
        }
        let Ok(rollout) = Rollout::read(&path) else {
            continue;
        };
        if let Some(session) = rollout.session(meta, &names, scan, modified) {
            sessions.push(session);
        }
    }
    sessions.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
    Ok(sessions)
}

/// The last `count` turns of session `id`, oldest first.
pub(crate) fn turns(scan: &VendorScan, id: &str, count: usize) -> anyhow::Result<Vec<VendorTurn>> {
    anyhow::ensure!(is_session_id(id), "{id:?} isn't a Codex session id");
    let ending = format!("-{id}.jsonl");
    let path = rollouts(&scan.home)
        .into_iter()
        .map(|(_, path)| path)
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&ending))
        })
        .with_context(|| format!("Codex has no session {id}"))?;
    let mut turns = Rollout::read(&path)?.turns;
    let skip = turns.len().saturating_sub(count);
    Ok(turns.split_off(skip))
}

/// Every `rollout-*.jsonl` under `sessions/<yyyy>/<mm>/<dd>/`, with when each was last written.
fn rollouts(home: &Path) -> Vec<(SystemTime, PathBuf)> {
    let mut dirs = vec![home.join(".codex").join("sessions")];
    for _ in 0..3 {
        dirs = dirs
            .iter()
            .flat_map(|dir| fs::read_dir(dir).into_iter().flatten())
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect();
    }
    dirs.iter()
        .flat_map(|dir| fs::read_dir(dir).into_iter().flatten())
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                return None;
            }
            let meta = entry.metadata().ok()?;
            meta.is_file().then(|| {
                (
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    entry.path(),
                )
            })
        })
        .collect()
}

/// The names given with `/rename` or picked by Codex, by session id. A later line wins.
fn thread_names(home: &Path) -> HashMap<String, String> {
    #[derive(Deserialize)]
    struct Entry {
        id: String,
        #[serde(default)]
        thread_name: Option<String>,
    }
    let Ok(file) = File::open(home.join(".codex").join("session_index.jsonl")) else {
        return HashMap::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Entry>(&line).ok())
        .filter_map(|entry| Some((entry.id, entry.thread_name?)))
        .filter(|(_, name)| !name.trim().is_empty())
        .collect()
}

/// Every record's kind and time. The payload is read only for the kinds that matter.
#[derive(Debug, Deserialize)]
struct Head {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Line<T> {
    payload: T,
}

fn payload<T: DeserializeOwned>(line: &str) -> Option<T> {
    serde_json::from_str::<Line<T>>(line)
        .ok()
        .map(|line| line.payload)
}

/// `session_meta`, the first record.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Meta {
    id: String,
    cwd: Option<PathBuf>,
    /// `"cli"`, `"vscode"`, `"exec"`, `"mcp"`, or `{"subagent": …}`. Older versions had none.
    source: Option<Value>,
    git: Option<Git>,
}

impl Meta {
    fn interactive(&self) -> bool {
        match &self.source {
            None => true,
            Some(Value::String(source)) => matches!(source.as_str(), "cli" | "vscode"),
            Some(_) => false,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Git {
    branch: Option<String>,
}

/// `turn_context`: the settings of a turn.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TurnContext {
    model: Option<String>,
}

/// `response_item`: a message, a tool call, or reasoning.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Item {
    #[serde(rename = "type")]
    kind: String,
    role: Option<String>,
    content: Option<Value>,
}

fn first_record(path: &Path) -> Option<Meta> {
    let file = File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line).ok()?;
    let head: Head = serde_json::from_str(&line).ok()?;
    (head.kind == "session_meta")
        .then(|| payload(&line))
        .flatten()
}

#[derive(Debug, Default)]
struct Rollout {
    model: Option<String>,
    updated: Option<OffsetDateTime>,
    turns: Vec<VendorTurn>,
}

impl Rollout {
    fn read(path: &Path) -> anyhow::Result<Self> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut rollout = Self::default();
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else { break };
            // A torn last line, or a kind of record this doesn't know, is skipped.
            let Ok(head) = serde_json::from_str::<Head>(&line) else {
                continue;
            };
            if let Some(at) = head.timestamp.as_deref().and_then(parse_time) {
                rollout.updated = Some(rollout.updated.map_or(at, |seen| seen.max(at)));
            }
            match head.kind.as_str() {
                "turn_context" => {
                    if let Some(model) = payload::<TurnContext>(&line).and_then(|turn| turn.model) {
                        rollout.model = Some(model);
                    }
                }
                "response_item" => {
                    if let Some(item) = payload::<Item>(&line) {
                        rollout.item(item);
                    }
                }
                _ => {}
            }
        }
        Ok(rollout)
    }

    fn item(&mut self, item: Item) {
        if item.kind != "message" {
            return;
        }
        let (from, text_kind) = match item.role.as_deref() {
            Some("user") => (Speaker::User, "input_text"),
            Some("assistant") => (Speaker::Agent, "output_text"),
            _ => return,
        };
        let texts: Vec<&str> = match &item.content {
            Some(Value::Array(parts)) => parts
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some(text_kind))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect(),
            Some(Value::String(text)) => vec![text.as_str()],
            _ => Vec::new(),
        };
        let text = texts.join("\n");
        let text = text.trim();
        if text.is_empty() || (from == Speaker::User && added_by_codex(text)) {
            return;
        }
        match self.turns.last_mut() {
            Some(turn) if turn.from == from && from == Speaker::Agent => {
                turn.text.push_str("\n\n");
                turn.text.push_str(text);
            }
            _ => self.turns.push(VendorTurn {
                from,
                text: text.to_owned(),
            }),
        }
    }

    fn session(
        self,
        meta: Meta,
        names: &HashMap<String, String>,
        scan: &VendorScan,
        modified: SystemTime,
    ) -> Option<VendorSession> {
        if !is_session_id(&meta.id) {
            return None;
        }
        let prompt = self
            .turns
            .iter()
            .find(|turn| turn.from == Speaker::User)?
            .text
            .clone();
        let title = names
            .get(&meta.id)
            .map_or_else(|| one_line(&prompt), |name| one_line(name));
        let last_message = self
            .turns
            .iter()
            .rev()
            .find(|turn| turn.from == Speaker::Agent)
            .map(|turn| one_line(&turn.text));
        Some(VendorSession {
            vendor: VendorId("codex".to_owned()),
            id: meta.id,
            title,
            prompt: Some(prompt),
            folder: scan.folder.clone(),
            updated: self
                .updated
                .unwrap_or_else(|| OffsetDateTime::from(modified)),
            model: self.model,
            branch: meta
                .git
                .and_then(|git| git.branch)
                .filter(|branch| !branch.is_empty()),
            last_message,
        })
    }
}

/// What Codex sends as the user besides your words: the folder's AGENTS.md, and context it
/// wraps in its own tags, such as `<environment_context>…</environment_context>`.
fn added_by_codex(text: &str) -> bool {
    text.starts_with("# AGENTS.md instructions") || wrapped_in_tags(text)
}

/// Starts with an opening tag and ends with a closing one, so `<div> is off by one` is still
/// yours.
fn wrapped_in_tags(text: &str) -> bool {
    let tag = |name: &str| {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || matches!(c, '-' | '_' | ' '))
    };
    let opens = text
        .strip_prefix('<')
        .and_then(|rest| rest.split_once('>'))
        .is_some_and(|(name, _)| tag(name));
    let closes = text
        .strip_suffix('>')
        .and_then(|rest| rest.rsplit_once("</"))
        .is_some_and(|(_, name)| tag(name));
    opens && closes
}

fn parse_time(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text, &Rfc3339).ok()
}

/// Whitespace runs as one space, cut to [`LINE_CHARS`].
fn one_line(text: &str) -> String {
    let mut line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > LINE_CHARS {
        line = line.chars().take(LINE_CHARS - 1).collect();
        line.push('…');
    }
    line
}

#[cfg(test)]
mod tests;
