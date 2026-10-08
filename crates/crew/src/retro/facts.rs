//! What happened to each card, read from the folded state: the facts the retro scores
//! predictions against, logs as outcomes, and digests for the `Retro` step.

use std::collections::BTreeMap;

use time::OffsetDateTime;

use argonauts_core::events::card::{CardKind, CardState, ReviewEvent};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::steps::card::{Predictions, RetroInput};
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::{CardId, RunId};
use argonauts_learning::routing::Draw;
use argonauts_learning::{Assignment, RecipeRef};

use super::brier::Happened;
use super::outcomes::{CardStep, Difficulty, OutcomeRecord, StepKey};
use crate::divide::{assign, predictions};
use crate::state::RunState;

#[derive(Debug, Clone, PartialEq)]
pub struct CardFacts {
    pub card: CardId,
    pub title: String,
    pub worker: Option<WorkerRef>,
    pub task_type: String,
    pub step: Option<StepKey>,
    pub predictions: Option<Predictions>,
    pub state: CardState,
    pub attempts: u32,
    /// Its first gate run passed: gates ran and none ever failed.
    pub first_try: bool,
    pub integrated: bool,
    /// Every session on the card, reviewers' included.
    pub spent_usd: f64,
    /// The owner's sessions only.
    pub owner_minutes: f64,
    pub time_cap_min: f64,
    pub handoffs: u32,
    pub failed_gates: Vec<String>,
    /// The exact model the owner's harness reported, from its newest session that did.
    pub model_id: Option<String>,
    /// Who reviewed it first, and whether that first review approved.
    pub reviewer: Option<WorkerRef>,
    pub approved_first_review: Option<bool>,
    pub difficulty: Difficulty,
}

/// What the retro adds to every card's outcome: the router's draws, the run's recipe, and when.
#[derive(Debug, Clone, Default)]
pub struct RunContext {
    pub draws: BTreeMap<CardId, Draw>,
    pub recipe: Option<RecipeRef>,
    pub at: Option<OffsetDateTime>,
}

impl CardFacts {
    /// Whether it ran at all. Cards that never started have nothing to score or learn from.
    pub fn ran(&self) -> bool {
        self.attempts > 0 && self.worker.is_some()
    }

    pub fn happened(&self) -> Happened {
        Happened {
            passed_gates_first_try: self.first_try,
            within_time_cap: self.owner_minutes <= self.time_cap_min,
            at_most_two_attempts: self.attempts <= 2,
        }
    }

    /// The card's outcome, measured by the conductor. An owner other than the one the router
    /// drew took the card over, so it counts as a fallback and never toward its first-pass rate.
    pub fn record(&self, run: &RunId, context: &RunContext) -> Option<OutcomeRecord> {
        let worker = self.worker.clone()?;
        let (assigned, assign_p) = match context.draws.get(&self.card) {
            Some(draw) if draw.chosen == worker => (Some(draw.reason), Some(draw.probability)),
            Some(_) => (Some(Assignment::Fallback), None),
            None => (None, None),
        };
        Some(OutcomeRecord {
            run: run.clone(),
            card: self.card.clone(),
            worker,
            task_type: self.task_type.clone(),
            first_try: self.first_try,
            attempts: self.attempts,
            integrated: self.integrated,
            spent_usd: self.spent_usd,
            step: self.step,
            at: context.at,
            model_id: self.model_id.clone(),
            assigned,
            assign_p,
            reviewer: self.reviewer.clone(),
            approved_first_review: self.approved_first_review,
            within_time_cap: Some(self.owner_minutes <= self.time_cap_min),
            minutes: Some(self.owner_minutes),
            difficulty: Some(self.difficulty),
            recipe: context.recipe.clone(),
        })
    }
}

/// Every card but spikes, which are experiments and never integrate.
pub fn card_facts(state: &RunState, settings: &Settings) -> Vec<CardFacts> {
    let run_task_type = state
        .run
        .started
        .as_ref()
        .and_then(|started| started.estimate.as_ref())
        .map(|estimate| estimate.task_type.as_str());
    let graph_predictions = |card: &CardId| {
        state
            .divide
            .graph
            .as_ref()?
            .cards
            .iter()
            .find(|spec| &spec.id == card)?
            .predictions
    };
    state
        .cards
        .by_id
        .values()
        .filter(|record| !matches!(record.spec.kind, CardKind::Spike { .. }))
        .map(|record| {
            let id = &record.spec.id;
            let sessions = state
                .sessions
                .by_key
                .values()
                .filter(|session| session.card.as_ref() == Some(id));
            let (mut spent_usd, mut owner_ms) = (0.0, 0u64);
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
                    owner_ms += end.duration_ms;
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
            CardFacts {
                card: id.clone(),
                title: record.spec.title.clone(),
                worker: record.owner.clone().or_else(|| record.spec.owner.clone()),
                task_type: assign::task_type(&record.spec.kind, run_task_type),
                step: StepKey::of_card(state, id),
                predictions: record.spec.predictions.or_else(|| graph_predictions(id)),
                state: record.state,
                attempts: record.attempts,
                first_try: !record.gates.is_empty() && record.gates.iter().all(|gate| gate.passed),
                integrated: record.state == CardState::Integrated,
                spent_usd,
                owner_minutes: owner_ms as f64 / 60_000.0,
                time_cap_min: predictions::time_cap_min(&record.spec, settings),
                handoffs: record.handoffs,
                failed_gates: record
                    .gates
                    .iter()
                    .filter(|gate| !gate.passed)
                    .map(|gate| gate.name.clone())
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

/// The code-built digest the `Retro` step reads.
pub fn digest(facts: &[CardFacts], settings: &Settings) -> RetroInput {
    let ran: Vec<&CardFacts> = facts.iter().filter(|fact| fact.ran()).collect();
    let integrated = ran.iter().filter(|fact| fact.integrated).count();
    let first_try = ran.iter().filter(|fact| fact.first_try).count();
    let spent: f64 = facts.iter().map(|fact| fact.spent_usd).sum();
    let summary = format!(
        "{} tasks ran: {integrated} merged into the run, {first_try} passed their checks on the first try. \
         ${spent:.2} spent on tasks.",
        ran.len()
    );
    let mut trouble = Vec::new();
    for fact in facts {
        if fact.attempts > 1 {
            trouble.push(format!("{} took {} attempts.", fact.card, fact.attempts));
        }
        if !fact.failed_gates.is_empty() {
            trouble.push(format!(
                "{} failed checks: {}.",
                fact.card,
                fact.failed_gates.join(", ")
            ));
        }
        if fact.handoffs > 0 {
            trouble.push(format!(
                "{} was handed off {} times.",
                fact.card, fact.handoffs
            ));
        }
        if !fact.integrated {
            trouble.push(format!(
                "{} ended {:?}, not merged into the run.",
                fact.card, fact.state
            ));
        }
    }
    RetroInput {
        summary,
        trouble,
        evolvable: settings.evolution.evolvable.clone(),
    }
}
