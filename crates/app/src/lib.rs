//! The interactive app, the per-run run host process, and Argonauts Code API.
//! Every other crate meets here, so its dependencies are the whole wiring.

mod adapters;
mod ancestry;
mod app_settings;
mod behaviors;
mod choices;
mod client;
mod commands;
mod confine;
mod controller;
mod conversation_store;
mod env;
mod example_prompt;
mod git;
#[cfg(test)]
mod golden;
mod hidden_git;
mod host;
mod issue;
mod keep_awake;
mod learning;
mod live_registry;
mod model_memory;
mod mods;
mod overview;
mod prompts;
mod repo_files;
mod run_command;
mod run_files;
pub mod self_update;
mod session_id;
mod setup;
mod startup;
mod telemetry;
mod trace;
mod trust;
mod usage_ledger;
mod views;
mod wake;
mod wire;
mod work_ledger;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use argonauts_core::agent_sessions::AgentSessionRef;
use argonauts_core::api::ConductorApi;
use argonauts_core::cli_sessions::CliSessionRef;
use argonauts_core::names::ConversationId;
use argonauts_core::{Paths, RunId};

pub use adapters::builtin_adapters;
pub use ancestry::refuse_inside_agent;
pub use behaviors::deep::command::{DeepTerminal, behaviors_deep};
pub use behaviors::deep::service::DeepMaps;
pub use behaviors::{BehaviorPeriod, behaviors_report, export_hodoscope};
pub use commands::{
    ApprovalsReport, DoctorExtras, ModelList, StatusReport, WaitingReminder, approvals_report,
    auto_off, cheapest_model, config_schema, config_set, config_show, current_settings,
    doctor_adapters, doctor_details, doctor_extras, doctor_version_checks, models, repo_root,
    settings_warnings, status, waiting_runs,
};
pub use learning::shell::{LearningShell, ShellOutput, learning, learning_proposals};
pub use live_registry::{LiveRunHost, live_run_hosts};
pub use mods::doctor_problems as mod_problems;
pub use run_command::{RunCommand, RunCommandExit, run_command};
pub use session_id::{FoundSession, find_session};
pub use startup::welcome;
pub use telemetry::{TelemetryAction, count_crashes, telemetry};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InteractiveOptions {
    /// Defaults to the repository holding the current directory.
    pub repo: Option<PathBuf>,
    /// `--set key=value` overrides.
    pub overrides: Vec<String>,
    pub screen_reader: bool,
    /// Forces the guided setup; the first start opens it anyway.
    pub guided_setup: bool,
}

/// What the app opens on besides the conversation. `argonauts resume` uses the other two.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FirstView {
    #[default]
    Conversation,
    /// The `/resume` picker.
    ResumePicker,
    /// Resumes this run and attaches to it.
    Resume(RunId),
    /// Attaches to a run just started, as `argonauts run` does.
    Attach(RunId),
    /// Carries on this earlier conversation, as `argonauts resume conversation-<n>` does.
    PastConversation(ConversationId),
    /// Carries on a CLI's own session, as `argonauts resume codex:<id>` does.
    CliSession(CliSessionRef),
    /// Carries on an agent's session from a run, as `argonauts resume run-<n>/s-<n>` does.
    Agent(AgentSessionRef),
    /// Carries on the newest conversation in this repo, as `argonauts -c` does.
    Continue,
}

/// True until the global settings file exists. Finishing or skipping the setup writes it.
pub fn needs_guided_setup(home: &Paths) -> bool {
    !home.global_config().exists()
}

/// Runs the conversation until you exit. Runs keep going after it closes.
pub async fn run_interactive(options: InteractiveOptions) -> anyhow::Result<()> {
    startup::run(options, FirstView::Conversation).await
}

/// `run_interactive`, opening on a run or the resume picker.
pub async fn run_interactive_with(
    options: InteractiveOptions,
    first: FirstView,
) -> anyhow::Result<()> {
    startup::run(options, first).await
}

/// Hosts one run: takes its lock, rebuilds it from its log, and drives the engine until it
/// finishes or waits.
pub async fn host(repo_root: &Path, run: &RunId) -> anyhow::Result<()> {
    if let Ok(env) = Paths::from_env()
        .map_err(anyhow::Error::from)
        .and_then(|paths| env::AppEnv::for_repo(paths, repo_root, false))
    {
        // The judge and the narrator run here, and read your templates for their prompts.
        prompts::catalog::use_files_of(&env);
    }
    host::host(repo_root, run).await
}

/// For the CLI and the MCP server: never reads the approval token and holds no `Approvals`.
pub fn conductor(repo_root: &Path) -> anyhow::Result<Arc<dyn ConductorApi>> {
    client::conductor(repo_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guided_setup_until_global_config_exists() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path());
        assert!(needs_guided_setup(&home));
        std::fs::write(home.global_config(), "# yours\n").unwrap();
        assert!(!needs_guided_setup(&home));
    }
}
