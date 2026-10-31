//! The run the screen follows, as the cards above the input and the usage panel draw it.

use argonauts_core::runs::api::{StatusSummary, TaskBrief};
use argonauts_core::runs::events::task::TaskState;

use crate::frame::state::RunState;

/// The run the screen follows, as its cards above the input draw it.
#[derive(Debug, Clone, PartialEq)]
pub struct PinnedRun {
    pub summary: StatusSummary,
    /// The run's own name, such as `export-reports-csv`.
    pub title: Option<String>,
}

impl PinnedRun {
    /// The run as its cards draw it, from what its events said so far. Its goal and plan
    /// aren't shown above the input, so they're left out.
    pub fn of(run: &RunState) -> Self {
        let tasks = run.tasks.values().map(|task| TaskBrief {
            task: task.id.clone(),
            title: task.title.clone(),
            state: task.state.unwrap_or(TaskState::Waiting),
            worker: task.worker.clone(),
            note: String::new(),
        });
        let waiting = run
            .approvals
            .iter()
            .filter(|approval| approval.opened.needs_you)
            .map(|approval| approval.opened.title.clone());
        Self {
            summary: StatusSummary {
                run: Some(run.id.clone()),
                tasks: tasks.collect(),
                spend: run.spend.clone(),
                waiting: waiting.collect(),
                ..StatusSummary::default()
            },
            title: run.named.as_ref().map(|named| named.name.clone()),
        }
    }
}
