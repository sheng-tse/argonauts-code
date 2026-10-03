//! Goal alignment steps: restate, differ, and the lead answering the open questions.

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

/// When the lead answers the crew's questions, it answers the goal's open questions in a
/// read-only session. Its answers become assumptions in the goal, and the approval after them
/// still follows its mode.
pub struct AnswerQuestions;

impl Step for AnswerQuestions {
    const NAME: StepName = StepName::AnswerQuestions;
    type Input = AnswerQuestionsInput;
    type Output = AnswerQuestionsOutput;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnswerQuestionsInput {
    pub request: String,
    /// The goal draft, as its file reads.
    pub goal: String,
    pub questions: Vec<OpenQuestion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OpenQuestion {
    /// 1-based.
    pub n: u8,
    pub text: String,
    pub options: Vec<String>,
    /// Index into `options` of the answer the differs recommend, if any.
    pub recommended: Option<u8>,
    /// Why they recommend it.
    pub basis: Option<String>,
    pub tag: DiffTag,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnswerQuestionsOutput {
    pub answers: Vec<LeadAnswer>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LeadAnswer {
    /// The question's `n`.
    pub n: u8,
    /// Index into the question's options. `None` only with `needs_user`.
    pub option: Option<u8>,
    /// Why this answer, in one line.
    pub why: String,
    /// Set only when the answer turns on a fact no agent can find out from the request, the
    /// repo, or the web, such as a credential or a choice only the user can make. Names that
    /// fact in one line.
    pub needs_user: Option<String>,
}
