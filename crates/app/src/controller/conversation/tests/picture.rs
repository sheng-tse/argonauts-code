//! A picture sent with your message reaches the main session's CLI as part of its prompt, and
//! only with the message it was sent with.

use argonauts_core::app::{AppEvent, ConversationEvent, UiCommand};
use argonauts_core::look::pictures::Picture;
use argonauts_core::runs::events::session::InputPartKind;
use argonauts_testkit::{FakeSession, Hold};

use crate::controller::base::test_support::{CliOptions, TestDriver, on_the_default_workflow};

fn turn_ended(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Conversation {
            event: ConversationEvent::TurnFinished { .. } | ConversationEvent::TurnFailed { .. }
        }
    )
}

fn pictures_of(
    spec: &argonauts_core::agents::session_runner::SessionSpec,
) -> Vec<(String, Option<String>)> {
    spec.prompt
        .iter()
        .filter(|part| part.kind == InputPartKind::Picture)
        .map(|part| (part.label.clone(), part.source.clone()))
        .collect()
}

#[tokio::test]
async fn a_picture_goes_with_its_message_to_the_main_session_and_no_further() {
    let mut driver = TestDriver::start().await;
    driver.directory.taking_tools("claude-code");
    let shot = driver.repo.root().join("shot.png");
    std::fs::write(&shot, [0x89, b'P', b'N', b'G']).unwrap();
    driver
        .agent
        .push(FakeSession::completed("The button overlaps the title."));
    driver.agent.push(FakeSession::completed("Yes."));
    driver
        .send(UiCommand::Submit {
            text: "what's off on this screen? [Image #1]".to_owned(),
            workflow_requested: false,
            pictures: vec![Picture {
                number: 1,
                path: shot.clone(),
            }],
        })
        .await;
    driver.until(turn_ended).await;
    driver.submit("is that all?").await;
    driver.until(turn_ended).await;
    let specs = driver.agent.specs();
    assert_eq!(
        pictures_of(&specs[0]),
        [("[Image #1]".to_owned(), Some(shot.display().to_string()))]
    );
    assert!(
        specs[0].prompt[0].text.contains("[Image #1]"),
        "the words keep their chip"
    );
    assert!(
        pictures_of(&specs[1]).is_empty(),
        "the next message has none"
    );
    driver.exit().await;
}

fn with_picture(text: &str, number: u32, path: &std::path::Path) -> UiCommand {
    UiCommand::Submit {
        text: text.to_owned(),
        workflow_requested: false,
        pictures: vec![Picture {
            number,
            path: path.to_path_buf(),
        }],
    }
}

#[tokio::test]
async fn messages_typed_while_a_turn_runs_each_keep_their_own_pictures() {
    let mut driver = TestDriver::start_with(on_the_default_workflow(CliOptions::default())).await;
    driver.directory.taking_tools("claude-code");
    let first = driver.repo.root().join("first.png");
    let second = driver.repo.root().join("second.png");
    for path in [&first, &second] {
        std::fs::write(path, [0x89, b'P', b'N', b'G']).unwrap();
    }
    let looking = Hold::new();
    driver
        .agent
        .push(FakeSession::completed("Looking.").held_after(0, &looking));
    driver.agent.push(FakeSession::completed("One."));
    driver.agent.push(FakeSession::completed("Two."));
    driver.submit("look at the login page").await;
    driver
        .send(with_picture("the header [Image #1]", 1, &first))
        .await;
    driver
        .send(with_picture("the footer [Image #2]", 2, &second))
        .await;
    driver.taken().await;
    looking.release();
    for _ in 0..3 {
        driver.until(turn_ended).await;
    }
    let specs = driver.agent.specs();
    assert!(pictures_of(&specs[0]).is_empty());
    let found: Vec<_> = specs[1..]
        .iter()
        .map(|spec| (spec.prompt[0].text.clone(), pictures_of(spec)))
        .collect();
    let of = |words: &str| {
        found
            .iter()
            .find(|(text, _)| text.contains(words))
            .map(|(_, pictures)| pictures.clone())
            .unwrap_or_else(|| panic!("no turn took {words}: {found:?}"))
    };
    assert_eq!(
        of("the header"),
        [("[Image #1]".to_owned(), Some(first.display().to_string()))]
    );
    assert_eq!(
        of("the footer"),
        [("[Image #2]".to_owned(), Some(second.display().to_string()))]
    );
    driver.exit().await;
}
