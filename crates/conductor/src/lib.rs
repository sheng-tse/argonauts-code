//! Runs agent sessions, worktrees, checks, and merges. Argonauts Code is code, not a model.

pub mod boards;
pub mod checks;
pub mod cli_runner;
pub mod doctor;
pub mod fallback;
pub mod gh;
pub mod git;
pub mod limits;
pub mod process;
pub mod reaper;
pub mod sandbox;
pub mod session;

#[cfg(test)]
mod testing;

pub use checks::run_check;
pub use cli_runner::CliRunner;
pub use doctor::{
    AgentCheck, AgentStatus, DoctorDetails, DoctorReport, GitCheck, check_agent, doctor,
    doctor_details,
};
pub use git::decision::{
    ChannelDecider, FixedDecider, MergeDecider, MergeRequest, PendingDecision, decision_channel,
};
pub use git::{Git, GitError};
pub use limits::control::{ConductorSettings, StopHandle};
pub use process::{AgentProcess, OutputLine, ProcessEvent, SpawnError, Stream, signal_group};
pub use session::recorder::{CONDUCTOR_SOURCE, EventSink, Recorder};
pub use session::run_host::{FinalResult, RunHost, SessionOutcome, WrapUpDelivery};
pub use session::services::{boards, boards_with, check_runner, fallback_planner, limits_service};
