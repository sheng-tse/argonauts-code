//! `/status` answers, built by code from the run's log, so asking costs nothing.

use argonauts_core::api::StatusSummary;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::card::CardState;
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};

use crate::cell::{Cell, Tone, ToolItem, ToolState};
use crate::format;

use super::steps::step;

/// Recent lines worth showing; the log keeps the rest.
const RECENT: usize = 5;

/// `● Status(run-06)` with the goal, plan, decisions, cards, recent events, spend, and what
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

/// The goal, plan, decisions, cards, recent events, spend, and what waits for you, each a group
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
    if !summary.cards.is_empty() {
        section(
            summary
                .cards
                .iter()
                .map(|card| {
                    let mut line = format!("{} {} · {}", card.card, card.title, state(card.state));
                    if let Some(owner) = &card.owner {
                        line.push_str(&format!(" · {}", names.worker(owner)));
                    }
                    if !card.note.trim().is_empty() {
                        line.push_str(&format!(" · {}", card.note.trim()));
                    }
                    line
                })
                .collect(),
        );
    }
    let skip = summary.recent.len().saturating_sub(RECENT);
    section(summary.recent.iter().skip(skip).cloned().collect());
    if let Some(budget) = &summary.budget {
        let spent = format::usd(budget.spent.cost_usd);
        let line = match &budget.limit {
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

fn amount(limit: &BudgetAmount) -> String {
    match limit.unit {
        BudgetUnit::Usd => format::usd(limit.amount),
        BudgetUnit::Tokens => format!("{} tokens", format::tokens(limit.amount.max(0.0) as u64)),
        BudgetUnit::Quota => format!("{:.0}% of quota", limit.amount * 100.0),
        BudgetUnit::Minutes => format!("{:.0} minutes", limit.amount),
    }
}

fn state(state: CardState) -> &'static str {
    match state {
        CardState::Waiting => "waiting",
        CardState::Ready => "ready",
        CardState::Running => "running",
        CardState::Blocked => "blocked on a question",
        CardState::NeedsDecision => "needs a decision",
        CardState::Gating => "running gates",
        CardState::InReview => "in review",
        CardState::Integrating => "integrating",
        CardState::Integrated => "integrated",
        CardState::Failed => "failed",
        CardState::Escalated => "escalated",
        CardState::Deferred => "waiting for budget",
        CardState::Cancelled => "cancelled",
        CardState::TakenOver => "taken over",
    }
}
