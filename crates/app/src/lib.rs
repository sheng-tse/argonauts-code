//! The interactive app, the per-run supervisor process, and the conductor API.
//! Every other crate meets here, so its dependencies are the whole wiring.

mod adapters;
mod ancestry;
mod app_settings;
mod chat_store;
mod choices;
mod client;
mod commands;
mod controller;
mod env;
mod example_prompt;
mod git;
mod hidden_git;
mod host;
mod keep_awake;
mod learning;
mod live_registry;
mod mods;
mod old_names;
mod overview;
mod prompts;
mod repo_files;
mod run_command;
mod run_files;
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
use argonauts_core::names::ChatId;
use argonauts_core::vendor_sessions::VendorSessionRef;
use argonauts_core::{Paths, RunId};

pub use adapters::builtin_adapters;
pub use ancestry::refuse_inside_agent;
pub use commands::{
    ApprovalsReport, DoctorExtras, ParkedReminder, StatusReport, approvals_report, auto_off,
    config_schema, config_set, config_show, current_settings, doctor_adapters, doctor_details,
    doctor_extras, inbox, models, repo_root, settings_warnings, status,
};
pub use learning::shell::{LearningShell, ShellOutput, learning, learning_inbox};
pub use live_registry::{LiveSupervisor, live_supervisors};
pub use mods::doctor_problems as mod_problems;
pub use run_command::{RunCommand, RunCommandExit, run_command};
pub use telemetry::{TelemetryAction, telemetry};

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

/// What the app opens on besides the chat. `argonauts resume` uses the other two.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FirstView {
    #[default]
    Chat,
    /// The `/resume` picker.
    ResumePicker,
    /// Resumes this run and attaches to it.
    Resume(RunId),
    /// Attaches to a run just started, as `argonauts run` does.
    Attach(RunId),
    /// Carries on this earlier conversation, as `argonauts resume chat-<n>` does.
    Conversation(ChatId),
    /// Carries on the main agent CLI's own session, as `argonauts resume codex:<id>` does.
    VendorSession(VendorSessionRef),
    /// Carries on an agent's session from a run, as `argonauts resume run-<n>/s-<n>` does.
    Agent(AgentSessionRef),
    /// Carries on the newest conversation in this repo, as `argonauts -c` does.
    Continue,
}

/// True until the global settings file exists. Finishing or skipping the setup writes it.
pub fn needs_guided_setup(home: &Paths) -> bool {
    !home.global_config().exists()
}

/// Runs the chat until you exit. Runs keep going after it closes.
pub async fn run_interactive(options: InteractiveOptions) -> anyhow::Result<()> {
    startup::run(options, FirstView::Chat).await
}

/// `run_interactive`, opening on a run or the resume picker.
pub async fn run_interactive_with(
    options: InteractiveOptions,
    first: FirstView,
) -> anyhow::Result<()> {
    startup::run(options, first).await
}

/// Hosts one run: takes its lock, rebuilds it from its log, and drives the engine until it
/// finishes or parks.
pub async fn supervise(repo_root: &Path, run: &RunId) -> anyhow::Result<()> {
    if let Ok(env) = Paths::from_env()
        .map_err(anyhow::Error::from)
        .and_then(|paths| env::AppEnv::for_repo(paths, repo_root, false))
    {
        // The approver and the narrator run here, and read your templates for their prompts.
        prompts::catalog::use_files_of(&env);
    }
    host::supervise(repo_root, run).await
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
