//! Crash anywhere, resume, and nothing happens twice: a full run's log is cut after every
//! event, rebuilt (from its start or from its latest snapshot), and driven on against the repo
//! the full run left behind, as after a crash that came after git moved but before the log did.

use crate::support;
use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::TaskId;
use argonauts_core::runs::engine::EngineExit;
use argonauts_core::runs::events::approval::ApprovalEvent;
use argonauts_core::runs::events::plan::PlanEvent;
use argonauts_core::runs::events::run::{RunLifecycle, RunMode};
use argonauts_core::runs::events::session::SessionEvent;
use argonauts_core::runs::events::shared::BlackboardEvent;
use argonauts_core::runs::events::task::{MergeEvent, TaskEvent};
use argonauts_core::runs::events::workflow::WorkflowEvent;
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::workflows::steps::StepName;
use argonauts_crew::engine::idempotency::task_line;
use argonauts_crew::state::RunState;
use argonauts_testkit::engine::services::FakeServices;

use support::{Crew, names};

const WORKER_FILE: &str = "verbose.txt";

/// The ids an event creates, and the side effects it records. Seeing one of these twice means
/// something happened twice.
fn created(event: &RunEvent) -> Option<String> {
    let on = event
        .task_id
        .as_ref()
        .map_or_else(|| "the run".to_owned(), ToString::to_string);
    match &event.kind {
        RunEventKind::Approval(ApprovalEvent::Opened(opened)) => {
            Some(format!("approval {}", opened.id))
        }
        RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
            Some(format!("decision on {}", decided.id))
        }
        RunEventKind::Plan(PlanEvent::Sealed { draft, .. }) => Some(format!("draft {draft}")),
        RunEventKind::Plan(PlanEvent::Decided(decision)) => {
            Some(format!("decision record {}", decision.id))
        }
        RunEventKind::Blackboard(BlackboardEvent::Posted(message)) => {
            Some(format!("message {}", message.id))
        }
        RunEventKind::Blackboard(BlackboardEvent::Delivered { message, to }) => {
            Some(format!("delivery of {message} to {to}"))
        }
        RunEventKind::Workflow(WorkflowEvent::Started(started)) => {
            Some(format!("workflow {}", started.id))
        }
        RunEventKind::Task(TaskEvent::Committed(commit)) => Some(match &commit.sha {
            Some(sha) => format!("commit {sha}"),
            None => format!("empty commit on {on} for {:?}", commit.message),
        }),
        RunEventKind::Merge(MergeEvent::Merged { task, .. }) => Some(format!("merge of {task}")),
        RunEventKind::Merge(MergeEvent::RunMerged { target, .. }) => {
            Some(format!("landing on {target}"))
        }
        RunEventKind::Merge(MergeEvent::FinalCheck { .. }) => Some("final check".to_owned()),
        RunEventKind::Check(check) => {
            Some(format!("{:?} check {} on {on}", check.kind, check.name))
        }
        RunEventKind::Run(RunLifecycle::Finished { .. }) => Some("finish".to_owned()),
        _ => None,
    }
}

/// What `prefix` already did. A check counts once its task moved on: a crash in the middle of
/// a task's checks runs them all again, but a finished round of them never runs twice.
fn done(prefix: &[RunEvent]) -> BTreeSet<String> {
    let mut done = BTreeSet::new();
    let mut checking: BTreeMap<Option<TaskId>, Vec<String>> = BTreeMap::new();
    for event in prefix {
        match &event.kind {
            RunEventKind::Check(_) => {
                let checks = checking.entry(event.task_id.clone()).or_default();
                checks.extend(created(event));
                continue;
            }
            RunEventKind::Task(TaskEvent::State { .. }) => {
                done.extend(checking.remove(&event.task_id).unwrap_or_default());
            }
            _ => {}
        }
        done.extend(created(event));
    }
    done
}

/// What the run host hands the engine: the log from its latest snapshot on.
fn history_from(prefix: &[RunEvent]) -> Vec<RunEvent> {
    let start = prefix
        .iter()
        .rposition(|event| matches!(event.kind, RunEventKind::Run(RunLifecycle::Snapshot(_))))
        .unwrap_or(0);
    prefix[start..].to_vec()
}

/// A fresh crew over `crew`'s repo: the worker writes a file, so the task commits and merges.
fn enlist(crew: &mut Crew) {
    crew.building_worker("claude-code/opus", "anthropic", WORKER_FILE, "--verbose\n");
    crew.building_worker("codex/gpt-x", "openai", WORKER_FILE, "--verbose\n");
}

