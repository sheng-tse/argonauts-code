//! The status summary, built by code from the event log at no token cost and filtered by who
//! is looking.

use argonauts_core::CardId;
use argonauts_core::api::StatusSummary;
use argonauts_core::events::RunEvent;
use argonauts_core::names::{PlanLabel, RoleName};

pub const NOT_IMPLEMENTED: &str = "the status summary is not implemented yet";

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
    let _ = viewer;
    StatusSummary {
        run: events.first().map(|event| event.run_id.clone()),
        recent: vec![NOT_IMPLEMENTED.to_owned()],
        ..StatusSummary::default()
    }
}

/// The text injected at session start and served by the MCP status tool.
pub fn render(summary: &StatusSummary) -> String {
    match &summary.run {
        Some(run) => format!("Run {run}: {NOT_IMPLEMENTED}."),
        None => format!("No run: {NOT_IMPLEMENTED}."),
    }
}
