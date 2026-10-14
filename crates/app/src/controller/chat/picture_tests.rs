//! A picture sent with your message reaches the main session's CLI as part of its prompt, and
//! only with the message it was sent with.

use argonauts_core::app::{AppEvent, ChatEvent, UiCommand};
use argonauts_core::events::session::InputPartKind;
use argonauts_core::pictures::Picture;
use argonauts_testkit::FakeSession;

use crate::controller::test_support::Harness;

fn turn_ended(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Chat {
            event: ChatEvent::TurnFinished { .. } | ChatEvent::TurnFailed { .. }
        }
    )
}

fn pictures_of(spec: &argonauts_core::worker::SessionSpec) -> Vec<(String, Option<String>)> {
    spec.prompt
        .iter()
        .filter(|part| part.kind == InputPartKind::Picture)
        .map(|part| (part.label.clone(), part.source.clone()))
        .collect()
}

#[tokio::test]
async fn a_picture_goes_with_its_message_to_the_main_session_and_no_further() {
    let mut harness = Harness::start().await;
    harness.directory.taking_tools("claude-code");
    let shot = harness.repo.root().join("shot.png");
    std::fs::write(&shot, [0x89, b'P', b'N', b'G']).unwrap();
    harness
        .agent
        .push(FakeSession::completed("The button overlaps the title."));
    harness.agent.push(FakeSession::completed("Yes."));
    harness
        .send(UiCommand::Submit {
            text: "what's off on this screen? [Image #1]".to_owned(),
            workflow_requested: false,
            pictures: vec![Picture {
                number: 1,
                path: shot.clone(),
            }],
        })
        .await;
    harness.until(turn_ended).await;
    harness.submit("is that all?").await;
    harness.until(turn_ended).await;
    let specs = harness.agent.specs();
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
    harness.exit().await;
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
    let mut harness = Harness::start().await;
    harness.directory.taking_tools("claude-code");
    let first = harness.repo.root().join("first.png");
    let second = harness.repo.root().join("second.png");
    for path in [&first, &second] {
        std::fs::write(path, [0x89, b'P', b'N', b'G']).unwrap();
    }
    harness
        .agent
        .push(FakeSession::completed("Looking.").with_delay(std::time::Duration::from_millis(200)));
    harness.agent.push(FakeSession::completed("One."));
    harness.agent.push(FakeSession::completed("Two."));
    harness.submit("look at the login page").await;
    harness
        .send(with_picture("the header [Image #1]", 1, &first))
        .await;
    harness
        .send(with_picture("the footer [Image #2]", 2, &second))
        .await;
    for _ in 0..3 {
        harness.until(turn_ended).await;
    }
    let specs = harness.agent.specs();
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
    harness.exit().await;
}
