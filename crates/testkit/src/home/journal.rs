//! Reading runs back from a test's private Argonauts Code home.

use std::cmp::Ordering;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use argonauts_core::base::layout::{repo_state, run_files};
use argonauts_core::runs::events::RunEvent;
use argonauts_core::runs::journal::read_all;
use time::OffsetDateTime;

/// `state/<repo>/runs/<run>` under `home`, oldest first. A run id starts with the second the
/// run began and ends in a random suffix, so runs begun in the same second go by their first
/// event's time, and one with no event yet is the newest of its second.
pub fn run_dirs(home: &Path) -> Vec<PathBuf> {
    let mut runs: Vec<(String, Option<OffsetDateTime>, PathBuf)> = subdirs(&home.join("state"))
        .into_iter()
        .flat_map(|repo| subdirs(&repo.join(repo_state::RUNS)))
        .map(|run| {
            let id = run
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let second = id
                .rsplit_once('-')
                .map_or(id.as_str(), |(second, _)| second);
            (second.to_owned(), started_at(&run), run)
        })
        .collect();
    runs.sort_by(|(a_second, a_start, a), (b_second, b_start, b)| {
        let started = match (a_start, b_start) {
            (Some(a), Some(b)) => a.cmp(b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        a_second
            .cmp(b_second)
            .then(started)
            .then_with(|| a.file_name().cmp(&b.file_name()))
    });
    runs.into_iter().map(|(_, _, run)| run).collect()
}

/// The newest run, as [`run_dirs`] orders them.
pub fn latest_run(home: &Path) -> Option<PathBuf> {
    run_dirs(home).pop()
}

/// The time of the run's first event, from the journal's first line alone.
fn started_at(run_dir: &Path) -> Option<OffsetDateTime> {
    let file = fs::File::open(run_dir.join(run_files::EVENTS)).ok()?;
    let mut first = String::new();
    BufReader::new(file).read_line(&mut first).ok()?;
    serde_json::from_str::<RunEvent>(&first)
        .ok()
        .map(|event| event.ts)
}

/// Torn lines are skipped, as the run host skips them.
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
    use argonauts_core::runs::events::RunEventKind;
    use argonauts_core::runs::journal::{Journal, RunLock};
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
                .append(
                    &RunId::new(run).unwrap(),
                    None,
                    None,
                    "argonauts-code",
                    note,
                )
                .unwrap();
        }
        assert_eq!(run_dirs(dir.path()).len(), 2);
        let latest = latest_run(dir.path()).unwrap();
        assert!(latest.ends_with("run-02"));
        assert_eq!(events(&latest).unwrap().len(), 1);
    }

    #[test]
    fn runs_begun_in_one_second_go_by_their_first_event() {
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("state/repo-abc/runs");
        // The later run's random suffix sorts first.
        for run in ["run-20261002-031638-6b34", "run-20261002-031638-1fb0"] {
            let run_dir = runs.join(run);
            let lock = RunLock::try_acquire(&run_dir).unwrap().unwrap();
            let mut journal = Journal::open(&run_dir.join(run_files::EVENTS), &lock).unwrap();
            let note = RunEventKind::Note(Note {
                level: NoteLevel::Info,
                text: run.to_owned(),
            });
            journal
                .append(
                    &RunId::new(run).unwrap(),
                    None,
                    None,
                    "argonauts-code",
                    note,
                )
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        fs::create_dir_all(runs.join("run-20261002-031638-0000")).unwrap();
        let names: Vec<String> = run_dirs(dir.path())
            .iter()
            .map(|run| run.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [
                "run-20261002-031638-6b34",
                "run-20261002-031638-1fb0",
                "run-20261002-031638-0000"
            ]
        );
    }
}
