//! What happened to each task, read from the folded state: the facts the run review scores
//! predictions against, logs as outcomes, and digests for the `RunReview` step.

use std::collections::BTreeMap;

use time::OffsetDateTime;

use argonauts_core::events::task::{ReviewEvent, TaskKind, TaskState};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::Settings;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::steps::task::{Predictions, RunReviewInput};
use argonauts_core::{RunId, TaskId};
use argonauts_learning::routing::Draw;
use argonauts_learning::{Assignment, LearnedWorkflowRef};

use super::brier::Happened;
use super::outcomes::{Difficulty, OutcomeRecord, StepKey, TaskStep};
use crate::divide::{assign, predictions};
use crate::state::RunState;

#[derive(Debug, Clone, PartialEq)]
pub struct TaskFacts {
    pub task: TaskId,
    pub title: String,
    pub worker: Option<ModelRef>,
    pub task_type: String,
    pub step: Option<StepKey>,
    pub predictions: Option<Predictions>,
    pub state: TaskState,
    pub tries: u32,
    /// Its first check run passed: checks ran and none ever failed.
    pub first_try: bool,
    pub merged: bool,
    /// Every session on the task, reviewers' included.
    pub spent_usd: f64,
    /// The worker's sessions only.
    pub worker_minutes: f64,
    pub time_cap_min: f64,
    pub fallbacks: u32,
    pub failed_checks: Vec<String>,
    /// The exact model the worker's CLI reported, from its newest session that did.
    pub model_id: Option<String>,
    /// Who reviewed it first, and whether that first review approved.
    pub reviewer: Option<ModelRef>,
    pub approved_first_review: Option<bool>,
    pub difficulty: Difficulty,
}

/// What the run review adds to every task's outcome: the router's draws, the run's learned
/// workflow, and when.
#[derive(Debug, Clone, Default)]
pub struct RunContext {
    pub draws: BTreeMap<TaskId, Draw>,
    pub learned_workflow: Option<LearnedWorkflowRef>,
    pub at: Option<OffsetDateTime>,
}

impl TaskFacts {
    /// Whether it ran at all. Tasks that never started have nothing to score or learn from.
    pub fn ran(&self) -> bool {
        self.tries > 0 && self.worker.is_some()
    }

    pub fn happened(&self) -> Happened {
        Happened {
            passed_checks_first_try: self.first_try,
            within_time_cap: self.worker_minutes <= self.time_cap_min,
            at_most_two_tries: self.tries <= 2,
        }
    }

    /// The task's outcome, measured by Argonauts Code. A worker other than the one the router
    /// drew built the task, so it counts as a fallback and never toward its first-pass rate.
    pub fn record(&self, run: &RunId, context: &RunContext) -> Option<OutcomeRecord> {
        let worker = self.worker.clone()?;
        let (assigned, assign_p) = match context.draws.get(&self.task) {
            Some(draw) if draw.chosen == worker => (Some(draw.reason), Some(draw.probability)),
            Some(_) => (Some(Assignment::Fallback), None),
            None => (None, None),
        };
        Some(OutcomeRecord {
            run: run.clone(),
            task: self.task.clone(),
            worker,
            task_type: self.task_type.clone(),
            first_try: self.first_try,
            tries: self.tries,
            merged: self.merged,
            spent_usd: self.spent_usd,
            step: self.step,
            at: context.at,
            model_id: self.model_id.clone(),
            assigned,
            assign_p,
            reviewer: self.reviewer.clone(),
            approved_first_review: self.approved_first_review,
            within_time_cap: Some(self.worker_minutes <= self.time_cap_min),
            minutes: Some(self.worker_minutes),
            difficulty: Some(self.difficulty),
            learned_workflow: context.learned_workflow.clone(),
        })
    }
}

