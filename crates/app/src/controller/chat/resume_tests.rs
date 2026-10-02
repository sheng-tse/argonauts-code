//! `/clear`, `/resume chat-<n>`, the picker's conversations, and `argonauts -c`, through the
//! real controller loop.

use argonauts_core::Paths;
use argonauts_core::app::{AppEvent, AppMode, ChatEvent, ReplayTurn, UiCommand, ViewData};
use argonauts_core::events::run::Party;
use argonauts_core::input::{ResumeTarget, SlashCommand};
use argonauts_core::names::ChatId;
use argonauts_core::worker::Worker as _;
use argonauts_testkit::{FakeSession, FakeWorker};
use serde_json::json;

use crate::FirstView;
use crate::chat_store::{ChatSession, ChatStore};
use crate::controller::Opening;
use crate::controller::test_support::{Harness, HarnessOptions};
use crate::env::AppEnv;

fn answer(text: &str) -> FakeSession {
    FakeSession::structured(json!({"kind": "answer", "answer_markdown": text, "estimate": null}))
}

fn slash(command: SlashCommand) -> UiCommand {
    UiCommand::Slash { command }
}

fn resume(chat: &str) -> UiCommand {
    slash(SlashCommand::Resume {
        target: Some(ResumeTarget::Conversation(ChatId::parse(chat).unwrap())),
    })
}

async fn turn(harness: &mut Harness, text: &str) {
    harness.submit(text).await;
    harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Chat {
                    event: ChatEvent::TurnFinished { .. } | ChatEvent::TurnFailed { .. }
                }
            )
        })
        .await;
    harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Mode {
                    mode: AppMode::Idle
                }
            )
        })
        .await;
}

async fn notice(harness: &mut Harness, part: &str) -> String {
    let event = harness
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains(part)))
        .await;
    match event {
        AppEvent::Notice { text, .. } => text,
        _ => unreachable!(),
    }
}

fn texts(turns: &[ReplayTurn]) -> Vec<(bool, String)> {
    turns
        .iter()
        .map(|turn| (turn.from == Party::User, turn.text.clone()))
        .collect()
}

fn has_primer(harness: &Harness, index: usize) -> bool {
    harness.agent.specs()[index]
        .prompt
        .iter()
        .any(|part| part.label == "chat so far")
}

#[tokio::test]
async fn clear_starts_a_new_conversation_and_resume_carries_the_old_one_on() {
    let mut harness = Harness::start().await;
    harness.agent.push(answer("Hi.").with_vendor_session("v-1"));
    harness
        .agent
        .push(answer("A new topic.").with_vendor_session("v-2"));
    harness
        .agent
        .push(answer("Back again.").with_vendor_session("v-1"));
    turn(&mut harness, "hi").await;

    harness.send(slash(SlashCommand::Clear)).await;
    let said = notice(&mut harness, "Started a new conversation").await;
    assert!(said.contains("/resume chat-001"), "{said}");
    turn(&mut harness, "something else").await;
    let specs = harness.agent.specs();
    assert_eq!(specs[1].resume, None, "a new conversation is a new session");
    assert!(!has_primer(&harness, 1));
    let chat_dir = harness.env.chat_dir();
    assert!(
        ChatStore::open(&chat_dir, &ChatId::numbered(2))
            .journal_path()
            .exists()
    );

    // `chat-1` names chat-001.
    harness.send(resume("chat-1")).await;
    let resumed = harness
        .until(|event| matches!(event, AppEvent::ChatResumed { .. }))
        .await;
    let AppEvent::ChatResumed {
        chat,
        title,
        worker,
        earlier,
        turns,
    } = resumed
    else {
        unreachable!()
    };
    assert_eq!(chat, ChatId::numbered(1));
    assert_eq!(title, "hi");
    assert_eq!(worker, Some("claude-code".parse().unwrap()));
    assert!(earlier.is_empty());
    assert_eq!(
        texts(&turns),
        vec![(true, "hi".to_owned()), (false, "Hi.".to_owned())]
    );
    let status = harness
        .until(|event| matches!(event, AppEvent::Status { .. }))
        .await;
    assert!(
        matches!(&status, AppEvent::Status { status } if status.title == "hi"),
        "{status:?}"
    );

    turn(&mut harness, "and again").await;
    let specs = harness.agent.specs();
    assert_eq!(specs[2].resume.as_deref(), Some("v-1"));
    // The resumed conversation goes on numbering its turns and keeps one journal.
    let events = ChatStore::open(&chat_dir, &ChatId::numbered(1))
        .events()
        .unwrap();
    let started: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::TurnStarted { turn, text, .. } => Some(format!("{turn} {text}")),
            _ => None,
        })
        .collect();
    assert_eq!(started, vec!["t-001 hi", "t-002 and again"]);
    harness.exit().await;
}

#[tokio::test]
async fn the_resume_picker_lists_conversations_with_runs() {
    let mut harness = Harness::start().await;
    harness.agent.push(answer("Hi."));
    turn(&mut harness, "what does export do?").await;
    harness
        .send(slash(SlashCommand::Resume { target: None }))
        .await;
    let view = harness
        .until(|event| matches!(event, AppEvent::View { data } if matches!(data.as_ref(), ViewData::Runs { .. })))
        .await;
    let AppEvent::View { data } = view else {
        unreachable!()
    };
    let ViewData::Runs { chats, .. } = *data else {
        unreachable!()
    };
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0].chat, ChatId::numbered(1));
    assert_eq!(chats[0].title, "what does export do?");
    assert_eq!(chats[0].turns, 1);
    harness.exit().await;
}

