//! Past records filed again from each run's own log, once. Lines written before each session
//! kept its own model, before a review went to the session that made it and before a judge's
//! verdict was a sign-off become what a run's end files now, and every other line goes under
//! the catalog's spelling of its model, so old and new rows join up. It reads only Argonauts
//! Code's own records: the two ledgers, and the runs' logs under `~/.argonauts/state`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use argonauts_core::RunId;
use argonauts_core::events::run::RunLifecycle;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::journal::read_all;
use argonauts_core::names::ModelRef;
use argonauts_core::paths::{EVENT_LOG_FILE, write_private_file};
use serde::Serialize;
use serde::de::DeserializeOwned;
use time::{Date, OffsetDateTime};

use super::{LedgerLine, UsageLedger, local_day, month_name};
use crate::work_ledger::{self, Work, WorkLedger, WorkLine};

/// In the usage ledger's folder, the mark that the rebuild is done. A later rebuild with more
/// to set right takes a new name.
const DONE: &str = ".rebuilt-1";
/// The mark that a process is rebuilding, so two never do at once.
const BUSY: &str = ".rebuilding";
/// A busy mark this old was left by a process that stopped midway.
const STALE: Duration = Duration::from_secs(10 * 60);
/// Times a month file is read again when a line was added while it was being rebuilt.
const TRIES: usize = 5;

impl UsageLedger {
    /// Rebuilds the ledgers before they're first read, once. A failure is logged, and the
    /// records are read as they are.
    pub(crate) fn rebuilt_once(&self) {
        if let Err(err) = self.rebuild_once() {
            tracing::warn!("couldn't rebuild the usage records from the runs' logs: {err}");
        }
    }

    /// Rebuilds the ledgers from the runs' logs, unless that was done or another process is
    /// doing it. A folder with no ledger yet has nothing to rebuild.
    pub(crate) fn rebuild_once(&self) -> io::Result<()> {
        if !self.dir.is_dir() || self.dir.join(DONE).exists() {
            return Ok(());
        }
        let Some(_busy) = Busy::take(&self.dir)? else {
            return Ok(());
        };
        self.rebuild()?;
        write_private_file(&self.dir.join(DONE), b"")
    }

    /// Files every run the ledgers name again from its log, on the day and at the hour they
    /// filed it, and every other line under the catalog's spelling of its model. A run whose
    /// log is gone keeps its lines. Doing it again changes nothing.
    pub(crate) fn rebuild(&self) -> io::Result<()> {
        let logs = run_logs(&self.state);
        let work = WorkLedger::beside(&self.dir);
        let mut named: BTreeMap<RunId, Filed> = BTreeMap::new();
        for line in self.read_since(None) {
            if let Some(run) = line.run {
                let filed = named.entry(run).or_insert_with(|| Filed::under(line.repo));
                filed.ended.get_or_insert(line.day);
                filed.hour = filed.hour.or(line.hour);
            }
        }
        for line in work.read_since(None) {
            let ended = matches!(line.what, Work::Run { .. }).then_some(line.day);
            let filed = named
                .entry(line.run)
                .or_insert_with(|| Filed::under(line.repo));
            filed.ended = filed.ended.or(ended);
        }

        let mut rebuilt = BTreeSet::new();
        let (mut usage, mut done) = (Vec::new(), Vec::new());
        for (run, filed) in &named {
            let Some(events) = logs.get(run).and_then(|log| read_all(log).ok()) else {
                continue;
            };
            let Some(at) = ended_at(&events.events) else {
                continue;
            };
            let repo = Path::new(&filed.repo);
            let ended = filed.ended.unwrap_or_else(|| local_day(at));
            usage.extend(
                (self.lines_of(repo, run, &events.events, at).into_iter()).map(|line| LedgerLine {
                    day: ended,
                    hour: filed.hour.or(line.hour),
                    ..line
                }),
            );
            done.extend(work.lines_of(repo, run, &events.events, ended));
            rebuilt.insert(run.clone());
        }

        let catalog = self.catalog;
        rewrite(
            &self.dir,
            usage,
            |line: &LedgerLine| line.day,
            |line| {
                if line.run.as_ref().is_some_and(|run| rebuilt.contains(run)) {
                    return None;
                }
                let model = ModelRef::new(line.agent.clone(), line.model.clone());
                Some(LedgerLine {
                    model: work_ledger::filed_as(catalog, &model).model,
                    ..line.clone()
                })
            },
        )?;
        rewrite(
            work.dir(),
            done,
            |line: &WorkLine| line.day,
            |line| {
                if rebuilt.contains(&line.run) {
                    return None;
                }
                let mut line = line.clone();
                work.refile(&mut line);
                Some(line)
            },
        )
    }
}

