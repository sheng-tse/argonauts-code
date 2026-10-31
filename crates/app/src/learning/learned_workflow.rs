//! The learned workflow a run uses. The Captain-or-crew choice finds the learned workflow whose
//! `match` fits the task in the main checkout; the run starts with its settings over the workflow
//! (or a canary's candidate version over it, merging as a pull request), and the version and file
//! hash are recorded with the run so its outcomes and its run line name what actually ran.

use std::path::Path;

use argonauts_core::settings::Settings;
use argonauts_crew::engine::replay_terms::{self, ReplayTerms};
use argonauts_crew::run_review::learned_workflow::{
    self as run_learned_workflow, RunLearnedWorkflow,
};
use argonauts_learning::Store;
use argonauts_learning::evolution::Evolution;
use argonauts_learning::learned_workflows::{self, LoadedLearnedWorkflow};
use argonauts_registry::LearnedWorkflowOverlay;

/// The learned workflow for a run, at the version that runs now.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ForRun {
    pub loaded: LoadedLearnedWorkflow,
    /// One of a candidate version's canary runs.
    pub canary: bool,
    /// One of the loop's replays, in an isolated repo, never merged, on these terms.
    pub replay: Option<ReplayTerms>,
}

/// The learned workflow whose `match` fits `task_type` and `labels`, under
/// `captain_or_crew.use_learned`, at the version its runs use now: a canary's, a promotion's that
/// hasn't merged yet, or the one a rollback went back to. The loop keeps those in Argonauts Code's
/// state, not your checkout.
pub(crate) fn matched(
    store: &Store,
    settings: &Settings,
    task_type: &str,
    labels: &[String],
) -> Option<ForRun> {
    if !settings.captain_or_crew.use_learned {
        return None;
    }
    let sets = match learned_workflows::load_all(store) {
        Ok(sets) => sets,
        Err(err) => {
            tracing::warn!("learned workflows unavailable: {err}");
            return None;
        }
    };
    let loaded = learned_workflows::matching(&sets, task_type, labels)?.clone();
    match running(store, &loaded.learned_workflow.name) {
        Ok(Some(running)) => Some(running),
        Ok(None) => Some(ForRun {
            loaded,
            canary: false,
            replay: None,
        }),
        Err(err) => {
            tracing::warn!(
                "the evolution loop's state for {} is unreadable, so the checkout's version runs: {err}",
                loaded.learned_workflow.name
            );
            Some(ForRun {
                loaded,
                canary: false,
                replay: None,
            })
        }
    }
}

/// The version of `name` the loop says runs now, and whether it's a canary's.
fn running(store: &Store, name: &str) -> argonauts_learning::Result<Option<ForRun>> {
    let evolution = Evolution::open(store)?;
    let Some(loaded) = evolution.running(name)? else {
        return Ok(None);
    };
    let canary = evolution
        .canaries()?
        .get(name)
        .is_some_and(|canary| canary.version == loaded.learned_workflow.version);
    Ok(Some(ForRun {
        loaded,
        canary,
        replay: None,
    }))
}

/// The learned workflow's settings as the workflow layer takes them. A learned workflow whose
/// settings can't be read is left out, with a warning, never half-applied.
pub(crate) fn overlay(learned_workflow: &ForRun) -> Option<LearnedWorkflowOverlay> {
    let table = match learned_workflows::settings_overlay(&learned_workflow.loaded) {
        Ok(table) => table,
        Err(err) => {
            tracing::warn!("{} is left out: {err}", learned_workflow.loaded.reference());
            return None;
        }
    };
    Some(LearnedWorkflowOverlay {
        label: format!("learned workflow {}", learned_workflow.loaded.reference()),
        settings: table.to_string(),
        canary: learned_workflow.canary,
        replay_limit_usd: None,
    })
}

