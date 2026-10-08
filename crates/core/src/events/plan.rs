//! Planning and dividing events.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::card::CardSpec;
use crate::CardId;
use crate::names::{DecisionId, DraftId, PlanLabel, WorkerRef};
use crate::settings::budget::{BudgetAmount, BudgetLineKind, BudgetUnit};
use crate::settings::protocol::{JudgingMethod, RoutingAlgorithm};
use crate::steps::card::Predictions;
use crate::steps::plan::{CritiquePoint, SpikeProposal, SpikeReport};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum PlanEvent {
    /// A draft was written and sealed. Its text sits in the run's `drafts/` folder, unreadable
    /// to every other session until all drafts are sealed.
    Sealed {
        draft: DraftId,
        label: PlanLabel,
        /// Hidden from judges.
        worker: WorkerRef,
        /// 1 for the first draft, 2 after revision.
        round: u8,
        /// The plan's one-paragraph summary, for the collapsed step.
        summary: String,
        path: PathBuf,
        sha256: String,
    },
    /// The disagreement map, after code checked every quote.
    Mapped {
        /// 1 for the drafts, 2 for the revisions.
        round: u8,
        worker: WorkerRef,
        topics: Vec<MappedTopic>,
    },
    /// No disputed topics: one agent merged the drafts, a judge checked completeness.
    FastPath {
        merged_by: WorkerRef,
    },
    Critiqued {
        critic: WorkerRef,
        /// Whether it's the run's designated critic.
        designated: bool,
        points: Vec<CritiquePoint>,
    },
    SpikeProposed {
        proposal: SpikeProposal,
        /// Code's check against `experiments.limit_usd`.
        fits_budget: bool,
    },
    SpikeFinished {
        card: CardId,
        report: SpikeReport,
    },
    /// One judge's scores for one plan.
    Scored {
        judge: WorkerRef,
        /// Its model family, which must differ from the plan author's.
        judge_family: String,
        plan: PlanLabel,
        /// 0 or 1: the order the plan was shown in, since plans are judged in both orders.
        order: u8,
        /// One score per rubric criterion.
        scores: Vec<f64>,
        rationale: String,
        /// Text addressed to the judges was found in this plan.
        flagged: bool,
    },
    /// The deterministic aggregate.
    Judged(JudgingResult),
    /// A decision record, also written to `.argonauts/decisions/`.
    Decided(DesignDecision),
    /// The coordinator's plan was accepted or rejected by code (`orchestrator`).
    Coordinated {
        coordinator: WorkerRef,
        /// Accepted, or the rule it broke.
        rejection: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MappedTopic {
    pub name: String,
    pub disputed: bool,
    pub positions: Vec<MappedPosition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MappedPosition {
    pub label: PlanLabel,
    /// A few words, such as "Redis".
    pub summary: String,
    pub quote: String,
    /// Whether the quote appears word for word in the sealed draft.
    pub quote_verified: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgingResult {
    pub method: JudgingMethod,
    pub plans: Vec<PlanTally>,
    /// The winner, when the scoring rule finds a clear one.
    pub winner: Option<PlanLabel>,
    pub margin: f64,
    /// A near tie goes to you at the plan checkpoint.
    pub near_tie: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanTally {
    pub label: PlanLabel,
    /// Shown to you only.
    pub author: WorkerRef,
    /// Mean score over counted judges.
    pub mean: f64,
    /// Largest difference between one judge's two orders.
    pub spread: f64,
    pub counted: u32,
    /// Scores dropped, such as a same-family judge's.
    pub dropped: u32,
}

/// A decision record (`D-003`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignDecision {
    pub id: DecisionId,
    pub title: String,
    pub choice: String,
    pub rationale: String,
    pub alternatives: Vec<String>,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DivideEvent {
    /// The task graph built from the plan and checked by code.
    Graph(TaskGraph),
    Owner {
        card: CardId,
        worker: WorkerRef,
        algorithm: RoutingAlgorithm,
        /// Whether this was an exploration pick.
        explored: bool,
        /// One line, such as "9 of 11 similar cards passed, 38% quota used".
        reason: String,
    },
    /// A card's reviewers, from other model families.
    Reviewers {
        card: CardId,
        reviewers: Vec<WorkerRef>,
        /// Whether they're blind to each other.
        blind: bool,
    },
    Predicted {
        card: CardId,
        predictions: Predictions,
    },
    Budget(BudgetPlan),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskGraph {
    /// Every card, contracts and test cards included.
    pub cards: Vec<CardSpec>,
    /// The longest chain, which the phase lead's card sits on.
    pub critical_path: Vec<CardId>,
}

/// The run budget split by line. The lines add up to the total.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetPlan {
    pub lines: Vec<BudgetLine>,
    pub total: BudgetAmount,
    /// An estimate range for the plan checkpoint, such as $9 to $12.
    pub estimate_low: f64,
    pub estimate_high: f64,
}

impl BudgetPlan {
    /// The plan of a run with no limit: no lines, and nothing to add up.
    pub fn unlimited() -> Self {
        Self {
            lines: Vec::new(),
            total: BudgetAmount {
                unit: BudgetUnit::Usd,
                amount: 0.0,
            },
            estimate_low: 0.0,
            estimate_high: 0.0,
        }
    }

    /// Whether the run has no limit, so there is no total to show or to fit into.
    pub fn is_unlimited(&self) -> bool {
        self.lines.is_empty() && self.total.amount == 0.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetLine {
    pub line: BudgetLineKind,
    pub amount: BudgetAmount,
}
