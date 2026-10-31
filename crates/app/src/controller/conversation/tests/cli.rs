//! `/resume` listing the CLIs' own sessions, and carrying one on or branching from it, through the real
//! controller loop and the real adapters reading recorded session stores.

use std::fs;
use std::path::Path;

use argonauts_core::agents::cli_sessions::{CliSessionRef, CliStep};
use argonauts_core::agents::session_runner::SessionRunner as _;
use argonauts_core::app::{AppEvent, AppMode, ConversationEvent, UiCommand, ViewData};
use argonauts_core::base::names::{AgentId, ConversationId};
use argonauts_core::input::{ResumeTarget, SlashCommand};
use argonauts_core::runs::events::run::Party;
use argonauts_testkit::{FakeRunner, FakeSession};
use serde_json::json;

use crate::FirstView;
use crate::controller::Opening;
use crate::controller::base::test_support::{CliOptions, TestDriver};
use crate::conversation::store::ConversationStore;

const LOOP: &str = "0a1b2c3d-1111-4aaa-8bbb-000000000001";
const EXPORT: &str = "0a1b2c3d-2222-4aaa-8bbb-000000000002";
const CODEX_INPUT: &str = "019a5770-e3f9-7f92-ab2b-5ba3b7602e8a";

/// The adapters' recorded stores, moved to `repo` under `home`: `/work/shop` becomes `repo`.
pub(super) fn install_stores(home: &Path, repo: &Path) {
    let adapters = Path::new(env!("CARGO_MANIFEST_DIR")).join("../adapters");
    let repo_text = repo.to_str().unwrap();
    let folder: String = repo_text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    copy_moved(
        &adapters.join("claude-code/session-store/projects/-work-shop"),
        &home.join(".claude/projects").join(folder),
        repo_text,
    );
    copy_moved(
        &adapters.join("codex/session-store"),
        &home.join(".codex"),
        repo_text,
    );
}

fn copy_moved(from: &Path, to: &Path, repo: &str) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_moved(&entry.path(), &target, repo);
        } else {
            let text = fs::read_to_string(entry.path()).unwrap();
            fs::write(target, text.replace("/work/shop", repo)).unwrap();
        }
    }
}

async fn start() -> TestDriver {
    let driver = TestDriver::start().await;
    install_stores(
        driver.env.user_home.as_deref().unwrap(),
        &driver.env.repo_root,
    );
    driver
}

fn resume(agent_id: &str, id: &str) -> UiCommand {
    UiCommand::Slash {
        command: SlashCommand::Resume {
            target: Some(ResumeTarget::CliSession(reference(agent_id, id))),
        },
    }
}

fn reference(agent_id: &str, id: &str) -> CliSessionRef {
    CliSessionRef {
        agent_id: AgentId(agent_id.into()),
        id: id.into(),
    }
}

fn answer(text: &str) -> FakeSession {
    FakeSession::structured(json!({"kind": "answer", "answer_markdown": text, "estimate": null}))
}

