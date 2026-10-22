//! The engine's one error type. Step kinds return it, and the runner turns it into a failed step,
//! a waiting run, or a crash the next run host recovers from.

use std::fmt;

use argonauts_core::events::agent_status::{JobKeeper, JobWait};
use argonauts_core::events::run::RunStop;
use argonauts_core::journal::JournalError;
use argonauts_core::session_runner::RunnerError;
use argonauts_core::steps::StepError;
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
    Runner(#[from] RunnerError),
    #[error("step: {0}")]
    Step(#[from] StepError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// A rule refused something, such as an invalid workflow or task graph. One line.
    #[error("{0}")]
    Refused(String),
    /// A step's session ended for an agent reason and no agent can take its role, so the run
    /// waits until one can, saying why and what fixes it.
    #[error("{0}")]
    Stalled(Box<Stall>),
}

pub type EngineResult<T> = Result<T, EngineError>;

impl EngineError {
    /// What ended a failed run: its first line in plain words where the error itself is
    /// plumbing, then the error as it came, which `r` shows.
    pub fn run_detail(&self) -> String {
        let plain = match self {
            Self::Step(StepError::SchemaMismatch { raw: None, .. }) => {
                "an agent ended without an answer"
            }
            Self::Step(StepError::SchemaMismatch { .. }) => {
                "an agent's answer didn't match the format Argonauts Code asked for"
            }
            _ => return self.to_string(),
        };
        format!("{plain}\n{self}")
    }
}

/// Why a job had nobody to fall back to, for the run's stop.
#[derive(Debug, Clone, PartialEq)]
pub struct Stall {
    /// `nobody to fall back to as the main agent`, for the wait line and the panel.
    pub detail: String,
    /// Who stopped, why, and that nobody else may take the job.
    pub line: String,
    /// The agents whose CLI says they're logged out, so logging in is the fix.
    pub logged_out: Vec<String>,
    /// When the stopped agent's plan resets, when a usage limit stopped it.
    pub resets_at: Option<OffsetDateTime>,
    /// The job's wait, when `fallback.same_cli` is all that keeps it from an agent of another
    /// CLI: the question about a CLI that is down then lists it.
    pub kept: Option<Box<JobWait>>,
    /// The job as the run's stop names it, its asked model, and what its own CLI said, in
    /// short words for its row (`signed out`) and whole for the stop (`agy is
    /// signed out`).
    pub job: StalledJob,
    /// Why nothing else could take it, one clause each: `OpenCode has no model set`.
    pub tried: Vec<String>,
    /// The CLIs at work in the job's step, which a job of the same step never shares.
    pub in_step: Vec<String>,
    /// The run's stop as a workflow's step words it, with the agents it left out before.
    pub stop: Option<Box<RunStop>>,
    /// Other models of the CLIs at work in the step that could take the job, as
    /// `fallback.same_step` lets them. `None` where it says never, or none could.
    pub beside: Option<Beside>,
}

/// Who could take a stalled job as a second agent of a CLI already at work in its step, best
/// first, and whether you're asked before one does.
#[derive(Debug, Clone, PartialEq)]
pub struct Beside {
    pub models: Vec<argonauts_core::names::ModelRef>,
    pub ask: bool,
}

impl Stall {
    /// The agent a review or a ruling waits for when only its builder's provider is left to
    /// take it: the one it was asked of. Its question decides, so the run isn't stopped.
    pub fn waits_for_its_agent(&self) -> Option<argonauts_core::names::AgentId> {
        let kept = self.kept.as_ref()?;
        matches!(kept.kept_by, JobKeeper::BuildersProvider { .. }).then(|| kept.stopped.agent_id())
    }

    /// The run's stop: the step's own words when it gave them, else this job's.
    pub fn run_stop(&self) -> RunStop {
        if let Some(stop) = &self.stop {
            return (**stop).clone();
        }
        let waits_for = self.job.asked.iter().map(|asked| {
            argonauts_core::display_names::DisplayNames::default().cli(asked.agent.as_str())
        });
        RunStop {
            why: format!("nothing else can take {}", self.job.name),
            agents: vec![format!("{} · {}", self.job.name, self.job.said)],
            tried: with_step(&self.tried, &self.in_step),
            kept: Vec::new(),
            waits_for: waits_for.collect(),
        }
    }
}

/// What was tried, and last that the CLIs at work in the step already work there.
pub fn with_step(tried: &[String], in_step: &[String]) -> Vec<String> {
    let mut tried = tried.to_vec();
    if !in_step.is_empty() {
        let works = if in_step.len() == 1 { "works" } else { "work" };
        let clis = argonauts_core::text::and_list(in_step);
        tried.push(format!("{clis} already {works} in this step"));
    }
    tried
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct StalledJob {
    /// `the task's worker`, or the call's own label where a workflow gave one.
    pub name: String,
    pub asked: Option<argonauts_core::names::ModelRef>,
    pub short: String,
    pub said: String,
    /// Its own CLI said it's signed out, so signing in is what would bring it back.
    pub signed_out: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_failed_by_an_answer_that_doesnt_fit_says_so_plainly_first() {
        let missed = EngineError::Step(StepError::SchemaMismatch {
            raw: Some("{}".to_owned()),
            errors: vec!["\"based_on\" is a required property".to_owned()],
        });
        let detail = missed.run_detail();
        let mut lines = detail.lines();
        assert_eq!(
            lines.next(),
            Some("an agent's answer didn't match the format Argonauts Code asked for")
        );
        assert_eq!(
            lines.next(),
            Some("step: output didn't match the schema: \"based_on\" is a required property")
        );

        let none = EngineError::Step(StepError::SchemaMismatch {
            raw: None,
            errors: Vec::new(),
        });
        assert!(
            none.run_detail()
                .starts_with("an agent ended without an answer\n")
        );
        // Errors that already say what happened stay as they are.
        let refused = EngineError::Refused("the workflow names no steps".to_owned());
        assert_eq!(refused.run_detail(), "the workflow names no steps");
    }
}
