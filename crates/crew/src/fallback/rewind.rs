//! `/rewind` to `fallback-<n>`: resets the task's branch to that tag after a confirm question,
//! keeping the work after it on `argonauts/<run>/<task>-replaced-<n>`. A task at rest goes back
//! in line. A task already in the run's branch has that work undone there too, by one commit,
//! and stays merged as it was at the tag; the merge question opens again on the new branch.
//!
//! Refused once the run's work has merged, gone out as a pull request, or been abandoned, since
//! a new change undoes it then, and for a task that isn't at rest, whose checks, review, merge,
//! or your own edits would lose the branch under them.

use argonauts_core::TaskId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::events::task::{TaskEvent, TaskState};
use argonauts_core::fallback::CheckpointTag;
use argonauts_core::names::ApprovalId;

use crate::approvals;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::fallback::rules;
use crate::merge::{branch, conductor, step};
use crate::state::RunState;
use crate::state::tasks::TaskEntry;

/// Called once you confirmed. Records `Task::Rewound`.
pub async fn rewind(host: &Host, task: &TaskId, checkpoint: u32) -> Result<(), String> {
    rewind_task(host, task, checkpoint)
        .await
        .map_err(|err| err.to_string())
}

async fn rewind_task(host: &Host, task: &TaskId, checkpoint: u32) -> EngineResult<()> {
    let record = rules::record(host, task)?;
    if let Some(why) = host.read(|state| refuse(state, &record)) {
        return Err(EngineError::Refused(why));
    }
    if checkpoint == 0 || checkpoint > record.fallbacks {
        return Err(EngineError::Refused(format!(
            "{task} has no checkpoint fallback-{checkpoint}"
        )));
    }
    let kept_on = reset_to(host, &record, checkpoint).await?;
    host.record(
        Some(task),
        None,
        RunEventKind::Task(TaskEvent::Rewound {
            to: checkpoint,
            kept_on,
        }),
    )?;
    let reason = format!("you rewound {task} to fallback-{checkpoint}");
    withdraw_stale(host, task, &reason)?;
    rules::back_in_line(host, task, &reason)
}

/// Why the task can't be rewound now, if it can't.
fn refuse(state: &RunState, record: &TaskEntry) -> Option<String> {
    let task = &record.spec.id;
    if state.merge.is_merged(task) && step::ended(&state.merge).is_some() {
        return Some(format!(
            "{task}'s work is merged, so it can't be rewound. Undo it with a new change."
        ));
    }
    if record.state == TaskState::Merged {
        if rules::session_running(state, task) {
            return Some(format!(
                "{task} is being reviewed. Rewind it once the review is done."
            ));
        }
        return branch_busy(state, task);
    }
    rules::refuse_reset(state, task)
}

/// Why the run's branch can't take an undo of `task`'s work now, if it can't: something is
/// being merged into it, or a started task builds on that work.
fn branch_busy(state: &RunState, task: &TaskId) -> Option<String> {
    if let Some(merging) = state
        .tasks
        .by_id
        .values()
        .find(|record| record.state == TaskState::Merging)
    {
        return Some(format!(
            "{} is being merged into the run's branch. Rewind {task} once it's in.",
            merging.spec.id
        ));
    }
    if state.merge.main_moving.is_some() {
        return Some(format!(
            "Main is being merged into the run's branch. Rewind {task} once it's in."
        ));
    }
    let builds_on = state.tasks.by_id.values().find(|record| {
        record.spec.depends_on.contains(task)
            && !matches!(
                record.state,
                TaskState::Waiting | TaskState::Ready | TaskState::Cancelled
            )
    })?;
    Some(format!(
        "{} builds on {task}'s work, so {task} can't be rewound on its own.",
        builds_on.spec.id
    ))
}

/// The branch moved under the merge question, and the task's debt may have gone with the work it
/// was for, so what was open over them opens again with what's true now.
fn withdraw_stale(host: &Host, task: &TaskId, reason: &str) -> EngineResult<()> {
    let stale: Vec<ApprovalId> = host.read(|state| {
        let merged = state
            .tasks
            .get(task)
            .is_some_and(|record| record.state == TaskState::Merged);
        let debt_gone = state
            .tasks
            .get(task)
            .is_some_and(|record| record.debt.is_none());
        state
            .approvals
            .open()
            .filter(|record| match record.opened.kind {
                ApprovalKind::Merge => merged,
                ApprovalKind::OriginalReturned => {
                    debt_gone && record.opened.task.as_ref() == Some(task)
                }
                _ => false,
            })
            .map(|record| record.opened.id.clone())
            .collect()
    });
    for id in stale {
        approvals::withdraw(host, &id, reason)?;
    }
    Ok(())
}

