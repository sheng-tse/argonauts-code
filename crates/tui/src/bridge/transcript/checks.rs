//! `● Checks(tests, lint, link check)`: a task's check results, printed together once it leaves
//! checking.

use argonauts_core::events::task::{CheckKind, CheckOutcome};

use crate::cell::ToolState;

use super::steps::step;
use super::{Cx, emit};

/// `● Checks(tests, lint, link check)` then `⎿  All 3 passed` or what failed. In a crew run the
/// task shows by its label, `Checks(build:flag · tests)`; `None` is the final check.
pub(super) fn report(task: Option<&str>, checks: &[CheckOutcome], label: bool, cx: &mut Cx<'_>) {
    if checks.is_empty() && task.is_none() {
        return;
    }
    let checks: Vec<&CheckOutcome> = checks.iter().filter(|check| !is_unset(check)).collect();
    let listed: Vec<&CheckOutcome> = checks
        .iter()
        .copied()
        .filter(|check| is_listed(check.kind))
        .collect();
    let names: Vec<&str> = listed.iter().map(|check| check.name.trim()).collect();
    let mut args = if names.is_empty() {
        "built-in checks".to_owned()
    } else {
        names.join(", ")
    };
    match task {
        None => args = format!("final check · {args}"),
        Some(task) if label => args = format!("{task} · {args}"),
        Some(_) => {}
    }
    let failed: Vec<&CheckOutcome> = checks
        .iter()
        .copied()
        .filter(|check| !check.passed)
        .collect();
    let mut details: Vec<String> = failed
        .iter()
        .map(|check| {
            let summary = check.summary.trim();
            if summary.is_empty() {
                format!("{} failed", check.name.trim())
            } else {
                format!("{} failed · {summary}", check.name.trim())
            }
        })
        .collect();
    if listed.is_empty() {
        details.push("No command checks set · /config checks.run".to_owned());
    } else if failed.is_empty() {
        details.push(match listed.as_slice() {
            [check] => passed_line(check),
            _ => format!("All {} passed", listed.len()),
        });
    }
    let state = if failed.is_empty() {
        ToolState::Done
    } else {
        ToolState::Failed
    };
    emit(cx, step("Checks", &args, state, &details));
}

/// One check that passed, as it ran: `python3 -m unittest -q · 12 passed in 0.03s`.
fn passed_line(check: &CheckOutcome) -> String {
    let summary = check.summary.trim();
    let counted = !summary.is_empty() && summary != "passed";
    match check.command.as_deref() {
        Some(command) if counted => format!(
            "{} · {summary} in {}",
            command.join(" "),
            seconds(check.duration_ms)
        ),
        Some(command) => format!("{} · passed", command.join(" ")),
        None => "Passed".to_owned(),
    }
}

/// `0.03s`, `1.5s`, `12s`: as a test runner says how long it took.
fn seconds(ms: u64) -> String {
    if ms >= 10_000 {
        return format!("{}s", ms / 1000);
    }
    let text = format!("{:.2}", ms as f64 / 1000.0);
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{text}s")
}

/// The engine's marker for an empty `checks.run`. Nothing ran, so it neither failed nor names a
/// check; the item says no command checks are set instead.
fn is_unset(check: &CheckOutcome) -> bool {
    check.kind == CheckKind::Command && check.command.is_none() && check.name == "checks.run"
}

/// Checks you set, and the suites the final check runs. The built-in checks show only when one
/// fails.
fn is_listed(kind: CheckKind) -> bool {
    matches!(
        kind,
        CheckKind::Command | CheckKind::AffectedTests | CheckKind::FullSuite
    )
}
