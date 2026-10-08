//! Workflows as data: named phases of typed steps. The lead picks or composes one per task,
//! code validates it, and the engine runs it. Gates before review, review by another family,
//! hard limits, approvals, budgets, and handoffs stay code, so no workflow can turn them off.

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::events::checkpoint::CheckpointKind;
use crate::names::{RoleName, WorkerRef, WorkflowId};

/// A workflow by name, looked up like presets: the project's `.argonauts/workflows/`, then
/// `~/.argonauts/workflows/`, then the built-in.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct WorkflowRef(pub String);

impl WorkflowRef {
    pub fn new(name: &str) -> Self {
        Self(name.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkflowRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Values for `{{name}}` placeholders, such as `target: diff` or `reviewers: 2`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct WorkflowParams(pub BTreeMap<String, Value>);

impl WorkflowParams {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Unknown keys are refused rather than dropped, so a misspelled rule never silently goes
/// missing from a workflow file or the lead's composed one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub name: String,
    /// One line, shown under the name in the workflow view.
    #[serde(default)]
    pub description: String,
    /// The example it started from, such as `council`, when the lead wrote it from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub based_on: Option<String>,
    /// Defaults that the lead fills in per task.
    #[serde(default, skip_serializing_if = "WorkflowParams::is_empty")]
    pub params: WorkflowParams,
    pub phases: Vec<Phase>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Phase {
    /// Any name, such as "Fix" or "Review". Files may call it `title`, as Claude Code's
    /// workflow scripts do.
    #[serde(alias = "title")]
    pub name: String,
    /// One line on who does what, shown under the title until the phase starts.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    pub standard: StandardPhase,
    pub steps: Vec<Step>,
}

/// The phase every step also belongs to, whatever its workflow calls it. It anchors the
/// working line, per-phase budgets, where approvals attach, and track records.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StandardPhase {
    Understand,
    Plan,
    Do,
    Check,
    Decide,
}

/// Which fields apply depends on `kind`, and the validator refuses the rest.
///
/// `role`, `workers`, and `count` pick who does a step that gives agents a task: `restate`,
/// `draft`, `fan_out`, `review`, `judge`, `refute`, `synthesize`, and `build`. Only `fan_out`
/// takes `roles`, sending the same task to each role at once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub kind: StepKind,
    /// Names the step for later steps that take its output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// What its agents are called in the views, as a pattern such as `fix:{item}` or
    /// `read:{family}` (see [`crate::labels`]). Code fills one in from the phase and the item
    /// when it's left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// For a `build`: the later phase that shows its review rows, so one card's owner, gates,
    /// and review run as a pipeline across two phases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_in: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<RoleName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<RoleName>,
    /// Workers by name, which the validator checks are allowed. The router picks when empty.
    #[serde(default, skip_serializing_if = "StepWorkers::is_empty")]
    pub workers: StepWorkers,
    /// Workers at once, for `draft` and `fan_out`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<Count>,
    /// Blind to each other's work.
    #[serde(default, skip_serializing_if = "is_false")]
    pub blind: bool,
    /// Picks workers from another model family than this one's, such as `author`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub different_family_from: Option<String>,
    /// A condition such as `reviewers_disagree`. The step is skipped when it doesn't hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// What the step's agents are asked, with `{{name}}` placeholders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The earlier step whose output this one takes, by name, or `goal`, `plan`, or `diff`.
    /// A `for_each` goes over its items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// What `loop_until` and `for_each` repeat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
    /// The condition that ends a `loop_until`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// A `loop_until`'s hard maximum of rounds, which every loop must have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointKind>,
    /// The workflow a `workflow` step runs, and its parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowRef>,
    #[serde(default, skip_serializing_if = "WorkflowParams::is_empty")]
    pub params: WorkflowParams,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// Each kind has typed inputs and outputs, and uses the LLM-step schemas where it calls a
/// model. A `gate` runs the configured gate commands, and a workflow can't add any.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Restate,
    Differ,
    Draft,
    /// Quote-checked comparison of drafts.
    Map,
    /// The same task to several workers or roles, in parallel.
    FanOut,
    /// One sub-step per item of an earlier step's output.
    ForEach,
    Review,
    /// Scores candidates, never the judge's own.
    Judge,
    /// Tries to disprove a finding or a claim.
    Refute,
    Synthesize,
    /// A card: an owner in a worktree, then its gates and review.
    Build,
    Gate,
    LoopUntil,
    Checkpoint,
    /// A fresh read-only agent from another company judges the done-when list against
    /// evidence, after the gates.
    Check,
    /// Integration and the merge checkpoint.
    Merge,
    /// Another workflow run as one step. Nesting stops at depth 2.
    Workflow,
    /// The workflow in use, run by its own steps: a file of `[[step]]` entries or a script, in
    /// the sandboxed runner.
    Script,
}

/// A number, or a `{{name}}` placeholder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Count {
    Fixed(u32),
    Param(String),
}

/// A list of workers, or a `{{name}}` placeholder the lead fills with one, such as the
/// reviewers it picked for this task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum StepWorkers {
    Named(Vec<WorkerRef>),
    Param(String),
}

impl StepWorkers {
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Named(workers) if workers.is_empty())
    }
}

impl Default for StepWorkers {
    fn default() -> Self {
        Self::Named(Vec::new())
    }
}

/// Where a step sits in a running workflow.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StepRef {
    pub workflow: WorkflowId,
    /// From 0.
    pub phase: u32,
    /// The step's index in its phase, then in its parent's `steps` for loops and `for_each`.
    pub path: Vec<u32>,
}
