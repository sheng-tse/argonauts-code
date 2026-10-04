//! The goal style's steps: the lead plans each attempt from the last check, and a fresh
//! read-only agent from another company checks the done-when list. Only the check's answers,
//! through code, mark a goal met.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Step, StepName};
use crate::events::goal::{Criterion, CriterionVerdict, GoalVerdict};

/// The lead writes the next attempt after reading the last check.
pub struct PlanAttempt;

impl Step for PlanAttempt {
    const NAME: StepName = StepName::PlanAttempt;
    type Input = PlanAttemptInput;
    type Output = AttemptPlan;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlanAttemptInput {
    /// The objective, one line.
    pub goal: String,
    pub criteria: Vec<Criterion>,
    /// The last check's verdicts and reason. `None` before the first attempt.
    pub last_check: Option<GoalCheck>,
    /// One line per earlier attempt, such as "1. Fix the even case: not yet, G4 unmet".
    pub attempts: Vec<String>,
}

/// One attempt, shown as one phase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AttemptPlan {
    /// One or two words in Title Case, such as `Empty List`.
    pub title: String,
    /// One line on who does what.
    pub detail: String,
    /// The phase's steps as YAML, in the workflow file's format, each with a `label`.
    pub steps: String,
}

/// Checks the done-when list against evidence, after the gates passed.
pub struct CheckGoal;

impl Step for CheckGoal {
    const NAME: StepName = StepName::CheckGoal;
    type Input = CheckGoalInput;
    type Output = GoalCheck;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CheckGoalInput {
    pub goal: String,
    pub criteria: Vec<Criterion>,
    /// Since the run started.
    pub diff: String,
    pub gate_output: String,
}

/// What the check found. Can't tell counts as not yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GoalCheck {
    pub verdicts: Vec<CriterionVerdict>,
    pub overall: GoalVerdict,
    /// One or two sentences: what's missing, or why it can't be met.
    pub reason: String,
}
