//! Branch names and the run's worktree. Every branch Argonauts Code makes starts with
//! `argonauts/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use argonauts_core::events::RunEventKind;
use argonauts_core::events::task::MergeEvent;
use argonauts_core::{RunId, TaskId};

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};

pub const PREFIX: &str = "argonauts/";

pub fn run_branch(run: &RunId) -> String {
    format!("{PREFIX}{run}/merged")
}

pub fn task_branch(run: &RunId, task: &TaskId) -> String {
    format!("{PREFIX}{run}/{task}")
}

/// A separate branch for one try at a task, such as a race candidate's.
pub fn try_branch(run: &RunId, task: &TaskId, try_number: u32) -> String {
    format!("{PREFIX}{run}/{task}-try-{try_number}")
}

/// Where the run branch is checked out: beside the task worktrees.
pub fn run_worktree(host: &Host) -> PathBuf {
    host.repo().worktrees.join("merge")
}

/// The commit the run started from.
pub fn run_base(host: &Host) -> EngineResult<String> {
    host.read(|state| {
        state
            .merge
            .base
            .clone()
            .or_else(|| state.run.started.as_ref().map(|s| s.base_commit.clone()))
    })
    .ok_or_else(|| EngineError::Refused("the run has no base commit".to_owned()))
}

/// Creates `argonauts/<run>/merged` from the run's base with its worktree, or finds it.
/// The goal file, decisions, notes, and dependency changes are committed here.
pub async fn ensure_run_branch(host: &Host) -> EngineResult<PathBuf> {
    let path = run_worktree(host);
    // Tasks start at once; only one of them creates the branch and its worktree.
    let lock = path_lock("setup", &path);
    let _held = lock.lock().await;
    let git = host.git();
    let root = host.repo().root.clone();
    let branch = run_branch(host.run_id());
    let base = run_base(host)?;
    if git.branch_head(&root, &branch).await?.is_none() {
        git.create_branch(&root, &branch, &base).await?;
    }
    let recorded = host.read(|state| state.merge.branch.is_some());
    if !recorded {
        let created = MergeEvent::BranchCreated {
            branch: branch.clone(),
            base,
        };
        host.record(None, None, RunEventKind::Merge(created))?;
    }
    if !path.join(".git").exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        git.add_worktree_on(&root, &path, &branch).await?;
    }
    Ok(path)
}

/// One lock per worktree and purpose, so two tasks never set up, or merge into, one worktree
/// at once.
pub(crate) fn path_lock(purpose: &'static str, path: &Path) -> Arc<tokio::sync::Mutex<()>> {
    type Locks = Mutex<BTreeMap<(&'static str, PathBuf), Arc<tokio::sync::Mutex<()>>>>;
    static LOCKS: OnceLock<Locks> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    locks
        .entry((purpose, path.to_path_buf()))
        .or_default()
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names_use_argonauts_prefix() {
        let run = RunId::new("run-01").unwrap();
        let task = TaskId::new("task-003").unwrap();
        assert_eq!(run_branch(&run), "argonauts/run-01/merged");
        assert_eq!(task_branch(&run, &task), "argonauts/run-01/task-003");
        assert_eq!(
            try_branch(&run, &task, 2),
            "argonauts/run-01/task-003-try-2"
        );
        for name in [
            run_branch(&run),
            task_branch(&run, &task),
            try_branch(&run, &task, 1),
        ] {
            assert!(name.starts_with(PREFIX), "{name}");
        }
    }

    #[test]
    fn one_lock_per_worktree_and_purpose() {
        let path = Path::new("/w/run-a/merged");
        let merge = path_lock("merge", path);
        assert!(Arc::ptr_eq(&merge, &path_lock("merge", path)));
        assert!(!Arc::ptr_eq(&merge, &path_lock("setup", path)));
        let other = Path::new("/w/run-b/merged");
        assert!(!Arc::ptr_eq(&merge, &path_lock("merge", other)));
    }
}
