//! Helpers for the scenario tests, which drive the real `argonauts` binary in a pty against
//! fake vendor CLIs. A scenario's `emit_file` paths start at its own folder, so recorded
//! fixtures are `../../../adapters/<adapter>/fixtures/...` from `scenarios/<area>/`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use argonauts_core::events::RunEvent;
use argonauts_testkit::{
    FakeVendors, Key, Pty, PtyCommand, TempRepo, Trace, argonauts_bin, journal,
};

mod leftovers;

pub const WAIT: Duration = Duration::from_secs(20);

/// A card that takes the keys by itself works only the arrows for its first 400 ms (the
/// screen's `CARD_GUARD`), so a key meant for something else never answers it. A test waits
/// that out before working a card, as someone reading it would.
pub fn past_the_card_guard() {
    std::thread::sleep(Duration::from_millis(500));
}

pub const ROWS: u16 = 32;
pub const COLS: u16 = 100;

pub fn scenario_file(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scenarios")
        .join(relative)
}

/// A repo with fake vendors. Dropping it stops the supervisors and fake vendors it left
/// running, then removes its folder.
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
                .args(inline_screen(args))
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

impl Drop for Scenario {
    fn drop(&mut self) {
        leftovers::stop(self);
    }
}

/// Scenarios read what the app printed from the terminal's scrollback, which only the inline
/// screen fills; the fullscreen mode has tests of its own in the tui crate.
pub fn inline_screen<'a>(args: &[&'a str]) -> Vec<&'a str> {
    if args.iter().any(|arg| arg.starts_with("ui.screen=")) {
        return args.to_vec();
    }
    let mut all = vec!["--set", "ui.screen=inline"];
    all.extend_from_slice(args);
    all
}

/// Pauses the run on screen at its next safe point, from its row in the list under the input:
/// `x` asks, and `p` pauses in place of stopping. Esc leaves a run's agents going,
/// so it no longer starts a pause. Back in the input, the working line says the run is
/// pausing.
pub fn pause_run(pty: &mut Pty) -> anyhow::Result<()> {
    pty.press(Key::Down)?;
    pty.press(Key::Char('x'))?;
    pty.wait_for("p to pause it", WAIT)?;
    pty.press(Key::Char('p'))?;
    pty.wait_for(r"Pausing… \(esc again to stop now\)", WAIT)?;
    // The list still has the keys, and Esc hands them back to the input.
    pty.press(Key::Esc)?;
    pty.wait_for(r"shift\+tab to cycle", WAIT)?;
    Ok(())
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

    #[test]
    fn a_dropped_scenario_stops_the_supervisors_it_left() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::os::unix::process::CommandExt as _;
        use std::time::Instant;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("versions.toml");
        FakeScenario::default().save(&path).unwrap();
        let scenario = Scenario::from_file(&path).unwrap();
        // A stand-in for `argonauts supervise`, leading its own group as the real one does,
        // with a child in that group.
        let script = dir.path().join("argonauts");
        std::fs::write(&script, "#!/bin/sh\nsleep 30 &\nwait\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut supervisor = std::process::Command::new(&script)
            .arg("supervise")
            .arg("--repo")
            .arg(scenario.repo.root())
            .args(["--run", "run-20261001-120000-00ab"])
            .process_group(0)
            .spawn()
            .unwrap();
        let group = supervisor.id();
        let in_group = || {
            let output = std::process::Command::new("/bin/ps")
                .args(["-axo", "pgid=,stat="])
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| !line.contains('Z'))
                .filter(|line| line.split_whitespace().next() == Some(&group.to_string()))
                .count()
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while in_group() < 2 {
            assert!(
                Instant::now() < deadline,
                "the fallback never started its child"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        drop(scenario);
        let deadline = Instant::now() + Duration::from_secs(5);
        while supervisor.try_wait().unwrap().is_none() || in_group() > 0 {
            assert!(
                Instant::now() < deadline,
                "the supervisor or its group outlived the scenario"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
