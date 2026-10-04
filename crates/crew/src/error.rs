//! The engine's one error type. Step kinds return it, and the runner turns it into a failed step,
//! a parked run, or a crash the next supervisor recovers from.

use std::fmt;

use argonauts_core::journal::JournalError;
use argonauts_core::steps::StepError;
use argonauts_core::worker::WorkerError;
use time::OffsetDateTime;

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
    /// A step's session ended for a vendor reason and no agent can take its role, so the run
    /// parks until one can, saying why and what fixes it.
    #[error("{0}")]
    Stalled(Box<Stall>),
}

pub type EngineResult<T> = Result<T, EngineError>;

/// Why nobody could take a seat over, for the run's park.
#[derive(Debug, Clone, PartialEq)]
pub struct Stall {
    /// `nobody could take over as the lead`, for the park line and the panel.
    pub detail: String,
    /// Who stopped, why, and that nobody else may take the seat.
    pub line: String,
    /// The agents whose CLI says they're logged out, so logging in is the fix.
    pub logged_out: Vec<String>,
    /// When the stopped agent's plan resets, when a usage limit stopped it.
    pub resets_at: Option<OffsetDateTime>,
}

impl fmt::Display for Stall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.line)?;
        if !self.logged_out.is_empty() {
            write!(f, " {}", login_fix(&self.logged_out))?;
        }
        Ok(())
    }
}

/// `Codex says it's logged out: log in again, then /resume.`
pub fn login_fix(names: &[String]) -> String {
    let says = if names.len() == 1 { "says" } else { "say" };
    format!(
        "{} {says} it's logged out: log in again, then /resume.",
        names.join(" and ")
    )
}

/// The body of every scaffold stub.
pub fn not_built<T>(what: &'static str) -> EngineResult<T> {
    Err(EngineError::NotBuilt(what))
}
