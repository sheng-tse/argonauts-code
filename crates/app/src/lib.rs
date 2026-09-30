//! The interactive app, the per-run supervisor process, and the conductor API.
//! Every other crate meets here, so its dependencies are the whole wiring.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use argonauts_core::api::ConductorApi;
use argonauts_core::{Paths, RunId};

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

/// True until the global settings file exists. Finishing or skipping the setup writes it.
pub fn needs_guided_setup(home: &Paths) -> bool {
    !home.global_config().exists()
}

/// Runs the chat until you exit. Runs keep going after it closes.
pub async fn run_interactive(options: InteractiveOptions) -> anyhow::Result<()> {
    let _ = options;
    anyhow::bail!(
        "the interactive app is not implemented yet; `argonauts run --solo <task>` still works"
    )
}

/// Hosts one run: takes its lock, rebuilds it from its log, and drives the engine until it
/// finishes or parks.
pub async fn supervise(repo_root: &Path, run: &RunId) -> anyhow::Result<()> {
    let _ = repo_root;
    anyhow::bail!("the run supervisor is not implemented yet (run {run})")
}

pub fn conductor(repo_root: &Path) -> anyhow::Result<Arc<dyn ConductorApi>> {
    anyhow::bail!(
        "the conductor API is not implemented yet ({})",
        repo_root.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_placeholders_say_they_are_not_built() {
        let err = run_interactive(InteractiveOptions::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not implemented"));
        let run = RunId::new("run-01").unwrap();
        assert!(supervise(Path::new("/repo"), &run).await.is_err());
        assert!(conductor(Path::new("/repo")).is_err());
    }

    #[test]
    fn guided_setup_until_global_config_exists() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path());
        assert!(needs_guided_setup(&home));
        std::fs::write(home.global_config(), "# yours\n").unwrap();
        assert!(!needs_guided_setup(&home));
    }
}
