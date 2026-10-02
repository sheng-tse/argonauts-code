//! The engine's one error type. Step kinds return it, and the runner turns it into a failed step,
//! a parked run, or a crash the next supervisor recovers from.

use argonauts_core::journal::JournalError;
use argonauts_core::steps::StepError;
use argonauts_core::worker::WorkerError;

use crate::git::GitError;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// A scaffold stub that hasn't been written yet. The string names the function.
    #[error("{0} is not built yet")]
    NotBuilt(&'static str),
    /// Pause, stop, or shutdown cut the step short. The step is not finished and runs again,
    /// idempotently, when the run resumes.
    #[error("interrupted")]
    Interrupted,
    #[error("journal: {0}")]
    Journal(#[from] JournalError),
    #[error("git: {0}")]
    Git(#[from] GitError),
    #[error("worker: {0}")]
    Worker(#[from] WorkerError),
    #[error("step: {0}")]
    Step(#[from] StepError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// A rule refused something, such as an invalid workflow or task graph. One line.
    #[error("{0}")]
    Refused(String),
}

pub type EngineResult<T> = Result<T, EngineError>;

/// The body of every scaffold stub.
pub fn not_built<T>(what: &'static str) -> EngineResult<T> {
    Err(EngineError::NotBuilt(what))
}
