//! A goal's steps: the orchestrator plans each next round from what the last check found
//! failing, and the judge, a fresh read-only agent from another provider than the work, reads
//! the result against each line. Only the check's answers, through code, mark a goal met.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Step, StepName};
use crate::runs::events::goal::{Criterion, CriterionVerdict, GoalVerdict};

/// The main agent writes the next try after reading the last check.
pub struct PlanTry;

impl Step for PlanTry {
    const NAME: StepName = StepName::PlanTry;
    type Input = PlanTryInput;
    type Output = TryPlan;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlanTryInput {
    /// The objective, one line.
    pub goal: String,
    pub criteria: Vec<Criterion>,
    /// The last check's verdicts and reason. `None` before the first try.
    pub last_check: Option<GoalCheck>,
    /// One line per earlier try, such as "1. Fix the even case: not yet, G4 unmet".
    pub tries: Vec<String>,
    /// Set once, when the same failure came back round after round: plan this one another way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_approach: Option<String>,
}

/// One try, shown as one phase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TryPlan {
    /// One or two words in Title Case, such as `Empty List`.
    pub name: String,
    /// One line on who does what.
    pub detail: String,
    /// The phase's steps as YAML, in the workflow file's format, each with a `label`.
    pub steps: String,
}

/// Checks the done-when list against evidence, after the checks passed.
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
    pub check_output: String,
}

/// What the check found. Can't tell counts as not yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GoalCheck {
    pub verdicts: Vec<CriterionVerdict>,
    pub overall: GoalVerdict,
    /// One or two sentences: what's missing, or why it can't be met.
    pub reason: String,
}
