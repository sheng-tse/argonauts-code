//! The import: what Claude Code and Codex recorded about each answer on this computer, taken
//! from their own session files into the Overview's records. Per answer only the hour, the CLI,
//! the model, the folder and the token counts are kept; what was said is skipped as each line
//! is read and never kept. It runs in the background after you say yes once, and each time
//! afterwards it reads only what was added since.
//!
//! Antigravity CLI keeps its conversations as binary protocol buffers whose layout is not
//! published, with no plain record of each answer's tokens, so it isn't imported: its share is
//! what ran through Argonauts.

mod claude;
mod codex;
mod files;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argonauts_core::overview::ImportStatus;
use time::OffsetDateTime;

use super::records::{FileRead, ImportState, Record, Store};

/// How long after one import the next may start, so a tab asked for every few seconds reads
/// the CLIs' folders now and then rather than each time.
const AGAIN_AFTER: Duration = Duration::from_secs(5 * 60);

/// Where each CLI keeps its session files, and how to open a compressed one.
#[derive(Debug, Clone)]
pub(crate) struct Locations {
    /// Claude Code's folder: `$CLAUDE_CONFIG_DIR`, or `~/.claude`.
    pub claude: Option<PathBuf>,
    /// Codex's folder: `$CODEX_HOME`, or `~/.codex`.
    pub codex: Option<PathBuf>,
    /// The `zstd` program, for Codex's compressed session files, when it's installed.
    pub zstd: Option<PathBuf>,
}

impl Locations {
    /// The folders each CLI documents, from the environment.
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let under_home = |name: &str| home.as_ref().map(|home| home.join(name));
        let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| under_home(".claude"));
        let codex = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| under_home(".codex"));
        Self {
            claude,
            codex,
            zstd: files::find_program("zstd"),
        }
    }
}

/// How far the import running now has got, and when the last one ended.
#[derive(Debug, Default)]
struct Progress {
    running: Option<(u32, u32)>,
    ended: Option<Instant>,
    failed: Option<String>,
}

static PROGRESS: Mutex<Progress> = Mutex::new(Progress {
    running: None,
    ended: None,
    failed: None,
});

fn progress() -> std::sync::MutexGuard<'static, Progress> {
    PROGRESS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Where the import stands, as the tab shows it.
pub(crate) fn status(state: &ImportState) -> ImportStatus {
    match state.import {
        None => ImportStatus::Unasked,
        Some(false) => ImportStatus::Declined,
        Some(true) => {
            let progress = progress();
            match (&progress.running, &progress.failed) {
                (Some((done, of)), _) => ImportStatus::Running {
                    done: *done,
                    of: *of,
                },
                (None, Some(why)) => ImportStatus::Failed { why: why.clone() },
                (None, None) => ImportStatus::Done,
            }
        }
    }
}

/// Starts an import in the background when you said yes and none ran lately. `now` asks for
/// one even so, as right after you answer.
pub(crate) fn start_if_due(store: Store, locations: Locations, now: bool) {
    {
        let mut progress = progress();
        if progress.running.is_some() {
            return;
        }
        let lately = progress
            .ended
            .is_some_and(|ended| ended.elapsed() < AGAIN_AFTER);
        if lately && !now {
            return;
        }
        progress.running = Some((0, 0));
        progress.failed = None;
    }
    std::thread::spawn(move || {
        let result = import(&store, &locations, &|done, of| {
            progress().running = Some((done, of));
        });
        let mut progress = progress();
        progress.running = None;
        progress.ended = Some(Instant::now());
        if let Err(err) = result {
            tracing::warn!("the import stopped: {err}");
            progress.failed = Some(err.to_string());
        }
    });
}

