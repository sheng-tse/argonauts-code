//! The detected-gates offer: `package.json` scripts named `test`, `lint`, `typecheck`;
//! `Cargo.toml` gives `cargo test` and `cargo clippy`; `pyproject.toml` gives `pytest`.

use std::path::Path;

use argonauts_core::app::{AppChoice, ChoiceKind};
use argonauts_core::names::ChoiceId;
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::settings::protocol::GateCommand;

use crate::choices::{app_choice, options};

/// In the repo's state directory once you skip the offer, so it's never made again there.
const SKIPPED: &str = "gates-offer-skipped";

const SCRIPTS: &[&str] = &["test", "lint", "typecheck"];

/// Gate commands the repo's files suggest, in that order.
pub(crate) fn detect(repo_root: &Path) -> Vec<String> {
    let mut commands = Vec::new();
    if let Some(scripts) = package_scripts(repo_root) {
        let runner = package_runner(repo_root);
        commands.extend(
            SCRIPTS
                .iter()
                .filter(|script| scripts.contains_key(**script))
                .map(|script| format!("{runner} run {script}")),
        );
    }
    if repo_root.join("Cargo.toml").is_file() {
        commands.push("cargo test".to_owned());
        commands.push("cargo clippy".to_owned());
    }
    if repo_root.join("pyproject.toml").is_file() {
        commands.push("pytest".to_owned());
    }
    commands
}

pub(crate) fn skipped(state_dir: &Path) -> bool {
    state_dir.join(SKIPPED).exists()
}

pub(crate) fn remember_skip(state_dir: &Path) -> anyhow::Result<()> {
    create_private_dir_all(state_dir)?;
    write_private_file(&state_dir.join(SKIPPED), b"")?;
    Ok(())
}

pub(crate) fn offer(id: ChoiceId, commands: &[String]) -> AppChoice {
    let mut body = vec!["Checks every card must pass, found in this repo:".to_owned()];
    body.extend(commands.iter().map(|command| format!("  {command}")));
    body.push(
        "They go to .argonauts/config.toml as gates.run. Gates are a safety setting, so your \
         answer here is the confirm."
            .to_owned(),
    );
    body.push("Edit them to change the list, one command per line.".to_owned());
    app_choice(
        id,
        ChoiceKind::DetectedGates,
        "Use these gate commands?",
        body,
        options(ChoiceKind::DetectedGates),
    )
}

/// One gate per non-empty line, each an argument vector split on spaces, never a shell string.
pub(crate) use argonauts_registry::gate_commands;

/// `gates.run` as TOML text.
pub(crate) fn gates_value(gates: &[GateCommand]) -> anyhow::Result<String> {
    Ok(toml::Value::try_from(gates)?.to_string())
}

fn package_scripts(repo_root: &Path) -> Option<serde_json::Map<String, serde_json::Value>> {
    let text = std::fs::read_to_string(repo_root.join("package.json")).ok()?;
    let package: serde_json::Value = serde_json::from_str(&text).ok()?;
    package.get("scripts")?.as_object().cloned()
}

/// The package manager the lockfile names, npm when there's none.
fn package_runner(repo_root: &Path) -> &'static str {
    [
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("bun.lockb", "bun"),
        ("bun.lock", "bun"),
    ]
    .into_iter()
    .find(|(lockfile, _)| repo_root.join(lockfile).is_file())
    .map_or("npm", |(_, runner)| runner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_come_from_the_files_present_in_order() {
        let repo = tempfile::tempdir().unwrap();
        assert!(detect(repo.path()).is_empty());
        std::fs::write(
            repo.path().join("package.json"),
            r#"{"scripts": {"typecheck": "tsc", "build": "x", "test": "vitest"}}"#,
        )
        .unwrap();
        std::fs::write(repo.path().join("pnpm-lock.yaml"), "").unwrap();
        std::fs::write(repo.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(repo.path().join("pyproject.toml"), "").unwrap();
        assert_eq!(
            detect(repo.path()),
            vec![
                "pnpm run test",
                "pnpm run typecheck",
                "cargo test",
                "cargo clippy",
                "pytest"
            ]
        );
    }

    #[test]
    fn edited_lines_become_named_argument_vectors() {
        let lines = [
            "npm run test",
            "",
            "cargo  test",
            "cargo clippy -- -D warnings",
        ]
        .map(str::to_owned);
        let gates = gate_commands(&lines);
        let names: Vec<&str> = gates.iter().map(|gate| gate.name.as_str()).collect();
        assert_eq!(names, vec!["test", "cargo-test", "clippy"]);
        assert_eq!(gates[1].command, vec!["cargo", "test"]);
        let value = gates_value(&gates[..1]).unwrap();
        assert_eq!(
            value,
            r#"[{ command = ["npm", "run", "test"], name = "test" }]"#
        );
    }

    #[test]
    fn a_skip_is_remembered() {
        let state = tempfile::tempdir().unwrap();
        assert!(!skipped(state.path()));
        remember_skip(state.path()).unwrap();
        assert!(skipped(state.path()));
    }
}
