//! The outcome log: one JSONL file per run in the repo's state folder, appended and never
//! edited. Track records, budgets from history, recipe statistics, the canary, and the workflow
//! examples the lead sees are all derived from it, so deleting what's derived loses nothing.
//!
//! Lines from before the learning crate have no `type` and read as outcomes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use argonauts_core::events::run::RunOutcome;
use argonauts_core::layout::project_files;
use argonauts_core::names::WorkerRef;
use argonauts_core::workflow::{StandardPhase, StepKind};
use argonauts_core::{CardId, RunId};

use crate::error::Result;
use crate::routing::Draw;
use crate::store::Store;
use crate::types::{Assignment, RecipeRef, RunRecord};

/// One line of a run's outcome log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LogLine {
    Prediction(Prediction),
    /// The router's draw for a card, so any assignment can be explained later.
    Draw(Draw),
    Outcome(Outcome),
    Run(RunRecord),
    Reversal(Reversal),
}

/// Probabilities a worker wrote before its card started. Scored, never acted on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prediction {
    pub run: RunId,
    pub card: CardId,
    pub worker: WorkerRef,
    pub role: Predictor,
    pub task_type: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_pass_gates_first_try: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_within_time_cap: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_at_most_two_attempts: Option<f64>,
    /// A reviewer's, for the card it reviews.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_passes_integration: Option<f64>,
    /// An optional point estimate, scored by |ln(actual / predicted)| and never routed on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_minutes: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Predictor {
    Owner,
    Reviewer,
}

/// What the conductor measured when a card ended. The first eight fields are the ones the
/// engine wrote before the learning crate, so old logs still read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub run: RunId,
    pub card: CardId,
    pub worker: WorkerRef,
    pub task_type: String,
    /// Passed every gate on the first try.
    pub first_try: bool,
    pub attempts: u32,
    pub integrated: bool,
    pub spent_usd: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<StepKey>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub at: Option<OffsetDateTime>,
    /// The exact model the harness reported at session start, which catches an alias that
    /// moved to a new model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned: Option<Assignment>,
    /// The probability the router's draw gave this owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assign_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<WorkerRef>,
    /// Approved at its first review, without requested changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_first_review: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within_time_cap: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minutes: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<Difficulty>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<RecipeRef>,
}

/// The kind and standard phase of the step that built a card, so records are kept per step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StepKey {
    pub kind: StepKind,
    pub standard: StandardPhase,
}

/// Simple signals of how hard a card was, so comparisons can account for who got the hard ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Difficulty {
    pub files_changed: u32,
    pub lines_changed: u32,
    pub dependencies: u32,
}

/// You reversed a decision after the run: rejected an auto-approval on spot check, reverted an
/// auto-merge, or closed a proposal. Counts against the agents involved and trips a canary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reversal {
    pub run: RunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<CardId>,
    /// One line, such as "reverted the auto-merge".
    pub what: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// How many runs and how many passed, per workflow file, for the lead's examples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkflowStats {
    pub runs: u32,
    pub passed: u32,
}

/// Appends `lines` to the run's log, creating it as needed.
pub fn append(store: &Store, run: &RunId, lines: &[LogLine]) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(store.outcomes_dir())?;
    // One write of whole lines, so a crash leaves at most one line cut short.
    let mut text = String::new();
    for line in lines {
        text.push_str(&serde_json::to_string(line)?);
        text.push('\n');
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_file(store, run))?;
    std::io::Write::write_all(&mut file, text.as_bytes())?;
    Ok(())
}

/// One run's lines, so a retro that runs again appends only what's missing.
pub fn read_run(store: &Store, run: &RunId) -> Result<Vec<LogLine>> {
    let file = run_file(store, run);
    if file.is_file() {
        read_file(&file)
    } else {
        Ok(Vec::new())
    }
}

/// Every run's lines, oldest file first. A line that doesn't parse, such as one cut short by a
/// crash, is skipped with a warning rather than losing the history.
pub fn read_all(store: &Store) -> Result<Vec<LogLine>> {
    let dir = store.outcomes_dir();
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            files.push(path);
        }
    }
    files.sort();
    let mut lines = Vec::new();
    for file in files {
        lines.extend(read_file(&file)?);
    }
    Ok(lines)
}

fn read_file(file: &Path) -> Result<Vec<LogLine>> {
    let text = std::fs::read_to_string(file)?;
    let mut lines = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let parsed = serde_json::from_str::<LogLine>(line)
            .or_else(|_| serde_json::from_str::<Outcome>(line).map(LogLine::Outcome));
        match parsed {
            Ok(parsed) => lines.push(parsed),
            Err(error) => tracing::warn!("skipping line {} of {}: {error}", n + 1, file.display()),
        }
    }
    Ok(lines)
}

/// The run's file.
pub fn run_file(store: &Store, run: &RunId) -> PathBuf {
    store.outcomes_dir().join(format!("{}.jsonl", run.as_str()))
}

/// With `outcomes.share`, copies a finished run's log into `.argonauts/outcomes/` of `worktree`,
/// the branch that lands with the run. Each file belongs to one run and never changes after.
///
/// `None` until the run has finished, which its `run` line marks, so a shared file is never a
/// partial one.
pub fn share(store: &Store, run: &RunId, worktree: &Path) -> Result<Option<PathBuf>> {
    let finished = read_run(store, run)?
        .iter()
        .any(|line| matches!(line, LogLine::Run(record) if &record.run == run));
    if !finished {
        return Ok(None);
    }
    let shared = worktree
        .join(".argonauts")
        .join(project_files::OUTCOMES)
        .join(format!("{}.jsonl", run.as_str()));
    if let Some(dir) = shared.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::copy(run_file(store, run), &shared)?;
    Ok(Some(shared))
}

