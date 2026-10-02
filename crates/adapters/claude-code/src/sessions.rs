//! Claude Code's own sessions. It keeps each one as `~/.claude/projects/<folder>/<id>.jsonl`,
//! one record a line, where `<folder>` is the folder it ran in with every character but letters
//! and digits turned into `-`. Only these transcripts are read, never credentials.

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
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Claude Code cuts a longer folder name and adds a hash, so such a folder is found by its
/// first part and the sessions' own `cwd`.
const LONGEST_FOLDER_NAME: usize = 200;

/// How long a title or a last message may be in the list.
const LINE_CHARS: usize = 200;

/// The sessions that ran in `scan.folder`, newest first, at most `limit`. Sessions started with
/// `-p`, such as Argonauts' own chat turns, are left out, as Claude Code's own picker does.
pub(crate) fn list(scan: &VendorScan, limit: usize) -> anyhow::Result<Vec<VendorSession>> {
    let mut files: Vec<(SystemTime, PathBuf)> = project_dirs(scan)
        .iter()
        .flat_map(|dir| transcripts(dir))
        .collect();
    // Newest written first, so a short list reads only the files it shows.
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    let mut sessions = Vec::new();
    for (modified, path) in files {
        if sessions.len() >= limit {
            break;
        }
        let Some(id) = session_id(&path) else {
            continue;
        };
        let Ok(transcript) = Transcript::read(&path) else {
            continue;
        };
        if let Some(session) = transcript.session(id, scan, modified) {
            sessions.push(session);
        }
    }
    sessions.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
    Ok(sessions)
}

/// The last `count` turns of session `id`, oldest first.
pub(crate) fn turns(scan: &VendorScan, id: &str, count: usize) -> anyhow::Result<Vec<VendorTurn>> {
    anyhow::ensure!(is_session_id(id), "{id:?} isn't a Claude Code session id");
    let path = project_dirs(scan)
        .into_iter()
        .map(|dir| dir.join(format!("{id}.jsonl")))
        .find(|path| path.is_file())
        .with_context(|| {
            format!(
                "Claude Code has no session {id} for {}",
                scan.folder.display()
            )
        })?;
    let mut turns = Transcript::read(&path)?.turns;
    let skip = turns.len().saturating_sub(count);
    Ok(turns.split_off(skip))
}

/// `/work/my.app` keeps its sessions in `-work-my-app`. JavaScript counts UTF-16 units, so a
/// character outside the basic plane turns into two dashes.
fn folder_name(folder: &Path) -> String {
    folder
        .to_string_lossy()
        .chars()
        .flat_map(|c| {
            let kept = c.is_ascii_alphanumeric().then_some(c);
            let dashes = if kept.is_some() { 0 } else { c.len_utf16() };
            kept.into_iter().chain(std::iter::repeat_n('-', dashes))
        })
        .collect()
}

fn project_dirs(scan: &VendorScan) -> Vec<PathBuf> {
    let projects = scan.home.join(".claude").join("projects");
    let name = folder_name(&scan.folder);
    if name.len() <= LONGEST_FOLDER_NAME {
        return vec![projects.join(name)];
    }
    let start = &name[..LONGEST_FOLDER_NAME];
    fs::read_dir(&projects)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(start))
        .map(|entry| entry.path())
        .collect()
}

fn transcripts(dir: &Path) -> Vec<(SystemTime, PathBuf)> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            session_id(&path)?;
            let meta = entry.metadata().ok()?;
            meta.is_file()
                .then(|| (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), path))
        })
        .collect()
}

fn session_id(path: &Path) -> Option<&str> {
    if path.extension()? != "jsonl" {
        return None;
    }
    let id = path.file_stem()?.to_str()?;
    is_session_id(id).then_some(id)
}

