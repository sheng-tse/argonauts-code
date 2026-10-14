//! The settings reloader the approval policy calls at each approval: the three files as they
//! are now, with the trust `trusted.json` holds now, plus exactly the overrides `start.json`
//! recorded, resolved with the `interactive` flag recorded beside them.

use std::sync::{Arc, Mutex, PoisonError};

use argonauts_core::engine::EventSink;
use argonauts_core::events::RunEventKind;
use argonauts_core::{Note, NoteLevel};
use argonauts_policy::SettingsReloader;
use argonauts_registry::resolve;

use crate::app_settings::sources;
use crate::env::AppEnv;
use crate::run_files::StartRecord;
use crate::trust::TrustStore;

/// A reload that fails returns `None`, and the policy keeps its earlier values. The run's log
/// says so once for each distinct failure.
pub(crate) fn reloader(
    env: &AppEnv,
    record: &StartRecord,
    sink: Arc<dyn EventSink>,
) -> SettingsReloader {
    let env = env.clone();
    let overrides = record.overrides.clone();
    let interactive = record.interactive;
    let replay = record.replay;
    let noted = Mutex::new(None::<String>);
    Arc::new(move || {
        let trust = TrustStore::load(&env.state_dir);
        let sources = sources(&env, &trust, overrides.clone(), interactive);
        let resolved = resolve(&sources);
        let mut noted = noted.lock().unwrap_or_else(PoisonError::into_inner);
        match resolved {
            Ok(resolved) => {
                *noted = None;
                let mut settings = resolved.settings;
                if replay {
                    crate::learning::replay::replay_approvals(&mut settings);
                }
                Some(settings)
            }
            Err(err) => {
                let failure = err.to_string();
                if noted.as_deref() != Some(failure.as_str()) {
                    note_failure(sink.as_ref(), &failure);
                    *noted = Some(failure);
                }
                None
            }
        }
    })
}

fn note_failure(sink: &dyn EventSink, failure: &str) {
    let text = format!(
        "Your settings didn't load at this approval, so the run keeps its earlier approval \
         settings until they do: {failure}"
    );
    let kind = RunEventKind::Note(Note {
        level: NoteLevel::Warn,
        text: text.clone(),
    });
    if let Err(err) = sink.record(None, None, super::SOURCE, kind) {
        tracing::warn!("{text} (and recording that failed: {err})");
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::api::StartRun;
    use argonauts_core::events::run::RunMode;
    use argonauts_core::{Paths, RunId};
    use argonauts_testkit::TempRepo;
    use argonauts_testkit::services::MemorySink;
    use time::OffsetDateTime;

    use super::*;

    fn record(env: &AppEnv, overrides: &[&str], interactive: bool) -> StartRecord {
        StartRecord {
            run: RunId::new("run-20260930-010000-0001").unwrap(),
            request: StartRun {
                request: "tidy the readme".to_owned(),
                mode: RunMode::Captain,
                forced: true,
                estimate: None,
                captain_or_crew: None,
                conversation_turn: None,
                continues: None,
                overrides: Vec::new(),
                include_uncommitted: false,
                main_agent: None,
                approval_mode: None,
                outside_agent: None,
                instructions: Vec::new(),
                request_settings: Vec::new(),
                required_steps: Vec::new(),
            },
            overrides: overrides.iter().map(|text| (*text).to_owned()).collect(),
            interactive,
            replay: false,
            repo_root: env.repo_root.clone(),
            created: OffsetDateTime::now_utc(),
        }
    }

    /// A replay's goal and plan approvals follow your rules at every reload, whatever your
    /// files say, so the stricter of the two never puts them back in your hands. Your other
    /// approval settings reload as they are.
    #[test]
    fn a_replays_reload_keeps_goal_and_plan_to_your_rules() {
        use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
        let repo = TempRepo::new().unwrap();
        let env = AppEnv::for_repo(Paths::new(repo.home()), repo.root(), false).unwrap();
        let mut recorded = record(&env, &[], false);
        let sink = Arc::new(MemorySink::new(recorded.run.clone()));
        let yours = reloader(&env, &recorded, sink.clone())().unwrap();
        // Your Understand approval also asks a reviewer model; a replay's uses rules only.
        assert_eq!(
            (
                yours.approvals.understand.mode,
                yours.approvals.understand.decide_with
            ),
            (ApprovalMode::Auto, DecideWith::RulesAndModel)
        );
        recorded.replay = true;
        let replay = reloader(&env, &recorded, sink)().unwrap();
        for (mode, with) in [
            (
                replay.approvals.understand.mode,
                replay.approvals.understand.decide_with,
            ),
            (
                replay.approvals.plan.mode,
                replay.approvals.plan.decide_with,
            ),
        ] {
            assert_eq!((mode, with), (ApprovalMode::Auto, DecideWith::Rules));
        }
        assert_eq!(replay.approvals.merge, yours.approvals.merge);
        assert_eq!(replay.approvals.stuck, yours.approvals.stuck);
    }

    #[test]
    fn settings_reloader_rereads_files_and_recorded_overrides() {
        let repo = TempRepo::new().unwrap();
        let env = AppEnv::for_repo(Paths::new(repo.home()), repo.root(), false).unwrap();
        let recorded = record(
            &env,
            &["runs.remind_waiting_days=3", "limits.spend_per_run=5.0"],
            true,
        );
        let sink = Arc::new(MemorySink::new(recorded.run.clone()));
        let reload = reloader(&env, &recorded, sink.clone());

        let first = reload().unwrap();
        assert_eq!(first.runs.remind_waiting_days, 3);
        assert_eq!(
            first.limits.spend.spend_per_run,
            Some(5.0),
            "a safety override recorded as vetted still resolves"
        );

        std::fs::write(
            env.global_config(),
            "[run_host]\nwait_grace = \"45s\"\n\n[runs]\nremind_waiting_days = 9\n",
        )
        .unwrap();
        let second = reload().unwrap();
        assert_eq!(
            second.run_host.wait_grace.0, "45s",
            "a file change shows up"
        );
        assert_eq!(
            second.runs.remind_waiting_days, 3,
            "the recorded override still wins over the file"
        );
        assert_eq!(second.limits.spend.spend_per_run, Some(5.0));

        assert!(sink.events().is_empty());

        let unvetted = reloader(
            &env,
            &record(&env, &["limits.spend_per_run=5.0"], false),
            sink.clone(),
        );
        assert!(
            unvetted().is_none(),
            "an unvetted safety override fails, so the run keeps its start values"
        );
        assert!(unvetted().is_none());
        let notes: Vec<_> = sink
            .kinds()
            .into_iter()
            .filter_map(|kind| match kind {
                RunEventKind::Note(note) => Some(note),
                _ => None,
            })
            .collect();
        assert_eq!(notes.len(), 1, "the same failure is noted once: {notes:?}");
        assert_eq!(notes[0].level, NoteLevel::Warn);
        assert!(
            notes[0].text.contains("keeps its earlier"),
            "{}",
            notes[0].text
        );
    }
}
