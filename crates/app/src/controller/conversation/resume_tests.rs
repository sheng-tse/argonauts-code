//! `/clear`, `/resume conversation-<n>`, the picker's conversations, and `argonauts -c`, through
//! the real controller loop.

use argonauts_core::Paths;
use argonauts_core::app::{AppEvent, AppMode, ConversationEvent, ReplayTurn, UiCommand, ViewData};
use argonauts_core::event::AgentEvent;
use argonauts_core::events::run::Party;
use argonauts_core::input::{ResumeTarget, SlashCommand};
use argonauts_core::names::ConversationId;
use argonauts_core::session_runner::{RunnerEvent, SessionRunner as _};
use argonauts_testkit::{FakeRunner, FakeSession};
use serde_json::json;

use super::ConversationState;
use crate::FirstView;
use crate::controller::Opening;
use crate::controller::test_support::{CliOptions, TestCtx, TestDriver};
use crate::conversation_store::{ConversationSession, ConversationStore};
use crate::env::AppEnv;

fn answer(text: &str) -> FakeSession {
    FakeSession::structured(json!({"kind": "answer", "answer_markdown": text, "estimate": null}))
}

fn slash(command: SlashCommand) -> UiCommand {
    UiCommand::Slash { command }
}

fn resume(conversation: &str) -> UiCommand {
    slash(SlashCommand::Resume {
        target: Some(ResumeTarget::Conversation(
            ConversationId::parse(conversation).unwrap(),
        )),
    })
}

async fn turn(driver: &mut TestDriver, text: &str) {
    driver.submit(text).await;
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
    driver
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

async fn notice(driver: &mut TestDriver, part: &str) -> String {
    let event = driver
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

fn has_primer(driver: &TestDriver, index: usize) -> bool {
    driver.agent.specs()[index]
        .prompt
        .iter()
        .any(|part| part.label == "conversation so far")
}

#[tokio::test]
async fn clear_starts_a_new_conversation_and_resume_carries_the_old_one_on() {
    let mut driver = TestDriver::start().await;
    // The first conversation publishes a page, which its journal keeps.
    let page = "https://claude.ai/code/artifact/0b1e7c9a-4f1d-4c57-9a55-2f6f0d3a8e11";
    let publishing = [
        AgentEvent::ToolStarted {
            id: "t1".into(),
            name: "Artifact".into(),
            input: json!({"file_path": "out/weekly-usage-chart.html", "icon": "document"}),
            parent: None,
        },
        AgentEvent::ToolFinished {
            id: "t1".into(),
            ok: true,
            output: Some(format!(
                "Published out/weekly-usage-chart.html at {page} (Version 1)"
            )),
            parent: None,
        },
    ];
    let hi = publishing
        .into_iter()
        .fold(answer("Hi."), |session, event| {
            session.with_event(RunnerEvent::Agent(event))
        });
    driver.agent.push(hi.with_cli_session("v-1"));
    driver
        .agent
        .push(answer("A new topic.").with_cli_session("v-2"));
    driver
        .agent
        .push(answer("Back again.").with_cli_session("v-1"));
    turn(&mut driver, "hi").await;

    driver.send(slash(SlashCommand::Clear)).await;
    let said = notice(&mut driver, "Started a new conversation").await;
    assert!(said.contains("/resume conversation-001"), "{said}");
    turn(&mut driver, "something else").await;
    let specs = driver.agent.specs();
    assert_eq!(specs[1].resume, None, "a new conversation is a new session");
    assert!(!has_primer(&driver, 1));
    let conversation_dir = driver.env.conversation_dir();
    assert!(
        ConversationStore::open(&conversation_dir, &ConversationId::numbered(2))
            .journal_path()
            .exists()
    );

    // `conversation-1` names conversation-001.
    driver.send(resume("conversation-1")).await;
    let resumed = driver
        .until(|event| matches!(event, AppEvent::ConversationResumed { .. }))
        .await;
    let AppEvent::ConversationResumed {
        conversation,
        title,
        worker,
        earlier,
        turns,
        artifacts,
    } = resumed
    else {
        unreachable!()
    };
    assert_eq!(conversation, ConversationId::numbered(1));
    assert_eq!(title, "hi");
    assert_eq!(worker, Some("claude-code".parse().unwrap()));
    assert!(earlier.is_empty());
    // The page it published comes back with it, for /artifacts and the key that opens it.
    let pages: Vec<(&str, &str)> = artifacts
        .iter()
        .map(|page| (page.name.as_str(), page.url.as_str()))
        .collect();
    assert_eq!(pages, [("weekly-usage-chart", page)]);
    assert_eq!(
        texts(&turns),
        vec![(true, "hi".to_owned()), (false, "Hi.".to_owned())]
    );
    let status = driver
        .until(|event| matches!(event, AppEvent::Status { .. }))
        .await;
    assert!(
        matches!(&status, AppEvent::Status { status } if status.title == "hi"),
        "{status:?}"
    );

    turn(&mut driver, "and again").await;
    let specs = driver.agent.specs();
    assert_eq!(specs[2].resume.as_deref(), Some("v-1"));
    // The resumed conversation goes on numbering its turns and keeps one journal.
    let events = ConversationStore::open(&conversation_dir, &ConversationId::numbered(1))
        .events()
        .unwrap();
    let started: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            ConversationEvent::TurnStarted { turn, text, .. } => Some(format!("{turn} {text}")),
            _ => None,
        })
        .collect();
    assert_eq!(started, vec!["t-001 hi", "t-002 and again"]);
    driver.exit().await;
}

