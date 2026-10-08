//! Planning steps: drafts, the map, critique, revision, spikes, judging, and cards.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Step, StepName};
use crate::names::PlanLabel;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GoalBrief {
    pub summary: String,
    /// Criteria as `G1: ...`.
    pub criteria: Vec<String>,
    /// Stated assumptions and your answers.
    pub assumptions: Vec<String>,
    pub sha256: String,
}

/// A blind plan draft, written read-only.
pub struct Draft;

impl Step for Draft {
    const NAME: StepName = StepName::Draft;
    type Input = DraftInput;
    type Output = PlanDraft;
}

/// What a drafter sees. Never another drafter's work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DraftInput {
    pub goal: GoalBrief,
    pub notes: Vec<String>,
    pub decisions: Vec<String>,
    /// The status summary, filtered for drafters.
    pub status: String,
    /// Who may own and who may review the plan's cards, so its pieces suit the crew.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub crew: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlanDraft {
    /// The plan in a paragraph.
    pub summary: String,
    /// The approach in more detail, Markdown.
    pub approach: String,
    /// The pieces the work splits into, along its seams.
    pub pieces: Vec<PlanPiece>,
    /// Choices the plan makes, one per topic.
    pub choices: Vec<PlanChoice>,
    /// Risks and how the plan handles them.
    pub risks: Vec<String>,
    pub test_strategy: String,
    pub new_dependencies: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlanPiece {
    pub name: String,
    pub description: String,
    /// Paths it will own.
    pub paths: Vec<String>,
    /// Interfaces it defines or relies on.
    pub interfaces: Vec<String>,
    /// Names of pieces it needs first.
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlanChoice {
    /// Such as "where counters live".
    pub topic: String,
    /// Such as "Redis".
    pub choice: String,
    pub rationale: String,
}

/// A plan under its anonymous label, as judges and the mapper see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LabeledPlan {
    /// 1 to N.
    pub label: PlanLabel,
    /// The plan as text, exactly as sealed. Quotes are checked against this text.
    pub text: String,
}

/// The mapper aligns drafts by topic, quoting each one.
pub struct Map;

