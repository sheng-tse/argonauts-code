//! The workflow a plan checkpoint shows: its name, the lead's reason, the parameters the lead
//! filled, and one line per phase in the words of the workflow file, with agents by name.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::checkpoint::PlanWorkflow;
use argonauts_core::workflow::{Count, Step, StepKind, StepWorkers, Workflow};
use serde::Serialize;
use serde_json::Value;

use super::item::AboutCell;
use crate::theme::Role;

/// Under the plan's summary, which already says the lead's reason when there's no plan of its
/// own.
pub(super) fn push(
    item: &mut AboutCell,
    shown: &PlanWorkflow,
    summary: &str,
    names: &DisplayNames,
) {
    let workflow = &shown.workflow;
    item.blank();
    let mut name = workflow.name.clone();
    if shown.from.is_none() {
        name.push_str(" (composed by the lead)");
    }
    if !workflow.description.trim().is_empty() {
        name.push_str(&format!(" · {}", workflow.description.trim()));
    }
    item.labeled("Workflow", name);
    if let Some(why) = shown.why.as_deref().map(str::trim)
        && !why.is_empty()
        && why != summary.trim()
    {
        item.push(why, Role::Text, 1);
    }
    if let Some(params) = params(workflow) {
        item.push(params, Role::Dim, 1);
    }
    for phase in &workflow.phases {
        let steps: Vec<String> = phase
            .steps
            .iter()
            .map(|step| step_text(step, names))
            .collect();
        item.push(
            format!("{}: {}", phase.name, steps.join("; ")),
            Role::Text,
            1,
        );
    }
}

/// For the card itself: `review-panel · 4 phases`.
pub(super) fn summary(shown: &PlanWorkflow) -> String {
    let phases = shown.workflow.phases.len();
    format!(
        "{} · {phases} phase{}",
        shown.workflow.name,
        if phases == 1 { "" } else { "s" }
    )
}

fn params(workflow: &Workflow) -> Option<String> {
    let pairs: Vec<String> = workflow
        .params
        .0
        .iter()
        .map(|(name, value)| format!("{name} {}", scalar(value)))
        .collect();
    (!pairs.is_empty()).then(|| pairs.join(" · "))
}

fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// `fan_out reviewer ×2, blind, another family than author`, or a loop with its steps inside.
fn step_text(step: &Step, names: &DisplayNames) -> String {
    if step.kind == StepKind::Checkpoint {
        return match step.checkpoint.and_then(|kind| word(&kind)) {
            Some(kind) => format!("{kind} checkpoint"),
            None => "checkpoint".to_owned(),
        };
    }
    let kind = word(&step.kind).unwrap_or_default();
    let mut text = match step.kind {
        StepKind::LoopUntil => "loop".to_owned(),
        _ => kind.clone(),
    };
    // A restater restating says nothing new; an adjudicator judging does.
    if let Some(role) = step.role.and_then(|role| word(&role))
        && !role.starts_with(&kind)
    {
        text.push_str(&format!(" {role}"));
    }
    let roles: Vec<String> = step.roles.iter().filter_map(word).collect();
    if !roles.is_empty() {
        text.push_str(&format!(" {}", roles.join(", ")));
    }
    if let StepWorkers::Named(workers) = &step.workers
        && !workers.is_empty()
    {
        let workers: Vec<String> = workers.iter().map(|worker| names.worker(worker)).collect();
        text.push_str(&format!(" ({})", workers.join(", ")));
    }
    match &step.count {
        Some(Count::Fixed(count)) => text.push_str(&format!(" ×{count}")),
        Some(Count::Param(name)) => text.push_str(&format!(" ×{name}")),
        None => {}
    }
    if let Some(workflow) = &step.workflow {
        text.push_str(&format!(" {workflow}"));
    }
    if step.blind {
        text.push_str(", blind");
    }
    if let Some(other) = &step.different_family_from {
        text.push_str(&format!(", another family than {other}"));
    }
    if let Some(until) = &step.until {
        text.push_str(&format!(" until {until}"));
        if let Some(max) = step.max {
            text.push_str(&format!(", at most {max}"));
        }
    }
    if let Some(when) = &step.when {
        text.push_str(&format!(", when {when}"));
    }
    if !step.steps.is_empty() {
        let inner: Vec<String> = step
            .steps
            .iter()
            .map(|inner| step_text(inner, names))
            .collect();
        text.push_str(&format!(" ({})", inner.join(", ")));
    }
    text
}

/// A kind or role as the workflow file spells it.
fn word(value: &impl Serialize) -> Option<String> {
    match serde_json::to_value(value).ok()? {
        Value::String(text) => Some(text),
        _ => None,
    }
}
