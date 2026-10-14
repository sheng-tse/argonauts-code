//! The merge rule (`merge_rule = "upgrade_review"`): a task with debt can't merge until the
//! original, or another model at least as strong, has reviewed it. With the original still out,
//! the merge question offers wait, review with a strong-enough candidate, or merge anyway, which
//! always needs you unless `approvals.rules.merge.allow_debt`. With it back, `merge_return`
//! decides what comes first.

use argonauts_core::TaskId;
use argonauts_core::events::run::Party;
use argonauts_core::events::task::TaskState;
use argonauts_core::fallback::QualityDebt;
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::fallback::MergeRule;

use crate::engine::host::Host;
use crate::fallback::{debt, rules};
use crate::state::RunState;

/// Merged tasks that still carry debt the rule enforces. A task the merge leaves out
/// brings no debt into it. Empty with `merge_rule = "none"`.
pub fn blocking(state: &RunState, enforce: bool) -> Vec<TaskId> {
    if !enforce {
        return Vec::new();
    }
    state
        .tasks
        .by_id
        .values()
        .filter(|record| record.state == TaskState::Merged && record.debt.is_some())
        .map(|record| record.spec.id.clone())
        .collect()
}

/// Whether the rule holds for this task's worker role.
pub fn enforced(host: &Host, task: &TaskId) -> bool {
    let Ok(record) = rules::record(host, task) else {
        return false;
    };
    let Some(worker) = rules::worker(&record) else {
        return false;
    };
    rules::applied_for(host, &record.spec, &worker).merge_rule == MergeRule::UpgradeReview
}

/// The debt that blocks merging now, task by task.
pub fn blocking_debt(host: &Host) -> Vec<(TaskId, QualityDebt)> {
    let tasks = host.read(|state| blocking(state, true));
    tasks
        .into_iter()
        .filter(|task| enforced(host, task))
        .filter_map(|task| {
            let debt = host.read(|state| state.tasks.get(&task)?.debt.clone())?;
            Some((task, debt))
        })
        .collect()
}

/// A model that could clear every listed debt instead of its original: usable now, not the
/// task's current worker, and at least as strong as each original.
pub fn stronger_candidate(host: &Host, blocked: &[(TaskId, QualityDebt)]) -> Option<ModelRef> {
    let settings = host.settings();
    let mut pool = crate::roles::candidates(settings, RoleName::Reviewer);
    if pool.is_empty() {
        pool = settings.crew.members();
    }
    pool.into_iter().find(|candidate| {
        rules::agent_usable(host, &candidate.agent_id())
            && blocked.iter().all(|(task, owed)| {
                let Ok(record) = rules::record(host, task) else {
                    return false;
                };
                let unknown = rules::applied_for(host, &record.spec, &owed.needs).unknown_tier;
                rules::worker(&record).as_ref() != Some(candidate)
                    && !debt::weaker(host, &record.spec, candidate, &owed.needs, unknown)
            })
    })
}

/// Merge anyway, with debt still open: yours to decide, unless your rules may
/// (`approvals.rules.merge.allow_debt`).
pub fn may_merge_with_debt(host: &Host, by: &Party) -> bool {
    matches!(by, Party::User) || host.settings().approvals.rules.merge.allow_debt
}
