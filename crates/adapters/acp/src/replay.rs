//! Fixture replays. A fixture folder holds `meta.json` (the profile, the session start, and
//! whether it's synthetic or what recorded it), `transcript.jsonl`, and `expected.json` (the
//! events and the end). Made-up transcripts live in `synthetic/`, and ones recorded from a real
//! agent in `recorded/<agent>/`, with the version in its `meta.json`.
//! Each transcript line is one of:
//! - `{"out": frame}`: the driver must send a frame that holds every field of `frame`;
//! - `{"in": frame}`: the agent sends `frame`;
//! - `{"control": ...}`: the worker sends `"stop_after_tool"`, `"cancel"`, or
//!   `{"deliver": text, "now": bool}`;
//! - `{"report": object}`: the report tool writes `report.json`.
//!
//! `{cwd}` and `{report_file}` in any of them stand for the session's folder and report file.
//! `ACP_REPLAY_BLESS=1` rewrites `expected.json` from what the driver did, for review.

use std::path::{Path, PathBuf};

use argonauts_core::driver::{DriverControl, DriverStart};
use argonauts_core::registry::acp::AcpProfile;
use argonauts_core::registry::cli::ErrorPattern;
use serde_json::Value;

use crate::driver::DriverProfile;
use crate::errors::ErrorRules;
use crate::testing;

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

fn read_json(path: &Path, cwd: &Path, report_file: &Path) -> Value {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    serde_json::from_str(&fill(&text, cwd, report_file))
        .unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

fn fill(text: &str, cwd: &Path, report_file: &Path) -> String {
    text.replace("{cwd}", &cwd.display().to_string())
        .replace("{report_file}", &report_file.display().to_string())
}

/// Every field of `expected` is in `actual`, recursively. Arrays match item by item.
fn assert_holds(expected: &Value, actual: &Value, at: &str) {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            for (key, value) in expected {
                let Some(found) = actual.get(key) else {
                    panic!(
                        "{at}.{key} is missing from {}",
                        Value::Object(actual.clone())
                    );
                };
                assert_holds(value, found, &format!("{at}.{key}"));
            }
        }
        (Value::Array(expected), Value::Array(actual)) => {
            assert_eq!(expected.len(), actual.len(), "{at}: {actual:?}");
            for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
                assert_holds(expected, actual, &format!("{at}[{index}]"));
            }
        }
        _ => assert_eq!(expected, actual, "{at}"),
    }
}

async fn replay(name: &str) {
    let dir = fixture_dir(name);
    let scratch = tempfile::tempdir().unwrap();
    let cwd = scratch.path().join("worktree");
    let report_file = scratch.path().join("report.json");
    std::fs::create_dir_all(&cwd).unwrap();

    let meta = read_json(&dir.join("meta.json"), &cwd, &report_file);
    let recorded = &meta["recorded"];
    let says_what_recorded_it = ["agent", "version", "date"].iter().all(|key| {
        recorded[*key]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    });
    assert!(
        meta["synthetic"] == true || (meta["synthetic"] == false && says_what_recorded_it),
        "{name}: mark made-up transcripts synthetic, and recorded ones with the agent, its \
         version, and the date"
    );
    let acp: AcpProfile = serde_json::from_value(meta["profile"].clone()).unwrap();
    let patterns: Vec<ErrorPattern> = serde_json::from_value(
        meta.get("errors")
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
    )
    .unwrap();
    let start: DriverStart = serde_json::from_value(meta["start"].clone()).unwrap();
    let profile = DriverProfile::new(acp, ErrorRules::compile(&patterns));

    let mut driven = testing::launch(profile, start);
    let transcript = std::fs::read_to_string(dir.join("transcript.jsonl")).unwrap();
    let mut deliveries = Vec::new();
    for (number, line) in transcript.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let step: Value = serde_json::from_str(&fill(line, &cwd, &report_file))
            .unwrap_or_else(|err| panic!("{name}:{}: {err}", number + 1));
        if let Some(expected) = step.get("out") {
            let actual = driven.agent.recv().await;
            assert_holds(expected, &actual, &format!("{name}:{}", number + 1));
        } else if let Some(frame) = step.get("in") {
            driven.agent.send(frame.clone()).await;
        } else if let Some(report) = step.get("report") {
            std::fs::write(&report_file, report.to_string()).unwrap();
        } else if let Some(control) = step.get("control") {
            match control {
                Value::String(kind) if kind == "stop_after_tool" => {
                    driven
                        .control
                        .send(DriverControl::StopAfterTool)
                        .await
                        .unwrap();
                }
                Value::String(kind) if kind == "cancel" => {
                    driven.control.send(DriverControl::Cancel).await.unwrap();
                }
                deliver => {
                    let text = deliver["deliver"].as_str().unwrap();
                    let now = deliver["now"].as_bool().unwrap_or(false);
                    deliveries.push(driven.deliver(text, now).await);
                }
            }
        } else {
            panic!("{name}:{}: an unknown step {step}", number + 1);
        }
    }
    driven
        .agent
        .silent_for(std::time::Duration::from_millis(50))
        .await;
    let testing::Driven {
        task,
        events,
        mut frames,
        agent,
        ..
    } = driven;
    let (end, events) = testing::finish(task, events).await;
    drop(agent);
    let mut logged = [0, 0];
    while let Ok(frame) = frames.try_recv() {
        logged[usize::from(frame.outgoing)] += 1;
    }
    let steps = |kind: &str| {
        transcript
            .lines()
            .filter(|line| line.starts_with(&format!("{{\"{kind}\"")))
            .count()
    };
    assert_eq!(
        logged,
        [steps("in"), steps("out")],
        "{name}: every frame both ways is logged"
    );

    let events = serde_json::to_value(&events).unwrap();
    if std::env::var_os("ACP_REPLAY_BLESS").is_some() {
        let actual = serde_json::json!({ "events": events, "end": end });
        let text = serde_json::to_string_pretty(&actual).unwrap() + "\n";
        let text = text
            .replace(&report_file.display().to_string(), "{report_file}")
            .replace(&cwd.display().to_string(), "{cwd}");
        std::fs::write(dir.join("expected.json"), text).unwrap();
    }
    let expected = read_json(&dir.join("expected.json"), &cwd, &report_file);
    let expected_events = expected["events"].as_array().unwrap();
    let actual_events = events.as_array().unwrap();
    for (index, (want, got)) in expected_events.iter().zip(actual_events).enumerate() {
        assert_eq!(want, got, "{name}: event {index}");
    }
    assert_eq!(
        expected_events.len(),
        actual_events.len(),
        "{name}: {events:#}"
    );
    assert_eq!(
        expected["end"],
        serde_json::to_value(&end).unwrap(),
        "{name}"
    );
    for delivered in deliveries {
        let how = delivered
            .await
            .map(|how| serde_json::to_value(how).unwrap());
        assert!(how.is_ok(), "{name}: a delivery never went out");
    }
}

#[tokio::test]
async fn acp_opencode_transcript_replays() {
    replay("synthetic/opencode").await;
}

#[tokio::test]
async fn acp_hermes_transcript_replays() {
    replay("synthetic/hermes").await;
}

#[tokio::test]
async fn recorded_opencode_transcript_replays() {
    replay("recorded/opencode").await;
}

#[tokio::test]
async fn recorded_hermes_transcript_replays() {
    replay("recorded/hermes").await;
}
