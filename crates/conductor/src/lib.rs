//! Supervises agent sessions, worktrees, gates, and merges. The conductor is code, not a model.

pub mod control;
pub mod decision;
pub mod doctor;
pub mod gates;
pub mod git;
pub mod harness_worker;
pub mod process;
pub mod recorder;
pub mod services;
pub mod solo;
pub mod supervisor;

#[cfg(test)]
mod testutil;

pub use control::{ConductorSettings, StopHandle};
pub use decision::{
    ChannelDecider, FixedDecider, MergeDecider, MergeRequest, PendingDecision, decision_channel,
};
pub use doctor::{AgentCheck, AgentStatus, DoctorReport, GitCheck, check_agent, doctor};
pub use gates::run_gate;
pub use git::{Git, GitError};
pub use harness_worker::HarnessWorker;
pub use process::{AgentProcess, OutputLine, ProcessEvent, SpawnError, Stream, signal_group};
pub use recorder::{CONDUCTOR_SOURCE, EventSink, Recorder};
pub use services::{boards, gate_runner, handoff_planner, limits_service};
pub use solo::{SoloOutcome, SoloRun, budget_problem};
pub use supervisor::{FinalResult, SessionOutcome, Supervisor, WrapUpDelivery};