/// Every task but experiments, which never merge.
pub fn task_facts(state: &RunState, settings: &Settings) -> Vec<TaskFacts> {
    let run_task_type = state
        .run
        .started
        .as_ref()
        .and_then(|started| started.estimate.as_ref())
        .map(|estimate| estimate.task_type.as_str());
    let graph_predictions = |task: &TaskId| {
        state
            .divide
            .graph
            .as_ref()?
            .tasks
            .iter()
            .find(|spec| &spec.id == task)?
            .predictions
    };
    state
        .tasks
        .by_id
        .values()
        .filter(|record| !matches!(record.spec.kind, TaskKind::Experiment { .. }))
        .map(|record| {
            let id = &record.spec.id;
            let sessions = state
                .sessions
                .by_key
                .values()
                .filter(|session| session.task.as_ref() == Some(id));
            let (mut spent_usd, mut worker_ms) = (0.0, 0u64);
            let mut model_id = None;
            for session in sessions {
                if session.started.role == RoleName::Worker
                    && let Some(model) = state.sessions.models.get(&session.key)
                {
                    model_id = Some(model.clone());
                }
                let Some(end) = &session.end else { continue };
                spent_usd += end.spend.cost_usd;
                if session.started.role == RoleName::Worker {
                    worker_ms += end.duration_ms;
                }
            }
            let first_review = record.reviews.iter().find_map(|review| match review {
                ReviewEvent::Reviewed {
                    reviewer, review, ..
                } => Some((reviewer.clone(), review.verdict == ReviewVerdict::Approve)),
                _ => None,
            });
            let difficulty = Difficulty {
                files_changed: record.commits.iter().map(|c| c.stat.files_changed).sum(),
                lines_changed: record
                    .commits
                    .iter()
                    .map(|c| c.stat.insertions + c.stat.deletions)
                    .sum(),
                dependencies: u32::try_from(record.spec.depends_on.len()).unwrap_or(u32::MAX),
            };
            TaskFacts {
                task: id.clone(),
                title: record.spec.title.clone(),
                worker: record.worker.clone().or_else(|| record.spec.worker.clone()),
                task_type: assign::task_type(&record.spec.kind, run_task_type),
                step: StepKey::of_task(state, id),
                predictions: record.spec.predictions.or_else(|| graph_predictions(id)),
                state: record.state,
                tries: record.tries,
                first_try: !record.checks.is_empty()
                    && record.checks.iter().all(|check| check.passed),
                merged: record.state == TaskState::Merged,
                spent_usd,
                worker_minutes: worker_ms as f64 / 60_000.0,
                time_cap_min: predictions::time_cap_min(&record.spec, settings),
                fallbacks: record.fallbacks,
                failed_checks: record
                    .checks
                    .iter()
                    .filter(|check| !check.passed)
                    .map(|check| check.name.clone())
                    .collect(),
                model_id,
                reviewer: first_review
                    .as_ref()
                    .map(|(reviewer, _)| reviewer.clone())
                    .or_else(|| record.spec.reviewers.first().cloned()),
                approved_first_review: first_review.map(|(_, approved)| approved),
                difficulty,
            }
        })
        .collect()
}

/// The code-built digest the `RunReview` step reads.
pub fn digest(facts: &[TaskFacts], settings: &Settings) -> RunReviewInput {
    let ran: Vec<&TaskFacts> = facts.iter().filter(|fact| fact.ran()).collect();
    let merged = ran.iter().filter(|fact| fact.merged).count();
    let first_try = ran.iter().filter(|fact| fact.first_try).count();
    let spent: f64 = facts.iter().map(|fact| fact.spent_usd).sum();
    let summary = format!(
        "{} tasks ran: {merged} merged into the run, {first_try} passed their checks on the first try. \
         ${spent:.2} spent on tasks.",
        ran.len()
    );
    let mut trouble = Vec::new();
    for fact in facts {
        if fact.tries > 1 {
            trouble.push(format!("{} took {} tries.", fact.task, fact.tries));
        }
        if !fact.failed_checks.is_empty() {
            trouble.push(format!(
                "{} failed checks: {}.",
                fact.task,
                fact.failed_checks.join(", ")
            ));
        }
        if fact.fallbacks > 0 {
            trouble.push(format!("{} fell back {} times.", fact.task, fact.fallbacks));
        }
        if !fact.merged {
            trouble.push(format!(
                "{} ended {:?}, not merged into the run.",
                fact.task, fact.state
            ));
        }
    }
    RunReviewInput {
        summary,
        trouble,
        evolvable: settings.evolution.evolvable.clone(),
    }
}
