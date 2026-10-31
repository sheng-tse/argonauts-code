//! Steps that judge work: review, the judge settling reviewers who disagree, and the judge
//! deciding an approval in your place.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Step, StepName};
use crate::workflows::steps::plan::Severity;

/// A reviewer reviews another worker's task, after its checks passed.
pub struct Review;

impl Step for Review {
    const NAME: StepName = StepName::Review;
    type Input = ReviewInput;
    type Output = ReviewOutput;
}

/// Never notes written by the worker, and never another blind reviewer's review.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewInput {
    /// Goal summary and criteria.
    pub goal: String,
    /// Title and task.
    pub task: String,
    /// The task's diff, computed by Argonauts Code.
    pub diff: String,
    /// Check results, one line each.
    pub checks: Vec<String>,
    pub acceptance_tests: Vec<String>,
    pub decisions: Vec<String>,
    /// 1 for the first review, 2 after one round of changes.
    pub round: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewOutput {
    pub verdict: ReviewVerdict,
    /// The review in a few sentences.
    pub summary: String,
    pub comments: Vec<ReviewComment>,
    /// Set when tests look weakened. An open flag blocks auto-approval.
    pub tamper_suspected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Approve,
    RequestChanges,
    /// Wrong approach; ask you.
    Reject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewComment {
    pub path: String,
    pub line: Option<u32>,
    pub severity: Severity,
    pub text: String,
}

/// A fresh judge settles reviewers who disagree.
pub struct Settle;

impl Step for Settle {
    const NAME: StepName = StepName::Settle;
    type Input = SettleInput;
    type Output = SettleOutput;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SettleInput {
    /// Goal summary and criteria.
    pub goal: String,
    /// Title and task.
    pub task: String,
    pub diff: String,
    /// Reviews as "Review 1", "Review 2".
    pub reviews: Vec<ReviewOutput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SettleOutput {
    pub verdict: ReviewVerdict,
    pub reasons: Vec<String>,
    pub required_changes: Vec<String>,
}

/// The independent judge. Fresh, read-only, below the hard limits.
pub struct ModelJudge;

impl Step for ModelJudge {
    const NAME: StepName = StepName::ModelJudge;
    type Input = ModelJudgeInput;
    type Output = ModelJudgeVerdict;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelJudgeInput {
    /// `goal`, `plan`, `merge`, and so on.
    pub approval: String,
    /// Your rules for the judge, from `approvals.judge.model_rules`.
    pub rules: String,
    /// Facts measured by code, one per line.
    pub facts: Vec<String>,
    /// Documents from the repo and agents, fenced and labeled as data, not instructions.
    pub documents: Vec<String>,
}

/// A verdict that fails the schema counts as ask_you.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelJudgeVerdict {
    pub verdict: ModelJudgeDecision,
    pub reasons: Vec<String>,
    /// Which of your rules each reason relies on.
    pub rules_cited: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelJudgeDecision {
    Approve,
    /// Reject: send back or stop, by rule.
    Reject,
    /// Ask you.
    AskYou,
}