/// What the ledgers say of a run they name: the repo they filed it under, and the day it
/// ended and the hour, as they filed them.
struct Filed {
    repo: String,
    ended: Option<Date>,
    hour: Option<u8>,
}

impl Filed {
    fn under(repo: String) -> Self {
        Self {
            repo,
            ended: None,
            hour: None,
        }
    }
}

/// The log of each run under `state`, by its id: `<repo>/runs/<run>/events.jsonl`.
fn run_logs(state: &Path) -> BTreeMap<RunId, PathBuf> {
    let folders = |dir: &Path| -> Vec<PathBuf> {
        fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(|entry| Some(entry.ok()?.path()))
                    .filter(|path| path.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut logs = BTreeMap::new();
    for repo in folders(state) {
        for run in folders(&repo.join("runs")) {
            let log = run.join(EVENT_LOG_FILE);
            let id = run.file_name().and_then(|name| name.to_str());
            if let Some(id) = id.and_then(|id| RunId::new(id).ok())
                && log.is_file()
            {
                logs.insert(id, log);
            }
        }
    }
    logs
}

/// When the run ended, by its log: its end, else the last thing it logged.
fn ended_at(events: &[RunEvent]) -> Option<OffsetDateTime> {
    let finished = events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Finished { .. }) => Some(event.ts),
        _ => None,
    });
    finished.or_else(|| events.last().map(|event| event.ts))
}

/// Writes each month file in `dir` again: each of its lines as `keep` gives it back, in their
/// order, then the `added` lines of its month. A line that doesn't read, or comes back as it
/// was, keeps its text. A month only `added` has gets a file.
fn rewrite<L: Serialize + DeserializeOwned + PartialEq>(
    dir: &Path,
    added: Vec<L>,
    day_of: fn(&L) -> Date,
    keep: impl Fn(&L) -> Option<L>,
) -> io::Result<()> {
    let mut by_month: BTreeMap<String, Vec<L>> = BTreeMap::new();
    for line in added {
        by_month
            .entry(month_name(day_of(&line)))
            .or_default()
            .push(line);
    }
    let mut months: BTreeSet<String> = by_month.keys().cloned().collect();
    if let Ok(entries) = fs::read_dir(dir) {
        let named = entries.filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            Some(name.strip_suffix(".jsonl")?.to_owned())
        });
        months.extend(named);
    }
    for month in months {
        let added = by_month.remove(&month).unwrap_or_default();
        let path = dir.join(format!("{month}.jsonl"));
        let mut tries = 0;
        loop {
            let had = fs::read_to_string(&path).unwrap_or_default();
            let text = month_text(&had, &added, &keep)?;
            if text == had {
                break;
            }
            let fresh = dir.join(format!(".{month}.jsonl.rebuilt"));
            write_private_file(&fresh, text.as_bytes())?;
            // A run that ended meanwhile added its lines: read the month again with them.
            tries += 1;
            if fs::read_to_string(&path).unwrap_or_default() != had && tries < TRIES {
                continue;
            }
            fs::rename(&fresh, &path)?;
            break;
        }
    }
    Ok(())
}

/// One month file's text after the rebuild, from the text it `had`.
fn month_text<L: Serialize + DeserializeOwned + PartialEq>(
    had: &str,
    added: &[L],
    keep: &impl Fn(&L) -> Option<L>,
) -> io::Result<String> {
    let mut text = String::new();
    for raw in had.lines().filter(|raw| !raw.trim().is_empty()) {
        match serde_json::from_str::<L>(raw) {
            Ok(line) => match keep(&line) {
                Some(kept) if kept == line => text.push_str(raw),
                Some(kept) => {
                    text.push_str(&serde_json::to_string(&kept).map_err(io::Error::other)?)
                }
                None => continue,
            },
            Err(_) => text.push_str(raw),
        }
        text.push('\n');
    }
    for line in added {
        text.push_str(&serde_json::to_string(line).map_err(io::Error::other)?);
        text.push('\n');
    }
    Ok(text)
}

/// The busy mark, taken by one process at a time and given back when dropped.
struct Busy(PathBuf);

impl Busy {
    /// Takes the mark in `dir`, or `None` while another process holds it.
    fn take(dir: &Path) -> io::Result<Option<Self>> {
        let path = dir.join(BUSY);
        let stale = fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .is_ok_and(|at| {
                SystemTime::now()
                    .duration_since(at)
                    .is_ok_and(|age| age > STALE)
            });
        if stale {
            let _ = fs::remove_file(&path);
        }
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => Ok(Some(Self(path))),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(None),
            Err(err) => Err(err),
        }
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
#[path = "rebuild_tests.rs"]
mod tests;
