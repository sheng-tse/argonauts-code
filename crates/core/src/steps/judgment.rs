//! Steps that judge work: review, adjudication, and the independent approver.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Step, StepName};
use crate::steps::plan::Severity;

/// A reviewer reviews another owner's card, after its gates passed.
pub struct Review;

impl Step for Review {
    const NAME: StepName = StepName::Review;
    type Input = ReviewInput;
    type Output = ReviewOutput;
}

/// Never notes written by the owner, and never another blind reviewer's review.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewInput {
    /// Goal summary and criteria.
    pub goal: String,
    /// Title and task.
    pub card: String,
    /// The card's diff, computed by the conductor.
    pub diff: String,
    /// Gate results, one line each.
    pub gates: Vec<String>,
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
    /// Wrong approach; escalate.
    Reject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewComment {
    pub path: String,
    pub line: Option<u32>,
    pub severity: Severity,
    pub text: String,
}

/// A fresh adjudicator settles reviewers who disagree.
pub struct Adjudicate;

impl Step for Adjudicate {
    const NAME: StepName = StepName::Adjudicate;
    type Input = AdjudicateInput;
    type Output = AdjudicateOutput;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AdjudicateInput {
    /// Goal summary and criteria.
    pub goal: String,
    /// Title and task.
    pub card: String,
    pub diff: String,
    /// Reviews as "Review 1", "Review 2".
    pub reviews: Vec<ReviewOutput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AdjudicateOutput {
    pub verdict: ReviewVerdict,
    pub reasons: Vec<String>,
    pub required_changes: Vec<String>,
}

/// The independent approver model. Fresh, read-only, below the hard limits.
pub struct ModelApprover;

impl Step for ModelApprover {
    const NAME: StepName = StepName::ModelApprover;
    type Input = ApproverInput;
    type Output = ApproverVerdict;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApproverInput {
    /// `goal`, `plan`, `merge`, and so on.
    pub checkpoint: String,
    /// Your rules for the approver, from `approvals.approver.model_rules`.
    pub rules: String,
    /// Facts measured by code, one per line.
    pub facts: Vec<String>,
    /// Documents from the repo and agents, fenced and labeled as data, not instructions.
    pub documents: Vec<String>,
}

/// A verdict that fails the schema counts as escalate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApproverVerdict {
    pub verdict: ApproverDecision,
    pub reasons: Vec<String>,
    /// Which of your rules each reason relies on.
    pub rules_cited: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApproverDecision {
    Approve,
    /// Reject: send back or stop, by rule.
    Reject,
    /// Ask you.
    Escalate,
}
