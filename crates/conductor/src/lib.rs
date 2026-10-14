//! Runs agent sessions, worktrees, checks, and merges. Argonauts Code is code, not a model.

pub mod agents;
pub mod agents_file;
pub mod checks;
pub mod cli_runner;
pub mod cli_versions;
pub mod control;
pub mod cpu;
pub mod decision;
pub mod doctor;
pub mod fallback;
pub mod filediff;
pub mod git;
pub mod health;
pub mod liveness;
pub mod meter;
pub mod model_health;
pub mod plan_usage;
pub mod process;
pub mod rawlog;
pub mod reaper;
pub mod recorder;
pub mod recovery;
pub mod run_host;
pub mod sandbox;
pub mod services;
pub mod slots;
pub mod watchdogs;

#[cfg(test)]
mod testutil;

pub use checks::run_check;
pub use cli_runner::CliRunner;
pub use control::{ConductorSettings, StopHandle};
pub use decision::{
    ChannelDecider, FixedDecider, MergeDecider, MergeRequest, PendingDecision, decision_channel,
};
pub use doctor::{
    AgentCheck, AgentStatus, DoctorDetails, DoctorReport, GitCheck, check_agent, doctor,
    doctor_details,
};
pub use git::{Git, GitError};
pub use process::{AgentProcess, OutputLine, ProcessEvent, SpawnError, Stream, signal_group};
pub use recorder::{CONDUCTOR_SOURCE, EventSink, Recorder};
pub use run_host::{FinalResult, RunHost, SessionOutcome, WrapUpDelivery};
pub use services::{boards, boards_with, check_runner, fallback_planner, limits_service};
