//! How the lead picks, composes, and adapts workflows, `[workflows]`. The rules a workflow
//! can't turn off (gates before review, review by another family, hard limits, approvals,
//! budgets, and handoffs) aren't settings here.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WorkflowSettings {
    /// The lead picks or composes a workflow per task. Off runs the preset's workflow as is.
    pub lead_designs: bool,
    /// The lead may compose new workflows from the step kinds. Code validates them first.
    pub allow_composed: bool,
    /// Nesting depth, at most 2.
    pub max_depth: u32,
    /// At a phase boundary the lead may propose a change, which goes through the
    /// `change_request` checkpoint.
    pub adapt_at_phase_ends: bool,
}

impl Default for WorkflowSettings {
    fn default() -> Self {
        Self {
            lead_designs: true,
            allow_composed: true,
            max_depth: 2,
            adapt_at_phase_ends: true,
        }
    }
}
