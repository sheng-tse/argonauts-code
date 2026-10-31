//! The Context tab before the first message, on a very first run: no `models.toml` and the
//! main session left on its CLI's default. Each CLI whose default the shipped catalog knows
//! shows that model's window at both real sizes; OpenCode runs whatever model its own setup
//! names, which only its list says, so before that list is read every figure shows with a
//! blank for the window, never an empty tab. With `ARGONAUTS_BIN` pointing at a release build,
//! that build is held to it.

use std::fs;

use anyhow::Context as _;
use argonauts_scenarios::{Scenario, WAIT, no_servers_of_yours};
use argonauts_testkit::{Pty, PtyCommand, argonauts_bin};

use crate::app_support::AGENTS;

/// Each CLI and the window its header gives on its default: Opus 5.5's 1M in Claude Code's own
/// spelling, Codex's 872,000 and agy's Gemini 1,048,576 read in 1,024s, all from the catalog,
/// and OpenCode's blank.
const DEFAULTS: [(&str, &str); 4] = [
    ("claude-code", "1m"),
    ("codex", "872k"),
    ("antigravity-cli", "1M"),
    ("opencode", "–"),
];

/// The sizes the approved drawings hold the Context tab to.
const SIZES: [(u16, u16); 2] = [(120, 38), (177, 40)];

#[test]
fn every_cli_left_on_its_default_shows_its_window_before_the_first_message() -> anyhow::Result<()> {
    let binary = argonauts_bin().context("finding the argonauts binary")?;
    for (cli, window) in DEFAULTS {
        for (cols, rows) in SIZES {
            let scenario = Scenario::new(AGENTS)?;
            let home = scenario.repo.home();
            let _ = fs::remove_file(home.join("models.toml"));
            let all = r#"["claude-code", "codex", "antigravity-cli", "opencode"]"#;
            fs::write(
                home.join("config.toml"),
                format!(
                    "workflow = \"council\"\n\n[crew]\nmain = \"{cli}\"\nworker = {all}\n\
                     reviewer = {all}\njudge = {all}\n"
                ),
            )?;
            let mut pty = Pty::spawn(
                PtyCommand::new(&binary)
                    .args(no_servers_of_yours(&[]))
                    .cwd(scenario.repo.root())
                    .envs(scenario.envs())
                    .env_remove("ARGONAUTS_SESSION")
                    .size(rows, cols),
            )?;
            pty.wait_for_visible("shift\\+tab to cycle", WAIT)?;
            pty.send("/context\r")?;
            let screen = pty.wait_for_visible("estimated", WAIT)?;
            let shown = screen.visible_text();
            let header = format!("/{window} tokens (");
            assert!(
                shown.contains(&header),
                "{cli} at {cols} x {rows} should show {header:?}:\n{shown}"
            );
            assert!(
                !home.join("models.toml").exists(),
                "no list was read before the tab was drawn"
            );
        }
    }
    Ok(())
}
