//! A direct turn whose CLI says macOS keeps it out of a folder, through the real controller
//! loop with a fake CLI: one `◆` line says where to let it in, in place of the CLI's words.

use argonauts_core::NoteLevel;
use argonauts_core::agents::session_runner::{RunnerEvent, SessionSpec};
use argonauts_core::app::{AppEvent, ConversationEvent};
use argonauts_core::runs::event::AgentEvent;
use argonauts_core::{Note, safety::macos_folders::CANT_READ_THIS_FOLDER};
use argonauts_testkit::{FakeSession, TempRepo};
use serde_json::json;

use crate::controller::base::test_support::{CliOptions, TestDriver};

/// A listing that failed in agy 1.4's words, for the folder `path`.
fn blocked_listing(id: &str, path: &str) -> [AgentEvent; 2] {
    [
        AgentEvent::ToolStarted {
            id: id.to_owned(),
            name: "list_dir".to_owned(),
            input: json!({ "DirectoryPath": path }),
            parent: None,
        },
        AgentEvent::ToolFinished {
            id: id.to_owned(),
            ok: false,
            output: Some(format!(
                "failed to read directory {path}. macOS privacy settings are blocking Antigravity"
            )),
            parent: None,
        },
    ]
}

#[tokio::test]
async fn a_folder_macos_blocks_is_said_once_with_where_to_let_the_cli_in() {
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
    let downloads = driver.repo.user_home().join("Downloads/notes");
    let downloads = downloads.display().to_string();
    let events: Vec<AgentEvent> = ["t1", "t2"]
        .into_iter()
        .flat_map(|id| blocked_listing(id, &downloads))
        .collect();
    driver.agent.respond(move |_: &SessionSpec| {
        events.iter().cloned().fold(
            FakeSession::completed("I couldn't read your notes."),
            |session, event| session.with_event(RunnerEvent::Agent(event)),
        )
    });
    driver.submit("Read my notes in Downloads.").await;
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
    let lines: Vec<Note> = events
        .iter()
        .filter_map(|event| match event {
            AppEvent::Notice { level, text } if text.contains(CANT_READ_THIS_FOLDER) => {
                Some(Note {
                    level: *level,
                    text: text.clone(),
                })
            }
            _ => None,
        })
        .collect();
    let [line] = lines.as_slice() else {
        panic!("said once: {lines:?}");
    };
    assert_eq!(line.level, NoteLevel::Ask);
    assert!(
        line.line()
            .ends_with("macOS keeps Claude Code out of Downloads"),
        "{line:?}"
    );
    assert_eq!(
        line.rows().collect::<Vec<_>>(),
        [
            "To let it in: System Settings › Privacy & Security › Files and Folders › \
          Antigravity › Downloads Folder"
        ]
    );
    // The failed calls reach the screen without the CLI's raw words.
    let raw = events.iter().any(|event| {
        matches!(event, AppEvent::Conversation {
            event: ConversationEvent::Agent {
                event: AgentEvent::ToolFinished { output: Some(said), .. }, ..
            },
        } if said.contains("privacy settings"))
    });
    assert!(!raw);
}
