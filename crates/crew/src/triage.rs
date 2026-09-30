//! Triage rules: solo or crew, computed by code from the default agent's estimate.

use argonauts_core::events::run::{RunMode, TriageDecision};
use argonauts_core::settings::Settings;
use argonauts_core::steps::intake::ChangeEstimate;

/// The placeholder's rule name, so nobody mistakes it for a real decision.
pub const NOT_IMPLEMENTED: &str = "not-implemented";

pub fn decide(estimate: &ChangeEstimate, settings: &Settings) -> TriageDecision {
    let _ = (estimate, settings);
    TriageDecision {
        mode: RunMode::Solo,
        escalate_to_crew: false,
        rule: NOT_IMPLEMENTED.to_owned(),
        reason: "the triage rules are not implemented yet".to_owned(),
    }
}
