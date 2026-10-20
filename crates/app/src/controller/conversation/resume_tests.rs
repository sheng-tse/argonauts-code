//! `/clear`, `/resume conversation-<n>`, the picker's conversations, and `argonauts -c`, through
//! the real controller loop.

use argonauts_core::app::{AppEvent, AppMode, ConversationEvent, UiCommand, ViewData, ViewRequest};
use argonauts_core::event::AgentEvent;
use argonauts_core::events::context::ContextUsage;
use argonauts_core::input::{ResumeTarget, SlashCommand};
use argonauts_core::names::ConversationId;
use argonauts_core::session_runner::{RunnerEvent, SessionRunner as _};
use argonauts_core::{Paths, Spend};
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

/// Waits for `/clear` to start the screen over, and returns the way back it names.
async fn cleared(driver: &mut TestDriver) -> Option<ConversationId> {
    match driver
        .until(|event| matches!(event, AppEvent::ConversationCleared { .. }))
        .await
    {
        AppEvent::ConversationCleared { way_back } => way_back,
        _ => unreachable!(),
    }
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
    let way_back = cleared(&mut driver).await;
    assert_eq!(
        way_back.as_ref().map(ToString::to_string).as_deref(),
        Some("conversation-001")
    );
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
        turns,
        artifacts,
    } = resumed
    else {
        unreachable!()
    };
    assert_eq!(conversation, ConversationId::numbered(1));
    assert_eq!(title, "hi");
    assert_eq!(worker, Some("claude-code".parse().unwrap()));
    // Its own turns come from its record, drawn by the printers that drew them.
    assert!(turns.is_empty());
    // The page it published comes back with it, for /artifacts and the key that opens it.
    let pages: Vec<(&str, &str)> = artifacts
        .iter()
        .map(|page| (page.name.as_str(), page.url.as_str()))
        .collect();
    assert_eq!(pages, [("weekly-usage-chart", page)]);
    let drawn = driver
        .until(|event| matches!(event, AppEvent::PastDrawn))
        .await;
    assert_eq!(drawn, AppEvent::PastDrawn);
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
        .until(|event| matches!(event, AppEvent::View { data } if matches!(data.as_ref(), ViewData::ResumeList { .. })))
        .await;
    let AppEvent::View { data } = view else {
        unreachable!()
    };
    let ViewData::ResumeList { conversations, .. } = *data else {
        unreachable!()
    };
    assert_eq!(conversations.len(), 1);
    assert_eq!(conversations[0].conversation, ConversationId::numbered(1));
    assert_eq!(conversations[0].title, "what does export do?");
    assert_eq!(conversations[0].turns, 1);
    driver.exit().await;
}

/// A run comes back after the turn that started it, drawn from its own record, and the turns
/// after it follow. The run is the conversation's own again, and nothing else lists it.
#[tokio::test]
async fn a_resumed_conversation_draws_its_run_after_the_turn_that_started_it() {
    use argonauts_core::RunId;
    use argonauts_core::events::RunEventKind;
    use argonauts_core::events::run::{Party, RunLifecycle};
    use argonauts_core::layout::run_files;
    use argonauts_core::names::TurnId;

    use crate::client::test_support as log;
    use crate::conversation_store::ConversationRun;

    let mut driver = TestDriver::start().await;
    driver.agent.push(answer("Starting it."));
    driver.agent.push(answer("Still going."));
    turn(&mut driver, "make the limiter per key").await;
    turn(&mut driver, "how is it going?").await;
    let run = RunId::new("run-01").unwrap();
    let mut started = log::run_started(0, "make the limiter per key", None);
    if let RunEventKind::Run(RunLifecycle::Started(started)) = &mut started.kind {
        started.conversation_turn = Some(TurnId::numbered(1));
    }
    let dir = driver.env.run_dir(&run);
    std::fs::create_dir_all(&dir).unwrap();
    let mut events = [started, log::queued(1, "keep the old header", Party::User)];
    for event in &mut events {
        event.run_id = run.clone();
    }
    log::write_journal(&dir.join(run_files::EVENTS), &events);
    let conversation_dir = driver.env.conversation_dir();
    ConversationStore::open(&conversation_dir, &ConversationId::numbered(1))
        .keep_run(&ConversationRun {
            run: run.clone(),
            title: "make the limiter per key".to_owned(),
        })
        .unwrap();
    driver.send(slash(SlashCommand::Clear)).await;
    cleared(&mut driver).await;
    let mark = driver.seen().len();
    driver.send(resume("conversation-001")).await;
    driver
        .until(|event| matches!(event, AppEvent::PastDrawn))
        .await;
    let order: Vec<String> = driver.seen()[mark..]
        .iter()
        .skip_while(|event| !matches!(event, AppEvent::PastBegins))
        .take_while(|event| !matches!(event, AppEvent::PastDrawn))
        .filter_map(|event| match event {
            AppEvent::Conversation {
                event: ConversationEvent::TurnFinished { turn, .. },
            } => Some(format!("{turn} finished")),
            AppEvent::Run { event } => Some(format!("run event {}", event.seq)),
            _ => None,
        })
        .collect();
    assert_eq!(
        order,
        [
            "t-001 finished",
            "run event 0",
            "run event 1",
            "t-002 finished"
        ]
    );
    driver.exit().await;
}

