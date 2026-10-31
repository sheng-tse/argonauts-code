//! Where a question goes: the worker of the piece it concerns, the consultant when addressed, or
//! you as a `Question` approval.

use argonauts_core::base::names::RoleName;
use argonauts_core::runs::events::shared::Recipient;
use argonauts_core::runs::events::task::TaskState;

use crate::blackboard::refs;
use crate::state::RunState;
use crate::state::tasks::TaskEntry;

/// The recipient for a question about these paths or tasks, `Recipient::User` when no worker
/// fits. A task it names outright wins; otherwise the task owning most of the cited paths, ties
/// going to the lower task id.
pub fn question(state: &RunState, text: &str, refs: &[String]) -> Recipient {
    if addresses_consultant(text) {
        return Recipient::Role {
            role: RoleName::Consultant,
        };
    }
    let named = refs
        .iter()
        .filter_map(|reference| refs::task_in(reference))
        .find(|task| state.tasks.get(task).is_some_and(still_working));
    if let Some(task) = named {
        return Recipient::Task { task };
    }
    let mut best = None;
    for (task, record) in &state.tasks.by_id {
        if !still_working(record) {
            continue;
        }
        let owned = refs
            .iter()
            .filter(|path| {
                record
                    .spec
                    .owned_paths
                    .iter()
                    .any(|pattern| refs::under(pattern, path))
            })
            .count();
        if owned > 0 && best.as_ref().is_none_or(|(most, _)| owned > *most) {
            best = Some((owned, task.clone()));
        }
    }
    match best {
        Some((_, task)) => Recipient::Task { task },
        None => Recipient::User,
    }
}

/// `@consultant` anywhere, or a question that starts with `consultant:`.
pub fn addresses_consultant(text: &str) -> bool {
    let text = text.trim_start().to_ascii_lowercase();
    text.starts_with("consultant:") || text.contains("@consultant")
}

/// A task whose worker can still answer: it has a session to come.
pub fn still_working(record: &TaskEntry) -> bool {
    !matches!(record.state, TaskState::Merged | TaskState::Cancelled)
}
