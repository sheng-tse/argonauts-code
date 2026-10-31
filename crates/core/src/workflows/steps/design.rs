//! The main agent's brief: the run's name, description, phases, labels, and done-when list, written
//! from the closest example or composed from the step kinds. Code validates the answer before
//! anything runs, and sends the errors back when it fails. At a phase end the same step adapts
//! the running workflow, given what it is and what its finished phases produced.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use super::design_shorthand::{names_a_test, starts_with_program};
use super::pursue::TryPlan;
use super::{Step, StepName};
use crate::runs::events::goal::Criterion;

pub struct DesignWorkflow;

impl Step for DesignWorkflow {
    const NAME: StepName = StepName::DesignWorkflow;
    type Input = DesignInput;
    type Output = DesignOutput;

    fn normalize(output: Value) -> Value {
        super::design_shorthand::design_brief(output)
    }
}

/// The same main agent session, resumed with what code refused in the brief it just wrote, answers
/// with the whole brief again. It's the main agent's design as much as the first answer was, so it
/// goes by the same step.
pub struct FixDesign;

impl Step for FixDesign {
    const NAME: StepName = StepName::DesignWorkflow;
    type Input = DesignFix;
    type Output = DesignOutput;

    fn normalize(output: Value) -> Value {
        super::design_shorthand::design_brief(output)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DesignFix {
    /// What code refused, one problem a line.
    pub errors: Vec<String>,
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
    /// `goal` when the run is a goal, so the brief plans its first try. A workflow leaves
    /// it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<crate::settings::workflows::RunStyle>,
    /// What the request asked of how to run it, each a must: "Codex orchestrates", or words for
    /// the main agent such as "no drafting phase".
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
/// each phase's name and detail, every agent's label, and the done-when list. Code validates it
/// like any workflow and sends the errors back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = says_what_runs)]
pub struct DesignOutput {
    /// Two to four task words, lowercase, joined with `-`, at most 32 cells, such as
    /// `fix-slugify-median`. Never an example's name.
    pub name: String,
    /// One line, the task in plain words, at most 72 cells. In a goal run, the objective.
    pub description: String,
    /// The example it started from, by name. `None` when composed from the step kinds.
    pub based_on: Option<String>,
    /// One line on why it is different from that example, for the start question.
    pub why: String,
    /// Values for the example's `{{name}}` placeholders, when `workflow` is `None` and the
    /// example runs as written.
    #[serde(default)]
    pub params: Vec<ParamSetting>,
    /// The whole workflow file in YAML: the example filled in and changed, or composed. Each
    /// phase has a `name` and a `detail`, and each step a `label`. `None` runs `based_on`.
    pub workflow: Option<String>,
    /// One to eight criteria, each one sentence a check can prove, and how.
    #[serde(default)]
    pub done_when: Vec<Criterion>,
    /// When the workflow builds from the goal rather than a drafted plan, the one task it
    /// builds, whose item names its labels. Empty when a phase drafts the plan.
    #[serde(default)]
    pub plan: Vec<BriefTask>,
    /// With `understand.when = "main"`: at most three open questions, each with a recommended
    /// answer.
    #[serde(default)]
    pub questions: Vec<BriefQuestion>,
    /// In a goal run, the first try, planned with the goal so the start question can estimate
    /// it.
    pub first_try: Option<TryPlan>,
}

/// A brief must say what runs, so `based_on` and `workflow` are both asked for, either of them
/// `null`. Normalizing fills in the one left out when the other is there.
fn says_what_runs(schema: &mut schemars::Schema) {
    if let Some(Value::Array(required)) = schema.get_mut("required") {
        for key in ["based_on", "workflow"] {
            if !required.iter().any(|name| name == key) {
                required.push(Value::from(key));
            }
        }
    }
}

impl DesignOutput {
    /// `based_on` as one of `examples` names it, when the main agent wrote another spelling of only
    /// that name, such as `Council` for `council` or `quick-fix` for your `quick_fix.yaml`.
    /// A name that is an example stays exactly as written.
    pub fn read_based_on(&mut self, examples: &[String]) {
        let Some(written) = self.based_on.as_deref() else {
            return;
        };
        if examples.iter().any(|example| example == written) {
            return;
        }
        let plain = super::design_shorthand::example_name(written);
        let mut spelled = examples
            .iter()
            .filter(|example| super::design_shorthand::example_name(example) == plain);
        if let (Some(example), None) = (spelled.next(), spelled.next()) {
            self.based_on = Some(example.clone());
        }
    }

    /// Whether `based_on` names `example`, as written or spelled another way, for a brief read
    /// back from the log as the main agent wrote it.
    pub fn based_on_names(&self, example: &str) -> bool {
        self.based_on.as_deref().is_some_and(|written| {
            written == example
                || super::design_shorthand::example_name(written)
                    == super::design_shorthand::example_name(example)
        })
    }
}

/// One task of a brief's plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BriefTask {
    /// The one-token name its labels use, such as `slugify` in `fix:slugify`.
    pub item: String,
    /// What the task's worker does, in one or two sentences.
    pub task: String,
}

/// An open question the main agent found itself, when it decides whether Understand runs.
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
