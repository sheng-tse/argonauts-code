//! Reading runs back from a test's private Argonauts home.

use std::fs;
use std::path::{Path, PathBuf};

use argonauts_core::events::RunEvent;
use argonauts_core::journal::read_all;
use argonauts_core::layout::{repo_state, run_files};

/// `state/<repo>/runs/<run>` under `home`, sorted by run id.
pub fn run_dirs(home: &Path) -> Vec<PathBuf> {
    let mut runs: Vec<PathBuf> = subdirs(&home.join("state"))
        .into_iter()
        .flat_map(|repo| subdirs(&repo.join(repo_state::RUNS)))
        .collect();
    runs.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    runs
}

/// The newest run by id (run ids start with the time).
pub fn latest_run(home: &Path) -> Option<PathBuf> {
    run_dirs(home).pop()
}

/// Torn lines are skipped, as the supervisor skips them.
pub fn events(run_dir: &Path) -> anyhow::Result<Vec<RunEvent>> {
    Ok(read_all(&run_dir.join(run_files::EVENTS))?.events)
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::RunEventKind;
    use argonauts_core::journal::{Journal, RunLock};
    use argonauts_core::{Note, NoteLevel, RunId};

    #[test]
    fn finds_runs_and_reads_their_events() {
        let dir = tempfile::tempdir().unwrap();
        assert!(latest_run(dir.path()).is_none());
        for run in ["run-01", "run-02"] {
            let run_dir = dir.path().join("state/repo-abc/runs").join(run);
            let lock = RunLock::try_acquire(&run_dir).unwrap().unwrap();
            let mut journal = Journal::open(&run_dir.join(run_files::EVENTS), &lock).unwrap();
            let note = RunEventKind::Note(Note {
                level: NoteLevel::Info,
                text: run.to_owned(),
            });
            journal
                .append(&RunId::new(run).unwrap(), None, None, "conductor", note)
                .unwrap();
        }
        assert_eq!(run_dirs(dir.path()).len(), 2);
        let latest = latest_run(dir.path()).unwrap();
        assert!(latest.ends_with("run-02"));
        assert_eq!(events(&latest).unwrap().len(), 1);
    }
}
