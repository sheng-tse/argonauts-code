//! Planning and dividing events.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::task::TaskSpec;
use crate::TaskId;
use crate::base::names::{DecisionId, DraftId, ModelRef, PlanLabel};
use crate::settings::limit::{LimitAmount, LimitLineKind, LimitUnit};
use crate::settings::protocol::{JudgingMethod, RoutingAlgorithm};
use crate::workflows::steps::plan::{CritiquePoint, ExperimentProposal, ExperimentReport};
use crate::workflows::steps::task::Predictions;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum PlanEvent {
    /// A draft was written and sealed. Its text sits in the run's `drafts/` folder, unreadable
    /// to every other session until all drafts are sealed.
    Sealed {
        draft: DraftId,
        label: PlanLabel,
        /// Hidden from judges.
        worker: ModelRef,
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
        worker: ModelRef,
        topics: Vec<MappedTopic>,
    },
    /// No disputed topics: one agent merged the drafts, a judge checked completeness.
    FastPath { merged_by: ModelRef },
    Critiqued {
        critic: ModelRef,
        /// Whether it's the run's designated critic.
        designated: bool,
        points: Vec<CritiquePoint>,
    },
    ExperimentProposed {
        proposal: ExperimentProposal,
        /// Code's check against `experiments.limit_usd`.
        fits_limit: bool,
    },
    ExperimentFinished {
        task: TaskId,
        report: ExperimentReport,
    },
    /// One judge's scores for one plan.
    Scored {
        judge: ModelRef,
        /// Its model provider, which must not be the plan author's.
        judge_provider: String,
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
    /// A decision record, also written to `decisions/` in the repo's state directory.
    Decided(DesignDecision),
    /// The orchestrator's plan was accepted or rejected by code (`orchestrator`).
    Orchestrated {
        orchestrator: ModelRef,
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
    /// A near tie goes to you at the plan approval.
    pub near_tie: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanTally {
    pub label: PlanLabel,
    /// Shown to you only.
    pub author: ModelRef,
    /// Mean score over counted judges.
    pub mean: f64,
    /// Largest difference between one judge's two orders.
    pub spread: f64,
    pub counted: u32,
    /// Scores dropped, such as a same-provider judge's.
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
    Worker {
        task: TaskId,
        worker: ModelRef,
        algorithm: RoutingAlgorithm,
        /// Whether this was an exploration pick.
        explored: bool,
        /// One line, such as "9 of 11 similar tasks passed, 38% plan usage used".
        reason: String,
    },
    /// A task's reviewers, from other model providers.
    Reviewers {
        task: TaskId,
        reviewers: Vec<ModelRef>,
        /// Whether they're blind to each other.
        blind: bool,
    },
    Predicted {
        task: TaskId,
        predictions: Predictions,
    },
    Limit(LimitPlan),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskGraph {
    /// Every task, contracts and test tasks included.
    pub tasks: Vec<TaskSpec>,
    /// The longest chain, which the tie-breaker's task sits on.
    pub critical_path: Vec<TaskId>,
}

/// The run limit split by line. The lines add up to the total.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LimitPlan {
    pub lines: Vec<LimitLine>,
    pub total: LimitAmount,
    /// An estimate range for the plan approval, such as $9 to $12.
    pub estimate_low: f64,
    pub estimate_high: f64,
}

impl LimitPlan {
    /// The plan of a run with no limit: no lines, and nothing to add up.
    pub fn unlimited() -> Self {
        Self {
            lines: Vec::new(),
            total: LimitAmount {
                unit: LimitUnit::Usd,
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
pub struct LimitLine {
    pub line: LimitLineKind,
    pub amount: LimitAmount,
}
