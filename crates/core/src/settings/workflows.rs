//! How the main agent picks, composes, and adapts workflows, `[workflows]`. The rules a workflow
//! can't turn off (checks before review, review by another provider, hard limits, approvals,
//! limits, and fallbacks) aren't settings here.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WorkflowSettings {
    /// How far the orchestrator may change your workflow for a task: `true` lets it size the
    /// workflow, `false` runs it as it is, and `"any"` lets it pick another or compose one.
    pub orchestrator_designs: OrchestratorDesigns,
    /// The main agent may compose new workflows from the step kinds. Code validates them first.
    pub allow_composed: bool,
    /// How many levels of workflows a run may hold inside its own: 0 for none, 1 by default for
    /// a workflow inside it, and 2 at most, for one inside that.
    pub max_depth: u32,
    /// At a phase boundary the main agent may propose a change, which goes through the
    /// `change_request` approval.
    pub adapt_at_phase_ends: bool,
    /// A model from another provider judges the done-when list before Merge.
    pub final_check: bool,
    /// How a workflow whose file says `adapt = true` runs, as the built-in ones do: `script`
    /// has the orchestrator write the steps for each request, and `phases` runs the fixed
    /// phases of the workflow's name.
    pub adapt_with: AdaptWith,
    /// The word `ultracode` in a prompt sends that request through a workflow, and Claude
    /// Code's own trigger follows this. Off, the word is only a word.
    pub ultracode_trigger: bool,
}

/// The journal's and the views' word for whether a goal goes round a run's workflow. Every run
/// is a workflow; a goal is a condition around it, set by you on any CLI (`settings::goal`),
/// and no setting picks this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStyle {
    /// The run ends when its workflow's phases finish.
    Workflow,
    /// The workflow goes round until its goal holds.
    Goal,
}

impl RunStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStyle::Workflow => "workflow",
            RunStyle::Goal => "goal",
        }
    }
}

impl Default for WorkflowSettings {
    fn default() -> Self {
        Self {
            orchestrator_designs: OrchestratorDesigns::Yours(true),
            allow_composed: true,
            max_depth: 1,
            adapt_at_phase_ends: true,
            final_check: true,
            adapt_with: AdaptWith::default(),
            ultracode_trigger: true,
        }
    }
}

/// The top-level keys of a workflow file that describe the workflow itself: its name, what it
/// is for, how its steps are followed, the steps, and its phases with the values they fill in.
/// Every other key in the file is a setting the workflow carries.
pub const WORKFLOW_FILE_KEYS: [&str; 9] = [
    "name",
    "about",
    "adapt",
    "goal",
    "check",
    "max_rounds",
    "step",
    "params",
    "phases",
];

/// `workflows.adapt_with`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdaptWith {
    /// The orchestrator writes the steps for each request as a script, keeping who plans,
    /// builds, reviews and signs off as the file says, and the runner runs it.
    #[default]
    Script,
    /// The workflow's own phases run: the fixed phases Argonauts Code had before
    /// workflow files had steps. A file with `adapt = false` and a script never come here: the
    /// runner always runs those.
    Phases,
}

/// `workflows.orchestrator_designs`, written `true`, `false`, or `"any"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum OrchestratorDesigns {
    /// `true`: the orchestrator sizes your workflow for the task. `false`: it runs as it is.
    Yours(bool),
    /// `"any"`: the orchestrator may run another template, or compose one when
    /// `allow_composed`.
    Any(AnyWorkflow),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnyWorkflow {
    Any,
}

impl OrchestratorDesigns {
    pub const ANY: Self = Self::Any(AnyWorkflow::Any);

    pub fn design(self) -> OrchestratorDesign {
        match self {
            Self::Yours(false) => OrchestratorDesign::Off,
            Self::Yours(true) => OrchestratorDesign::Size,
            Self::Any(AnyWorkflow::Any) => OrchestratorDesign::Any,
        }
    }
}

impl From<bool> for OrchestratorDesigns {
    fn from(on: bool) -> Self {
        Self::Yours(on)
    }
}

/// How far the orchestrator may change your workflow for one task, least first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OrchestratorDesign {
    /// Your workflow runs as it is.
    Off,
    /// The orchestrator keeps your workflow and sizes it: more or fewer agents at a step, or a
    /// step that doesn't apply left out.
    Size,
    /// The orchestrator may run another template or compose one.
    Any,
}
/// `workflow_size`: how many agents a workflow may plan for when the request doesn't say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowSize {
    Small,
    #[default]
    Medium,
    Large,
    /// No guideline: the orchestrator sizes it from the request alone.
    None,
}

impl WorkflowSize {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkflowSize::Small => "small",
            WorkflowSize::Medium => "medium",
            WorkflowSize::Large => "large",
            WorkflowSize::None => "none",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize, Serialize)]
    struct File {
        workflows: WorkflowSettings,
    }

    #[test]
    fn no_setting_picks_a_kind_of_run() {
        // No setting picks the kind of run: a goal is a condition you set, on any CLI.
        let written = toml::to_string(&File {
            workflows: WorkflowSettings::default(),
        })
        .expect("written");
        assert!(!written.contains("style"), "{written}");
    }

    #[test]
    fn orchestrator_designs_reads_a_switch_or_any() {
        for (text, design) in [
            ("true", OrchestratorDesign::Size),
            ("false", OrchestratorDesign::Off),
            ("\"any\"", OrchestratorDesign::Any),
        ] {
            let file: File =
                toml::from_str(&format!("[workflows]\norchestrator_designs = {text}\n"))
                    .unwrap_or_else(|err| panic!("{text}: {err}"));
            assert_eq!(file.workflows.orchestrator_designs.design(), design);
            let written = toml::to_string(&file).expect("written");
            assert!(
                written.contains(&format!("orchestrator_designs = {text}")),
                "{written}"
            );
        }
        assert!(toml::from_str::<File>("[workflows]\norchestrator_designs = \"all\"\n").is_err());
    }
}
