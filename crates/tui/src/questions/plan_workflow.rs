//! The workflow a plan approval shows: its name, the main agent's reason, the parameters the main
//! agent filled, and its phases in plain words. One line per phase in the words of the workflow
//! file, with its conditions and loop rules, waits for Ctrl+O.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::approval::PlanWorkflow;
use argonauts_core::workflow::{Count, Phase, Step, StepKind, StepModels, Workflow};
use serde::Serialize;
use serde_json::Value;

use super::item::AboutCell;
use crate::theme::Role;

/// Under the plan's summary, which already says the main agent's reason when there's no plan of its
/// own.
pub(super) fn push(
    item: &mut AboutCell,
    shown: &PlanWorkflow,
    summary: &str,
    names: &DisplayNames,
) {
    let workflow = &shown.workflow;
    item.blank();
    let mut name = argonauts_core::text::shown_name(&workflow.name);
    if shown.from.is_none() {
        name.push_str(" (composed by the main agent)");
    }
    if !workflow.description.trim().is_empty() {
        name.push_str(&format!(" · {}", workflow.description.trim()));
    }
    // The run goes by its own name; the example it started from shows here once.
    if let Some(from) = shown
        .from
        .as_ref()
        .filter(|from| from.as_str() != workflow.name)
    {
        let from = argonauts_core::text::shown_name(from.as_str());
        name.push_str(&format!(" · started from {from}"));
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
    let flow = flow(workflow);
    if !flow.is_empty() {
        item.push(flow, Role::Text, 1);
    }
    for phase in &workflow.phases {
        let steps: Vec<String> = phase
            .steps
            .iter()
            .map(|step| step_text(step, names))
            .collect();
        // A phase of a script has no steps of this kind: its one line says who does what.
        let says = if steps.is_empty() {
            phase.detail.trim().to_owned()
        } else {
            steps.join("; ")
        };
        let line = if says.is_empty() {
            phase.name.clone()
        } else {
            format!("{}: {says}", phase.name)
        };
        item.more(line, Role::Dim, 1);
    }
}

/// For the question itself: `review-panel · review → settle (if needed)`.
pub(super) fn summary(shown: &PlanWorkflow) -> String {
    let flow = flow(&shown.workflow);
    if flow.is_empty() {
        return shown.workflow.name.clone();
    }
    format!("{} · {flow}", shown.workflow.name)
}

/// The phases in plain words: `understand (up to 2 rounds) → plan (up to 3 rounds) → build →
/// merge`.
fn flow(workflow: &Workflow) -> String {
    workflow
        .phases
        .iter()
        .map(phase_words)
        .filter(|words| !words.is_empty())
        .collect::<Vec<_>>()
        .join(" →  ")
}

fn phase_words(phase: &Phase) -> String {
    let mut words = phase.name.trim().to_lowercase();
    if words.is_empty() {
        return words;
    }
    let rounds = phase
        .steps
        .iter()
        .find(|step| step.kind == StepKind::LoopUntil)
        .and_then(|step| step.max);
    if let Some(max) = rounds {
        let unit = if max == 1 { "round" } else { "rounds" };
        words.push_str(&format!(" (up to {max} {unit})"));
    } else if !phase.steps.is_empty() && phase.steps.iter().all(|step| step.when.is_some()) {
        words.push_str(" (if needed)");
    }
    words
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

/// `fan_out reviewer ×2, blind, another provider than author`, or a loop with its steps inside.
fn step_text(step: &Step, names: &DisplayNames) -> String {
    if step.kind == StepKind::Approval {
        return match step.approval.and_then(|kind| word(&kind)) {
            Some(kind) => format!("{kind} approval"),
            None => "approval".to_owned(),
        };
    }
    let kind = word(&step.kind).unwrap_or_default();
    let mut text = match step.kind {
        StepKind::LoopUntil => "loop".to_owned(),
        _ => kind.clone(),
    };
    // A reader's reading says nothing new; a critic critiquing does.
    if let Some(role) = step.role.and_then(|role| word(&role))
        && !role.starts_with(&kind)
    {
        text.push_str(&format!(" {role}"));
    }
    let roles: Vec<String> = step.roles.iter().filter_map(word).collect();
    if !roles.is_empty() {
        text.push_str(&format!(" {}", roles.join(", ")));
    }
    if let StepModels::Named(workers) = &step.models
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
    if let Some(other) = &step.different_provider_from {
        text.push_str(&format!(", another provider than {other}"));
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
