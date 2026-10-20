//! The agents scenarios' fake-agent files parse, and every file they play exists. This one
//! runs now, while the scenarios wait for the app.

use std::fs;

use argonauts_testkit::{FakeScenario, FakeStep};

/// The files the agents scenarios name.
const USED: [&str; 6] = [
    "config-layers.toml",
    "claude-worker.toml",
    "opencode-worker.toml",
    "manifest-cli.toml",
    "openclaw-final-json.toml",
    "hermes-acp.toml",
];

#[test]
fn agents_scenario_files_parse() -> anyhow::Result<()> {
    let dir = argonauts_scenarios::scenario_file("agents");
    for name in USED {
        assert!(dir.join(name).is_file(), "{name} is missing");
    }
    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        let scenario = FakeScenario::load(&path)?;
        for rule in &scenario.rules {
            for step in &rule.steps {
                if let FakeStep::EmitFile(file) = step {
                    let played = dir.join(file);
                    assert!(
                        played.is_file(),
                        "{} plays {file}, which is missing",
                        path.display()
                    );
                }
            }
        }
    }
    Ok(())
}
