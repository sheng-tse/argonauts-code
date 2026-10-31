//! A session carried on in its CLI's own screen, through the real controller loop and the real
//! adapters reading recorded session stores: what `/resume` says to type there, `o`, and the
//! turns made there read back.

use std::fs;
use std::path::Path;
use std::time::SystemTime;

use argonauts_core::Spend;
use argonauts_core::agents::cli_sessions::{CliSessionRef, InItsCli};
use argonauts_core::app::{AppEvent, ConversationEvent, TurnReply, UiCommand, ViewData};
use argonauts_core::base::names::{AgentId, ConversationId, TurnId};
use argonauts_core::input::{ResumeTarget, SlashCommand};
use argonauts_core::runs::events::run::Party;
use argonauts_testkit::FakeSession;
use serde_json::json;

use super::cli::install_stores;
use crate::controller::base::test_support::TestDriver;
use crate::conversation::store::{ConversationSession, ConversationStore};

const LOOP: &str = "0a1b2c3d-1111-4aaa-8bbb-000000000001";
const EXPORT: &str = "0a1b2c3d-2222-4aaa-8bbb-000000000002";

/// Between the recorded session's two prompts, at 10:01 and 10:03.
const BETWEEN_THE_PROMPTS: u64 = 1_790_589_720;

async fn start() -> TestDriver {
    let driver = TestDriver::start().await;
    install_stores(
        driver.env.user_home.as_deref().unwrap(),
        &driver.env.repo_root,
    );
    driver
}

fn loop_session() -> CliSessionRef {
    CliSessionRef {
        agent_id: AgentId("claude-code".into()),
        id: LOOP.into(),
    }
}

/// An earlier conversation that carried on Claude Code's session `LOOP`, last used between its
/// two prompts, so the second was made in Claude Code itself.
fn a_conversation_that_left_after_the_first_prompt(driver: &TestDriver) -> ConversationId {
    let id = ConversationId::numbered(7);
    let store = ConversationStore::open(&driver.env.conversation_dir(), &id);
    store
        .append(&ConversationEvent::TurnStarted {
            turn: TurnId::numbered(1),
            text: "How does the input loop read keys?".to_owned(),
            worker: "claude-code".parse().unwrap(),
            read_only: true,
            commit: "abc".to_owned(),
            uncommitted: Vec::new(),
            settings_sha256: None,
            continues: false,
        })
        .unwrap();
    store
        .append(&ConversationEvent::TurnFinished {
            turn: TurnId::numbered(1),
            reply: TurnReply::Answer {
                markdown: "It reads keys in one loop.".to_owned(),
            },
            spend: Spend::default(),
            elapsed_ms: 10,
        })
        .unwrap();
    store
        .save_session(&ConversationSession {
            worker: "claude-code".parse().unwrap(),
            cli_session: LOOP.to_owned(),
            fork: false,
        })
        .unwrap();
    left_at(store.journal_path(), BETWEEN_THE_PROMPTS);
    id
}

fn left_at(journal: &Path, unix: u64) {
    let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(unix);
    fs::File::options()
        .write(true)
        .open(journal)
        .unwrap()
        .set_modified(at)
        .unwrap();
}

#[tokio::test]
async fn a_conversations_session_is_its_row_alone_with_what_to_type_in_its_cli() {
    let mut driver = start().await;
    let conversation = a_conversation_that_left_after_the_first_prompt(&driver);
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Resume { target: None },
        })
        .await;
    let AppEvent::View { data } = driver
        .until(|event| matches!(event, AppEvent::View { data } if matches!(data.as_ref(), ViewData::ResumeList { .. })))
        .await
    else {
        unreachable!()
    };
    let ViewData::ResumeList {
        sessions, in_cli, ..
    } = *data
    else {
        unreachable!()
    };
    // The session the conversation carries is listed as the conversation, never again alone.
    assert!(sessions.iter().all(|session| session.id != LOOP));
    assert!(sessions.iter().any(|session| session.id == EXPORT));
    let row = in_cli
        .iter()
        .find(|row| row.conversation.as_ref() == Some(&conversation))
        .expect("the conversation's row");
    assert_eq!(
        *row,
        InItsCli {
            session: loop_session(),
            conversation: Some(conversation),
            cli: "Claude Code".to_owned(),
            command: format!("claude --resume {LOOP}"),
            local_model: false,
            also: None,
            made_there: 1,
        }
    );
    let own = in_cli
        .iter()
        .find(|row| row.conversation.is_none() && row.session.id == EXPORT)
        .expect("Claude Code's own session");
    assert_eq!(own.command, format!("claude --resume {EXPORT}"));
    assert_eq!(own.made_there, 0);
    driver.exit().await;
}