#[tokio::test]
async fn the_resume_picker_lists_conversations_with_runs() {
    let mut driver = TestDriver::start().await;
    driver.agent.push(answer("Hi."));
    turn(&mut driver, "what does export do?").await;
    driver
        .send(slash(SlashCommand::Resume { target: None }))
        .await;
    let view = driver
        .until(|event| matches!(event, AppEvent::View { data } if matches!(data.as_ref(), ViewData::Runs { .. })))
        .await;
    let AppEvent::View { data } = view else {
        unreachable!()
    };
    let ViewData::Runs { conversations, .. } = *data else {
        unreachable!()
    };
    assert_eq!(conversations.len(), 1);
    assert_eq!(conversations[0].conversation, ConversationId::numbered(1));
    assert_eq!(conversations[0].title, "what does export do?");
    assert_eq!(conversations[0].turns, 1);
    driver.exit().await;
}

#[tokio::test]
async fn a_long_conversation_prints_its_last_turns_and_folds_the_rest() {
    let mut driver = TestDriver::start().await;
    for n in 1..=5 {
        driver.agent.push(answer(&format!("answer {n}")));
    }
    for n in 1..=5 {
        turn(&mut driver, &format!("question {n}")).await;
    }
    driver.send(slash(SlashCommand::Clear)).await;
    notice(&mut driver, "Started a new conversation").await;
    driver.send(resume("conversation-001")).await;
    let AppEvent::ConversationResumed { earlier, turns, .. } = driver
        .until(|event| matches!(event, AppEvent::ConversationResumed { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(earlier.len(), 4);
    assert_eq!(turns.len(), 6);
    assert_eq!(turns[0].text, "question 3");
    assert_eq!(turns[5].text, "answer 5");
    driver.exit().await;
}

#[tokio::test]
async fn another_agent_starts_fresh_with_the_conversation_so_far() {
    let mut driver = TestDriver::start().await;
    driver.agent.push(answer("Use a middleware."));
    driver.agent.push(answer("Nine files."));
    // Codex isn't here to resume its session, and Claude Code can't resume another CLI's.
    written_by(&driver, 7, "codex", "codex-1");
    driver.send(resume("conversation-007")).await;
    driver
        .until(|event| matches!(event, AppEvent::ConversationResumed { .. }))
        .await;
    turn(&mut driver, "how many files?").await;
    let spec = &driver.agent.specs()[0];
    assert_eq!(spec.resume, None);
    let primer = spec
        .prompt
        .iter()
        .find(|part| part.label == "conversation so far")
        .expect("the conversation so far primes the session");
    assert!(
        primer.text.contains("With a token bucket."),
        "{}",
        primer.text
    );
    driver.exit().await;
}

/// A conversation of one turn, whose session `worker` wrote as `cli_session`.
fn written_by(driver: &TestDriver, number: u32, worker: &str, cli_session: &str) {
    let old = ConversationStore::open(
        &driver.env.conversation_dir(),
        &ConversationId::numbered(number),
    );
    old.append(&ConversationEvent::TurnStarted {
        turn: argonauts_core::names::TurnId::numbered(1),
        text: "how do we add rate limiting?".to_owned(),
        worker: worker.parse().unwrap(),
        read_only: true,
        commit: "abc".to_owned(),
        uncommitted: Vec::new(),
        settings_sha256: None,
        continues: false,
    })
    .unwrap();
    old.append(&ConversationEvent::TurnFinished {
        turn: argonauts_core::names::TurnId::numbered(1),
        reply: argonauts_core::app::TurnReply::Answer {
            markdown: "With a token bucket.".to_owned(),
        },
        spend: argonauts_core::Spend::default(),
        elapsed_ms: 5,
    })
    .unwrap();
    old.save_session(&ConversationSession {
        worker: worker.parse().unwrap(),
        cli_session: cli_session.to_owned(),
        fork: false,
    })
    .unwrap();
}

#[tokio::test]
async fn a_conversation_another_cli_wrote_carries_on_in_that_cli() {
    let mut driver = TestDriver::start().await;
    let codex = driver.directory.add(FakeRunner::cli("codex", "openai"));
    codex.push(answer("Nine files.").with_cli_session("codex-1"));
    written_by(&driver, 7, "codex", "codex-1");
    driver.send(resume("conversation-007")).await;
    driver
        .until(|event| matches!(event, AppEvent::ConversationResumed { .. }))
        .await;
    let said = notice(&mut driver, "wrote this conversation").await;
    assert_eq!(
        said,
        "Codex wrote this conversation, so it is the main agent now, as /model would make it, \
         and your next message goes to its session through its own resume."
    );

    turn(&mut driver, "how many files?").await;
    assert!(driver.agent.specs().is_empty());
    let spec = &codex.specs()[0];
    assert_eq!(spec.resume.as_deref(), Some("codex-1"));
    assert!(
        spec.prompt
            .iter()
            .all(|part| part.label != "conversation so far"),
        "{:?}",
        spec.prompt
    );
    driver.exit().await;
}

#[tokio::test]
async fn branching_from_a_conversation_carries_on_a_copy_of_its_session() {
    let mut driver = TestDriver::start_with(CliOptions {
        flags: vec![r#"crew.main="codex""#.to_owned()],
        ..CliOptions::default()
    })
    .await;
    let codex = FakeRunner::cli("codex", "openai");
    let mut capabilities = codex.capabilities();
    capabilities.fork = true;
    let codex = driver.directory.add(codex.with_capabilities(capabilities));
    codex.push(answer("Nine files.").with_cli_session("the-copy"));
    written_by(&driver, 7, "codex", "codex-1");
    driver
        .send(slash(SlashCommand::Resume {
            target: Some(ResumeTarget::BranchConversation(ConversationId::numbered(
                7,
            ))),
        }))
        .await;
    let said = notice(&mut driver, "Branched from").await;
    assert_eq!(
        said,
        "Branched from conversation-007 into conversation-001. Your next message carries on a \
         copy of its session, through codex's own fork. /resume conversation-007 goes back to \
         the original."
    );

    turn(&mut driver, "how many files?").await;
    let spec = &codex.specs()[0];
    assert_eq!(spec.resume.as_deref(), Some("codex-1"));
    assert!(spec.fork);
    assert!(
        spec.prompt
            .iter()
            .all(|part| part.label != "conversation so far"),
        "{:?}",
        spec.prompt
    );
    let conversation_dir = driver.env.conversation_dir();
    let session = |number| {
        let store = ConversationStore::open(&conversation_dir, &ConversationId::numbered(number));
        let session = store.session().unwrap();
        (session.cli_session, session.fork)
    };
    assert_eq!(session(7), ("codex-1".to_owned(), false), "the original");
    assert_eq!(session(1), ("the-copy".to_owned(), false), "the branch");
    driver.exit().await;
}

/// A CLI that says a copy has the original's id is not believed: the branch goes on copying.
#[tokio::test]
async fn a_copy_that_names_the_original_never_becomes_the_conversations_session() {
    let mut driver = TestDriver::start_with(CliOptions {
        flags: vec![r#"crew.main="codex""#.to_owned()],
        ..CliOptions::default()
    })
    .await;
    let codex = FakeRunner::cli("codex", "openai");
    let mut capabilities = codex.capabilities();
    capabilities.fork = true;
    let codex = driver.directory.add(codex.with_capabilities(capabilities));
    codex.push(answer("Nine files.").with_cli_session("codex-1"));
    codex.push(answer("Still nine.").with_cli_session("the-copy"));
    written_by(&driver, 7, "codex", "codex-1");
    driver
        .send(slash(SlashCommand::Resume {
            target: Some(ResumeTarget::BranchConversation(ConversationId::numbered(
                7,
            ))),
        }))
        .await;
    notice(&mut driver, "Branched from").await;
    turn(&mut driver, "how many files?").await;
    turn(&mut driver, "and now?").await;
    let specs = codex.specs();
    assert_eq!(specs[1].resume.as_deref(), Some("codex-1"));
    assert!(specs[1].fork, "the second turn copies it again");
    driver.exit().await;
}

#[tokio::test]
async fn a_conversation_whose_cli_is_gone_says_who_takes_it() {
    let mut driver = TestDriver::start().await;
    driver.agent.push(answer("Nine files."));
    written_by(&driver, 7, "codex", "codex-1");
    driver.send(resume("conversation-007")).await;
    let said = notice(&mut driver, "wrote this conversation").await;
    assert_eq!(
        said,
        "Codex wrote this conversation and can't carry its session on here, so claude-code \
         takes the next message, primed with its last turns."
    );
    driver.exit().await;
}

#[tokio::test]
async fn a_agent_that_cannot_resume_gets_a_fallback_note() {
    let mut driver = TestDriver::start_with(CliOptions {
        flags: vec![r#"crew.main="codex""#.to_owned()],
        ..CliOptions::default()
    })
    .await;
    let codex = FakeRunner::cli("codex", "openai");
    let mut capabilities = codex.capabilities();
    capabilities.resume = false;
    let codex = driver.directory.add(codex.with_capabilities(capabilities));
    codex.push(answer("one").with_cli_session("c-1"));
    codex.push(answer("two").with_cli_session("c-1"));
    turn(&mut driver, "first").await;
    turn(&mut driver, "second").await;
    let specs = codex.specs();
    assert_eq!(specs[1].resume, None);
    assert!(
        specs[1]
            .prompt
            .iter()
            .any(|part| part.label == "conversation so far")
    );
    driver.exit().await;
}

#[tokio::test]
async fn resuming_a_missing_conversation_says_so() {
    let mut driver = TestDriver::start().await;
    driver.send(resume("conversation-042")).await;
    let said = notice(&mut driver, "conversation-042").await;
    assert!(said.contains("no conversation"), "{said}");
    driver.exit().await;
}

#[tokio::test]
async fn continue_opens_the_newest_conversation() {
    let opening = Opening {
        guided_setup: false,
        first: FirstView::Continue,
    };
    let mut driver = TestDriver::start_with(CliOptions {
        opening,
        prepare: Some(Box::new(|repo| {
            let env = AppEnv::for_repo(Paths::new(repo.home()), repo.root(), true).unwrap();
            for (n, text) in [(1, "older"), (2, "newest")] {
                let store =
                    ConversationStore::open(&env.conversation_dir(), &ConversationId::numbered(n));
                store
                    .append(&ConversationEvent::TurnStarted {
                        turn: argonauts_core::names::TurnId::numbered(1),
                        text: text.to_owned(),
                        worker: "claude-code".parse().unwrap(),
                        read_only: true,
                        commit: "abc".to_owned(),
                        uncommitted: Vec::new(),
                        settings_sha256: None,
                        continues: false,
                    })
                    .unwrap();
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        })),
        ..CliOptions::default()
    })
    .await;
    let AppEvent::ConversationResumed {
        conversation,
        title,
        ..
    } = driver
        .until(|event| matches!(event, AppEvent::ConversationResumed { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(conversation, ConversationId::numbered(2));
    assert_eq!(title, "newest");
    driver.exit().await;
}

/// Two app sessions opened in the same repo before either writes a thing get a conversation
/// each, so a run one of them opens is never kept with the other's. One left unused gives its
/// number back when the app closes.
#[tokio::test]
async fn app_sessions_opened_together_never_share_a_conversation() {
    let (mut parts, _) = TestCtx::new(CliOptions::default());
    let mut first = ConversationState::new(&parts.ctx).unwrap();
    let second = ConversationState::new(&parts.ctx).unwrap();
    assert_ne!(first.id, second.id);

    super::close(&mut parts.ctx, &mut first);
    assert!(!first.store.journal_path().exists());
    assert!(second.store.journal_path().exists());
}
