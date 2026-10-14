//! Understand's steps: read, compare, and the main agent answering the open questions.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Step, StepName};

/// A crew agent reads the request back in a read-only session.
pub struct Read;

impl Step for Read {
    const NAME: StepName = StepName::Read;
    type Input = ReadInput;
    type Output = Reading;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReadInput {
    pub request: String,
    /// Relevant excerpts of the crew's notes and the repository's `.argonauts/notes`.
    pub notes: Vec<String>,
    /// Relevant earlier decisions, `D-003: ...`.
    pub decisions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Reading {
    /// The goal in two or three sentences.
    pub summary: String,
    /// Checkable success criteria, one per line.
    pub success_criteria: Vec<String>,
    pub in_scope: Vec<String>,
    pub out_of_scope: Vec<String>,
    pub assumptions: Vec<String>,
    pub open_questions: Vec<String>,
}

/// A comparer compares the readings in a fresh session and tags each difference.
pub struct Compare;

impl Step for Compare {
    const NAME: StepName = StepName::Compare;
    type Input = CompareInput;
    type Output = CompareOutput;
}

/// Readings under anonymous labels (A, B, C).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompareInput {
    pub request: String,
    pub readings: Vec<LabeledReading>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LabeledReading {
    pub label: String,
    pub reading: Reading,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompareOutput {
    /// Points every reading shares.
    pub agreements: Vec<String>,
    pub differences: Vec<Difference>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Difference {
    /// What it is about, such as "limit per user or per API key".
    pub topic: String,
    pub readings: Vec<TopicReading>,
    /// Code applies the materiality rules to it.
    pub tag: DiffTag,
    /// A multiple-choice question you could answer, in case code decides to ask.
    pub question: Option<McQuestion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TopicReading {
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
    /// Cost or performance limit.
    Cost,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McQuestion {
    pub text: String,
    /// Two to four options.
    pub options: Vec<String>,
    /// Index into `options` of the recommended answer.
    pub recommended: u8,
    /// Why it's recommended, such as "2 of 3 readings read it this way".
    pub basis: String,
}

/// When the main agent answers the crew's questions, it answers the goal's open questions in a
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
    /// Index into `options` of the answer the comparers recommend, if any.
    pub recommended: Option<u8>,
    /// Why they recommend it.
    pub basis: Option<String>,
    pub tag: DiffTag,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnswerQuestionsOutput {
    pub answers: Vec<MainAgentAnswer>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MainAgentAnswer {
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
