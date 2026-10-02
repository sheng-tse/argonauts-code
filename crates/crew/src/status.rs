//! The status summary, built by code from the event log at no token cost and filtered by who
//! is looking.

use std::fmt::Write as _;

use argonauts_core::CardId;
use argonauts_core::api::StatusSummary;
use argonauts_core::events::RunEvent;
use argonauts_core::names::{PlanLabel, RoleName};
use argonauts_core::settings::budget::BudgetUnit;

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
        card: Option<CardId>,
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
        "Cards",
        summary.cards.iter().map(|card| {
            let mut row = format!(
                "{} [{}] {}",
                card.card,
                folder::words(&card.state),
                card.title
            );
            if let Some(owner) = &card.owner {
                let _ = write!(row, " · {owner}");
            }
            if !card.note.is_empty() {
                let _ = write!(row, " · {}", card.note);
            }
            row
        }),
    );
    section(&mut out, "Recent", summary.recent.iter().cloned());
    if let Some(budget) = &summary.budget {
        let spent = format!("${:.2}", budget.spent.cost_usd);
        let line = match &budget.limit {
            Some(limit) => format!("{spent} of {}", amount(limit.unit, limit.amount)),
            None => spent,
        };
        let _ = writeln!(out, "\nBudget: {line}");
    }
    section(
        &mut out,
        "Quotas",
        summary.quotas.iter().map(|quota| {
            let mut row = format!("{} {}", quota.vendor, folder::words(&quota.status));
            if let Some(used) = quota.used {
                let _ = write!(row, ", {:.0}% used", used * 100.0);
            }
            if let Some(window) = &quota.window {
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

fn amount(unit: BudgetUnit, amount: f64) -> String {
    match unit {
        BudgetUnit::Usd => format!("${amount:.2}"),
        BudgetUnit::Tokens => format!("{amount:.0} tokens"),
        BudgetUnit::Quota => format!("{:.0}% of the quota window", amount * 100.0),
        BudgetUnit::Minutes => format!("{amount:.0} minutes"),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::RunId;
    use argonauts_core::api::CardBrief;
    use argonauts_core::events::card::CardState;
    use argonauts_core::names::{DecisionId, WorkerRef};

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
            cards: vec![CardBrief {
                card: CardId::new("card-002").unwrap(),
                title: "The Redis store".to_owned(),
                state: CardState::Running,
                owner: Some(WorkerRef::new("codex", None)),
                note: String::new(),
            }],
            recent: vec!["card-001 merged into the integration branch".to_owned()],
            waiting: vec!["Merge this run into main?".to_owned()],
            ..StatusSummary::default()
        };
        let text = render(&summary);
        assert!(text.starts_with("Status of run-01\n"));
        assert!(text.contains("\nGoal: Rate limit the API\n  G1: 429 past the limit\n"));
        assert!(text.contains("\nPlan: Redis counters behind auth\n"));
        assert!(text.contains("  D-001 Store: Redis\n"));
        assert!(text.contains("  card-002 [running] The Redis store · codex\n"));
        assert!(text.contains("\nWaiting on you:\n  Merge this run into main?\n"));
        assert!(!text.contains("Quotas"));
        assert_eq!(render(&StatusSummary::default()), "No run is going.\n");
    }
}
