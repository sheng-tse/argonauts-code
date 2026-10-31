//! What a workflow's script finished, kept beside the run so a run that stops picks the script
//! up where it was: the calls it already made answer from here, and nothing runs twice.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use argonauts_core::base::names::SessionKey;
use argonauts_core::base::paths::{create_private_dir_all, write_private_file};
use argonauts_runner::JournalEntry;

use super::folder;
use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::workflow::StepCall;

pub(super) fn place(host: &Host, call: &StepCall, file: &str) -> PathBuf {
    folder(host).join(call.workflow().as_str()).join(file)
}

/// The session behind each agent the script has run so far, by the agent's index.
pub(super) fn sessions(host: &Host, call: &StepCall) -> BTreeMap<u32, SessionKey> {
    fs::read(place(host, call, "sessions.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(super) fn keep_sessions(
    host: &Host,
    call: &StepCall,
    sessions: &BTreeMap<u32, SessionKey>,
) -> EngineResult<()> {
    let path = place(host, call, "sessions.json");
    if let Some(dir) = path.parent() {
        create_private_dir_all(dir)?;
    }
    write_private_file(&path, &serde_json::to_vec(sessions)?)?;
    Ok(())
}

/// Every call saved so far, oldest first. A line cut short by a crash is left out.
pub(super) fn read(host: &Host, call: &StepCall) -> EngineResult<Vec<JournalEntry>> {
    let text = match fs::read_to_string(place(host, call, "calls.jsonl")) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

pub(super) fn append(host: &Host, call: &StepCall, entry: &JournalEntry) -> EngineResult<()> {
    let path = place(host, call, "calls.jsonl");
    if let Some(dir) = path.parent() {
        create_private_dir_all(dir)?;
    }
    if !path.exists() {
        write_private_file(&path, b"")?;
    }
    let mut line = serde_json::to_string(entry)?;
    line.push('\n');
    OpenOptions::new()
        .append(true)
        .open(&path)?
        .write_all(line.as_bytes())?;
    Ok(())
}

/// The script the orchestrator wrote for this run after you sent its steps back `sent_back`
/// times, once it has. Empty when none it wrote could be used.
pub(super) fn written(host: &Host, call: &StepCall, sent_back: usize) -> Option<String> {
    fs::read_to_string(place(host, call, &script_file(sent_back))).ok()
}

pub(super) fn keep_written(
    host: &Host,
    call: &StepCall,
    sent_back: usize,
    source: &str,
) -> EngineResult<()> {
    let path = place(host, call, &script_file(sent_back));
    if let Some(dir) = path.parent() {
        create_private_dir_all(dir)?;
    }
    write_private_file(&path, source.as_bytes())?;
    Ok(())
}

/// `script.js` as first written, then `script-2.js` and on, so each one you saw is kept.
fn script_file(sent_back: usize) -> String {
    match sent_back {
        0 => "script.js".to_owned(),
        times => format!("script-{}.js", times + 1),
    }
}
