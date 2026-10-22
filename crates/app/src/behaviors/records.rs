//! Every action Argonauts Code recorded, read on demand: each run's log under
//! `~/.argonauts/state/<repo>/runs/`, and the main agent's own turns in each conversation's
//! journal. A run's labels are kept in `~/.argonauts/behaviors/labels/` once it has ended, so
//! its log is read once and nothing is written while it goes.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use argonauts_core::app::ConversationEvent;
use argonauts_core::behaviors::LIST_VERSION;
use argonauts_core::events::run::RunLifecycle;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::journal::read_all;
use argonauts_core::layout::repo_state;
use argonauts_core::model_catalog::ModelCatalog;
use argonauts_core::paths::{EVENT_LOG_FILE, write_private_file};
use argonauts_core::{Paths, create_private_dir_all};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::actions::{self, Action};
use super::labeller::RULES_VERSION;
use super::turn_times::TurnTimes;

/// One run's labelled actions as kept, with what they were worked out from.
#[derive(Serialize, Deserialize)]
struct Kept {
    list: u32,
    rules: u32,
    /// The log's length when it was read; a log that has grown since is read again.
    log_bytes: u64,
    actions: Vec<Action>,
}

/// Where the actions are read from and kept.
pub(crate) struct Records {
    state: PathBuf,
    kept: PathBuf,
    catalog: &'static ModelCatalog,
}

impl Records {
    pub fn of(paths: &Paths) -> Self {
        Self {
            state: paths.home().join("state"),
            kept: paths.home().join("behaviors").join("labels"),
            catalog: argonauts_registry::models::shipped_catalog(),
        }
    }

    /// Every action taken at or after `since`, from every project, oldest first.
    pub fn actions(&self, since: Option<OffsetDateTime>) -> Vec<Action> {
        self.read(since, true)
    }

    /// As [`Self::actions`], with each call's input, read from the logs themselves.
    pub fn actions_with_input(&self, since: Option<OffsetDateTime>) -> Vec<Action> {
        self.read(since, false)
    }

    fn read(&self, since: Option<OffsetDateTime>, use_kept: bool) -> Vec<Action> {
        let mut all = Vec::new();
        for repo in folders(&self.state) {
            let Some(repo_id) = name_of(&repo) else {
                continue;
            };
            let mut runs_kept = Vec::new();
            for run in folders(&repo.join("runs")) {
                let Some(run_id) = name_of(&run) else {
                    continue;
                };
                runs_kept.push(format!("{run_id}.json"));
                all.extend(self.run(&repo_id, &run_id, &run.join(EVENT_LOG_FILE), use_kept));
            }
            self.forget_deleted(&repo_id, &runs_kept);
            let root = root_of(&repo.join("runs"));
            all.extend(self.conversations(
                &repo_id,
                &repo.join(repo_state::CONVERSATION),
                root.as_deref(),
            ));
        }
        if let Some(since) = since {
            all.retain(|action| action.at >= since);
        }
        all.sort_by_key(|action| action.at);
        all
    }

    fn run(&self, repo: &str, run: &str, log: &Path, use_kept: bool) -> Vec<Action> {
        let Ok(log_bytes) = fs::metadata(log).map(|meta| meta.len()) else {
            return Vec::new();
        };
        let kept_file = self.kept.join(repo).join(format!("{run}.json"));
        let kept = use_kept
            .then(|| fs::read(&kept_file).ok())
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<Kept>(&bytes).ok())
            .filter(|kept| {
                kept.list == LIST_VERSION
                    && kept.rules == RULES_VERSION
                    && kept.log_bytes == log_bytes
            });
        if let Some(kept) = kept {
            return kept.actions;
        }
        let Ok(read) = read_all(log) else {
            return Vec::new();
        };
        let older = if read.skipped.is_empty() {
            Default::default()
        } else {
            let skipped: BTreeSet<u64> = read.skipped.iter().map(|(line, _)| *line).collect();
            let text = fs::read_to_string(log).unwrap_or_default();
            let lines = (1u64..)
                .zip(text.lines())
                .filter(|(at, _)| skipped.contains(at))
                .map(|(_, line)| line);
            actions::older_starts(lines)
        };
        let mut labelled = actions::of_run(run, &read.events, &older, self.catalog);
        for action in &mut labelled {
            action.repo = repo.to_owned();
        }
        let ended = read
            .events
            .iter()
            .any(|event| matches!(event.kind, RunEventKind::Run(RunLifecycle::Finished { .. })));
        // A read for the calls' inputs, as the Behaviors map's, leaves what's kept alone.
        if ended && use_kept {
            let kept = Kept {
                list: LIST_VERSION,
                rules: RULES_VERSION,
                log_bytes,
                actions: labelled,
            };
            if let Err(err) = keep(&kept_file, &kept) {
                tracing::warn!("couldn't keep the behavior labels of {run}: {err}");
            }
            return kept.actions;
        }
        labelled
    }

    /// A deleted run's labels go with it.
    fn forget_deleted(&self, repo: &str, runs: &[String]) {
        let Ok(entries) = fs::read_dir(self.kept.join(repo)) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".json") && !runs.iter().any(|run| *run == name) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    /// The main agent's own actions in each conversation of the repo, each dated by its turn,
    /// with files judged below `root`, the project's folder, where a run said it.
    fn conversations(&self, repo: &str, dir: &Path, root: Option<&str>) -> Vec<Action> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut all = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|extension| extension != "jsonl")
            {
                continue;
            }
            let Some(conversation) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let times = TurnTimes::read(&path, &text);
            let events: Vec<ConversationEvent> = text
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
            let mut actions =
                actions::of_conversation(conversation, &events, &times, root, self.catalog);
            for action in &mut actions {
                action.repo = repo.to_owned();
            }
            all.extend(actions);
        }
        all
    }
}

/// The project's folder, as the newest of its runs says it in its first lines. A conversation's
/// journal doesn't say it.
fn root_of(runs: &Path) -> Option<String> {
    use std::io::BufRead as _;
    folders(runs).iter().rev().find_map(|run| {
        let file = fs::File::open(run.join(EVENT_LOG_FILE)).ok()?;
        std::io::BufReader::new(file)
            .lines()
            .take(8)
            .map_while(Result::ok)
            .filter(|line| line.contains("\"repo_root\""))
            .filter_map(|line| serde_json::from_str::<RunEvent>(&line).ok())
            .find_map(|event| match event.kind {
                RunEventKind::Run(RunLifecycle::Started(started)) => {
                    Some(started.repo_root.to_string_lossy().into_owned())
                }
                _ => None,
            })
    })
}

/// Written whole under another name first, so a reader never sees half a file.
fn keep(file: &Path, kept: &Kept) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        create_private_dir_all(dir)?;
    }
    let bytes = serde_json::to_vec(kept).map_err(std::io::Error::other)?;
    let partial = file.with_extension("json.partial");
    write_private_file(&partial, &bytes)?;
    fs::rename(&partial, file)
}

fn folders(dir: &Path) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| Some(entry.ok()?.path()))
                .filter(|path| path.is_dir())
                .collect()
        })
        .unwrap_or_default();
    folders.sort();
    folders
}

fn name_of(path: &Path) -> Option<String> {
    path.file_name()?.to_str().map(str::to_owned)
}

/// A repo's folder name without the hash that keeps two folders of one name apart:
/// `council-0123456789ab` is `council`.
pub(crate) fn project_name(repo: &str) -> &str {
    match repo.rsplit_once('-') {
        Some((name, hash))
            if !name.is_empty()
                && hash.len() == 12
                && hash.chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            name
        }
        _ => repo,
    }
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
