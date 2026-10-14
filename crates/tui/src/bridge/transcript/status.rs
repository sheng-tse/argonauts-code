//! `/status` answers, built by code from the run's log, so asking costs nothing.

use argonauts_core::api::StatusSummary;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::task::TaskState;
use argonauts_core::settings::limit::{LimitAmount, LimitUnit};

use crate::cell::{Cell, Tone, ToolItem, ToolState};
use crate::format;

use super::steps::step;

/// Recent lines worth showing; the log keeps the rest.
const RECENT: usize = 5;

/// `● Status(run-06)` with the goal, plan, decisions, tasks, recent events, spend, and what
/// waits for you, each under its own `⎿`.
pub fn status_cells(summary: &StatusSummary, names: &DisplayNames) -> Vec<Cell> {
    let args = summary
        .run
        .as_ref()
        .map_or_else(|| "no run".to_owned(), ToString::to_string);
    let mut call = step("Status", &args, ToolState::Done, &[]);
    for lines in status_sections(summary, names) {
        call.push(ToolItem::Text {
            text: lines.join("\n"),
            tone: Tone::Dim,
        });
    }
    if call.body.is_empty() {
        call.push(ToolItem::Text {
            text: NOTHING_RUNNING.to_owned(),
            tone: Tone::Dim,
        });
    }
    vec![call.into()]
}

/// What a status with no sections says.
pub const NOTHING_RUNNING: &str = "Nothing running · describe a change to start one";

/// The goal, plan, decisions, tasks, recent events, spend, and what waits for you, each a group
/// of trimmed, non-empty lines. The `/status` view draws the same groups.
pub fn status_sections(summary: &StatusSummary, names: &DisplayNames) -> Vec<Vec<String>> {
    let mut sections: Vec<Vec<String>> = Vec::new();
    let mut section = |lines: Vec<String>| sections.push(lines);
    if let Some((first, criteria)) = summary.goal.split_first() {
        let mut lines = vec![format!("Goal: {first}")];
        lines.extend(criteria.iter().cloned());
        section(lines);
    }
    if let Some(plan) = &summary.plan {
        section(vec![format!("Plan: {plan}")]);
    }
    if !summary.decisions.is_empty() {
        section(
            summary
                .decisions
                .iter()
                .map(|(id, title)| format!("{id} {title}"))
                .collect(),
        );
    }
    if !summary.tasks.is_empty() {
        section(
            summary
                .tasks
                .iter()
                .map(|task| {
                    let mut line = format!("{} {} · {}", task.task, task.title, state(task.state));
                    if let Some(worker) = &task.worker {
                        line.push_str(&format!(" · {}", names.worker(worker)));
                    }
                    if !task.note.trim().is_empty() {
                        line.push_str(&format!(" · {}", task.note.trim()));
                    }
                    line
                })
                .collect(),
        );
    }
    let skip = summary.recent.len().saturating_sub(RECENT);
    section(summary.recent.iter().skip(skip).cloned().collect());
    if let Some(spend) = &summary.spend {
        let spent = format::spend(&spend.spent);
        let line = match &spend.limit {
            Some(limit) => format!("Spent {spent} of {}", amount(limit)),
            None => format!("Spent {spent}"),
        };
        section(vec![line]);
    }
    if !summary.waiting.is_empty() {
        section(vec![format!(
            "Waiting on you: {}",
            summary.waiting.join(" · ")
        )]);
    }
    sections
        .into_iter()
        .map(|lines| {
            lines
                .iter()
                .map(|line| line.trim().to_owned())
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|lines| !lines.is_empty())
        .collect()
}

fn amount(limit: &LimitAmount) -> String {
    match limit.unit {
        LimitUnit::Usd => format::usd(limit.amount),
        LimitUnit::Tokens => format!("{} tokens", format::tokens(limit.amount.max(0.0) as u64)),
        LimitUnit::PlanUsage => format!("{:.0}% of plan usage", limit.amount * 100.0),
        LimitUnit::Minutes => format!("{:.0} minutes", limit.amount),
    }
}

fn state(state: TaskState) -> &'static str {
    match state {
        TaskState::Waiting => "waiting",
        TaskState::Ready => "ready",
        TaskState::Running => "running",
        TaskState::Blocked => "blocked on a question",
        TaskState::NeedsDecision => "needs a decision",
        TaskState::Checking => "running checks",
        TaskState::InReview => "in review",
        TaskState::Merging => "merging",
        TaskState::Merged => "merged into the run",
        TaskState::Failed => "failed",
        TaskState::NeedsYou => "needs you",
        TaskState::Deferred => "waiting for limit",
        TaskState::Cancelled => "cancelled",
        TaskState::TakenOver => "taken over",
    }
}