impl Step for Map {
    const NAME: StepName = StepName::Map;
    type Input = MapInput;
    type Output = MapOutput;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MapInput {
    pub plans: Vec<LabeledPlan>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MapOutput {
    pub topics: Vec<MapTopic>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MapTopic {
    /// Such as "middleware position".
    pub name: String,
    /// Whether the positions conflict.
    pub disputed: bool,
    pub positions: Vec<MapPosition>,
}

/// One plan's position, with a quote code checks word for word.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MapPosition {
    pub label: PlanLabel,
    /// The position in a few words, such as "after auth".
    pub summary: String,
    /// A verbatim quote from that plan.
    pub quote: String,
}

/// One critique round, on disputed topics only.
pub struct Critique;

impl Step for Critique {
    const NAME: StepName = StepName::Critique;
    type Input = CritiqueInput;
    type Output = CritiqueOutput;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CritiqueInput {
    pub goal: GoalBrief,
    pub plans: Vec<LabeledPlan>,
    pub disputed: Vec<MapTopic>,
    pub checklist: Vec<String>,
    /// Set for the run's designated critic, who argues against the leading plan.
    pub designated: bool,
    /// The leading plan, for the designated critic.
    pub leading: Option<PlanLabel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CritiqueOutput {
    pub points: Vec<CritiquePoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CritiquePoint {
    pub plan: PlanLabel,
    pub topic: String,
    pub claim: String,
    pub severity: Severity,
    pub suggestion: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    /// Blocks the plan or the card.
    High,
}

/// One revision round. Resumes the drafter's own session.
pub struct Revise;

impl Step for Revise {
    const NAME: StepName = StepName::Revise;
    type Input = ReviseInput;
    type Output = PlanDraft;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReviseInput {
    pub label: PlanLabel,
    pub critique: Vec<CritiquePoint>,
    pub disputed: Vec<MapTopic>,
}

/// Proposes one small experiment per remaining dispute.
pub struct ProposeSpike;

impl Step for ProposeSpike {
    const NAME: StepName = StepName::ProposeSpike;
    type Input = ProposeSpikeInput;
    type Output = SpikeProposals;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProposeSpikeInput {
    /// Topics still disputed after revision.
    pub disputed: Vec<MapTopic>,
    pub budget_usd: f64,
    /// Time box per spike, in minutes.
    pub time_box_min: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SpikeProposals {
    /// One per testable dispute. Untestable ones are left out.
    pub proposals: Vec<SpikeProposal>,
}

/// One experiment. Code checks it against the spike budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SpikeProposal {
    pub topic: String,
    pub question: String,
    /// The options to build, usually two.
    pub options: Vec<SpikeOption>,
    /// What to measure, such as "p50 latency and counters after a restart".
    pub measure: String,
    pub decides_by: String,
    /// Estimated minutes per option.
    pub est_minutes: f64,
    /// Estimated dollars for all options.
    pub est_cost_usd: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SpikeOption {
    /// Such as "Redis".
    pub label: String,
    pub build: String,
}

pub struct SpikeReportStep;

impl Step for SpikeReportStep {
    const NAME: StepName = StepName::SpikeReport;
    type Input = SpikeProposal;
    type Output = SpikeReport;
}

/// What one spike option measured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SpikeReport {
    pub topic: String,
    pub option: String,
    pub measurements: Vec<Measurement>,
    /// The finding in one sentence.
    pub finding: String,
    pub works: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Measurement {
    /// Such as `p50_ms`.
    pub metric: String,
    pub value: f64,
    /// Such as `ms`.
    pub unit: String,
}

/// A judge scores the plans it didn't write, anonymized, against the rubric.
pub struct Judge;

impl Step for Judge {
    const NAME: StepName = StepName::Judge;
    type Input = JudgeInput;
    type Output = JudgeOutput;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JudgeInput {
    pub goal: GoalBrief,
    pub rubric: Vec<RubricCriterion>,
    /// Plans in presentation order. Never the judge's own.
    pub plans: Vec<LabeledPlan>,
    pub spikes: Vec<SpikeReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RubricCriterion {
    /// Such as `testability`.
    pub name: String,
    /// What a 10 means.
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JudgeOutput {
    /// One entry per plan shown.
    pub scores: Vec<PlanScore>,
}

/// One plan's scores, 0 to 10 per criterion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlanScore {
    pub plan: PlanLabel,
    /// Score per rubric criterion, in rubric order.
    pub criteria: Vec<f64>,
    pub rationale: String,
    /// Set when the plan contains text addressed to judges. Blocks auto-approval.
    pub addressed_to_judges: bool,
}

/// Turns the chosen plan into a task graph proposal. Code builds and checks the graph.
pub struct PlanToCards;

impl Step for PlanToCards {
    const NAME: StepName = StepName::PlanToCards;
    type Input = PlanToCardsInput;
    type Output = TaskGraphProposal;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlanToCardsInput {
    pub goal: GoalBrief,
    pub plan: PlanDraft,
    pub decisions: Vec<String>,
    /// Paths no card may own.
    pub forbidden_paths: Vec<String>,
    pub max_tasks: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskGraphProposal {
    pub cards: Vec<CardProposal>,
}

/// Keys are local to the proposal; code assigns card ids.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CardProposal {
    /// A short unique key, such as `store`.
    pub key: String,
    pub title: String,
    /// The task text the owner gets.
    pub task: String,
    pub kind: ProposedKind,
    pub owned_paths: Vec<String>,
    /// Paths it reads but must not edit, such as frozen interfaces.
    pub read_only_paths: Vec<String>,
    /// Keys of cards it needs first.
    pub depends_on: Vec<String>,
    /// Goal criteria it serves, `G1`.
    pub criteria: Vec<String>,
}

/// Test cards are added by code, one per build card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposedKind {
    /// Interfaces with stubs that compile and fail "not implemented".
    Interface,
    Build,
    /// Integration or cleanup work.
    Chore,
    Docs,
}

/// The coordinator plans and delegates in one structured output (`orchestrator`).
pub struct Coordinate;

impl Step for Coordinate {
    const NAME: StepName = StepName::Coordinate;
    type Input = CoordinateInput;
    type Output = CoordinateOutput;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CoordinateInput {
    pub goal: GoalBrief,
    /// Workers it may assign, as `worker/model` with their families.
    pub crew: Vec<String>,
    /// The rules code will check its output against.
    pub rules: Vec<String>,
    pub rejection: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CoordinateOutput {
    pub plan: PlanDraft,
    pub cards: Vec<CardProposal>,
    pub assignments: Vec<Assignment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Assignment {
    /// The card key.
    pub card: String,
    /// `worker/model`.
    pub owner: String,
    /// `worker/model` each.
    pub reviewers: Vec<String>,
}
