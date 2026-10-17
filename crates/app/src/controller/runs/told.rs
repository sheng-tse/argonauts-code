//! What a start says about runs that can't go on by themselves, each once. A run written by
//! an older Argonauts Code can't be opened at all, and a run whose run host died goes on with
//! `/resume`. Either is said at the first start after it happened and then lives in Status,
//! so the same line never comes back with every session.

use std::path::Path;

use argonauts_core::NoteLevel;
use argonauts_core::api::{RunState, RunSummary};
use argonauts_core::paths::write_private_file;
use time::format_description::well_known::Rfc3339;

use crate::client;

/// In a run's folder once its state was said, holding when the run last changed: a run that
/// stops again after a resume is said again.
const TOLD: &str = "told";

/// The line for `run`, when it has one and it wasn't said before. Saying it is remembered in
/// `run_dir`, the run's folder.
pub(crate) fn once(run_dir: &Path, run: &RunSummary) -> Option<(NoteLevel, String)> {
    let line = line(run)?;
    let stamp = run.updated.format(&Rfc3339).ok()?;
    let marker = run_dir.join(TOLD);
    if std::fs::read_to_string(&marker).is_ok_and(|told| told.trim() == stamp) {
        return None;
    }
    // A line that can't be remembered is said again next time, which beats never saying it.
    if let Err(err) = write_private_file(&marker, stamp.as_bytes()) {
        tracing::warn!("remembering {}: {err}", marker.display());
    }
    Some(line)
}

/// What a start says of `run`. A run that goes or waits says nothing here, and neither does
/// one that never started, which lost nothing.
fn line(run: &RunSummary) -> Option<(NoteLevel, String)> {
    let title = run.name.as_deref().unwrap_or(&run.title);
    match run.state {
        RunState::FromAnotherBuild => Some((
            NoteLevel::Info,
            client::from_another_build_line(run, "opened"),
        )),
        RunState::Crashed => Some((NoteLevel::Warn, format!("\"{title}\" {}", run.stopped_at))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::run::RunMode;
    use argonauts_core::{RunId, Spend};
    use time::macros::datetime;

    use super::*;

    fn run(state: RunState, title: &str, stopped_at: &str) -> RunSummary {
        RunSummary {
            run: RunId::new("run-20261004-072329-63c3").unwrap(),
            title: title.to_owned(),
            name: None,
            mode: RunMode::Captain,
            workflow: String::new(),
            state,
            stopped_at: stopped_at.to_owned(),
            updated: datetime!(2026-10-04 07:23:29 UTC),
            spent: Spend::default(),
            sessions_to_resume: 0,
            waiting: Vec::new(),
            live: false,
            remind: false,
            shape_known: true,
        }
    }

    /// The row of the picture that starts with `first`: what a start says here is
    /// compared with it whole.
    fn pictured(first: &str) -> &'static str {
        include_str!("../../../../tui/tests/golden/old_runs.txt")
            .lines()
            .find(|row| row.starts_with(first))
            .expect("the picture has the row")
    }

    #[test]
    fn a_run_from_an_older_build_is_said_once_and_plainly() {
        let dir = tempfile::tempdir().unwrap();
        let old = run(RunState::FromAnotherBuild, "hi", "");
        let (level, text) = once(dir.path(), &old).unwrap();
        assert_eq!(level, NoteLevel::Info);
        assert_eq!(format!("● {text}"), pictured("● \"hi\" (Oct 4)"));
        assert_eq!(once(dir.path(), &old), None, "the next start says nothing");
    }

    #[test]
    fn a_stopped_run_is_said_once_with_what_resume_does_and_again_if_it_stops_again() {
        let dir = tempfile::tempdir().unwrap();
        let words = "stopped at 3m10s when its run host exited · /resume carries it on from \
                     Review";
        let mut stopped = run(RunState::Crashed, "Add due dates", words);
        let (level, text) = once(dir.path(), &stopped).unwrap();
        assert_eq!(level, NoteLevel::Warn);
        assert_eq!(format!("● {text}"), pictured("● \"Add due dates\" stopped"));
        assert_eq!(once(dir.path(), &stopped), None);

        stopped.updated = datetime!(2026-10-05 09:00:00 UTC);
        assert!(once(dir.path(), &stopped).is_some());
    }

    #[test]
    fn runs_that_go_wait_or_never_started_say_nothing_at_a_start() {
        let dir = tempfile::tempdir().unwrap();
        for state in [
            RunState::Running,
            RunState::Waiting,
            RunState::NeverStarted,
            RunState::Merged,
            RunState::Stopped,
        ] {
            assert_eq!(once(dir.path(), &run(state, "x", "")), None, "{state:?}");
        }
        assert!(!dir.path().join(TOLD).exists());
    }
}
