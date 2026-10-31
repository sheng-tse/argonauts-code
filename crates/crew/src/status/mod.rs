//! The status summary, built by code from the event log at no token cost and filtered by who
//! is looking.

use std::fmt::Write as _;

use argonauts_core::TaskId;
use argonauts_core::base::names::{PlanLabel, RoleName};
use argonauts_core::runs::api::StatusSummary;
use argonauts_core::runs::events::RunEvent;
use argonauts_core::settings::limit::LimitUnit;

pub mod closing;
pub mod files;
pub mod folder;
pub mod visibility;
pub mod working;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Viewer {
    /// Sees everything.
    User,
    Session {
        role: RoleName,
        task: Option<TaskId>,
        /// Set while drafts are sealed.
        label: Option<PlanLabel>,
    },
    /// Sees digests only.
    Narrator,
}

pub fn summary(events: &[RunEvent], viewer: &Viewer) -> StatusSummary {
    let mut folder = folder::Folder::default();
    for event in events {
        folder.push(event);
    }
    folder.summary(viewer)
}

/// The text injected at session start and served by the MCP status tool.
pub fn render(summary: &StatusSummary) -> String {
    let Some(run) = &summary.run else {
        return "No run is going.\n".to_owned();
    };
    let mut out = format!("Status of {run}\n");
    if let Some((first, criteria)) = summary.goal.split_first() {
        let _ = writeln!(out, "\nGoal: {first}");
        for criterion in criteria {
            let _ = writeln!(out, "  {criterion}");
        }
    }
    if let Some(plan) = &summary.plan {
        let _ = writeln!(out, "\nPlan: {plan}");
    }
    section(
        &mut out,
        "Decisions",
        summary
            .decisions
            .iter()
            .map(|(id, text)| format!("{id} {text}")),
    );
    section(
        &mut out,
        "Tasks",
        summary.tasks.iter().map(|task| {
            let mut row = format!(
                "{} [{}] {}",
                task.task,
                folder::words(&task.state),
                task.title
            );
            if let Some(worker) = &task.worker {
                let _ = write!(row, " · {worker}");
            }
            if !task.note.is_empty() {
                let _ = write!(row, " · {}", task.note);
            }
            row
        }),
    );
    section(&mut out, "Recent", summary.recent.iter().cloned());
    if let Some(spend) = &summary.spend {
        let spent = format!("${:.2}", spend.spent.cost_usd);
        let line = match &spend.limit {
            Some(limit) => format!("{spent} of {}", amount(limit.unit, limit.amount)),
            None => spent,
        };
        let _ = writeln!(out, "\nLimit: {line}");
    }
    section(
        &mut out,
        "PlanUsages",
        summary.plan_windows.iter().map(|plan_usage| {
            let mut row = format!(
                "{} {}",
                plan_usage.agent_id,
                folder::words(&plan_usage.status)
            );
            if let Some(used) = plan_usage.used {
                let _ = write!(row, ", {:.0}% used", used * 100.0);
            }
            if let Some(window) = &plan_usage.window {
                let _ = write!(row, " ({window})");
            }
            row
        }),
    );
    section(&mut out, "Waiting on you", summary.waiting.iter().cloned());
    out
}

fn section(out: &mut String, title: &str, rows: impl Iterator<Item = String>) {
    let mut rows = rows.peekable();
    if rows.peek().is_none() {
        return;
    }
    let _ = writeln!(out, "\n{title}:");
    for row in rows {
        let _ = writeln!(out, "  {row}");
    }
}

fn amount(unit: LimitUnit, amount: f64) -> String {
    match unit {
        LimitUnit::Usd => format!("${amount:.2}"),
        LimitUnit::Tokens => format!("{amount:.0} tokens"),
        LimitUnit::PlanUsage => format!("{:.0}% of the plan usage window", amount * 100.0),
        LimitUnit::Minutes => format!("{amount:.0} minutes"),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::RunId;
    use argonauts_core::base::names::{DecisionId, ModelRef};
    use argonauts_core::runs::api::TaskBrief;
    use argonauts_core::runs::events::task::TaskState;

    use super::*;

    #[test]
    fn render_lists_each_part_once() {
        let summary = StatusSummary {
            run: Some(RunId::new("run-01").unwrap()),
            goal: vec![
                "Rate limit the API".to_owned(),
                "G1: 429 past the limit".to_owned(),
            ],
            plan: Some("Redis counters behind auth".to_owned()),
            decisions: vec![(DecisionId::numbered(1), "Store: Redis".to_owned())],
            tasks: vec![TaskBrief {
                task: TaskId::new("task-002").unwrap(),
                title: "The Redis store".to_owned(),
                state: TaskState::Running,
                worker: Some(ModelRef::new("codex", None)),
                note: String::new(),
            }],
            recent: vec!["task-001 merged into the run branch".to_owned()],
            waiting: vec!["Merge this run into main?".to_owned()],
            ..StatusSummary::default()
        };
        let text = render(&summary);
        assert!(text.starts_with("Status of run-01\n"));
        assert!(text.contains("\nGoal: Rate limit the API\n  G1: 429 past the limit\n"));
        assert!(text.contains("\nPlan: Redis counters behind auth\n"));
        assert!(text.contains("  D-001 Store: Redis\n"));
        assert!(text.contains("  task-002 [running] The Redis store · codex\n"));
        assert!(text.contains("\nWaiting on you:\n  Merge this run into main?\n"));
        assert!(!text.contains("PlanUsages"));
        assert_eq!(render(&StatusSummary::default()), "No run is going.\n");
    }
}
