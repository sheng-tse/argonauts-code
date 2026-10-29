//! Claude Code's own sessions. It keeps each one as `~/.claude/projects/<folder>/<id>.jsonl`,
//! one record a line, where `<folder>` is the folder it ran in with every character but letters
//! and digits turned into `-`. What a session's subagents did is beside it, each one's as
//! `<id>/subagents/agent-<agent id>.jsonl`. Only these transcripts are read, never credentials.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Context as _;
use argonauts_core::AgentEvent;
use argonauts_core::cli_sessions::{
    CliCall, CliPatch, CliScan, CliSession, CliStep, CliTurn, Speaker, is_session_id,
};
use argonauts_core::names::AgentId;
use serde::Deserialize;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::json::{bool_at, str_at, string_at};
use crate::parser::tool_output_text;
use crate::patch::file_patch;

/// Claude Code cuts a longer folder name and adds a hash, so such a folder is found by its
/// first part and the sessions' own `cwd`.
const LONGEST_FOLDER_NAME: usize = 200;

/// How long a title or a last message may be in the list.
const LINE_CHARS: usize = 200;

/// How deep a subagent's own subagents are read. A transcript that named itself as its own
/// subagent would otherwise never end.
const SUBAGENT_DEPTH: usize = 4;

/// The sessions that ran in `scan.folder`, newest first, at most `limit`. Sessions started with
/// `-p`, such as Argonauts Code's own conversation turns, are left out, as Claude Code's own picker
/// does.
pub(crate) fn list(scan: &CliScan, limit: usize) -> anyhow::Result<Vec<CliSession>> {
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
pub(crate) fn turns(scan: &CliScan, id: &str, count: usize) -> anyhow::Result<Vec<CliTurn>> {
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
    let mut transcript = Transcript::read(&path)?;
    let skip = transcript.turns.len().saturating_sub(count);
    let mut turns = transcript.turns.split_off(skip);
    // Only the turns that reprint have their subagents read.
    let subagents = path.with_file_name(id).join("subagents");
    add_subagents(
        &mut turns,
        &transcript.subagents,
        &subagents,
        SUBAGENT_DEPTH,
    );
    Ok(turns)
}

/// Gives each call that started a subagent what the subagent did, from the transcript Claude
/// Code keeps for it in `dir`. `started` names the subagent of each such call, by the call's
/// id. A subagent whose transcript is gone leaves its call as it was.
fn add_subagents(
    turns: &mut [CliTurn],
    started: &HashMap<String, String>,
    dir: &Path,
    depth: usize,
) {
    if depth == 0 || started.is_empty() {
        return;
    }
    let calls = turns
        .iter_mut()
        .flat_map(|turn| &mut turn.steps)
        .filter_map(|step| match step {
            CliStep::Ran(call) => Some(call),
            _ => None,
        });
    for call in calls {
        // The id comes from a transcript and names a file, so it must be a plain name.
        let Some(agent) = started.get(&call.id).filter(|agent| is_session_id(agent)) else {
            continue;
        };
        let Ok(mut own) = Transcript::read_subagent(&dir.join(format!("agent-{agent}.jsonl")))
        else {
            continue;
        };
        add_subagents(&mut own.turns, &own.subagents, dir, depth - 1);
        // Its prompt is the call's own input, so only what it did is kept.
        call.steps = own
            .turns
            .into_iter()
            .filter(|turn| turn.from == Speaker::Agent)
            .flat_map(|turn| turn.steps)
            .collect();
    }
}

/// How many prompts a person typed, which is a session's length.
fn prompts(turns: &[CliTurn]) -> u32 {
    let count = turns
        .iter()
        .filter(|turn| turn.from == Speaker::User)
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
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

fn project_dirs(scan: &CliScan) -> Vec<PathBuf> {
    let projects = scan.home.join(".claude").join("projects");
    if scan.every_folder {
        return fs::read_dir(&projects)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
    }
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
    /// What a tool's result says beyond its text, such as an edit's patch.
    tool_use_result: Option<Value>,
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
    /// `text`, `thinking`, `tool_use` and `tool_result` blocks, read by their `type`.
    Blocks(Vec<Value>),
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
    turns: Vec<CliTurn>,
    /// A subagent's own transcript, whose every record is one of its own. A session's
    /// transcript leaves out any record marked as a subagent's.
    of_a_subagent: bool,
    /// The subagent each call started, by the call's id, as the call's result names it.
    subagents: HashMap<String, String>,
}

impl Transcript {
    fn read(path: &Path) -> anyhow::Result<Self> {
        Self::default().filled_from(path)
    }

    fn read_subagent(path: &Path) -> anyhow::Result<Self> {
        let transcript = Self {
            of_a_subagent: true,
            ..Self::default()
        };
        transcript.filled_from(path)
    }

    fn filled_from(self, path: &Path) -> anyhow::Result<Self> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut transcript = self;
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
            "user" | "assistant" if self.of_a_subagent || !record.is_sidechain => {
                self.message(record);
            }
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
            if let Some(Content::Blocks(blocks)) = &message.content {
                self.tool_results(blocks, record.tool_use_result.as_ref());
            }
            let by_a_person = record
                .origin
                .and_then(|origin| origin.kind)
                .is_none_or(|kind| kind == "human");
            if let Some(text) = message.content.as_ref().and_then(prompt_text)
                && by_a_person
                && !record.is_meta
            {
                let sent = record.timestamp.as_deref().and_then(parse_time);
                self.turns.push(CliTurn::prompt(text).sent_at(sent));
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
        for block in &blocks {
            let step = match str_at(block, "type") {
                Some("text") => {
                    let text = str_at(block, "text").unwrap_or_default();
                    if !text.trim().is_empty() {
                        self.agent_turn().wrote(text);
                    }
                    continue;
                }
                Some("thinking") => CliStep::Thought {
                    text: str_at(block, "thinking")
                        .unwrap_or_default()
                        .trim()
                        .to_owned(),
                },
                Some("tool_use") => CliStep::Ran(CliCall::new(
                    str_at(block, "id").unwrap_or_default(),
                    str_at(block, "name").unwrap_or_default(),
                    block.get("input").cloned().unwrap_or_default(),
                )),
                _ => continue,
            };
            self.agent_turn().steps.push(step);
        }
    }

    /// The agent's turn being read, started by the first thing it does after a prompt.
    fn agent_turn(&mut self) -> &mut CliTurn {
        if self
            .turns
            .last()
            .is_none_or(|turn| turn.from != Speaker::Agent)
        {
            self.turns.push(CliTurn::of_the_agent());
        }
        self.turns.last_mut().expect("a turn was just pushed")
    }

    /// Gives each call the result a user record brings back for it. The call may sit in an
    /// earlier turn than the one being read: a command left running reports after your next
    /// prompt.
    fn tool_results(&mut self, blocks: &[Value], structured: Option<&Value>) {
        let results: Vec<&Value> = blocks
            .iter()
            .filter(|block| str_at(block, "type") == Some("tool_result"))
            .collect();
        // The record's own result belongs to the message, so it is only unambiguous for one.
        let structured = structured.filter(|_| results.len() == 1);
        for block in results {
            let id = string_at(block, "tool_use_id").unwrap_or_default();
            let made_by_the_agent = self
                .turns
                .iter_mut()
                .rev()
                .filter(|turn| turn.from == Speaker::Agent);
            let Some(call) = made_by_the_agent.flat_map(|turn| turn.call_mut(&id)).next() else {
                continue;
            };
            if let Some(agent) = structured.and_then(|result| str_at(result, "agentId")) {
                self.subagents.insert(id.clone(), agent.to_owned());
            }
            let ok = !bool_at(block, "is_error");
            call.ok = Some(ok);
            call.output = tool_output_text(block.get("content"));
            let patch = ok
                .then(|| file_patch(&id, &call.name, &call.input, structured, None))
                .flatten();
            if let Some(AgentEvent::FilePatch { path, hunks, .. }) = patch {
                call.patches.push(CliPatch {
                    path,
                    hunks,
                    numbered: true,
                });
            }
        }
    }

    /// The list's row, or `None` for a session the list leaves out.
    fn session(self, id: &str, scan: &CliScan, modified: SystemTime) -> Option<CliSession> {
        // `sdk-cli` for `-p`, `sdk-ts` and `sdk-py` for the SDKs; the terminal and the editors
        // have their own names.
        let interactive = self
            .entrypoint
            .as_deref()
            .is_none_or(|entry| !entry.starts_with("sdk"));
        let here = scan.lists(self.cwd.as_deref());
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
            .find(|turn| turn.from == Speaker::Agent && !turn.text.is_empty())
            .map(|turn| one_line(&turn.text));
        Some(CliSession {
            agent_id: AgentId("claude-code".to_owned()),
            id: id.to_owned(),
            title,
            prompt: Some(prompt),
            folder: self.cwd.unwrap_or_else(|| scan.folder.clone()),
            updated: self
                .updated
                .unwrap_or_else(|| OffsetDateTime::from(modified)),
            model: self.model,
            branch: self.branch,
            last_message,
            turns: prompts(&self.turns),
        })
    }
}

/// A person's words, or `None` for tool results and what Claude Code adds itself: a slash
/// command's echo and output, a shell escape, a reminder, or an interruption.
fn prompt_text(content: &Content) -> Option<String> {
    let text = match content {
        Content::Text(text) => text.clone(),
        Content::Blocks(blocks) => {
            if blocks
                .iter()
                .any(|block| str_at(block, "type") == Some("tool_result"))
            {
                return None;
            }
            let texts: Vec<&str> = blocks
                .iter()
                .filter(|block| str_at(block, "type") == Some("text"))
                .filter_map(|block| str_at(block, "text"))
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