/// One line of a transcript. Most kinds of record are of no use here and read as nothing.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    is_sidechain: bool,
    is_meta: bool,
    entrypoint: Option<String>,
    cwd: Option<PathBuf>,
    git_branch: Option<String>,
    timestamp: Option<String>,
    message: Option<Message>,
    origin: Option<Origin>,
    custom_title: Option<String>,
    ai_title: Option<String>,
    summary: Option<String>,
    continued_in_session_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Message {
    model: Option<String>,
    content: Option<Content>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Block {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

/// Who or what sent a user record. Only a person's words count as a prompt.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Origin {
    kind: Option<String>,
}

/// What a transcript holds that the list and the reprint need.
#[derive(Debug, Default)]
struct Transcript {
    /// From the first record that has one.
    cwd: Option<PathBuf>,
    entrypoint: Option<String>,
    branch: Option<String>,
    model: Option<String>,
    updated: Option<OffsetDateTime>,
    custom_title: Option<String>,
    ai_title: Option<String>,
    summary: Option<String>,
    continued_in: Option<String>,
    turns: Vec<VendorTurn>,
}

impl Transcript {
    fn read(path: &Path) -> anyhow::Result<Self> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut transcript = Self::default();
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else { break };
            // A torn last line, or a kind of record this doesn't know, is skipped.
            if let Ok(record) = serde_json::from_str::<Record>(&line) {
                transcript.add(record);
            }
        }
        Ok(transcript)
    }

    fn add(&mut self, record: Record) {
        if let Some(at) = record.timestamp.as_deref().and_then(parse_time) {
            self.updated = Some(self.updated.map_or(at, |seen| seen.max(at)));
        }
        match record.kind.as_str() {
            "custom-title" => self.custom_title = record.custom_title.or(self.custom_title.take()),
            "ai-title" => self.ai_title = record.ai_title.or(self.ai_title.take()),
            "summary" => self.summary = record.summary.or(self.summary.take()),
            "continued-in" => self.continued_in = record.continued_in_session_id,
            "user" | "assistant" if !record.is_sidechain => self.message(record),
            _ => {}
        }
    }

    fn message(&mut self, record: Record) {
        if self.cwd.is_none() {
            self.cwd = record.cwd;
        }
        if self.entrypoint.is_none() {
            self.entrypoint = record.entrypoint;
        }
        if let Some(branch) = record.git_branch.filter(|branch| !branch.is_empty()) {
            self.branch = Some(branch);
        }
        let Some(message) = record.message else {
            return;
        };
        if record.kind == "user" {
            let by_a_person = record
                .origin
                .and_then(|origin| origin.kind)
                .is_none_or(|kind| kind == "human");
            if let Some(text) = message.content.as_ref().and_then(prompt_text)
                && by_a_person
                && !record.is_meta
            {
                self.turns.push(VendorTurn {
                    from: Speaker::User,
                    text,
                });
            }
            return;
        }
        // Claude Code writes its own error notes as `<synthetic>` replies.
        if message.model.as_deref() == Some("<synthetic>") {
            return;
        }
        if let Some(model) = message.model.filter(|model| !model.is_empty()) {
            self.model = Some(model);
        }
        let Some(Content::Blocks(blocks)) = message.content else {
            return;
        };
        for text in blocks
            .into_iter()
            .filter(|block| block.kind == "text")
            .filter_map(|block| block.text)
            .filter(|text| !text.trim().is_empty())
        {
            match self.turns.last_mut() {
                Some(turn) if turn.from == Speaker::Agent => {
                    turn.text.push_str("\n\n");
                    turn.text.push_str(text.trim());
                }
                _ => self.turns.push(VendorTurn {
                    from: Speaker::Agent,
                    text: text.trim().to_owned(),
                }),
            }
        }
    }

    /// The list's row, or `None` for a session the list leaves out.
    fn session(self, id: &str, scan: &VendorScan, modified: SystemTime) -> Option<VendorSession> {
        // `sdk-cli` for `-p`, `sdk-ts` and `sdk-py` for the SDKs; the terminal and the editors
        // have their own names.
        let interactive = self
            .entrypoint
            .as_deref()
            .is_none_or(|entry| !entry.starts_with("sdk"));
        let here = self.cwd.as_deref().is_none_or(|cwd| cwd == scan.folder);
        // Carried on under another id, which lists in its place.
        let moved = self.continued_in.is_some();
        let prompt = self
            .turns
            .iter()
            .find(|turn| turn.from == Speaker::User)?
            .text
            .clone();
        if !interactive || !here || moved {
            return None;
        }
        let title = [&self.custom_title, &self.ai_title, &self.summary]
            .into_iter()
            .flatten()
            .find(|title| !title.trim().is_empty())
            .map_or_else(|| one_line(&prompt), |title| one_line(title));
        let last_message = self
            .turns
            .iter()
            .rev()
            .find(|turn| turn.from == Speaker::Agent)
            .map(|turn| one_line(&turn.text));
        Some(VendorSession {
            vendor: VendorId("claude-code".to_owned()),
            id: id.to_owned(),
            title,
            prompt: Some(prompt),
            folder: scan.folder.clone(),
            updated: self
                .updated
                .unwrap_or_else(|| OffsetDateTime::from(modified)),
            model: self.model,
            branch: self.branch,
            last_message,
        })
    }
}

/// A person's words, or `None` for tool results and what Claude Code adds itself: a slash
/// command's echo and output, a shell escape, a reminder, or an interruption.
fn prompt_text(content: &Content) -> Option<String> {
    let text = match content {
        Content::Text(text) => text.clone(),
        Content::Blocks(blocks) => {
            if blocks.iter().any(|block| block.kind == "tool_result") {
                return None;
            }
            let texts: Vec<&str> = blocks
                .iter()
                .filter(|block| block.kind == "text")
                .filter_map(|block| block.text.as_deref())
                .collect();
            texts.join("\n")
        }
    };
    let text = text.trim();
    let added = wrapped_in_tags(text) || text.starts_with("[Request interrupted");
    (!text.is_empty() && !added).then(|| text.to_owned())
}

/// `<command-name>/model</command-name>…<command-args></command-args>`: text a CLI wraps in its
/// own tags. A prompt that only starts with a tag, such as `<div> is off by one`, isn't.
pub(crate) fn wrapped_in_tags(text: &str) -> bool {
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
