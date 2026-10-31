//! The wonders kept in `~/.argonauts/behaviors/wonders.json`: each one risen, which stays when
//! the run it rose from is deleted, and each run's tally, which goes with its run.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use argonauts_core::base::layout::{repo_state, user_files};
use argonauts_core::base::paths::{EVENT_LOG_FILE, write_private_file};
use argonauts_core::behaviors::behavior_world::RisenWonder;
use argonauts_core::{Paths, create_private_dir_all};
use serde::{Deserialize, Serialize};

use super::tally::Tally;

/// Bumped when the file's shape changes in a way an older app can't read.
const STORE_VERSION: u32 = 1;

const WONDERS_FILE: &str = "wonders.json";

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct Kept {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risen: Vec<RisenWonder>,
    /// By `<project folder>/<run>`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub runs: BTreeMap<String, KeptRun>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct KeptRun {
    pub rules: u32,
    /// The log's length when it was read; a log that has grown since is read again.
    pub log_bytes: u64,
    pub tally: Tally,
}

/// One run's log on disk.
pub(super) struct RunLog {
    /// `<project folder>/<run>`, as the store keys it.
    pub key: String,
    pub path: PathBuf,
}

pub(super) struct Store {
    file: PathBuf,
    state: PathBuf,
}

impl Store {
    pub fn of(paths: &Paths) -> Self {
        Self {
            file: paths.home().join(user_files::BEHAVIORS).join(WONDERS_FILE),
            state: paths.home().join("state"),
        }
    }

    /// What is kept, and whether this app may write it back. A file from a newer app is read
    /// but never written over; one that can't be read is moved aside so nothing in it is lost.
    pub fn read(&self) -> (Kept, bool) {
        let bytes = match fs::read(&self.file) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return (Kept::default(), true);
            }
            Err(err) => {
                tracing::warn!(
                    "couldn't read the wonders in {}: {err}",
                    self.file.display()
                );
                return (Kept::default(), false);
            }
        };
        match serde_json::from_slice::<Kept>(&bytes) {
            Ok(kept) if kept.version > STORE_VERSION => (kept, false),
            Ok(kept) => (kept, true),
            Err(err) => {
                let aside = self.file.with_extension("unreadable.json");
                tracing::warn!(
                    "the wonders in {} couldn't be read ({err}), so they were moved to {}",
                    self.file.display(),
                    aside.display()
                );
                let moved = fs::rename(&self.file, &aside);
                (Kept::default(), moved.is_ok())
            }
        }
    }

    /// Written whole under another name first, so a reader never sees half a file.
    pub fn write(&self, kept: &Kept) -> std::io::Result<()> {
        if let Some(dir) = self.file.parent() {
            create_private_dir_all(dir)?;
        }
        let kept = KeptRef {
            version: STORE_VERSION,
            risen: &kept.risen,
            runs: &kept.runs,
        };
        let bytes = serde_json::to_vec(&kept).map_err(std::io::Error::other)?;
        let partial = self
            .file
            .with_extension(format!("json.{}.partial", std::process::id()));
        write_private_file(&partial, &bytes)?;
        fs::rename(&partial, &self.file)
    }

    /// Every run's log in every project, by project and then by run.
    pub fn logs(&self) -> Vec<RunLog> {
        let mut logs = Vec::new();
        for project in folders(&self.state) {
            let Some(project_name) = name_of(&project) else {
                continue;
            };
            for run in folders(&project.join(repo_state::RUNS)) {
                let Some(run_name) = name_of(&run) else {
                    continue;
                };
                logs.push(RunLog {
                    key: format!("{project_name}/{run_name}"),
                    path: run.join(EVENT_LOG_FILE),
                });
            }
        }
        logs
    }
}

#[derive(Serialize)]
struct KeptRef<'a> {
    version: u32,
    #[serde(skip_serializing_if = "<[RisenWonder]>::is_empty")]
    risen: &'a [RisenWonder],
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    runs: &'a BTreeMap<String, KeptRun>,
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
