//! The limits scenarios' own checks, which run now: every fake script parses, renders with its
//! reset times, and names fixtures that exist, and the sandbox probe reports each attempt.

mod limits_support;

use std::fs;
use std::process::Command;

use argonauts_scenarios::scenario_file;
use argonauts_testkit::FakeScenario;
use argonauts_testkit::FakeStep;
use argonauts_testkit::acp::AcpStep;
use limits_support::*;

#[test]
fn every_limits_scenario_renders_with_real_fixtures() {
    let folder = scenario_file("limits");
    let mut names: Vec<String> = fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(names.len() >= 30, "{names:?}");

    let out = tempfile::tempdir().unwrap();
    for name in &names {
        let rendered = render_scenario(name, out.path()).unwrap();
        let text = fs::read_to_string(&rendered).unwrap();
        assert!(
            !text.contains(RESET_SOON) && !text.contains(RESET_LATER),
            "{name}"
        );
        let scenario = FakeScenario::load(&rendered).unwrap();
        assert!(
            !scenario.rules.is_empty() || !scenario.acp.is_empty(),
            "{name} scripts nothing"
        );
        for step in scenario.rules.iter().flat_map(|rule| &rule.steps) {
            match step {
                FakeStep::EmitFile(path) => assert!(fs::metadata(path).is_ok(), "{name}: {path}"),
                FakeStep::Emit(line) => {
                    let parsed: Result<serde_json::Value, _> = serde_json::from_str(line);
                    assert!(parsed.is_ok(), "{name}: not JSON: {line}");
                }
                _ => {}
            }
        }
        let acp_files = scenario
            .acp
            .iter()
            .flat_map(|rule| &rule.turns)
            .flat_map(|turn| &turn.steps)
            .filter_map(|step| match step {
                AcpStep::UpdateFile(path) => Some(path),
                _ => None,
            });
        for path in acp_files {
            assert!(fs::metadata(path).is_ok(), "{name}: {path}");
        }
    }
}

#[test]
fn the_sandbox_probe_reports_each_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let readable = dir.path().join("readable.txt");
    fs::write(&readable, "x").unwrap();
    let checks = [
        try_write(&dir.path().join("written.txt")),
        try_write(&dir.path().join("missing-folder/written.txt")),
        try_read(&readable),
        try_read_matching(dir.path(), "*/nothing-here"),
    ];
    let probe = write_probe(&dir.path().join("probe"), "true", &checks).unwrap();

    let output = Command::new(&probe).arg("-p").output().unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(probe_lines(&stderr, LEAKED, "write").len(), 1, "{stderr}");
    assert_eq!(probe_lines(&stderr, BLOCKED, "write").len(), 1, "{stderr}");
    assert_eq!(probe_lines(&stderr, LEAKED, "read").len(), 1, "{stderr}");
    assert_eq!(probe_lines(&stderr, BLOCKED, "read").len(), 1, "{stderr}");

    let version = Command::new(&probe).arg("--version").output().unwrap();
    assert!(version.stderr.is_empty(), "--version must skip the checks");
}
