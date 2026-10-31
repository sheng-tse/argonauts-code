//! Where the run's workflow is, in words: the standard phase first, then the phase names from
//! the run's workflow down through nested ones, then the step, such as
//! "check · Check › review-panel › Look, step 1 of 2: fan out".

use argonauts_core::workflows::workflow::{StepKind, StepRef};

use crate::state::workflow::Workflows;
use crate::status::folder::words;

/// The working line for what runs now, or `None` between steps and once the workflow ends.
pub fn line(workflows: &Workflows) -> Option<String> {
    let standard = workflows.current_standard()?;
    let running = workflows.running();
    let (_, progress) = running.last()?;
    let at = place(workflows, &progress.step, progress.kind, progress.round);
    Some(format!("{} · {at}", words(&standard)))
}

/// A step's breadcrumb, its number among its phase's steps, and its name.
pub fn place(workflows: &Workflows, step: &StepRef, kind: StepKind, round: u32) -> String {
    let crumbs = breadcrumb(workflows, step).join(" › ");
    let record = workflows.by_id.get(&step.workflow);
    let number = step.path.first().map_or(1, |index| index + 1);
    let count = record
        .and_then(|record| record.phase_definition(step.phase))
        .map(|phase| phase.steps.len());
    let name = record
        .and_then(|record| record.step_definition(step))
        .and_then(|definition| definition.name.clone())
        .unwrap_or_else(|| words(&kind));
    let mut text = match count {
        Some(count) => format!("{crumbs}, step {number} of {count}: {name}"),
        None => format!("{crumbs}, step {number}: {name}"),
    };
    if round > 1 {
        text.push_str(&format!(", round {round}"));
    }
    text
}

/// Phase names from the run's workflow down to the step's, with each nested workflow's name
/// between its parent's phase and its own.
fn breadcrumb(workflows: &Workflows, step: &StepRef) -> Vec<String> {
    let mut crumbs = Vec::new();
    let mut at = Some(step);
    // Nesting is at most 2 deep. The bound only keeps a malformed log from looping forever.
    for _ in 0..8 {
        let Some(step) = at else { break };
        let Some(record) = workflows.by_id.get(&step.workflow) else {
            break;
        };
        crumbs.push(
            record
                .phase_definition(step.phase)
                .map(|phase| phase.name.clone())
                .unwrap_or_else(|| format!("phase {}", step.phase + 1)),
        );
        at = record.parent.as_ref();
        if at.is_some() {
            crumbs.push(record.workflow.name.clone());
        }
    }
    crumbs.reverse();
    crumbs
}