/// Runs and passes per workflow, keyed by the workflow's name and file SHA-256. A run passed
/// when it merged or opened a pull request and its final checks passed.
pub fn workflow_stats(lines: &[LogLine]) -> BTreeMap<(String, String), WorkflowStats> {
    // A retro that runs again may repeat a run line; the newest one stands.
    let mut runs: BTreeMap<&RunId, &RunRecord> = BTreeMap::new();
    for line in lines {
        if let LogLine::Run(record) = line {
            runs.insert(&record.run, record);
        }
    }
    let mut stats: BTreeMap<(String, String), WorkflowStats> = BTreeMap::new();
    for record in runs.values() {
        let Some((name, sha)) = &record.workflow else {
            continue;
        };
        let entry = stats.entry((name.clone(), sha.clone())).or_default();
        entry.runs += 1;
        let landed = matches!(record.outcome, RunOutcome::Merged | RunOutcome::PullRequest);
        if landed && record.final_checks_passed {
            entry.passed += 1;
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(card: u32) -> Outcome {
        Outcome {
            run: RunId::new("run-01").unwrap(),
            card: CardId::numbered(card),
            worker: "codex".parse().unwrap(),
            task_type: "backend".to_owned(),
            first_try: true,
            attempts: 1,
            integrated: true,
            spent_usd: 1.5,
            step: None,
            at: None,
            model_id: Some("gpt-5.6".to_owned()),
            assigned: Some(Assignment::Exploit),
            assign_p: Some(0.8),
            reviewer: None,
            approved_first_review: None,
            within_time_cap: None,
            minutes: None,
            difficulty: None,
            recipe: None,
        }
    }

    #[test]
    fn outcome_log_reads_old_and_new_lines() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path(), dir.path());
        let run = RunId::new("run-01").unwrap();
        // Lines as the engine wrote them before the learning crate: no type, no new fields,
        // and one with a step added later. Then a line cut short by a crash.
        std::fs::create_dir_all(store.outcomes_dir()).unwrap();
        std::fs::write(
            run_file(&store, &run),
            "{\"run\":\"run-01\",\"card\":\"card-001\",\"worker\":\"claude-code\",\
             \"task_type\":\"docs\",\"first_try\":false,\"attempts\":2,\"integrated\":true,\
             \"spent_usd\":0.4}\n\
             {\"run\":\"run-01\",\"card\":\"card-003\",\"worker\":\"codex/gpt-5.6\",\
             \"task_type\":\"backend\",\"first_try\":true,\"attempts\":1,\"integrated\":true,\
             \"spent_usd\":0.9,\"step\":{\"kind\":\"build\",\"standard\":\"do\"}}\n\
             {\"cut short\n",
        )
        .unwrap();
        append(&store, &run, &[LogLine::Outcome(outcome(2))]).unwrap();
        // New lines carry their type, written by hand as another tool would.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(run_file(&store, &run))
            .unwrap();
        std::io::Write::write_all(
            &mut file,
            b"{\"type\":\"prediction\",\"run\":\"run-01\",\"card\":\"card-002\",\
              \"worker\":\"codex\",\"role\":\"owner\",\"task_type\":\"backend\",\
              \"at\":\"2026-09-30T10:00:00Z\",\"p_pass_gates_first_try\":0.7}\n\
              {\"type\":\"reversal\",\"run\":\"run-01\",\"what\":\"reverted the auto-merge\",\
              \"at\":\"2026-09-30T12:00:00Z\"}\n",
        )
        .unwrap();

        let lines = read_run(&store, &run).unwrap();
        assert_eq!(lines.len(), 5, "{lines:?}");
        let LogLine::Outcome(old) = &lines[0] else {
            panic!("{lines:?}");
        };
        assert_eq!(old.attempts, 2);
        assert_eq!(old.model_id, None);
        assert_eq!(old.at, None);
        let LogLine::Outcome(stepped) = &lines[1] else {
            panic!("{lines:?}");
        };
        assert_eq!(
            stepped.step,
            Some(StepKey {
                kind: StepKind::Build,
                standard: StandardPhase::Do
            })
        );
        assert_eq!(stepped.worker.to_string(), "codex/gpt-5.6");
        assert_eq!(lines[2], LogLine::Outcome(outcome(2)));
        let LogLine::Prediction(prediction) = &lines[3] else {
            panic!("{lines:?}");
        };
        assert_eq!(prediction.role, Predictor::Owner);
        assert_eq!(prediction.p_pass_gates_first_try, Some(0.7));
        assert_eq!(prediction.p_within_time_cap, None);
        assert!(matches!(&lines[4], LogLine::Reversal(reversal) if reversal.card.is_none()));

        // Every run's file, oldest name first.
        let earlier = RunId::new("run-00").unwrap();
        append(&store, &earlier, &[LogLine::Outcome(outcome(9))]).unwrap();
        let all = read_all(&store).unwrap();
        assert_eq!(all.len(), 6);
        assert_eq!(all[0], LogLine::Outcome(outcome(9)));
        assert_eq!(&all[1..], &lines[..]);
    }
}