#[tokio::test]
async fn a_long_conversation_prints_its_last_turns_and_folds_the_rest() {
    let mut harness = Harness::start().await;
    for n in 1..=5 {
        harness.agent.push(answer(&format!("answer {n}")));
    }
    for n in 1..=5 {
        turn(&mut harness, &format!("question {n}")).await;
    }
    harness.send(slash(SlashCommand::Clear)).await;
    notice(&mut harness, "Started a new conversation").await;
    harness.send(resume("chat-001")).await;
    let AppEvent::ChatResumed { earlier, turns, .. } = harness
        .until(|event| matches!(event, AppEvent::ChatResumed { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(earlier.len(), 4);
    assert_eq!(turns.len(), 6);
    assert_eq!(turns[0].text, "question 3");
    assert_eq!(turns[5].text, "answer 5");
    harness.exit().await;
}

#[tokio::test]
async fn another_agent_starts_fresh_with_the_conversation_so_far() {
    let mut harness = Harness::start().await;
    harness.agent.push(answer("Use a middleware."));
    harness.agent.push(answer("Nine files."));
    let chat_dir = harness.env.chat_dir();
    let old = ChatStore::open(&chat_dir, &ChatId::numbered(7));
    old.append(&ChatEvent::TurnStarted {
        turn: argonauts_core::names::TurnId::numbered(1),
        text: "how do we add rate limiting?".to_owned(),
        worker: "codex".parse().unwrap(),
        read_only: true,
        commit: "abc".to_owned(),
        uncommitted: Vec::new(),
        settings_sha256: None,
    })
    .unwrap();
    old.append(&ChatEvent::TurnFinished {
        turn: argonauts_core::names::TurnId::numbered(1),
        reply: argonauts_core::app::TurnReply::Answer {
            markdown: "With a token bucket.".to_owned(),
        },
        spend: argonauts_core::Spend::default(),
        elapsed_ms: 5,
    })
    .unwrap();
    // Codex's session can't be resumed by Claude Code.
    old.save_session(&ChatSession {
        worker: "codex".parse().unwrap(),
        vendor_session: "codex-1".to_owned(),
    })
    .unwrap();
    harness.send(resume("chat-007")).await;
    harness
        .until(|event| matches!(event, AppEvent::ChatResumed { .. }))
        .await;
    turn(&mut harness, "how many files?").await;
    let spec = &harness.agent.specs()[0];
    assert_eq!(spec.resume, None);
    let primer = spec
        .prompt
        .iter()
        .find(|part| part.label == "chat so far")
        .expect("the conversation so far primes the session");
    assert!(
        primer.text.contains("With a token bucket."),
        "{}",
        primer.text
    );
    harness.exit().await;
}

#[tokio::test]
async fn a_vendor_that_cannot_resume_gets_a_handoff_note() {
    let mut harness = Harness::start_with(HarnessOptions {
        flags: vec![r#"default_agent="codex""#.to_owned()],
        ..HarnessOptions::default()
    })
    .await;
    let codex = FakeWorker::harness("codex", "gpt");
    let mut capabilities = codex.capabilities();
    capabilities.resume = false;
    let codex = harness.directory.add(codex.with_capabilities(capabilities));
    codex.push(answer("one").with_vendor_session("c-1"));
    codex.push(answer("two").with_vendor_session("c-1"));
    turn(&mut harness, "first").await;
    turn(&mut harness, "second").await;
    let specs = codex.specs();
    assert_eq!(specs[1].resume, None);
    assert!(
        specs[1]
            .prompt
            .iter()
            .any(|part| part.label == "chat so far")
    );
    harness.exit().await;
}

#[tokio::test]
async fn resuming_a_missing_conversation_says_so() {
    let mut harness = Harness::start().await;
    harness.send(resume("chat-042")).await;
    let said = notice(&mut harness, "chat-042").await;
    assert!(said.contains("no conversation"), "{said}");
    harness.exit().await;
}

#[tokio::test]
async fn continue_opens_the_newest_conversation() {
    let opening = Opening {
        guided_setup: false,
        first: FirstView::Continue,
    };
    let mut harness = Harness::start_with(HarnessOptions {
        opening,
        prepare: Some(Box::new(|repo| {
            let env = AppEnv::for_repo(Paths::new(repo.home()), repo.root(), true).unwrap();
            for (n, text) in [(1, "older"), (2, "newest")] {
                let store = ChatStore::open(&env.chat_dir(), &ChatId::numbered(n));
                store
                    .append(&ChatEvent::TurnStarted {
                        turn: argonauts_core::names::TurnId::numbered(1),
                        text: text.to_owned(),
                        worker: "claude-code".parse().unwrap(),
                        read_only: true,
                        commit: "abc".to_owned(),
                        uncommitted: Vec::new(),
                        settings_sha256: None,
                    })
                    .unwrap();
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        })),
        ..HarnessOptions::default()
    })
    .await;
    let AppEvent::ChatResumed { chat, title, .. } = harness
        .until(|event| matches!(event, AppEvent::ChatResumed { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(chat, ChatId::numbered(2));
    assert_eq!(title, "newest");
    harness.exit().await;
}
