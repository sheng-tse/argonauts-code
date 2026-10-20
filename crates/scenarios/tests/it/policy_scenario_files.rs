//! Every policy scenario file parses, and every recorded fixture it plays exists.

use std::fs;

use argonauts_scenarios::scenario_file;
use argonauts_testkit::{FakeScenario, FakeStep};

#[test]
fn policy_scenario_files_load() -> anyhow::Result<()> {
    let dir = scenario_file("policy");
    let mut checked = 0;
    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|extension| extension != "toml") {
            continue;
        }
        let scenario = FakeScenario::load(&path)?;
        assert!(
            !scenario.rules.is_empty(),
            "{} has no rules",
            path.display()
        );
        for rule in &scenario.rules {
            for step in &rule.steps {
                if let FakeStep::EmitFile(fixture) = step {
                    let fixture = dir.join(fixture);
                    assert!(
                        fixture.is_file(),
                        "{} plays a missing {}",
                        path.display(),
                        fixture.display()
                    );
                }
                if let FakeStep::Emit(line) = step {
                    serde_json::from_str::<serde_json::Value>(line)
                        .map_err(|err| anyhow::anyhow!("{}: {line}: {err}", path.display()))?;
                }
            }
        }
        checked += 1;
    }
    assert!(
        checked >= 3,
        "found {checked} scenario files in {}",
        dir.display()
    );
    Ok(())
}
