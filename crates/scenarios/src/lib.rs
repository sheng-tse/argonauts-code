//! Helpers for the scenario tests, which drive the real `argonauts` binary in a pty against
//! fake vendor CLIs. A scenario's `emit_file` paths start at its own folder, so recorded
//! fixtures are `../../../adapters/<adapter>/fixtures/...` from `scenarios/<area>/`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use argonauts_core::events::RunEvent;
use argonauts_testkit::{FakeVendors, Pty, PtyCommand, TempRepo, Trace, argonauts_bin, journal};

pub const WAIT: Duration = Duration::from_secs(20);

pub const ROWS: u16 = 32;
pub const COLS: u16 = 100;

pub fn scenario_file(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scenarios")
        .join(relative)
}

pub struct Scenario {
    /// The repository and its private homes.
    pub repo: TempRepo,
    pub vendors: FakeVendors,
    pub trace: Trace,
}

impl Scenario {
    /// `scenario` is a path under `scenarios/`, such as `app/hi.toml`.
    pub fn new(scenario: &str) -> anyhow::Result<Self> {
        Self::from_file(&scenario_file(scenario))
    }

    /// Sets up a scenario file anywhere, such as one a test wrote with `FakeScenario::save`.
    pub fn from_file(scenario: &Path) -> anyhow::Result<Self> {
        let repo = TempRepo::new()?;
        let vendors = FakeVendors::install(repo.scratch(), scenario)?;
        let trace = Trace::new(repo.scratch().join("trace.jsonl"));
        Ok(Self {
            repo,
            vendors,
            trace,
        })
    }

    /// Everything the binary needs: private homes, the fakes first on `PATH`, and the trace.
    pub fn envs(&self) -> Vec<(String, OsString)> {
        let mut envs = self.repo.envs();
        envs.extend(self.vendors.envs());
        envs.extend(self.trace.envs());
        envs
    }

    pub fn launch(&self, args: &[&str]) -> anyhow::Result<Pty> {
        self.launch_sized(args, ROWS, COLS)
    }

    pub fn launch_sized(&self, args: &[&str], rows: u16, cols: u16) -> anyhow::Result<Pty> {
        let binary = argonauts_bin().context("finding the argonauts binary")?;
        Pty::spawn(
            PtyCommand::new(binary)
                .args(args.iter().copied())
                .cwd(self.repo.root())
                .envs(self.envs())
                .env_remove("ARGONAUTS_SESSION")
                .size(rows, cols),
        )
    }

    pub fn latest_run_events(&self) -> anyhow::Result<Vec<RunEvent>> {
        let run = journal::latest_run(self.repo.home())
            .ok_or_else(|| anyhow::anyhow!("no run under {}", self.repo.home().display()))?;
        journal::events(&run)
    }

    /// Saves the screen for the screenshot review when `ARGONAUTS_SHOTS_DIR` is set.
    pub fn shot(&self, pty: &Pty, name: &str) -> anyhow::Result<()> {
        pty.dump(name).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_testkit::FakeScenario;

    #[test]
    fn doctor_finds_the_fake_vendors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("versions.toml");
        FakeScenario::default()
            .version("claude", "2.1.284 (Claude Code)")
            .version("codex", "codex-cli 0.158.0")
            .version("agy", "1.2.13")
            .save(&path)
            .unwrap();
        let scenario = Scenario::from_file(&path).unwrap();
        let mut pty = scenario.launch(&["doctor"]).unwrap();
        pty.wait_for("0\\.158\\.0", WAIT).unwrap();
        pty.wait_for("2\\.1\\.284", WAIT).unwrap();
        pty.wait_for_exit(WAIT).unwrap();
        let called: Vec<String> = scenario
            .vendors
            .invocations()
            .unwrap()
            .into_iter()
            .map(|invocation| invocation.argv0)
            .collect();
        assert!(called.contains(&"codex".to_owned()), "{called:?}");
        scenario.shot(&pty, "doctor").unwrap();
    }
}
