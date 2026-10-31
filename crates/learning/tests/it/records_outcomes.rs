//! The outcome log: appended, shared once a run finishes, and counted per workflow.

use crate::records_support;
use argonauts_core::RunId;
use argonauts_core::runs::events::run::RunOutcome;
use argonauts_learning::Store;
use argonauts_learning::outcomes::{self, LogLine, WorkflowStats};
use argonauts_learning::store::types::RunRecord;

use records_support::{AS_OF, outcome};

fn run_line(
    run: &str,
    workflow: Option<(&str, &str)>,
    run_merged: RunOutcome,
    checks: bool,
) -> LogLine {
    LogLine::Run(RunRecord {
        run: RunId::new(run).unwrap(),
        at: AS_OF,
        task_type: "backend".to_owned(),
        learned_workflow: None,
        workflow: workflow.map(|(name, sha)| (name.to_owned(), sha.to_owned())),
        outcome: run_merged,
        stuck_tasks: 0,
        final_checks_passed: checks,
        cost_usd: 3.0,
        minutes: 20.0,
        canary: false,
    })
}

#[test]
fn outcome_log_appends_and_never_edits() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("state"), dir.path().join("repo"));
    let run = RunId::new("run-01").unwrap();
    let file = outcomes::run_file(&store, &run);

    outcomes::append(&store, &run, &[]).unwrap();
    assert!(!file.exists(), "nothing to append makes no file");

    let first = [
        LogLine::Outcome(outcome(1, "codex", "backend", true, 2.0)),
        LogLine::Outcome(outcome(2, "codex", "backend", false, 1.0)),
    ];
    outcomes::append(&store, &run, &first).unwrap();
    let before = std::fs::read(&file).unwrap();
    assert_eq!(before.iter().filter(|byte| **byte == b'\n').count(), 2);

    let second = [run_line("run-01", None, RunOutcome::Merged, true)];
    outcomes::append(&store, &run, &second).unwrap();
    let after = std::fs::read(&file).unwrap();
    assert!(
        after.starts_with(&before),
        "earlier lines are never rewritten"
    );
    assert!(after.ends_with(b"\n"), "whole lines only");

    let read = outcomes::read_run(&store, &run).unwrap();
    let expected: Vec<LogLine> = first.iter().chain(&second).cloned().collect();
    assert_eq!(read, expected);
    let other = RunId::new("run-02").unwrap();
    assert!(outcomes::read_run(&store, &other).unwrap().is_empty());
}

#[test]
fn outcomes_share_copies_a_finished_run() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("state"), dir.path().join("repo"));
    let worktree = dir.path().join("worktree");
    let run = RunId::new("run-01").unwrap();
    let shared = worktree.join(".argonauts/outcomes/run-01.jsonl");

    assert_eq!(outcomes::share(&store, &run, &worktree).unwrap(), None);
    outcomes::append(
        &store,
        &run,
        &[LogLine::Outcome(outcome(1, "codex", "backend", true, 0.0))],
    )
    .unwrap();
    assert_eq!(
        outcomes::share(&store, &run, &worktree).unwrap(),
        None,
        "a run still going has no run line yet"
    );
    assert!(!shared.exists());

    outcomes::append(
        &store,
        &run,
        &[run_line("run-01", None, RunOutcome::Merged, true)],
    )
    .unwrap();
    assert_eq!(
        outcomes::share(&store, &run, &worktree).unwrap(),
        Some(shared.clone())
    );
    let source = std::fs::read(outcomes::run_file(&store, &run)).unwrap();
    assert_eq!(std::fs::read(&shared).unwrap(), source);
    assert!(
        !store.shared_outcomes_dir().exists(),
        "only the worktree it lands with gets the copy"
    );
}

#[test]
fn workflow_stats_count_runs_and_passes() {
    let api = Some(("api-feature", "aa11"));
    let lines = vec![
        run_line("run-01", api, RunOutcome::Merged, true),
        run_line("run-02", api, RunOutcome::PullRequest, true),
        run_line("run-03", api, RunOutcome::Merged, false),
        run_line("run-04", api, RunOutcome::Abandoned, true),
        run_line(
            "run-05",
            Some(("api-feature", "bb22")),
            RunOutcome::Merged,
            true,
        ),
        run_line("run-06", None, RunOutcome::Merged, true),
        LogLine::Outcome(outcome(1, "codex", "backend", true, 0.0)),
        // The run review ran twice for run-02; its run counts once.
        run_line("run-02", api, RunOutcome::PullRequest, true),
    ];
    let stats = outcomes::workflow_stats(&lines);
    assert_eq!(stats.len(), 2);
    assert_eq!(
        stats[&("api-feature".to_owned(), "aa11".to_owned())],
        WorkflowStats { runs: 4, passed: 2 }
    );
    assert_eq!(
        stats[&("api-feature".to_owned(), "bb22".to_owned())],
        WorkflowStats { runs: 1, passed: 1 }
    );
}
