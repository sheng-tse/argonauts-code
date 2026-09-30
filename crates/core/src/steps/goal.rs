//! Goal alignment steps: restate and differ.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Step, StepName};

/// A crew agent restates the goal in a read-only session.
pub struct Restate;

impl Step for Restate {
    const NAME: StepName = StepName::Restate;
    type Input = RestateInput;
    type Output = Restatement;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RestateInput {
    pub request: String,
    /// Relevant `.argonauts/notes` excerpts.
    pub notes: Vec<String>,
    /// Relevant earlier decisions, `D-003: ...`.
    pub decisions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Restatement {
    /// The goal in two or three sentences.
    pub summary: String,
    /// Checkable success criteria, one per line.
    pub success_criteria: Vec<String>,
    pub in_scope: Vec<String>,
    pub out_of_scope: Vec<String>,
    pub assumptions: Vec<String>,
    pub open_questions: Vec<String>,
}

/// A differ compares the restatements in a fresh session and tags each difference.
pub struct Differ;

impl Step for Differ {
    const NAME: StepName = StepName::Differ;
    type Input = DifferInput;
    type Output = DifferOutput;
}

/// Restatements under anonymous labels (A, B, C).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DifferInput {
    pub request: String,
    pub restatements: Vec<LabeledRestatement>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LabeledRestatement {
    pub label: String,
    pub restatement: Restatement,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DifferOutput {
    /// Points every restatement shares.
    pub agreements: Vec<String>,
    pub differences: Vec<Difference>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Difference {
    /// What it is about, such as "limit per user or per API key".
    pub topic: String,
    pub readings: Vec<Reading>,
    /// Code applies the materiality rules to it.
    pub tag: DiffTag,
    /// A multiple-choice question you could answer, in case code decides to ask.
    pub question: Option<McQuestion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Reading {
    pub label: String,
    pub reading: String,
}

/// How much a difference matters. Scope, security, data, and cost always reach you.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiffTag {
    /// Same meaning, different words. Dropped.
    Wording,
    /// A two-of-three reading becomes a stated assumption.
    Detail,
    Scope,
    Security,
    /// Data handling or migration.
    Data,
    /// Cost or performance budget.
    Cost,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McQuestion {
    pub text: String,
    /// Two to four options.
    pub options: Vec<String>,
    /// Index into `options` of the recommended answer.
    pub recommended: u8,
    /// Why it's recommended, such as "2 of 3 restatements read it this way".
    pub basis: String,
}