/// One pass: every session file not read to its end yet, from where it was left. Records go
/// to the store a file at a time, with how far that file was read, so a pass cut short
/// starts the next from there.
pub(crate) fn import(
    store: &Store,
    locations: &Locations,
    on_progress: &dyn Fn(u32, u32),
) -> std::io::Result<()> {
    let mut state = store.state();
    let mut found: Vec<(Source, PathBuf)> = Vec::new();
    if let Some(dir) = &locations.claude {
        found.extend(
            claude::session_files(dir)
                .into_iter()
                .map(|p| (Source::Claude, p)),
        );
    }
    if let Some(dir) = &locations.codex {
        found.extend(
            codex::session_files(dir)
                .into_iter()
                .map(|p| (Source::Codex, p)),
        );
    }
    // Only files that grew since they were read are opened at all.
    let due: Vec<(Source, PathBuf, u64)> = found
        .into_iter()
        .filter_map(|(source, path)| {
            let len = std::fs::metadata(&path).ok()?.len();
            let read = state.files.get(&key(&path)).map(|file| file.read);
            let compressed = files::is_compressed(&path);
            let fresh = match read {
                None => true,
                // A compressed file is finished, so once read it's done.
                Some(_) if compressed => false,
                Some(read) => len > read,
            };
            fresh.then_some((source, path, len))
        })
        .collect();
    let of = due.len() as u32;
    on_progress(0, of);
    for (done, (source, path, _)) in due.iter().enumerate() {
        let mut file = state.files.get(&key(path)).cloned().unwrap_or_default();
        if file.session == 0 {
            file.session = session_for(&mut state, *source, path);
        }
        let text = match files::read_from(path, file.read, locations.zstd.as_deref()) {
            Ok(Some(text)) => text,
            Ok(None) => {
                state.skipped += 1;
                file.read = u64::MAX;
                state.files.insert(key(path), file);
                continue;
            }
            // A file that went away or can't be read is left for next time.
            Err(_) => continue,
        };
        let answers = match source {
            Source::Claude => claude::answers(&text.lines, &mut file, claude::is_subagent(path)),
            Source::Codex => codex::answers(&text.lines, &mut file),
        };
        file.read = file.read.saturating_add(text.consumed);
        let records = to_records(source.cli(), file.session, answers);
        store.append(&records)?;
        state.files.insert(key(path), file);
        store.save_state(&state)?;
        on_progress(done as u32 + 1, of);
    }
    store.save_state(&state)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Claude,
    Codex,
}

impl Source {
    fn cli(self) -> &'static str {
        match self {
            Source::Claude => "claude-code",
            Source::Codex => "codex",
        }
    }
}

/// One answer's counts, as a session file gives them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Answer {
    pub at: OffsetDateTime,
    pub model: String,
    pub folder: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub subagent: bool,
}

/// The answers summed by hour, model, folder and whether a subagent gave them.
fn to_records(cli: &str, session: u32, answers: Vec<Answer>) -> Vec<Record> {
    let mut by: BTreeMap<(OffsetDateTime, String, String, bool), Record> = BTreeMap::new();
    for answer in answers {
        let hour = answer
            .at
            .replace_minute(0)
            .and_then(|at| at.replace_second(0))
            .and_then(|at| at.replace_nanosecond(0))
            .unwrap_or(answer.at)
            .to_offset(time::UtcOffset::UTC);
        let record = by
            .entry((
                hour,
                answer.model.clone(),
                answer.folder.clone(),
                answer.subagent,
            ))
            .or_insert_with(|| Record {
                at: hour,
                cli: cli.to_owned(),
                model: answer.model.clone(),
                folder: answer.folder.clone(),
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                session,
                subagent: answer.subagent,
            });
        record.input += answer.input;
        record.output += answer.output;
        record.cache_read += answer.cache_read;
        record.cache_write += answer.cache_write;
    }
    by.into_values()
        .filter(|record| record.input + record.output + record.cache_read + record.cache_write > 0)
        .collect()
}

/// The session a file belongs to: a Claude Code subagent's file shares its parent session's
/// number, and every other file starts a session of its own.
fn session_for(state: &mut ImportState, source: Source, path: &Path) -> u32 {
    if source == Source::Claude
        && let Some(parent) = claude::parent_session(path)
    {
        if let Some(file) = state.files.get(&key(&parent)) {
            return file.session;
        }
        state.sessions += 1;
        let session = state.sessions;
        state.files.insert(
            key(&parent),
            FileRead {
                session,
                ..FileRead::default()
            },
        );
        return session;
    }
    state.sessions += 1;
    state.sessions
}

fn key(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests;
