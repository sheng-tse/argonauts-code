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
        .answered_by
        .as_ref()
        .and_then(|answerer| answerer.model());
    if let Some(model) = answerer {
        let key = "approvals.answered_by";
        if !workers.is_known(&model.worker) {
            warnings.push(format!(
                "{}. Until one does, the lead answers in its place",
                roles::unknown(key, model)
            ));
        } else if workers.is_off(&model.worker) {
            warnings.push(roles::off(key, model));
        }
    }
    if let (Some(model), Some(approver)) = (answerer, approvals.approver.model.as_ref())
        && let Some(company) = one_company(model, approver)
    {
        warnings.push(format!(
            "approvals.answered_by ({model}) and approvals.approver.model ({approver}) both come \
             from {company}, so the approver can't check that model's answers, and in full auto \
             the agents are told to use their best judgment instead. Pick an approver from \
             another company"
        ));
    }
    if approvals.full_auto {
        for (name, mode) in [
            ("goal", approvals.goal.mode),
            ("plan", approvals.plan.mode),
            ("merge", approvals.merge.mode),
            ("escalation", approvals.escalation.mode),
            ("budget", approvals.budget.mode),
            ("change_request", approvals.change_request.mode),
        ] {
            if !mode.goes_ahead() {
                warnings.push(format!(
                    "approvals.full_auto is on, but approvals.{name}.mode = \"{}\" keeps that \
                     checkpoint for you, so a run still waits there",
                    mode.label()
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
