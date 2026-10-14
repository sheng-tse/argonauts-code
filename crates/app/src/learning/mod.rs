//! The app's side of learning: which learned workflow a run uses and recording it, the Learning
//! pickers and their actions, the shell's learning commands, the background replay worker with its
//! `ReplayRunner`, and the promotion approval through your approval settings.
//!
//! Everything here reads the learning crate's files and asks its crate to change them; the
//! fixed core (approvals, limits, the evaluation, the mutation rules) is read, never written.

pub(crate) mod act;
pub(crate) mod learned_workflow;
pub(crate) mod promotion;
pub(crate) mod replay;
pub mod shell;
pub(crate) mod view;

use argonauts_learning::{LearningError, Store};

use crate::env::AppEnv;

/// The repo's learning files: its state folder and its main checkout.
pub(crate) fn store(env: &AppEnv) -> Store {
    Store::new(env.state_dir.clone(), env.repo_root.clone())
}

/// A learning result where a part that isn't built yet counts as nothing to show.
pub(crate) fn or_empty<T: Default>(result: argonauts_learning::Result<T>) -> anyhow::Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(LearningError::NotYet(what)) => {
            tracing::debug!("{what} isn't built yet");
            Ok(T::default())
        }
        Err(err) => Err(err.into()),
    }
}
