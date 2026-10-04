//! How the lead picks, composes, and adapts workflows, `[workflows]`. The rules a workflow
//! can't turn off (gates before review, review by another family, hard limits, approvals,
//! budgets, and handoffs) aren't settings here.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WorkflowSettings {
    /// How far the lead may change your preset's workflow for a task: `true` lets it size the
    /// workflow, `false` runs it as it is, and `"any"` lets it pick another or compose one.
    pub lead_designs: LeadDesigns,
    /// The lead may compose new workflows from the step kinds. Code validates them first.
    pub allow_composed: bool,
    /// Nesting depth, at most 2.
    pub max_depth: u32,
    /// At a phase boundary the lead may propose a change, which goes through the
    /// `change_request` checkpoint.
    pub adapt_at_phase_ends: bool,
    /// How a crew run works: as a workflow (the lead's phases, done when they finish), as a
    /// goal (attempts until a check finds every criterion met), or as the lead's CLI does.
    pub style: StyleSetting,
    /// What `follow_lead` picks, by the lead's CLI. Any CLI, ACP agent, or endpoint not
    /// listed leads a workflow.
    pub style_by_lead: BTreeMap<String, RunStyle>,
    /// A check from another company judges the done-when list before Merge.
    pub final_check: bool,
}

/// `workflows.style`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StyleSetting {
    /// Whatever the lead's CLI does on its own (`workflows.style_by_lead`).
    #[default]
    FollowLead,
    Workflow,
    Goal,
}

/// How one crew run works, fixed when it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStyle {
    /// Claude Code's dynamic workflow: the lead writes the phases up front, and the run ends
    /// when they finish.
    Workflow,
    /// Codex's `/goal`, run by a crew: an objective and a done-when list, pursued in attempts
    /// until a check from another company finds every criterion met.
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

/// Claude Code leads a workflow. Codex and Antigravity CLI lead a goal, since their own
/// `/goal` runs until the goal is achieved.
const STYLE_BY_LEAD: &[(&str, RunStyle)] = &[
    ("claude-code", RunStyle::Workflow),
    ("codex", RunStyle::Goal),
    ("antigravity-cli", RunStyle::Goal),
];

impl WorkflowSettings {
    /// The style a new crew run takes under `lead`, a CLI's agent id, before any command,
    /// words, or card for that run changes it. A CLI your `style_by_lead` leaves out keeps the
    /// built-in pick, so setting one CLI doesn't drop the others.
    pub fn style_for_lead(&self, lead: &str) -> RunStyle {
        match self.style {
            StyleSetting::Workflow => RunStyle::Workflow,
            StyleSetting::Goal => RunStyle::Goal,
            StyleSetting::FollowLead => self
                .style_by_lead
                .get(lead)
                .copied()
                .or_else(|| {
                    STYLE_BY_LEAD
                        .iter()
                        .find(|(cli, _)| *cli == lead)
                        .map(|(_, style)| *style)
                })
                .unwrap_or(RunStyle::Workflow),
        }
    }
}

impl Default for WorkflowSettings {
    fn default() -> Self {
        Self {
            lead_designs: LeadDesigns::Preset(true),
            allow_composed: true,
            max_depth: 2,
            adapt_at_phase_ends: true,
            style: StyleSetting::FollowLead,
            style_by_lead: STYLE_BY_LEAD
                .iter()
                .map(|(cli, style)| ((*cli).to_owned(), *style))
                .collect(),
            final_check: true,
        }
    }
}

/// `workflows.lead_designs`, written `true`, `false`, or `"any"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum LeadDesigns {
    /// `true`: the lead sizes your preset's workflow for the task. `false`: it runs as it is.
    Preset(bool),
    /// `"any"`: the lead may run another template, or compose one when `allow_composed`.
    Any(AnyWorkflow),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnyWorkflow {
    Any,
}

impl LeadDesigns {
    pub const ANY: Self = Self::Any(AnyWorkflow::Any);

    pub fn design(self) -> LeadDesign {
        match self {
            Self::Preset(false) => LeadDesign::Off,
            Self::Preset(true) => LeadDesign::Size,
            Self::Any(AnyWorkflow::Any) => LeadDesign::Any,
        }
    }
}

impl From<bool> for LeadDesigns {
    fn from(on: bool) -> Self {
        Self::Preset(on)
    }
}

/// How far the lead may change the preset's workflow for one task, least first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LeadDesign {
    /// The preset's workflow runs as it is.
    Off,
    /// The lead keeps the preset's workflow and sizes it: more or fewer agents at a step, or a
    /// step that doesn't apply left out.
    Size,
    /// The lead may run another template or compose one.
    Any,
}
#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize, Serialize)]
    struct File {
        workflows: WorkflowSettings,
    }

    #[test]
    fn the_style_follows_the_lead_unless_set() {
        let mut settings = WorkflowSettings::default();
        assert_eq!(settings.style_for_lead("claude-code"), RunStyle::Workflow);
        assert_eq!(settings.style_for_lead("codex"), RunStyle::Goal);
        assert_eq!(settings.style_for_lead("antigravity-cli"), RunStyle::Goal);
        assert_eq!(settings.style_for_lead("opencode"), RunStyle::Workflow);
        // A file that lists only Codex keeps the others' built-in picks.
        settings.style_by_lead = [("codex".to_owned(), RunStyle::Workflow)].into();
        assert_eq!(settings.style_for_lead("codex"), RunStyle::Workflow);
        assert_eq!(settings.style_for_lead("antigravity-cli"), RunStyle::Goal);
        settings.style = StyleSetting::Goal;
        assert_eq!(settings.style_for_lead("claude-code"), RunStyle::Goal);
    }

    #[test]
    fn lead_designs_reads_a_switch_or_any() {
        for (text, design) in [
            ("true", LeadDesign::Size),
            ("false", LeadDesign::Off),
            ("\"any\"", LeadDesign::Any),
        ] {
            let file: File = toml::from_str(&format!("[workflows]\nlead_designs = {text}\n"))
                .unwrap_or_else(|err| panic!("{text}: {err}"));
            assert_eq!(file.workflows.lead_designs.design(), design);
            let written = toml::to_string(&file).expect("written");
            assert!(
                written.contains(&format!("lead_designs = {text}")),
                "{written}"
            );
        }
        assert!(toml::from_str::<File>("[workflows]\nlead_designs = \"all\"\n").is_err());
    }
}
