//! Branch names and the integration worktree. Every branch Argonauts makes starts with
//! `argonauts/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::IntegrationEvent;
use argonauts_core::{CardId, RunId};

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};

pub const PREFIX: &str = "argonauts/";

pub fn integration_branch(run: &RunId) -> String {
    format!("{PREFIX}{run}/integration")
}

pub fn card_branch(run: &RunId, card: &CardId) -> String {
    format!("{PREFIX}{run}/{card}")
}

/// A separate branch for one attempt at a card, such as a race candidate's.
pub fn attempt_branch(run: &RunId, card: &CardId, attempt: u32) -> String {
    format!("{PREFIX}{run}/{card}-attempt-{attempt}")
}

/// Where the integration branch is checked out: beside the card worktrees.
pub fn integration_worktree(host: &Host) -> PathBuf {
    host.repo().worktrees.join("integration")
}

/// The commit the run started from.
pub fn run_base(host: &Host) -> EngineResult<String> {
    host.read(|state| {
        state
            .integration
            .base
            .clone()
            .or_else(|| state.run.started.as_ref().map(|s| s.base_commit.clone()))
    })
    .ok_or_else(|| EngineError::Refused("the run has no base commit".to_owned()))
}

/// Creates `argonauts/<run>/integration` from the run's base with its worktree, or finds it.
/// The goal file, decisions, notes, and dependency changes are committed here.
pub async fn ensure_integration(host: &Host) -> EngineResult<PathBuf> {
    let path = integration_worktree(host);
    // Cards start at once; only one of them creates the branch and its worktree.
    let lock = path_lock("setup", &path);
    let _held = lock.lock().await;
    let git = host.git();
    let root = host.repo().root.clone();
    let branch = integration_branch(host.run_id());
    let base = run_base(host)?;
    if git.branch_head(&root, &branch).await?.is_none() {
        git.create_branch(&root, &branch, &base).await?;
    }
    let recorded = host.read(|state| state.integration.branch.is_some());
    if !recorded {
        let created = IntegrationEvent::BranchCreated {
            branch: branch.clone(),
            base,
        };
        host.record(None, None, RunEventKind::Integration(created))?;
    }
    if !path.join(".git").exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        git.add_worktree_on(&root, &path, &branch).await?;
    }
    Ok(path)
}

/// One lock per worktree and purpose, so two cards never set up, or merge into, one worktree
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
        let card = CardId::new("card-003").unwrap();
        assert_eq!(integration_branch(&run), "argonauts/run-01/integration");
        assert_eq!(card_branch(&run, &card), "argonauts/run-01/card-003");
        assert_eq!(
            attempt_branch(&run, &card, 2),
            "argonauts/run-01/card-003-attempt-2"
        );
        for name in [
            integration_branch(&run),
            card_branch(&run, &card),
            attempt_branch(&run, &card, 1),
        ] {
            assert!(name.starts_with(PREFIX), "{name}");
        }
    }

    #[test]
    fn one_lock_per_worktree_and_purpose() {
        let path = Path::new("/w/run-a/integration");
        let merge = path_lock("merge", path);
        assert!(Arc::ptr_eq(&merge, &path_lock("merge", path)));
        assert!(!Arc::ptr_eq(&merge, &path_lock("setup", path)));
        let other = Path::new("/w/run-b/integration");
        assert!(!Arc::ptr_eq(&merge, &path_lock("merge", other)));
    }
}
