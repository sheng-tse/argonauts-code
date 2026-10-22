//! A direct turn's work outside your folder, through the real controller loop with a fake CLI
//! and a temp home: on autopilot it reads and writes anywhere but the never-list, each write
//! there leaves a dim line, and supervised keeps to the folder and asks as before.

use std::path::{Path, PathBuf};

use argonauts_conductor::sandbox::profile::{ProfileInput, can_read, can_write, session_profile};
use argonauts_core::app::{AppEvent, ConversationEvent};
use argonauts_core::event::AgentEvent;
use argonauts_core::session_runner::{Access, RunnerEvent, SandboxProfile, SessionSpec};
use argonauts_core::{Asks, NoteLevel};
use argonauts_testkit::{FakeSession, TempRepo};
use serde_json::json;

use crate::controller::test_support::{CliOptions, TestDriver};

const TASK: &str = "Look at the screenshot in Downloads and write what it shows to notes.";

fn options(config: &'static str) -> CliOptions {
    CliOptions {
        prepare: Some(Box::new(move |repo: &TempRepo| {
            repo.write("README.md", "notes\n").unwrap();
            repo.commit_all("start").unwrap();
            std::fs::write(
                repo.global_config(),
                format!("{config}\n[captain_or_crew]\nforce = \"captain\"\n"),
            )
            .unwrap();
            let home = repo.user_home();
            std::fs::create_dir_all(home.join("Downloads")).unwrap();
            std::fs::create_dir_all(home.join(".ssh")).unwrap();
            std::fs::write(home.join("Downloads/x.png"), "picture").unwrap();
            std::fs::write(home.join(".ssh/id_rsa"), "not a real key").unwrap();
        })),
        ..CliOptions::default()
    }
}

/// A CLI that writes `~/Downloads/y.md` with its Write tool and answers.
fn writing_outside(home: PathBuf) -> impl Fn(&SessionSpec) -> FakeSession + Send + Sync {
    move |_: &SessionSpec| {
        let notes = home.join("Downloads/y.md");
        std::fs::write(&notes, "a screenshot of a chart\n").unwrap();
        FakeSession::completed("Wrote the notes.")
            .with_event(RunnerEvent::Agent(AgentEvent::ToolStarted {
                id: "w1".into(),
                name: "Write".into(),
                input: json!({ "file_path": notes }),
                parent: None,
            }))
            .with_event(RunnerEvent::Agent(AgentEvent::ToolFinished {
                id: "w1".into(),
                ok: true,
                output: None,
                parent: None,
            }))
            .with_event(RunnerEvent::Agent(AgentEvent::FileChanged {
                path: notes.display().to_string(),
                kind: "add".into(),
            }))
    }
}

/// What each CLI prints for a write to `path` that worked, as its adapter reads it.
pub(super) fn written_by(agent: &str, path: &Path) -> Vec<AgentEvent> {
    let at = |template: &str| template.replace("PATH", &path.display().to_string());
    let lines: Vec<String> = match agent {
        "claude-code" => [
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Write","input":{"file_path":"PATH","content":"hi\n"}}]},"parent_tool_use_id":null}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"File created successfully at: PATH"}]},"parent_tool_use_id":null,"tool_use_result":{"type":"create","filePath":"PATH","content":"hi\n"}}"#,
        ]
        .map(at)
        .to_vec(),
        "codex" => [
            r#"{"type":"item.started","item":{"id":"item_1","type":"file_change","changes":[{"path":"PATH","kind":"add"}],"status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"file_change","changes":[{"path":"PATH","kind":"add"}],"status":"completed"}}"#,
        ]
        .map(at)
        .to_vec(),
        "agy write_to_file" | "agy sed_file" => {
            let tool = agent.trim_start_matches("agy ");
            let step = |state: &str| {
                at(&format!(
                    r#"{{"event":"step_update","step_update":{{"conversation_id":"c1","step_index":5,"state":"{state}","step_type":"tool","tool_name":"{tool}","tool_info":{{"name":"{tool}","parameters":{{"TargetFile":"PATH"}}}}}}}}"#
                ))
            };
            vec![step("ACTIVE"), step("DONE")]
        }
        "opencode" => [
            r#"{"type":"tool_use","part":{"type":"tool","tool":"edit","callID":"call_1","state":{"status":"completed","input":{"path":"PATH","oldString":"a","newString":"b"},"output":"Edit applied successfully."}}}"#,
        ]
        .map(at)
        .to_vec(),
        _ => unreachable!("{agent}"),
    };
    let id = agent.split(' ').next().unwrap_or(agent);
    let adapter = crate::adapters::builtin_adapters()
        .into_iter()
        .find(|adapter| adapter.id() == id.replace("agy", "antigravity-cli"))
        .unwrap();
    let mut parser = adapter.parser();
    lines
        .iter()
        .flat_map(|line| parser.parse_stdout_line(line))
        .collect()
}

