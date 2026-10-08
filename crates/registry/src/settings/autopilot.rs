//! Full auto's settings that may not do what you meant: a model picked to answer that no agent
//! or endpoint provides, or that the approver can't check, and a checkpoint the switch leaves
//! with you.

use argonauts_core::names::WorkerRef;
use argonauts_core::registry::company_of;
use argonauts_core::settings::Settings;

use super::roles::{self, Workers};
use crate::models::resolve_family;

pub(crate) fn warnings(settings: &Settings, workers: &Workers<'_>) -> Vec<String> {
    let approvals = &settings.approvals;
    let mut warnings = Vec::new();
    let answerer = approvals
        .answers
        .as_ref()
        .and_then(|answerer| answerer.model());
    if let Some(model) = answerer {
        let key = "approvals.answers";
        if !workers.is_known(&model.worker) {
            warnings.push(format!(
                "{}. Until one does, the main agent answers in its place",
                roles::unknown(key, model)
            ));
        } else if workers.is_off(&model.worker) {
            warnings.push(roles::off(key, model));
        }
    }
    if let (Some(model), Some(approver)) = (answerer, approvals.approver_model())
        && let Some(company) = one_company(model, approver)
    {
        warnings.push(format!(
            "approvals.answers ({model}) and approvals.decides ({approver}) both come \
             from {company}, so the approver can't check that model's answers, and on autopilot \
             the agents are told to use their best judgment instead. Pick an approver from \
             another provider"
        ));
    }
    if approvals.autopilot {
        for (name, mode) in [
            ("goal", approvals.goal.mode),
            ("plan", approvals.plan.mode),
            ("merge", approvals.merge.mode),
            ("stuck", approvals.escalation.mode),
            ("limit", approvals.budget.mode),
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

/// The company both workers come from, when they share one the family table knows.
fn one_company(a: &WorkerRef, b: &WorkerRef) -> Option<String> {
    let company = |worker: &WorkerRef| company_of(&resolve_family(worker, None, None));
    let theirs = company(a);
    (!theirs.is_empty() && theirs != "unknown" && theirs == company(b)).then_some(theirs)
}
