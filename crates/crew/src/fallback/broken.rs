//! `fallback.broken_work`: whether a stopped task's new worker continues from the work it left
//! or restarts from the base. With `restart`, the checks run on that work first, and work that
//! fails them is kept on a side branch while the task starts again from its base.

use argonauts_core::TaskId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::session::EndReason;
use argonauts_core::events::task::TaskEvent;
use argonauts_core::settings::fallback::BrokenWork;

use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::fallback::{rewind, rules};
use crate::state::tasks::TaskEntry;
use crate::task::checks;
use crate::task::support::task_events;

/// Runs the checks on the work a stopped session left, when `restart` needs their verdict. A
/// rotation or your own stop leaves the work in the same hands, so it needs none.
pub async fn check(host: &Host, task: &TaskId, reason: &EndReason) -> EngineResult<()> {
    let same_hands = matches!(
        reason,
        EndReason::Completed | EndReason::Paused | EndReason::Stopped | EndReason::ContextFull
    );
    let record = rules::record(host, task)?;
    if same_hands || !restarts(host, &record) || record.branch.is_none() {
        return Ok(());
    }
    if verdict(host, task)?.is_none() {
        checks::run(host, task).await?;
    }
    Ok(())
}

/// Whether the checks passed on the task's newest work, once they ran on it.
pub fn verdict(host: &Host, task: &TaskId) -> EngineResult<Option<bool>> {
    let events = task_events(host, task)?;
    let since = events
        .iter()
        .rposition(|event| {
            matches!(&event.kind, RunEventKind::Task(TaskEvent::Committed(info)) if info.sha.is_some())
        })
        .map_or(0, |at| at + 1);
    let mut ran = false;
    let mut passed = true;
    for event in &events[since..] {
        if let RunEventKind::Check(check) = &event.kind {
            ran = true;
            passed &= check.passed;
        }
    }
    Ok(ran.then_some(passed))
}

/// With `restart`, work that failed its checks moves to a side branch and the task's branch
/// goes back to its base before the new worker starts. Returns that side branch.
pub async fn set_aside(host: &Host, record: &TaskEntry) -> EngineResult<Option<String>> {
    let task = &record.spec.id;
    if !restarts(host, record) || verdict(host, task)? != Some(false) {
        return Ok(None);
    }
    let kept_on = rewind::reset_to(host, record, 0).await?;
    host.record(
        Some(task),
        None,
        RunEventKind::Task(TaskEvent::Rewound {
            to: 0,
            kept_on: kept_on.clone(),
        }),
    )?;
    Ok(Some(kept_on))
}

fn restarts(host: &Host, record: &TaskEntry) -> bool {
    rules::worker(record).is_some_and(|worker| {
        rules::applied_for(host, &record.spec, &worker).broken_work == BrokenWork::Restart
    })
}