/// A resumed conversation comes back whole, in order, between the past's two marks, with no
/// turn cut and nothing summed up.
#[tokio::test]
async fn a_long_conversation_is_drawn_whole_from_its_record() {
    let mut driver = TestDriver::start().await;
    for n in 1..=5 {
        driver.agent.push(answer(&format!("answer {n}")));
    }
    for n in 1..=5 {
        turn(&mut driver, &format!("question {n}")).await;
    }
    driver.send(slash(SlashCommand::Clear)).await;
    cleared(&mut driver).await;
    let mark = driver.seen().len();
    driver.send(resume("conversation-001")).await;
    driver
        .until(|event| matches!(event, AppEvent::PastDrawn))
        .await;
    let past: Vec<String> = driver.seen()[mark..]
        .iter()
        .skip_while(|event| !matches!(event, AppEvent::PastBegins))
        .take_while(|event| !matches!(event, AppEvent::PastDrawn))
        .filter_map(|event| match event {
            AppEvent::Conversation {
                event: ConversationEvent::TurnStarted { text, .. },
            } => Some(text.clone()),
            AppEvent::Conversation {
                event: ConversationEvent::TurnFinished { reply, .. },
            } => Some(format!("{reply:?}")),
            _ => None,
        })
        .collect();
    assert_eq!(past.len(), 10, "{past:#?}");
    assert_eq!(past[0], "question 1");
    assert!(past[9].contains("answer 5"), "{past:#?}");
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

/// Each turn keeps how it left its session's context in the record as it ends, the CLI's own
/// count with it, so the conversation resumed counts its session at once, before a turn runs.
#[tokio::test]
async fn a_resumed_conversation_counts_its_context_at_once() {
    let usage: ContextUsage = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adapters/claude-code/tests/data/context-usage-full.usage.json"
    )))
    .unwrap();
    let mut driver = TestDriver::start().await;
    let events = [
        AgentEvent::ToolStarted {
            id: "t1".into(),
            name: "Read".into(),
            input: json!({"file_path": "README"}),
            parent: None,
        },
        AgentEvent::ContextFill {
            used: 700_000,
            size: Some(1_000_000),
            sent: None,
        },
        AgentEvent::ContextBreakdown {
            usage: Box::new(usage.clone()),
        },
    ];
    let spend = Spend {
        input_tokens: 1_200,
        output_tokens: 80,
        cached_tokens: 0,
        cost_usd: 0.02,
        cost_partial: false,
        cost_estimated: false,
    };
    let hi = events
        .into_iter()
        .fold(answer("Hi."), |session, event| {
            session.with_event(RunnerEvent::Agent(event))
        })
        .with_spend(spend)
        .with_cli_session("v-1");
    driver.agent.push(hi);
    turn(&mut driver, "hi").await;
    let store =
        ConversationStore::open(&driver.env.conversation_dir(), &ConversationId::numbered(1));
    let kept: Vec<(u64, Option<Box<ContextUsage>>)> = store
        .every_event()
        .into_iter()
        .filter_map(|event| match event {
            ConversationEvent::ContextLeft { used, usage, .. } => Some((used, usage)),
            _ => None,
        })
        .collect();
    assert_eq!(
        kept,
        vec![(usage.total_tokens, Some(Box::new(usage.clone())))]
    );

    driver.send(slash(SlashCommand::Clear)).await;
    cleared(&mut driver).await;
    driver.send(resume("conversation-1")).await;
    driver
        .until(|event| matches!(event, AppEvent::PastDrawn))
        .await;
    driver
        .send(UiCommand::Request {
            view: ViewRequest::Context,
        })
        .await;
    let shown = driver
        .until(|event| matches!(event, AppEvent::View { data } if matches!(**data, ViewData::Context { .. })))
        .await;
    let AppEvent::View { data } = shown else {
        unreachable!()
    };
    let ViewData::Context { view } = *data else {
        unreachable!()
    };
    let row = &view.rows[0];
    assert_eq!(
        (row.used, row.window, row.tool_calls),
        (usage.total_tokens, Some(1_000_000), 1)
    );
    assert_eq!(row.spent, spend);
    assert_eq!(row.recorded.map(|recorded| recorded.turns), Some(1));
}

/// A slash command you typed is kept in the record where you typed it, with what it printed
/// back after it, and the conversation drawn again brings both back there, after the turn
/// before them.
#[tokio::test]
async fn a_typed_command_comes_back_where_it_was_typed() {
    let mut driver = TestDriver::start().await;
    driver.agent.push(answer("Hi.").with_cli_session("v-1"));
    turn(&mut driver, "hi").await;
    driver
        .send(UiCommand::CommandTyped {
            text: " /usage week ".to_owned(),
        })
        .await;
    let reply = argonauts_core::app::CommandReply {
        text: "Usage dialog dismissed".to_owned(),
        value: None,
        after: None,
        dim: true,
        error: false,
    };
    driver
        .send(UiCommand::CommandReplied {
            reply: reply.clone(),
        })
        .await;
    driver.send(slash(SlashCommand::Clear)).await;
    cleared(&mut driver).await;
    let mark = driver.seen().len();
    driver.send(resume("conversation-1")).await;
    driver
        .until(|event| matches!(event, AppEvent::PastDrawn))
        .await;
    let past: Vec<&ConversationEvent> = driver.seen()[mark..]
        .iter()
        .skip_while(|event| !matches!(event, AppEvent::PastBegins))
        .take_while(|event| !matches!(event, AppEvent::PastDrawn))
        .filter_map(|event| match event {
            AppEvent::Conversation { event } => Some(event),
            _ => None,
        })
        .collect();
    let typed = past
        .iter()
        .position(|event| {
            matches!(event, ConversationEvent::CommandTyped { text, .. } if text == "/usage week")
        })
        .expect("the command comes back");
    let finished = past
        .iter()
        .position(|event| matches!(event, ConversationEvent::TurnFinished { .. }))
        .unwrap();
    assert!(finished < typed, "{past:#?}");
    assert!(
        matches!(past.get(typed + 1), Some(ConversationEvent::CommandReplied { reply: kept }) if *kept == reply),
        "what it printed back comes right after it: {past:#?}"
    );
}
