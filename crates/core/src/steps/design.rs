//! The lead's brief: the run's name, description, phases, labels, and done-when list, written
//! from the closest example or composed from the step kinds. Code validates the answer before
//! anything runs, and sends the errors back when it fails. At a phase end the same step adapts
//! the running workflow, given what it is and what its finished phases produced.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::pursue::AttemptPlan;
use super::{Step, StepName};
use crate::events::goal::Criterion;

pub struct DesignWorkflow;

impl Step for DesignWorkflow {
    const NAME: StepName = StepName::DesignWorkflow;
    type Input = DesignInput;
    type Output = DesignOutput;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DesignInput {
    pub request: String,
    /// The approved goal's summary and criteria, when there is one.
    pub goal: Vec<String>,
    /// Templates and past workflows that did well on similar tasks, best first. The engine
    /// sends them as a markdown part after the input instead, one `### name` section each, so
    /// their files read as YAML; it leaves this empty.
    pub examples: Vec<WorkflowExample>,
    /// Each step kind with one line on what it does.
    pub step_kinds: Vec<String>,
    pub allow_composed: bool,
    /// The validator's errors on the last answer. Empty the first time.
    pub errors: Vec<String>,
    /// When adapting mid-run, the running workflow as YAML with its finished phases marked.
    /// `None` when designing one for a new task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// One line per finished phase or step of `current`, such as "Review: 2 reviewers
    /// disagreed on the retry header".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub progress: Vec<String>,
    /// `goal` when the run is a goal, so the brief plans its first attempt. A workflow leaves
    /// it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<crate::settings::workflows::RunStyle>,
    /// What the request asked of how to run it, each a must: "Codex coordinates", or words for
    /// the lead such as "no drafting phase".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub as_you_asked: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowExample {
    pub name: String,
    pub description: String,
    /// The workflow file.
    pub yaml: String,
    /// Finished runs of similar tasks, and how many of them passed.
    pub runs: u32,
    pub passed: u32,
}

/// The run's brief, as Claude Code's orchestrator writes its script's `meta` and `agent()`
/// options. Everything the views say about the run comes from here: its name and description,
/// each phase's title and detail, every agent's label, and the done-when list. Code validates it
/// like any workflow and sends the errors back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DesignOutput {
    /// Two to four task words, lowercase, joined with `-`, at most 32 cells, such as
    /// `fix-slugify-median`. Never an example's name.
    pub name: String,
    /// One line, the task in plain words, at most 72 cells. In a goal run, the objective.
    pub description: String,
    /// The example it started from, by name. `None` when composed from the step kinds.
    pub based_on: Option<String>,
    /// One line on why it differs from that example, for the start card.
    pub why: String,
    /// Values for the example's `{{name}}` placeholders, when `workflow` is `None` and the
    /// example runs as written.
    #[serde(default)]
    pub params: Vec<ParamSetting>,
    /// The whole workflow file in YAML: the example filled in and changed, or composed. Each
    /// phase has a `title` and a `detail`, and each step a `label`. `None` runs `based_on`.
    pub workflow: Option<String>,
    /// One to eight criteria, each one sentence a check can prove, and how.
    #[serde(default)]
    pub done_when: Vec<Criterion>,
    /// When the workflow builds from the goal rather than a drafted plan, the one card it
    /// builds, whose item names its labels. Empty when a phase drafts the plan.
    #[serde(default)]
    pub plan: Vec<BriefCard>,
    /// With `understand.when = "lead"`: at most three open questions, each with a recommended
    /// answer.
    #[serde(default)]
    pub questions: Vec<BriefQuestion>,
    /// In a goal run, the first attempt, planned with the goal so the start card can estimate
    /// it.
    pub first_attempt: Option<AttemptPlan>,
}

/// One card of a brief's plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BriefCard {
    /// The one-token name its labels use, such as `slugify` in `fix:slugify`.
    pub item: String,
    /// What the card's owner does, in one or two sentences.
    pub task: String,
}

/// An open question the lead found itself, when it decides whether Understand runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BriefQuestion {
    /// One or two words for its chip, such as `Accented letters`.
    pub topic: String,
    pub text: String,
    /// Two to four answers, the recommended one first.
    pub options: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ParamSetting {
    pub name: String,
    /// As YAML scalar text, such as `diff` or `2`.
    pub value: String,
}