/// The OS profile the runner builds from the turn's spec, as its CLI would run in it.
fn os_profile(spec: &SessionSpec, home: &Path, argonauts: &Path) -> SandboxProfile {
    let input = ProfileInput {
        access: spec.access,
        agent_id: "claude-code",
        workdir: &spec.workdir,
        session_dir: &spec.session_dir,
        argonauts_home: argonauts,
        user_home: home,
        base_worktree: None,
        status_file: None,
        extra_writable: &[],
        sees_other_worktrees: false,
        commits: false,
    };
    session_profile(&spec.sandbox, &input)
}

fn turn_ended(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Conversation {
            event: ConversationEvent::TurnFinished { .. } | ConversationEvent::TurnFailed { .. }
        }
    )
}

fn notices(events: &[AppEvent]) -> Vec<(NoteLevel, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            AppEvent::Notice { level, text, .. } => Some((*level, text.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_direct_turn_on_autopilot_works_outside_but_never_in_the_never_list() {
    let mut driver = TestDriver::start_with(options("[approvals]\nautopilot = true\n")).await;
    let home = driver.repo.user_home().to_owned();
    driver.agent.respond(writing_outside(home.clone()));
    driver.submit(TASK).await;
    driver
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.starts_with("wrote")))
        .await;
    let spec = driver.agent.specs().remove(0);
    let argonauts = driver.env.paths.home().to_owned();
    let events = driver.exit().await;

    assert_eq!(spec.access, Access::Direct);
    let outside = &spec.sandbox.outside;
    assert!(outside.read && outside.write);
    assert!(outside.never.refuses_read(&home.join(".ssh/id_rsa")));
    assert!(
        outside
            .never
            .refuses_read(&home.join("Downloads/../.ssh/id_rsa"))
    );
    let profile = os_profile(&spec, &home, &argonauts);
    assert!(can_read(&profile, &home.join("Downloads/x.png")));
    assert!(can_write(&profile, &home.join("Downloads/y.md")));
    assert!(!can_read(&profile, &home.join(".ssh/id_rsa")));
    assert!(!can_write(&profile, &home.join(".ssh/authorized_keys")));

    let said = notices(&events);
    let line = "wrote ~/Downloads/y.md, outside the project".to_owned();
    assert!(said.contains(&(NoteLevel::Quiet, line.clone())), "{said:?}");
    assert_eq!(said.iter().filter(|(_, text)| *text == line).count(), 1);
}

#[tokio::test]
async fn a_supervised_direct_turn_keeps_to_its_folder_and_asks_like_its_cli() {
    let mut driver =
        TestDriver::start_with(options("[approvals.understand]\nmode = \"supervised\"\n")).await;
    let home = driver.repo.user_home().to_owned();
    driver.agent.push(FakeSession::completed("Looked."));
    driver.submit(TASK).await;
    driver.until(turn_ended).await;
    let spec = driver.agent.specs().remove(0);
    let argonauts = driver.env.paths.home().to_owned();
    driver.exit().await;
    assert!(spec.sandbox.outside.is_closed());
    assert_eq!(
        spec.asking.as_ref().map(|asking| asking.asks),
        Some(Asks::LikeCli)
    );
    let profile = os_profile(&spec, &home, &argonauts);
    assert!(!can_write(&profile, &home.join("Downloads/y.md")));
}

#[tokio::test]
async fn each_cli_s_write_outside_your_folder_leaves_its_line() {
    let writers = [
        "claude-code",
        "codex",
        "agy write_to_file",
        "agy sed_file",
        "opencode",
    ];
    let mut driver = TestDriver::start_with(options("[approvals]\nautopilot = true\n")).await;
    let home = driver.repo.user_home().to_owned();
    let events: Vec<AgentEvent> = writers
        .iter()
        .flat_map(|writer| {
            let name = writer.replace(' ', "-");
            written_by(writer, &home.join(format!("Downloads/{name}.md")))
        })
        .collect();
    for writer in writers {
        let name = writer.replace(' ', "-");
        let path = home
            .join(format!("Downloads/{name}.md"))
            .display()
            .to_string();
        let reported = events.iter().any(|event| {
            matches!(event, AgentEvent::FileChanged { path: changed, .. } if *changed == path)
        });
        assert!(reported, "{writer}: {events:?}");
    }
    driver.agent.respond(move |_: &SessionSpec| {
        events
            .iter()
            .cloned()
            .fold(FakeSession::completed("Wrote them."), |session, event| {
                session.with_event(RunnerEvent::Agent(event))
            })
    });
    driver.submit(TASK).await;
    driver.until(turn_ended).await;
    let said = notices(&driver.exit().await);

    for writer in writers {
        let name = writer.replace(' ', "-");
        let line = format!("wrote ~/Downloads/{name}.md, outside the project");
        let times = said.iter().filter(|(_, text)| *text == line).count();
        assert_eq!(times, 1, "{writer}: {said:?}");
    }
}
