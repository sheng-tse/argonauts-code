//! `@` files through the real controller loop: the list the screen gets, and what reaches the
//! agent when a message mentions one.

use argonauts_core::app::{AppEvent, ConversationEvent, UiCommand, ViewRequest};
use argonauts_core::events::session::InputPartKind;
use argonauts_testkit::{FakeSession, TempRepo};
use serde_json::json;

use crate::controller::test_support::{CliOptions, TestDriver};

fn answer(text: &str) -> FakeSession {
    FakeSession::structured(json!({"kind": "answer", "answer_markdown": text, "estimate": null}))
}

fn repo_files(event: &AppEvent) -> Option<Vec<String>> {
    match event {
        AppEvent::RepoFiles { paths } => {
            let mut paths = paths.clone();
            paths.sort();
            Some(paths)
        }
        _ => None,
    }
}

fn with_files() -> CliOptions {
    CliOptions {
        prepare: Some(Box::new(|repo: &TempRepo| {
            repo.write(".gitignore", "secret.env\n").unwrap();
            repo.write("README.md", "# Demo\nA tiny demo.\n").unwrap();
            repo.write("src/lib.rs", "pub fn add() {}\n").unwrap();
            repo.write("secret.env", "TOKEN=shh\n").unwrap();
        })),
        ..CliOptions::default()
    }
}

#[tokio::test]
async fn the_screen_gets_the_repo_files_at_start_and_when_it_asks() {
    let mut driver = TestDriver::start_with(with_files()).await;
    let first = driver.until(|e| repo_files(e).is_some()).await;
    let files = repo_files(&first).unwrap();
    assert!(files.contains(&"README.md".to_owned()), "{files:?}");
    assert!(files.contains(&"src/lib.rs".to_owned()), "{files:?}");
    assert!(
        !files.contains(&"secret.env".to_owned()),
        "ignored: {files:?}"
    );

    driver.repo.write("notes.txt", "new\n").unwrap();
    driver
        .send(UiCommand::Request {
            view: ViewRequest::RepoFiles,
        })
        .await;
    let again = driver
        .until(|e| repo_files(e).is_some_and(|f| f.contains(&"notes.txt".to_owned())))
        .await;
    assert_eq!(repo_files(&again).unwrap().len(), files.len() + 1);
}

#[tokio::test]
async fn a_mentioned_file_reaches_the_agent_with_its_contents() {
    let mut driver = TestDriver::start_with(with_files()).await;
    driver.agent.push(answer("A tiny demo."));
    // Led by a file, the message is a request about it, not a mention of an agent.
    driver
        .submit("@README.md what is this? Compare @src/ and @secret.env")
        .await;
    let started = driver
        .until(|e| {
            matches!(
                e,
                AppEvent::Conversation {
                    event: ConversationEvent::TurnStarted { .. }
                }
            )
        })
        .await;
    let AppEvent::Conversation {
        event: ConversationEvent::TurnStarted { text, .. },
    } = started
    else {
        unreachable!()
    };
    assert_eq!(
        text,
        "@README.md what is this? Compare @src/ and @secret.env"
    );
    driver
        .until(|e| {
            matches!(
                e,
                AppEvent::Conversation {
                    event: ConversationEvent::TurnFinished { .. }
                }
            )
        })
        .await;

    let spec = driver.agent.specs().remove(0);
    let files: Vec<_> = spec
        .prompt
        .iter()
        .filter(|part| part.kind == InputPartKind::File)
        .collect();
    let labels: Vec<&str> = files.iter().map(|part| part.label.as_str()).collect();
    assert_eq!(labels, ["@README.md", "@src/", "@secret.env"]);
    assert!(
        files[0]
            .text
            .ends_with("<file path=\"README.md\">\n# Demo\nA tiny demo.\n</file>"),
        "{}",
        files[0].text
    );
    assert!(
        files[1].text.contains("It holds lib.rs."),
        "{}",
        files[1].text
    );
    assert!(
        !files[2].text.contains("TOKEN"),
        "an ignored file goes by name"
    );
    // The files follow the request, which still reads as you wrote it.
    let request = spec
        .prompt
        .iter()
        .position(|part| part.kind == InputPartKind::Prompt)
        .unwrap();
    let first_file = spec
        .prompt
        .iter()
        .position(|part| part.kind == InputPartKind::File)
        .unwrap();
    assert!(request < first_file);
    assert!(
        spec.prompt[request]
            .text
            .contains("@README.md what is this?")
    );
}

#[tokio::test]
async fn a_message_with_no_file_mentions_adds_nothing() {
    let mut driver = TestDriver::start_with(with_files()).await;
    driver.agent.push(answer("Hi."));
    driver.submit("hi @nobody, mail me@example.com").await;
    driver
        .until(|e| {
            matches!(
                e,
                AppEvent::Conversation {
                    event: ConversationEvent::TurnFinished { .. }
                }
            )
        })
        .await;
    let spec = driver.agent.specs().remove(0);
    assert!(
        spec.prompt
            .iter()
            .all(|part| part.kind != InputPartKind::File)
    );
}