/// How many merge commits on the run's branches bring in each task, and how often main moved.
fn merges_and_moves(crew: &Crew, tasks: &BTreeSet<TaskId>) -> anyhow::Result<(Vec<usize>, usize)> {
    let merges = crew.repo.git(&[
        "log",
        "--merges",
        "--format=%H%n%B%x1e",
        "--branches=argonauts/*",
    ])?;
    let per_task = tasks
        .iter()
        .map(|task| {
            let line = task_line(task);
            merges
                .split('\x1e')
                .filter(|entry| entry.lines().any(|text| text.trim_end() == line))
                .count()
        })
        .collect();
    let moves = crew
        .repo
        .git(&["reflog", "show", "--format=%H", "refs/heads/main"])?
        .lines()
        .count()
        .saturating_sub(1);
    Ok((per_task, moves))
}

async fn replay_every_prefix(snapshot_every: u32) -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.run_host.snapshot_every = snapshot_every;
    // Every cut is resumed against the same repository, and the count of merges at the end
    // reads the run's branches, so the run keeps them.
    crew.settings.runs.keep_copies = true;
    enlist(&mut crew);
    let outcomes = crew
        .run_to_end(RunMode::Captain, "add a --verbose flag", 12)
        .await?;
    let last = &outcomes[outcomes.len() - 1];
    assert!(
        matches!(last.exit, EngineExit::Finished(_)),
        "{:?}: {:?}",
        last.exit,
        names(&last.events)
    );
    let full = last.events.clone();
    if snapshot_every > 0 {
        assert!(names(&full).contains(&"run/snapshot".to_owned()));
    }
    let tasks: BTreeSet<TaskId> = full
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Merge(MergeEvent::Merged { task, .. }) => Some(task.clone()),
            _ => None,
        })
        .collect();
    assert!(!tasks.is_empty(), "no task merged: {:?}", names(&full));
    let merged = full.iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Merge(MergeEvent::RunMerged { .. })
        )
    });
    assert!(merged, "the run never merged: {:?}", names(&full));
    let once = (vec![1; tasks.len()], 1);
    assert_eq!(
        merges_and_moves(&crew, &tasks)?,
        once,
        "one merge commit per task and one move of main"
    );

    for cut in 1..full.len() {
        let prefix = &full[..cut];
        let before = RunState::fold(prefix);
        let history = history_from(prefix);
        // Fresh fakes, so every session they see is one the resumed run started.
        crew.services = FakeServices::new();
        enlist(&mut crew);
        let resumed = crew.resume(history.clone(), Vec::new()).await?;
        let after = &resumed.events[history.len()..];

        let mut seen = done(prefix);
        for event in after {
            if let Some(id) = created(event) {
                assert!(
                    seen.insert(id.clone()),
                    "cut at {cut} (snapshots every {snapshot_every}): {id} again, after {:?}",
                    names(after)
                );
            }
            if let RunEventKind::Session(SessionEvent::Started(started)) = &event.kind {
                let key = event.session.as_ref().expect("a session event has its key");
                if let Some(earlier) = before.sessions.get(key) {
                    assert!(
                        earlier.resumable(),
                        "cut at {cut}: {key} ended but started again"
                    );
                    assert_eq!(
                        started.resumed_from, earlier.cli_session,
                        "cut at {cut}: {key} resumes its own agent session"
                    );
                }
            }
        }
        // A crash mid-build resumes the worker's unfinished session under its own key.
        for unfinished in before.sessions.by_key.values().filter(|session| {
            session.end.is_none() && session.started.step == Some(StepName::Build)
        }) {
            let next_build = after.iter().find_map(|event| match &event.kind {
                RunEventKind::Session(SessionEvent::Started(started))
                    if started.step == Some(StepName::Build)
                        && event.task_id == unfinished.task =>
                {
                    event.session.clone()
                }
                _ => None,
            });
            if let Some(next_build) = next_build {
                assert_eq!(
                    next_build, unfinished.key,
                    "cut at {cut}: the build started a new session instead of resuming"
                );
            }
        }
        let fresh: Vec<u32> = after
            .iter()
            .filter_map(|event| event.session.as_ref())
            .filter(|key| before.sessions.get(key).is_none())
            .filter_map(|key| argonauts_crew::state::ids::number(key.as_str()))
            .collect();
        assert!(
            fresh.iter().all(|n| *n > before.ids.sessions),
            "cut at {cut}: a new session reused a number"
        );
    }
    // Every cut resumed against the same repo, so a second merge or landing anywhere shows.
    assert_eq!(
        merges_and_moves(&crew, &tasks)?,
        once,
        "merged or merged again"
    );
    Ok(())
}

#[tokio::test]
async fn replay_from_the_start_repeats_nothing() -> anyhow::Result<()> {
    replay_every_prefix(0).await
}

#[tokio::test]
async fn replay_from_snapshots_repeats_nothing() -> anyhow::Result<()> {
    replay_every_prefix(7).await
}