/// Resets the task's branch to `fallback-<n>` (its base for 0), keeping the work after it on
/// the replaced branch. When the task's work is in the run branch, the work after the
/// tag is undone there too, or nothing changes. Returns the replaced branch.
pub async fn reset_to(host: &Host, record: &TaskEntry, n: u32) -> EngineResult<String> {
    let task = &record.spec.id;
    let git = host.git();
    let root = host.repo().root.clone();
    let tag = CheckpointTag {
        run: host.run_id().clone(),
        task: task.clone(),
        n,
    };
    let target = if n == 0 {
        record.base.clone()
    } else {
        git.tag_target(&root, &tag.tag_name()).await?
    };
    let target =
        target.ok_or_else(|| EngineError::Refused(format!("{task} has no {}", tag.label())))?;
    let branch = record
        .branch
        .clone()
        .ok_or_else(|| EngineError::Refused(format!("{task} has no branch yet")))?;
    let head = git.resolve(&root, &branch).await?;
    let kept_on = tag.replaced_branch();
    if head != target {
        keep(host, &kept_on, &head).await?;
    }
    move_branch(host, record, &branch, &target, &head).await?;
    if host.read(|state| state.merge.is_merged(task)) {
        let undone = undo_merged(host, task, &tag.label(), &target, &head).await;
        if let Err(err) = undone {
            move_branch(host, record, &branch, &head, &target).await?;
            return Err(err);
        }
    }
    Ok(kept_on)
}

/// Points the task's branch at `to`, through its worktree when it has one.
async fn move_branch(
    host: &Host,
    record: &TaskEntry,
    branch: &str,
    to: &str,
    from: &str,
) -> EngineResult<()> {
    let git = host.git();
    match &record.worktree {
        Some(path) => git.reset_hard(path, to).await?,
        None => {
            git.update_branch(&host.repo().root, branch, to, from)
                .await?
        }
    }
    Ok(())
}

/// Undoes, on the run branch, the task's commits after `target` that were merged
/// there, with one commit of Argonauts Code's. Refused with nothing changed when that work came in
/// through a merge, whose other side the undo can't tell apart, or when later work on the
/// branch conflicts with undoing it.
async fn undo_merged(
    host: &Host,
    task: &TaskId,
    label: &str,
    target: &str,
    head: &str,
) -> EngineResult<()> {
    let git = host.git();
    let worktree = branch::ensure_run_branch(host).await?;
    let _held = conductor::hold(&worktree).await;
    let merged = git.resolve(&worktree, "HEAD").await?;
    // The newest of the task's commits the branch holds.
    let Some(merged) = git.merge_base(&worktree, head, &merged).await? else {
        return Ok(());
    };
    if git.is_ancestor(&worktree, &merged, target).await? {
        return Ok(());
    }
    let refused = |why: String| {
        Err(EngineError::Refused(format!(
            "{why}, so nothing was rewound. Undo it with a new change."
        )))
    };
    if !git.tracked_changes(&worktree).await?.is_empty() {
        return refused("The run's branch has uncommitted changes".to_owned());
    }
    let commits = git
        .log_messages(&worktree, &format!("{target}..{merged}"))
        .await?;
    let mut undo = Vec::new();
    for (sha, _) in commits {
        if git.parents(&worktree, &sha).await?.len() > 1 {
            return refused(format!(
                "{task}'s work after {label} came in through a merge"
            ));
        }
        undo.push(sha);
    }
    let conflicts = git.revert_staged(&worktree, &undo).await?;
    if !conflicts.is_empty() {
        return refused(format!(
            "Undoing {task}'s work after {label} conflicts with later work on the run's branch \
             in {}",
            conflicts.join(", ")
        ));
    }
    let message = conductor::message(host.run_id(), &format!("Rewind {task} to {label}"));
    git.commit_index(&worktree, &message).await?;
    Ok(())
}

/// Creates `branch` at `head`. One that already holds exactly this work is fine; one that
/// holds other work is never overwritten.
async fn keep(host: &Host, branch: &str, head: &str) -> EngineResult<()> {
    let git = host.git();
    let root = host.repo().root.clone();
    // An empty old value makes git create the branch only if it doesn't exist.
    if git.update_branch(&root, branch, head, "").await.is_ok() {
        return Ok(());
    }
    if git.resolve(&root, branch).await? == head {
        return Ok(());
    }
    Err(EngineError::Refused(format!(
        "{branch} already holds other replaced work, so nothing was reset"
    )))
}
