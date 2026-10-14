//! The import: what Claude Code and Codex recorded about each answer on this computer, taken
//! from their own session files into the Overview's records. Per answer only the hour, the CLI,
//! the model, the folder and the token counts are kept; what was said is skipped as each line
//! is read and never kept. It runs in the background after you say yes once, and each time
//! afterwards it reads only what was added since. Days whose transcripts Claude Code has
//! deleted come from its own totals.
//!
//! Antigravity CLI keeps its conversations as binary protocol buffers whose layout is not
//! published, with no plain record of each answer's tokens, so it isn't imported: its share is
//! what ran through Argonauts Code.

mod claude;
mod codex;
mod doctor;
mod files;
mod seen;
mod stats_cache;

pub(crate) use self::doctor::doctor_lines;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argonauts_core::overview::{ImportStatus, OwnCount};
use time::{Date, OffsetDateTime, UtcOffset};

use self::seen::Seen;
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

/// What Claude Code's own /stats adds up from `first` on, or over every day, and the last
/// day it has counted. Nothing while its totals aren't there to read.
pub(crate) fn claude_count(locations: &Locations, first: Option<Date>) -> Option<OwnCount> {
    let cache = stats_cache::read(locations.claude.as_deref()?).ok()??;
    let through = cache.through?;
    // A period it hasn't counted a day of has no count, which is not a count of nothing.
    if first.is_some_and(|first| through < first) {
        return None;
    }
    Some(OwnCount {
        tokens: cache.since(first),
        through,
    })
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

/// What the import found, as `found` says it, and what it couldn't read, in one line:
/// `Claude Code since Oct 2025, 812 sessions; Codex since Feb, 221 sessions; 3 lines couldn't
/// be read`.
pub(crate) fn found_line(found: Option<String>, state: &ImportState) -> Option<String> {
    let mut short = Vec::new();
    match state.unreadable {
        0 => {}
        1 => short.push("1 line couldn't be read".to_owned()),
        n => short.push(format!("{n} lines couldn't be read")),
    }
    match state.skipped {
        0 => {}
        1 => short.push("1 compressed file needs zstd".to_owned()),
        n => short.push(format!("{n} compressed files need zstd")),
    }
    if state.stats_cache_problem.is_some() {
        short.push("Claude Code's stats-cache.json couldn't be read".to_owned());
    }
    if short.is_empty() {
        return found;
    }
    let found = found.unwrap_or_else(|| "nothing".to_owned());
    Some(format!("{found}; {}", short.join("; ")))
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

/// The way of reading that made the records. When it changes, everything is read again once,
/// by itself, and the new records replace the old ones.
pub(crate) const VERSION: u32 = 2;

/// One pass. Records made by an older way of reading are replaced: everything is read again
/// into a folder beside them, which takes their place once it's whole, so the numbers shown
/// stay whole meanwhile and a pass cut short carries on there.
pub(crate) fn import(
    store: &Store,
    locations: &Locations,
    on_progress: &dyn Fn(u32, u32),
) -> std::io::Result<()> {
    let state = store.state();
    if state.version == VERSION {
        return pass(store, state, locations, on_progress);
    }
    let next = store.next();
    let mut fresh = next.state();
    if fresh.version != VERSION {
        next.clear()?;
        fresh = ImportState {
            version: VERSION,
            ..ImportState::default()
        };
    }
    fresh.import = state.import;
    pass(&next, fresh, locations, on_progress)?;
    store.replace_with(&next)
}

/// Every session file not read to its end yet, from where it was left. Records go to the
/// store every little while, with how far each file was read, so a pass cut short starts the
/// next from there.
fn pass(
    store: &Store,
    mut state: ImportState,
    locations: &Locations,
    on_progress: &dyn Fn(u32, u32),
) -> std::io::Result<()> {
    if let Some(kept) = state.kept {
        store.take_back_to(kept)?;
    }
    let mut seen = Seen::load(&store.answers_file());
    let mut found: Vec<Found> = Vec::new();
    if let Some(dir) = &locations.claude {
        found.extend(claude::session_files(dir).into_iter().map(|path| Found {
            source: Source::Claude,
            key: key(&path),
            path,
        }));
    }
    if let Some(dir) = &locations.codex {
        found.extend(codex::rollouts(dir).into_iter().map(|(id, path)| Found {
            source: Source::Codex,
            key: format!("codex:{id}"),
            path,
        }));
    }
    // Only files that grew since they were read are opened at all.
    let mut skipped = 0;
    let due: Vec<Found> = found
        .into_iter()
        .filter(|found| {
            let Ok(meta) = std::fs::metadata(&found.path) else {
                return false;
            };
            let compressed = files::is_compressed(&found.path);
            let fresh = match state.files.get(&found.key) {
                None => true,
                // A compressed file is finished, so it's read again only when it changed.
                Some(file) if compressed => file.packed != Some(meta.len()),
                Some(file) => meta.len() > file.read,
            };
            // Without zstd it can't be opened: it's counted, and read once zstd is there.
            if fresh && compressed && locations.zstd.is_none() {
                skipped += 1;
                return false;
            }
            fresh
        })
        .collect();
    state.skipped = skipped;
    let of = due.len() as u32;
    on_progress(0, of);
    let mut batch = Batch::default();
    for (done, Found { source, path, key }) in due.into_iter().enumerate() {
        let mut file = state.files.get(&key).cloned().unwrap_or_default();
        if file.session == 0 {
            file.session = session_for(&mut state, source, &path);
        }
        let packed = files::is_compressed(&path)
            .then(|| std::fs::metadata(&path).ok().map(|meta| meta.len()))
            .flatten();
        let text = match files::read_from(&path, file.read, locations.zstd.as_deref()) {
            Ok(Some(text)) => text,
            Ok(None) => {
                state.skipped += 1;
                continue;
            }
            // A file that went away or can't be read is left for next time.
            Err(_) => continue,
        };
        let mut unreadable = text.unreadable;
        let answers = match source {
            Source::Claude => claude::answers(
                &text.lines,
                &mut file,
                claude::is_subagent(&path),
                &mut seen,
                &mut unreadable,
            ),
            Source::Codex => codex::answers(&text.lines, &mut file, &mut unreadable),
        };
        state.unreadable = state.unreadable.saturating_add(unreadable);
        file.read = text.consumed;
        file.packed = packed;
        batch
            .records
            .extend(to_records(source.cli(), file.session, answers));
        state.files.insert(key, file);
        if batch.due() {
            batch.flush(store, &mut seen, &mut state)?;
        }
        on_progress(done as u32 + 1, of);
    }
    batch.flush(store, &mut seen, &mut state)?;
    if let Some(dir) = &locations.claude {
        // Claude Code's days rewrite the records rather than add to them, and are worked out
        // again from what is there, so nothing is taken back over them.
        state.kept = None;
        store.save_state(&state)?;
        days_from_stats_cache(store, &mut state, dir)?;
        state.kept = Some(store.kept());
        store.save_state(&state)?;
    }
    Ok(())
}

/// The days Claude Code's transcripts no longer hold, from its own totals. A day is never
/// counted from both: a day with any transcript read is the transcripts', and a day taken from
/// the totals is dropped once a transcript of it turns up.
fn days_from_stats_cache(
    store: &Store,
    state: &mut ImportState,
    dir: &Path,
) -> std::io::Result<()> {
    let claude = Source::Claude.cli();
    let mut read: BTreeSet<Date> = BTreeSet::new();
    let mut taken: BTreeSet<Date> = BTreeSet::new();
    for record in store.records().iter().filter(|record| record.cli == claude) {
        let day = record.at.to_offset(UtcOffset::UTC).date();
        if record.whole_day {
            taken.insert(day);
        } else {
            read.insert(day);
        }
    }
    if !taken.is_disjoint(&read) {
        store.retain(&|record| {
            let day = record.at.to_offset(UtcOffset::UTC).date();
            !(record.whole_day && record.cli == claude && read.contains(&day))
        })?;
    }
    let cache = match stats_cache::read(dir) {
        Ok(Some(cache)) => cache,
        Ok(None) => return Ok(()),
        Err(why) => {
            state.stats_cache_problem = Some(why);
            return Ok(());
        }
    };
    state.stats_cache_problem = None;
    let mut records = Vec::new();
    for day in &cache.days {
        let after = cache.through.is_some_and(|through| day.date > through);
        if after || read.contains(&day.date) || taken.contains(&day.date) {
            continue;
        }
        records.extend(stats_cache::records(day, &cache.models, state.sessions + 1));
        state.sessions += day.sessions.max(1);
    }
    store.append(&records)
}

/// Records waiting to be kept. They go to the store with the state that says how far each
/// file was read, every couple of seconds rather than after each file, since the state names
/// every file and a first pass reads thousands.
#[derive(Default)]
struct Batch {
    records: Vec<Record>,
    kept: Option<Instant>,
}

impl Batch {
    const EVERY: Duration = Duration::from_secs(2);

    fn due(&self) -> bool {
        self.kept.is_none_or(|kept| kept.elapsed() >= Self::EVERY)
    }

    /// The records, then the answers counted, then the state that says how far each file was
    /// read and how much of the two it accounts for. Stopped anywhere before the state is
    /// saved, the next pass takes back what came after the last state and reads it again.
    fn flush(
        &mut self,
        store: &Store,
        seen: &mut Seen,
        state: &mut ImportState,
    ) -> std::io::Result<()> {
        store.append(&self.records)?;
        seen.save()?;
        state.kept = Some(store.kept());
        store.save_state(state)?;
        self.records.clear();
        self.kept = Some(Instant::now());
        Ok(())
    }
}

/// A count that is missing, `null` or not a whole number is read as zero, never as a reason to
/// drop the line it's on.
pub(super) fn count<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    use serde::Deserialize as _;
    let value = Option::<serde_json::Value>::deserialize(d)?;
    Ok(value.as_ref().map_or(0, number))
}

/// A count as a whole number, zero when it isn't one.
pub(super) fn number(value: &serde_json::Value) -> u64 {
    match value {
        serde_json::Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_f64().filter(|n| *n > 0.0).map(|n| n as u64))
            .unwrap_or(0),
        _ => 0,
    }
}

/// A session file and the key its progress is kept under: its path for Claude Code, its id for
/// Codex, whose rollouts move to the archive and get compressed.
struct Found {
    source: Source,
    path: PathBuf,
    key: String,
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
                whole_day: false,
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
