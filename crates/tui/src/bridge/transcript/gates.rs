//! `● Gates(tests, lint, link check)`: a card's gate results, printed together once it leaves
//! gating.

use argonauts_core::events::card::{GateKind, GateOutcome};

use crate::cell::ToolState;

use super::steps::step;
use super::{Cx, emit};

/// `● Gates(tests, lint, link check)` then `⎿  All 3 passed` or what failed. In a crew run the
/// card shows by its label, `Gates(build:flag · tests)`; `None` is the final check.
pub(super) fn report(card: Option<&str>, gates: &[GateOutcome], label: bool, cx: &mut Cx<'_>) {
    if gates.is_empty() && card.is_none() {
        return;
    }
    let gates: Vec<&GateOutcome> = gates.iter().filter(|gate| !is_unset(gate)).collect();
    let listed: Vec<&GateOutcome> = gates
        .iter()
        .copied()
        .filter(|gate| is_listed(gate.kind))
        .collect();
    let names: Vec<&str> = listed.iter().map(|gate| gate.name.trim()).collect();
    let mut args = if names.is_empty() {
        "built-in checks".to_owned()
    } else {
        names.join(", ")
    };
    match card {
        None => args = format!("final check · {args}"),
        Some(card) if label => args = format!("{card} · {args}"),
        Some(_) => {}
    }
    let failed: Vec<&GateOutcome> = gates.iter().copied().filter(|gate| !gate.passed).collect();
    let mut details: Vec<String> = failed
        .iter()
        .map(|gate| {
            let summary = gate.summary.trim();
            if summary.is_empty() {
                format!("{} failed", gate.name.trim())
            } else {
                format!("{} failed · {summary}", gate.name.trim())
            }
        })
        .collect();
    if listed.is_empty() {
        details.push("No command checks set · /config checks.run".to_owned());
    } else if failed.is_empty() {
        details.push(match listed.len() {
            1 => "Passed".to_owned(),
            n => format!("All {n} passed"),
        });
    }
    let state = if failed.is_empty() {
        ToolState::Done
    } else {
        ToolState::Failed
    };
    emit(cx, step("Checks", &args, state, &details));
}

/// The engine's marker for an empty `gates.run`. Nothing ran, so it neither failed nor names a
/// gate; the item says no command gates are set instead.
fn is_unset(gate: &GateOutcome) -> bool {
    gate.kind == GateKind::Command && gate.command.is_none() && gate.name == "checks.run"
}

/// Gates you set, and the suites the final check runs. The built-in checks show only when one
/// fails.
fn is_listed(kind: GateKind) -> bool {
    matches!(
        kind,
        GateKind::Command | GateKind::AffectedTests | GateKind::FullSuite
    )
}
