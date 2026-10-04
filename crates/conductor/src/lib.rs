//! Supervises agent sessions, worktrees, gates, and merges. The conductor is code, not a model.

pub mod agents;
pub mod cli_versions;
pub mod control;
pub mod cpu;
pub mod decision;
pub mod doctor;
pub mod filediff;
pub mod gates;
pub mod git;
pub mod handoff;
pub mod harness_worker;
pub mod health;
pub mod liveness;
pub mod meter;
pub mod process;
pub mod quota;
pub mod rawlog;
pub mod reaper;
pub mod recorder;
pub mod recovery;
pub mod sandbox;
pub mod services;
pub mod slots;
pub mod solo;
pub mod supervisor;
pub mod vendors_file;
pub mod watchdogs;

#[cfg(test)]
mod testutil;

pub use control::{ConductorSettings, StopHandle};
pub use decision::{
    ChannelDecider, FixedDecider, MergeDecider, MergeRequest, PendingDecision, decision_channel,
};
pub use doctor::{
    AgentCheck, AgentStatus, DoctorDetails, DoctorReport, GitCheck, check_agent, doctor,
    doctor_details,
};
pub use gates::run_gate;
pub use git::{Git, GitError};
pub use harness_worker::HarnessWorker;
pub use process::{AgentProcess, OutputLine, ProcessEvent, SpawnError, Stream, signal_group};
pub use recorder::{CONDUCTOR_SOURCE, EventSink, Recorder};
pub use services::{boards, boards_with, gate_runner, handoff_planner, limits_service};
pub use solo::{SoloOutcome, SoloRun, budget_problem};
pub use supervisor::{FinalResult, SessionOutcome, Supervisor, WrapUpDelivery};