#[tokio::test]
async fn resuming_a_conversation_draws_the_turns_made_in_its_cli_since_you_left() {
    let mut driver = start().await;
    let conversation = a_conversation_that_left_after_the_first_prompt(&driver);
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Resume {
                target: Some(ResumeTarget::Conversation(conversation)),
            },
        })
        .await;
    let AppEvent::TurnsMadeInCli { said, turns } = driver
        .until(|event| matches!(event, AppEvent::TurnsMadeInCli { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(said, "1 turn made in Claude Code since you left");
    assert_eq!(turns[0].from, Party::User);
    assert_eq!(turns[0].text, "And the paste handling?");
    assert_eq!(turns.len(), 2, "the prompt and its answer");
    driver.exit().await;
}

#[tokio::test]
async fn o_opens_the_conversation_in_its_cli_and_draws_what_was_made_there() {
    let mut driver = start().await;
    let conversation = a_conversation_that_left_after_the_first_prompt(&driver);
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Resume {
                target: Some(ResumeTarget::Conversation(conversation.clone())),
            },
        })
        .await;
    driver
        .until(|event| matches!(event, AppEvent::TurnsMadeInCli { .. }))
        .await;
    // Back on screen it is the conversation's own, and o hands the terminal to Claude Code.
    driver
        .send(UiCommand::OpenInItsCli {
            session: loop_session(),
            conversation: Some(conversation),
        })
        .await;
    let AppEvent::RunInTerminal {
        argv, cwd, quiet, ..
    } = driver
        .until(|event| matches!(event, AppEvent::RunInTerminal { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(argv, ["fake-claude-code", "resume", LOOP]);
    assert_eq!(cwd, driver.env.repo_root);
    assert!(quiet, "the app says what came back");
    driver.send(UiCommand::TerminalReturned { argv }).await;
    let AppEvent::TurnsMadeInCli { said, turns } = driver
        .until(|event| matches!(event, AppEvent::TurnsMadeInCli { .. }))
        .await
    else {
        unreachable!()
    };
    // Its journal still says it left between the prompts.
    assert_eq!(said, "Back from Claude Code · 1 turn made there");
    assert_eq!(turns[0].text, "And the paste handling?");
    driver.exit().await;
}

/// `o` twice on a council seat: the seat sits out the talk, its session opens in its CLI's own
/// screen, and when that CLI quits the seat goes back to the talk rather than the conversation
/// taking the session on.
#[tokio::test]
async fn a_council_seat_carried_on_in_its_cli_goes_back_to_the_talk() {
    use argonauts_core::RunId;
    use argonauts_core::base::names::SeatId;
    use argonauts_core::runs::events::council::CouncilAction;

    use crate::controller::base::test_support::Call;

    let mut driver = start().await;
    let run = RunId::new("run-001").unwrap();
    let seat = SeatId::numbered(2);
    driver
        .send(UiCommand::CarryOnSeat {
            run: run.clone(),
            seat: seat.clone(),
            session: loop_session(),
        })
        .await;
    let AppEvent::RunInTerminal { argv, .. } = driver
        .until(|event| matches!(event, AppEvent::RunInTerminal { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(argv, ["fake-claude-code", "resume", LOOP]);
    let sat_out = Call::Council {
        run: run.clone(),
        action: CouncilAction::CarryOn { seat: seat.clone() },
    };
    assert!(driver.conductor.calls().contains(&sat_out));
    // You type to the seat in Claude Code, which keeps it in the session's journal.
    typed_in_claude_code(&driver, "Keep the window until March.");
    driver.send(UiCommand::TerminalReturned { argv }).await;
    let back = Call::Council {
        run,
        action: CouncilAction::Rejoin {
            seat,
            said: vec!["Keep the window until March.".to_owned()],
        },
    };
    let deadline = tokio::time::Instant::now() + crate::controller::base::test_support::max_wait();
    while !driver.conductor.calls().contains(&back) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:#?}",
            driver.conductor.calls()
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    driver.exit().await;
}

/// `text` sent now in Claude Code's own screen on the recorded session `LOOP`.
fn typed_in_claude_code(driver: &TestDriver, text: &str) {
    let repo = driver.env.repo_root.to_str().unwrap();
    let folder: String = repo
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let journal = driver
        .env
        .user_home
        .as_deref()
        .unwrap()
        .join(".claude/projects")
        .join(folder)
        .join(format!("{LOOP}.jsonl"));
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let line = json!({
        "parentUuid": null,
        "isSidechain": false,
        "userType": "external",
        "cwd": repo,
        "sessionId": LOOP,
        "version": "2.1.285",
        "gitBranch": "main",
        "entrypoint": "cli",
        "type": "user",
        "message": { "role": "user", "content": text },
        "uuid": "u-typed",
        "timestamp": now,
    });
    let mut kept = fs::read_to_string(&journal).unwrap();
    kept.push_str(&format!("{line}\n"));
    fs::write(&journal, kept).unwrap();
}

#[tokio::test]
async fn o_on_a_session_no_conversation_holds_says_so() {
    let mut driver = start().await;
    driver
        .send(UiCommand::OpenInItsCli {
            session: loop_session(),
            conversation: Some(ConversationId::numbered(9)),
        })
        .await;
    let AppEvent::Notice { text, .. } = driver
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains("to open")))
        .await
    else {
        unreachable!()
    };
    assert_eq!(
        text,
        format!("Claude Code has no session claude-code:{LOOP} to open. /resume lists them.")
    );
    driver.exit().await;
}

#[tokio::test]
async fn the_lines_left_on_quit_and_the_status_tab_say_how_to_carry_it_on_in_claude_code() {
    let mut driver = start().await;
    driver.agent.push(
        FakeSession::structured(
            json!({"kind": "answer", "answer_markdown": "It reads keys.", "estimate": null}),
        )
        .with_cli_session(LOOP),
    );
    driver.submit("How does the input loop read keys?").await;
    let AppEvent::Status { status } = driver
        .until(|event| matches!(event, AppEvent::Status { status } if status.line.in_cli.is_some()))
        .await
    else {
        unreachable!()
    };
    let conversation = status
        .line
        .conversation
        .clone()
        .expect("the conversation on screen");
    let in_cli = status.line.in_cli.clone().unwrap();
    assert_eq!(in_cli.command, format!("claude --resume {LOOP}"));
    assert_eq!(in_cli.cli, "Claude Code");
    assert_eq!(in_cli.conversation, Some(conversation));
    driver
        .send(UiCommand::Request {
            view: argonauts_core::app::ViewRequest::StatusTab,
        })
        .await;
    let AppEvent::View { data } = driver
        .until(|event| matches!(event, AppEvent::View { data } if matches!(data.as_ref(), ViewData::StatusTab { .. })))
        .await
    else {
        unreachable!()
    };
    let ViewData::StatusTab { view } = *data else {
        unreachable!()
    };
    assert_eq!(view.in_cli, Some(in_cli));
    driver.exit().await;
}

#[tokio::test]
async fn a_sessions_id_alone_is_found_where_it_carries_on() {
    use crate::FoundSession;

    let driver = start().await;
    let conversation = a_conversation_that_left_after_the_first_prompt(&driver);
    let settings =
        crate::startup::app_settings::AppSettings::load(&driver.env, Vec::new(), false).unwrap();
    let find = |id: &str| {
        crate::controller::conversation::in_cli::find_any(&driver.env, settings.resolved(), id)
    };
    // The conversation that carries a session is where it carries on, by its id or its start.
    assert_eq!(
        find(LOOP).unwrap(),
        FoundSession::Conversation(conversation.clone())
    );
    assert_eq!(
        find("0a1b2c3d-1111").unwrap(),
        FoundSession::Conversation(conversation)
    );
    // A CLI's own session is found in its list.
    assert_eq!(
        find(EXPORT).unwrap(),
        FoundSession::CliSession(CliSessionRef {
            agent_id: AgentId("claude-code".into()),
            id: EXPORT.into(),
        })
    );
    // An id no list has could be agy's, which keeps none.
    let FoundSession::Unlisted(unlisted) = find("055a398f").unwrap() else {
        panic!("no list has it");
    };
    assert_eq!(
        unlisted,
        [(
            "agy".to_owned(),
            CliSessionRef {
                agent_id: AgentId("antigravity-cli".into()),
                id: "055a398f".into(),
            }
        )]
    );
    assert!(find("--last").is_err());
    driver.exit().await;
}

#[tokio::test]
async fn an_agy_conversation_carries_on_by_the_id_you_give() {
    let mut driver = start().await;
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Resume {
                target: Some(ResumeTarget::CliSession(CliSessionRef {
                    agent_id: AgentId("antigravity-cli".into()),
                    id: "055a398f".into(),
                })),
            },
        })
        .await;
    let AppEvent::ConversationResumed { title, turns, .. } = driver
        .until(|event| matches!(event, AppEvent::ConversationResumed { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(title, "agy conversation 055a398f");
    assert!(turns.is_empty(), "agy's turns can't be read back");
    driver.exit().await;
}

#[tokio::test]
async fn the_main_session_is_named_by_the_conversations_title_on_every_turn() {
    let mut driver = start().await;
    for (n, words) in [
        "How does the input loop read keys?",
        "And the paste handling?",
    ]
    .into_iter()
    .enumerate()
    {
        driver.agent.push(
            FakeSession::structured(
                json!({"kind": "answer", "answer_markdown": "It reads keys.", "estimate": null}),
            )
            .with_cli_session(LOOP),
        );
        driver.submit(words).await;
        driver
            .until(|event| {
                matches!(
                    event,
                    AppEvent::Conversation {
                        event: ConversationEvent::TurnFinished { .. }
                    }
                )
            })
            .await;
        let specs = driver.agent.specs();
        assert_eq!(specs.len(), n + 1);
        assert_eq!(
            specs[n].preferences.title.as_deref(),
            Some("How does the input loop read keys?"),
            "turn {n}"
        );
    }
    driver.exit().await;
}