async fn notice(driver: &mut TestDriver, part: &str) -> String {
    match driver
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains(part)))
        .await
    {
        AppEvent::Notice { text, .. } => text,
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

#[tokio::test]
async fn the_picker_lists_every_clis_own_sessions_newest_first() {
    let mut driver = start().await;
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
    let ViewData::ResumeList { sessions, .. } = *data else {
        unreachable!()
    };
    // Claude Code is the main agent, and Codex's sessions list beside its own.
    let of = |cli: &str| -> Vec<&str> {
        sessions
            .iter()
            .filter(|session| session.agent_id.0 == cli)
            .map(|session| session.id.as_str())
            .collect()
    };
    assert_eq!(of("claude-code"), vec![EXPORT, LOOP]);
    assert!(of("codex").contains(&CODEX_INPUT), "{:?}", of("codex"));
    assert!(
        sessions
            .windows(2)
            .all(|pair| pair[0].updated >= pair[1].updated)
    );
    let input = sessions.iter().find(|s| s.id == LOOP).unwrap();
    assert_eq!(input.title, "Input loop notes");
    assert_eq!(input.folder, driver.env.repo_root);
    assert_eq!(input.turns, 2);
    driver.exit().await;
}

#[tokio::test]
async fn branching_from_a_session_starts_a_new_one_and_leaves_the_original() {
    let mut driver = start().await;
    driver.agent.push(answer("It debounces."));
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Resume {
                target: Some(ResumeTarget::BranchCliSession(reference(
                    "claude-code",
                    LOOP,
                ))),
            },
        })
        .await;
    let AppEvent::ConversationResumed { title, turns, .. } = driver
        .until(|event| matches!(event, AppEvent::ConversationResumed { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(title, "Input loop notes");
    assert_eq!(turns.len(), 4);
    let said = notice(&mut driver, "Branched from").await;
    assert_eq!(
        said,
        "Branched from Claude Code session 0a1b2c3d, which stays as it was. Your next message \
         starts a new session that has its last turns."
    );

    turn(&mut driver, "and the mouse?").await;
    let spec = &driver.agent.specs()[0];
    // This CLI has no fork of its own. Nothing resumes its session, so nothing is written to
    // it.
    assert_eq!(spec.resume, None);
    assert!(!spec.fork);
    assert!(
        spec.prompt
            .iter()
            .any(|part| part.label == "conversation so far"),
        "{:?}",
        spec.prompt
    );
    driver.exit().await;
}

/// A driver whose main agent is Codex, with a fork of its own.
async fn start_with_a_cli_that_forks() -> (TestDriver, std::sync::Arc<FakeRunner>) {
    let driver = TestDriver::start_with(CliOptions {
        flags: vec![r#"crew.main="codex""#.to_owned()],
        ..CliOptions::default()
    })
    .await;
    install_stores(
        driver.env.user_home.as_deref().unwrap(),
        &driver.env.repo_root,
    );
    let codex = FakeRunner::cli("codex", "openai");
    let mut capabilities = codex.capabilities();
    capabilities.fork = true;
    let codex = driver.directory.add(codex.with_capabilities(capabilities));
    (driver, codex)
}

#[tokio::test]
async fn branching_from_a_session_carries_on_a_copy_where_its_cli_forks() {
    let (mut driver, codex) = start_with_a_cli_that_forks().await;
    codex.push(answer("It debounces.").with_cli_session("the-copy"));
    codex.push(answer("Yes.").with_cli_session("the-copy"));
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Resume {
                target: Some(ResumeTarget::BranchCliSession(reference(
                    "codex",
                    CODEX_INPUT,
                ))),
            },
        })
        .await;
    driver
        .until(|event| matches!(event, AppEvent::ConversationResumed { .. }))
        .await;
    let said = notice(&mut driver, "Branched from").await;
    assert_eq!(
        said,
        "Branched from Codex session 019a5770, which stays as it was. Your next message \
         carries on a copy of it, through codex's own fork."
    );
    // Kept as a session to copy, so the branch still copies it after a restart, and the list
    // still shows the session as Codex's own.
    let conversation_dir = driver.env.conversation_dir();
    let store = ConversationStore::open(&conversation_dir, &ConversationId::numbered(1));
    let kept = store.session().unwrap();
    assert_eq!((kept.cli_session.as_str(), kept.fork), (CODEX_INPUT, true));
    assert!(crate::controller::conversation::crew_cli_sessions(&conversation_dir).is_empty());

    turn(&mut driver, "and the mouse?").await;
    let first = &codex.specs()[0];
    assert_eq!(first.resume.as_deref(), Some(CODEX_INPUT));
    assert!(first.fork, "the first turn copies the session");
    assert!(
        first
            .prompt
            .iter()
            .all(|part| part.label != "conversation so far"),
        "{:?}",
        first.prompt
    );
    // From then on the conversation is the copy.
    let kept = store.session().unwrap();
    assert_eq!((kept.cli_session.as_str(), kept.fork), ("the-copy", false));
    turn(&mut driver, "always?").await;
    let second = &codex.specs()[1];
    assert_eq!(second.resume.as_deref(), Some("the-copy"));
    assert!(!second.fork);
    driver.exit().await;
}

/// Enter on the session a branch is about to copy carries the session itself on.
#[tokio::test]
async fn a_branch_not_yet_used_is_not_the_session_it_copies() {
    let (mut driver, codex) = start_with_a_cli_that_forks().await;
    codex.push(answer("Same loop.").with_cli_session(CODEX_INPUT));
    let branch = UiCommand::Slash {
        command: SlashCommand::Resume {
            target: Some(ResumeTarget::BranchCliSession(reference(
                "codex",
                CODEX_INPUT,
            ))),
        },
    };
    driver.send(branch).await;
    notice(&mut driver, "Branched from").await;
    driver.send(resume("codex", CODEX_INPUT)).await;
    let said = notice(&mut driver, "Carrying on").await;
    assert!(said.contains("through codex's own resume"), "{said}");
    turn(&mut driver, "and in vim mode?").await;
    let spec = &codex.specs()[0];
    assert_eq!(spec.resume.as_deref(), Some(CODEX_INPUT));
    assert!(!spec.fork);
    driver.exit().await;
}

#[tokio::test]
async fn picking_a_session_reprints_it_and_the_next_turn_resumes_it() {
    let mut driver = start().await;
    driver
        .agent
        .push(answer("It debounces.").with_cli_session(LOOP));
    driver.send(resume("claude-code", LOOP)).await;
    let AppEvent::ConversationResumed {
        conversation,
        title,
        worker,
        turns,
        ..
    } = driver
        .until(|event| matches!(event, AppEvent::ConversationResumed { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(conversation, ConversationId::numbered(1));
    assert_eq!(title, "Input loop notes");
    assert_eq!(worker, Some("claude-code".parse().unwrap()));
    let said: Vec<(bool, &str)> = turns
        .iter()
        .map(|turn| (turn.from == Party::User, turn.text.as_str()))
        .collect();
    assert_eq!(said.len(), 4);
    assert_eq!(said[0], (true, "How does the input loop\nread keys?"));
    assert_eq!(
        said[3],
        (
            false,
            "A paste arrives as one event,\nso it never runs a command."
        )
    );
    // An agent's turn comes with what it ran, so the reprint draws its calls where they were.
    assert!(turns[0].steps.is_empty());
    assert!(
        turns[1]
            .steps
            .iter()
            .any(|step| matches!(step, CliStep::Ran(call) if call.name == "Read")),
        "{:?}",
        turns[1].steps
    );
    let said = notice(&mut driver, "Carrying on").await;
    assert_eq!(
        said,
        "Carrying on Claude Code session 0a1b2c3d. Your next message goes to it through \
         claude's own resume."
    );

    turn(&mut driver, "and the mouse?").await;
    let spec = &driver.agent.specs()[0];
    assert_eq!(spec.resume.as_deref(), Some(LOOP));
    // The session's model stays, since none is set for claude-code.
    assert_eq!(spec.model.model.as_deref(), Some("claude-opus-5-5"));
    assert!(
        spec.prompt
            .iter()
            .all(|part| part.label != "conversation so far")
    );
    // The conversation keeps the session, so `/resume conversation-001` carries it on later too.
    let store =
        ConversationStore::open(&driver.env.conversation_dir(), &ConversationId::numbered(1));
    assert_eq!(store.session().unwrap().cli_session, LOOP);

    driver.send(resume("claude-code", LOOP)).await;
    notice(&mut driver, "is the conversation on screen").await;
    driver.exit().await;
}

#[tokio::test]
async fn a_short_id_picks_the_one_session_it_starts() {
    let mut driver = start().await;
    driver.send(resume("claude-code", "0a1b2c3d")).await;
    let said = notice(&mut driver, "could be").await;
    assert!(said.contains("could be 2 sessions"), "{said}");

    driver.send(resume("claude-code", "0a1b2c3d-2222")).await;
    let AppEvent::ConversationResumed { title, .. } = driver
        .until(|event| matches!(event, AppEvent::ConversationResumed { .. }))
        .await
    else {
        unreachable!()
    };
    assert!(title.starts_with("Fix the flaky export test."), "{title}");

    driver.send(resume("claude-code", "ffff")).await;
    let said = notice(&mut driver, "ffff").await;
    assert!(said.contains("Claude Code has no session ffff"), "{said}");
    driver.exit().await;
}

#[tokio::test]
async fn another_clis_session_carries_on_in_that_cli() {
    let mut driver = start().await;
    let codex = driver.directory.add(FakeRunner::cli("codex", "codex"));
    codex.push(answer("Same loop.").with_cli_session(CODEX_INPUT));
    driver.send(resume("codex", CODEX_INPUT)).await;
    let AppEvent::ConversationResumed { worker, .. } = driver
        .until(|event| matches!(event, AppEvent::ConversationResumed { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(worker, Some("codex".parse().unwrap()));
    let said = notice(&mut driver, "Carrying on").await;
    assert!(
        said.ends_with(
            "Codex is the main agent now, as /model would make it, and your next message goes \
             to the session through codex's own resume."
        ),
        "{said}"
    );

    turn(&mut driver, "and in vim mode?").await;
    assert!(driver.agent.specs().is_empty());
    let spec = &codex.specs()[0];
    assert_eq!(spec.resume.as_deref(), Some(CODEX_INPUT));
    assert!(
        spec.prompt
            .iter()
            .all(|part| part.label != "conversation so far")
    );
    driver.exit().await;
}

#[tokio::test]
async fn a_cli_that_cant_take_a_turn_leaves_its_session_to_the_main_agent() {
    let mut driver = start().await;
    driver.agent.push(answer("Same loop."));
    driver.send(resume("codex", CODEX_INPUT)).await;
    driver
        .until(|event| matches!(event, AppEvent::ConversationResumed { .. }))
        .await;
    let said = notice(&mut driver, "Carrying on").await;
    assert!(
        said.contains("Codex can't take a turn here, so claude-code takes the next message"),
        "{said}"
    );
    turn(&mut driver, "and in vim mode?").await;
    let spec = &driver.agent.specs()[0];
    assert_eq!(spec.resume, None);
    assert_eq!(spec.model.model, None);
    let primer = spec
        .prompt
        .iter()
        .find(|part| part.label == "conversation so far")
        .expect("the session so far primes the main agent");
    assert!(
        primer.text.contains("It reads keys in one loop."),
        "{}",
        primer.text
    );
    driver.exit().await;
}

#[tokio::test]
async fn argonauts_resume_opens_on_a_cli_session() {
    let opening = Opening {
        guided_setup: false,
        first: FirstView::CliSession(reference("claude-code", EXPORT)),
    };
    let mut driver = TestDriver::start_with(CliOptions {
        opening,
        prepare: Some(Box::new(|repo| {
            let root = repo.root().canonicalize().unwrap();
            install_stores(repo.user_home(), &root);
        })),
        ..CliOptions::default()
    })
    .await;
    let AppEvent::ConversationResumed { turns, .. } = driver
        .until(|event| matches!(event, AppEvent::ConversationResumed { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(turns.len(), 2);
    driver.exit().await;
}
