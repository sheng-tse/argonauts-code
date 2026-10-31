//! Autopilot's settings that may not do what you meant: a model picked to answer that no agent
//! or endpoint provides, or that the judge can't check, and an approval the switch leaves
//! with you.

use argonauts_core::base::names::ModelRef;
use argonauts_core::models::registry::provider::is_known;
use argonauts_core::settings::Settings;

use super::roles::{self, AgentIds};
use crate::models::resolve_provider;

pub(crate) fn warnings(settings: &Settings, agents: &AgentIds<'_>) -> Vec<String> {
    let approvals = &settings.approvals;
    let mut warnings = Vec::new();
    let answerer = approvals
        .answers
        .as_ref()
        .and_then(|answerer| answerer.model());
    if let Some(model) = answerer {
        let key = "approvals.answers";
        if !agents.is_known(&model.agent) {
            warnings.push(format!(
                "{}. Until one does, the main agent answers in its place",
                roles::unknown(key, model)
            ));
        } else if agents.is_off(&model.agent) {
            warnings.push(roles::off(key, model));
        }
    }
    if let (Some(model), Some(judge)) = (answerer, approvals.judge_model())
        && let Some(provider) = one_provider(model, judge)
    {
        warnings.push(format!(
            "approvals.answers ({model}) and approvals.decides ({judge}) both come \
             from {provider}, so the judge can't check that model's answers, and on autopilot \
             the agents are told to use their best judgment instead. Pick a judge from \
             another provider"
        ));
    }
    if approvals.autopilot {
        for (name, mode) in [
            ("goal", approvals.understand.mode),
            ("plan", approvals.plan.mode),
            ("merge", approvals.merge.mode),
            ("stuck", approvals.stuck.mode),
            ("limit", approvals.limit.mode),
            ("change_request", approvals.change_request.mode),
        ] {
            if !mode.goes_ahead() {
                warnings.push(format!(
                    "approvals.autopilot is on, but approvals.{name}.mode = \"{}\" keeps that \
                     approval for you, so a run still waits there",
                    // The setting's own spelling, which has no space.
                    mode.label().replace(' ', "_")
                ));
            }
        }
    }
    warnings
}

/// The provider both models come from, when they share one the provider table knows.
fn one_provider(a: &ModelRef, b: &ModelRef) -> Option<String> {
    let provider = |model: &ModelRef| resolve_provider(model, None, None);
    let theirs = provider(a);
    (is_known(&theirs) && theirs == provider(b)).then_some(theirs)
}
