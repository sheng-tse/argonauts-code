//! A direct turn's read the never-list refused, through the real controller loop with a fake
//! CLI and a temp home: whatever words its CLI gives the refusal, one dim line after them names
//! the path.

use std::path::Path;

use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, ConversationEvent};
use argonauts_core::event::AgentEvent;
use argonauts_core::session_runner::{RunnerEvent, SessionSpec};
use argonauts_testkit::{FakeSession, TempRepo};

use crate::controller::test_support::{CliOptions, TestDriver};

/// What each CLI prints for a read of `path` that was refused, in its own words, as its
/// adapter reads it.
fn refused_by(agent: &str, path: &Path) -> Vec<AgentEvent> {
    let at = |template: &str| template.replace("PATH", &path.display().to_string());
    let lines: Vec<String> = match agent {
        "claude-code" => [
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"PATH"}}]},"parent_tool_use_id":null}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"Permission to read PATH has been denied by your permission settings.","is_error":true}]},"parent_tool_use_id":null}"#,
        ]
        .map(at)
        .to_vec(),
        "codex" => [
            r#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc \"cat PATH\"","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc \"cat PATH\"","aggregated_output":"cat: PATH: Operation not permitted\n","exit_code":1,"status":"failed"}}"#,
        ]
        .map(at)
        .to_vec(),
        "agy" => {
            let step = |state: &str, error: &str| {
                at(&format!(
                    r#"{{"event":"step_update","step_update":{{"conversation_id":"c1","step_index":5,"state":"{state}","step_type":"tool","tool_name":"view_file","tool_info":{{"name":"view_file","parameters":{{"AbsolutePath":"PATH"}}{error}}}}}}}"#
                ))
            };
            vec![
                step("ACTIVE", ""),
                step("ERROR", r#","error":"open PATH: operation not permitted""#),
            ]
        }
        "opencode" => [
            r#"{"type":"tool_use","part":{"type":"tool","tool":"read","callID":"call_1","state":{"status":"error","input":{"filePath":"PATH"},"error":"Unknown: FileSystem.stat"}}}"#,
        ]
        .map(at)
        .to_vec(),
        _ => unreachable!("{agent}"),
    };
    let adapter = crate::adapters::builtin_adapters()
        .into_iter()
        .find(|adapter| adapter.id() == agent.replace("agy", "antigravity-cli"))
        .unwrap();
    let mut parser = adapter.parser();
    lines
        .iter()
        .flat_map(|line| parser.parse_stdout_line(line))
        .collect()
}

#[tokio::test]
async fn each_cli_s_read_the_never_list_refused_leaves_one_line() {
    let clis = ["claude-code", "codex", "agy", "opencode"];
    let mut driver = TestDriver::start_with(CliOptions {
        prepare: Some(Box::new(|repo: &TempRepo| {
            repo.write("README.md", "notes\n").unwrap();
            repo.commit_all("start").unwrap();
            std::fs::write(
                repo.global_config(),
                "[captain_or_crew]\nforce = \"captain\"\n",
            )
            .unwrap();
        })),
        ..CliOptions::default()
    })
    .await;
    let home = driver.repo.user_home().to_owned();
    let events: Vec<AgentEvent> = clis
        .iter()
        .flat_map(|cli| refused_by(cli, &home.join(format!(".ssh/{cli}_key"))))
        .collect();
    for cli in clis {
        let refused = events.iter().any(|event| {
            matches!(event, AgentEvent::ToolFinished { ok: false, output: Some(said), .. }
                if said.contains(&format!("{cli}_key")) || said.contains("FileSystem.stat"))
        });
        assert!(refused, "{cli}: {events:?}");
    }
    driver.agent.respond(move |_: &SessionSpec| {
        events.iter().cloned().fold(
            FakeSession::completed("Couldn't read them."),
            |session, event| session.with_event(RunnerEvent::Agent(event)),
        )
    });
    driver.submit("Read the keys in ~/.ssh.").await;
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Conversation {
                    event: ConversationEvent::TurnFinished { .. }
                        | ConversationEvent::TurnFailed { .. }
                }
            )
        })
        .await;
    let events = driver.exit().await;
    let said: Vec<(NoteLevel, String)> = events
        .iter()
        .filter_map(|event| match event {
            AppEvent::Notice { level, text, .. } => Some((*level, text.clone())),
            _ => None,
        })
        .collect();
    for cli in clis {
        let line = format!("kept out: ~/.ssh/{cli}_key is on the never-list");
        let times = said
            .iter()
            .filter(|(level, text)| *level == NoteLevel::Quiet && *text == line)
            .count();
        assert_eq!(times, 1, "{cli}: {said:?}");
        // It follows the refusal the screen draws in the CLI's own words.
        let at = events
            .iter()
            .position(|event| matches!(event, AppEvent::Notice { text, .. } if *text == line))
            .unwrap();
        let before = events[..at].iter().rev().find_map(|event| match event {
            AppEvent::Conversation {
                event: ConversationEvent::Agent { event, .. },
            } => Some(event),
            _ => None,
        });
        assert!(
            matches!(before, Some(AgentEvent::ToolFinished { ok: false, .. })),
            "{cli}: {before:?}"
        );
    }
}

/// A container shows the turn nothing of this machine outside its own paths, so the never-list
/// holds nothing there, and a refused read is put down to the container, not to the list.
#[tokio::test]
async fn a_turn_in_a_container_leaves_no_line_blaming_the_list() {
    let mut driver = TestDriver::start_with(CliOptions {
        prepare: Some(Box::new(|repo: &TempRepo| {
            repo.write("README.md", "notes\n").unwrap();
            repo.commit_all("start").unwrap();
            std::fs::write(
                repo.global_config(),
                "[captain_or_crew]\nforce = \"captain\"\n[sandbox]\nmode = \"container\"\n",
            )
            .unwrap();
        })),
        ..CliOptions::default()
    })
    .await;
    let home = driver.repo.user_home().to_owned();
    let events = refused_by("claude-code", &home.join(".ssh/id_rsa"));
    driver.agent.respond(move |_: &SessionSpec| {
        events.iter().cloned().fold(
            FakeSession::completed("Couldn't read it."),
            |session, event| session.with_event(RunnerEvent::Agent(event)),
        )
    });
    driver.submit("Read the key in ~/.ssh.").await;
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Conversation {
                    event: ConversationEvent::TurnFinished { .. }
                        | ConversationEvent::TurnFailed { .. }
                }
            )
        })
        .await;
    let spec = driver.agent.specs().remove(0);
    let events = driver.exit().await;
    assert_eq!(
        spec.sandbox.mode,
        argonauts_core::session_runner::SandboxMode::Container
    );
    let blamed = events.iter().any(
        |event| matches!(event, AppEvent::Notice { text, .. } if text.starts_with("kept out")),
    );
    assert!(!blamed);
}