/// Records the learned workflow with the run, beside its settings, and a replay's terms with it.
pub(crate) fn record(run_dir: &Path, learned_workflow: &ForRun) -> std::io::Result<()> {
    run_learned_workflow::write(
        run_dir,
        &RunLearnedWorkflow {
            learned_workflow: learned_workflow.loaded.reference(),
            sha256: learned_workflow.loaded.sha256.clone(),
            canary: learned_workflow.canary,
            dir: Some(
                learned_workflows::learned_workflow_dir(&learned_workflow.loaded).to_path_buf(),
            ),
        },
    )?;
    match &learned_workflow.replay {
        Some(terms) => replay_terms::write(run_dir, terms),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use argonauts_core::RunId;
    use argonauts_core::runs::events::run::{Party, RunOutcome};
    use argonauts_core::workflows::steps::task::ProcessChange;
    use argonauts_learning::evolution::{Action, CandidateState};
    use argonauts_learning::{LearnedWorkflowRef, ReplayResult, RunRecord};
    use argonauts_testkit::home::repo::TempRepo;

    use super::*;

    fn api(version: u32) -> LearnedWorkflowRef {
        LearnedWorkflowRef {
            name: "api-feature".to_owned(),
            version,
        }
    }

    fn run(n: usize, learned_workflow: LearnedWorkflowRef, canary: bool) -> RunRecord {
        RunRecord {
            run: RunId::new(format!("run-{n:03}")).unwrap(),
            at: OffsetDateTime::now_utc(),
            task_type: "backend".to_owned(),
            learned_workflow: Some(learned_workflow),
            workflow: None,
            outcome: if canary {
                RunOutcome::PullRequest
            } else {
                RunOutcome::Merged
            },
            stuck_tasks: 0,
            final_checks_passed: true,
            cost_usd: 1.0,
            minutes: 5.0,
            canary,
        }
    }

    /// Ten tuning tasks the child solves and the parent doesn't, and two holdout tasks they
    /// share, three tries each.
    fn winning(parent: &LearnedWorkflowRef, child: &LearnedWorkflowRef) -> Vec<ReplayResult> {
        let mut results = Vec::new();
        for (holdout, tasks) in [(false, 10), (true, 2)] {
            for task in 0..tasks {
                for try_number in 1..=3 {
                    for (learned_workflow, passed) in [(parent, holdout), (child, true)] {
                        results.push(ReplayResult {
                            task: format!("task-{holdout}-{task}"),
                            learned_workflow: learned_workflow.clone(),
                            try_number,
                            passed,
                            cost_usd: 1.0,
                            minutes: 5.0,
                            holdout,
                        });
                    }
                }
            }
        }
        results
    }

    /// Once promoted, a candidate that lives only in the loop's state is what the learned
    /// workflow's runs use, as canary runs, until `canary_runs` of them make it current; a rollback
    /// puts its parent back at once.
    #[test]
    fn runs_use_the_canary_then_the_promoted_version() {
        let repo = TempRepo::new().unwrap();
        let store = Store::new(repo.scratch().join("state"), repo.root());
        repo.write(
            ".argonauts/learned/api-feature/v1.yaml",
            "name: api-feature\nversion: 1\ndescription: HTTP endpoints\nmode: crew\n\
             match: { task_types: [backend] }\ncritique: { rounds: 1 }\n",
        )
        .unwrap();
        repo.commit_all("a learned workflow").unwrap();
        let mut settings = Settings::default();
        settings.skills.export_after_canary = false;
        let evolution = Evolution::open(&store).unwrap();
        let matched_now = || matched(&store, &settings, "backend", &[]).unwrap();
        assert_eq!(matched_now().loaded.reference(), api(1));
        assert!(!matched_now().canary);

        let proposal = ProcessChange {
            setting: "critique.rounds".to_owned(),
            value: "2".to_owned(),
            why: "one round missed the risk".to_owned(),
        };
        evolution
            .after_run(&settings, &run(1, api(1), false), Some(&proposal))
            .unwrap();
        let actions = evolution
            .after_replay(
                &settings,
                "api-feature-v2",
                &winning(&api(1), &api(2)),
                10.0,
            )
            .unwrap();
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, Action::AskPromotion { .. })),
            "{actions:?}"
        );
        evolution
            .decide("api-feature-v2", true, &Party::Rules)
            .unwrap();
        // Your checkout still says v1; the canary's version runs, as a canary.
        assert!(
            !repo
                .root()
                .join(".argonauts/learned/api-feature/v2.yaml")
                .exists()
        );
        let canary = matched_now();
        assert_eq!(canary.loaded.reference(), api(2));
        assert!(canary.canary);
        assert_eq!(
            canary.loaded.learned_workflow.settings["critique"]["rounds"],
            2
        );
        let recorded = canary.loaded.path.parent().unwrap().to_path_buf();
        assert_eq!(recorded, evolution.candidate_dir("api-feature-v2"));

        // Each canary run counts, and after `canary_runs` the version is current.
        for n in 0..settings.evolution.canary_runs as usize {
            assert!(matched_now().canary);
            evolution
                .after_run(&settings, &run(10 + n, api(2), true), None)
                .unwrap();
        }
        assert_eq!(
            evolution
                .candidate("api-feature-v2")
                .unwrap()
                .unwrap()
                .state,
            CandidateState::Current
        );
        let current = matched_now();
        assert_eq!(current.loaded.reference(), api(2));
        assert!(!current.canary);

        // Rolled back by you, the version before runs again.
        evolution
            .rollback("api-feature", "rolled back by you")
            .unwrap();
        assert_eq!(matched_now().loaded.reference(), api(1));
    }

    /// Closing a promoted candidate from /learned counts against the run whose run review proposed
    /// it, in the outcome log, and its parent runs again at once. A candidate that isn't
    /// promoted has nothing to close.
    #[test]
    fn closing_a_promotion_rolls_back_and_counts_against_it() {
        use argonauts_core::app::views::LearningCommand;
        use argonauts_learning::outcomes::{self, LogLine};

        use crate::learning::act::{Asker, act};

        let repo = TempRepo::new().unwrap();
        let env = crate::startup::env::AppEnv::for_repo(
            argonauts_core::Paths::new(repo.home()),
            repo.root(),
            false,
        )
        .unwrap();
        let store = crate::learning::store(&env);
        repo.write(
            ".argonauts/learned/api-feature/v1.yaml",
            "name: api-feature\nversion: 1\ndescription: HTTP endpoints\nmode: crew\n\
             match: { task_types: [backend] }\ncritique: { rounds: 1 }\n",
        )
        .unwrap();
        repo.commit_all("a learned workflow").unwrap();
        let mut settings = Settings::default();
        settings.skills.export_after_canary = false;
        let evolution = Evolution::open(&store).unwrap();
        let proposal = ProcessChange {
            setting: "critique.rounds".to_owned(),
            value: "2".to_owned(),
            why: "one round missed the risk".to_owned(),
        };
        let proposer = run(1, api(1), false);
        evolution
            .after_run(&settings, &proposer, Some(&proposal))
            .unwrap();
        let close = LearningCommand::Close {
            candidate: "api-feature-v2".to_owned(),
        };
        let here = Asker { at_terminal: true };
        assert!(act(&env, &settings, &close, here).is_err());

        evolution
            .after_replay(
                &settings,
                "api-feature-v2",
                &winning(&api(1), &api(2)),
                10.0,
            )
            .unwrap();
        evolution
            .decide("api-feature-v2", true, &Party::Rules)
            .unwrap();
        assert_eq!(
            matched(&store, &settings, "backend", &[])
                .unwrap()
                .loaded
                .reference(),
            api(2)
        );
        let said = act(&env, &settings, &close, here).unwrap();
        assert!(said.contains("api-feature"), "{said}");
        assert!(matches!(
            evolution
                .candidate("api-feature-v2")
                .unwrap()
                .unwrap()
                .state,
            CandidateState::RolledBack { .. }
        ));
        assert_eq!(
            matched(&store, &settings, "backend", &[])
                .unwrap()
                .loaded
                .reference(),
            api(1)
        );
        let reversals: Vec<_> = outcomes::read_all(&store)
            .unwrap()
            .into_iter()
            .filter_map(|line| match line {
                LogLine::Reversal(reversal) => Some(reversal),
                _ => None,
            })
            .collect();
        assert_eq!(reversals.len(), 1);
        assert_eq!(reversals[0].run, proposer.run);
        assert!(reversals[0].what.contains("closed"), "{reversals:?}");
    }
}
