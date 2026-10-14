//! Every CLI session needs a sandbox (`SandboxMode::None` is for endpoints only), and
//! build and experiment sessions also need the approved goal's hash.

use argonauts_core::policy::{HardLimit, Violation};
use argonauts_core::session_runner::{
    Access, RunnerDirectory, RunnerKind, SandboxMode, SessionSpec,
};
use argonauts_core::settings::Settings;

use super::violation;

pub fn check(
    spec: &SessionSpec,
    settings: &Settings,
    workers: &dyn RunnerDirectory,
) -> Vec<Violation> {
    let mut violations = Vec::new();
    // A worker the directory doesn't know could be anything, so it's held to a CLI's rules.
    let cli = workers
        .get(&spec.model)
        .map_or(true, |worker| worker.kind() == RunnerKind::Cli);
    if cli && spec.sandbox.mode == SandboxMode::None {
        let mode = settings.sandbox.mode;
        let detail = if mode.ours_off() {
            format!(
                "{} runs without our sandbox because you set sandbox.mode to {}, so every \
                 approval of this run asks you",
                spec.model,
                mode.as_str()
            )
        } else {
            format!("{} is a CLI and has no sandbox", spec.model)
        };
        violations.push(violation(HardLimit::NoSandbox, detail, Vec::new()));
    }
    let hashed = spec
        .goal_sha256
        .as_deref()
        .is_some_and(|hash| !hash.trim().is_empty());
    if spec.access.writes() && !hashed {
        let what = match spec.access {
            Access::Build => "a build session needs the approved goal's hash",
            Access::Experiment => "an experiment session needs the approved goal's hash",
            _ => "a direct turn needs the hash of your message",
        };
        violations.push(violation(
            HardLimit::NoGoalHash,
            what.to_owned(),
            Vec::new(),
        ));
    }
    violations
}
