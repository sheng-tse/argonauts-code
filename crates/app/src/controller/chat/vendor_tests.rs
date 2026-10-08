//! `/resume` listing the main agent CLI's own sessions, and carrying one on, through the real
//! controller loop and the real adapters reading recorded session stores.

use std::fs;
use std::path::Path;

use argonauts_core::app::{AppEvent, AppMode, ChatEvent, UiCommand, ViewData};
use argonauts_core::events::run::Party;
use argonauts_core::input::{ResumeTarget, SlashCommand};
use argonauts_core::names::{ChatId, VendorId};
use argonauts_core::vendor_sessions::VendorSessionRef;
use argonauts_testkit::FakeSession;
use serde_json::json;

use crate::FirstView;
use crate::chat_store::ChatStore;
use crate::controller::Opening;
use crate::controller::test_support::{Harness, HarnessOptions};

const LOOP: &str = "0a1b2c3d-1111-4aaa-8bbb-000000000001";
const EXPORT: &str = "0a1b2c3d-2222-4aaa-8bbb-000000000002";
const CODEX_INPUT: &str = "019a5770-e3f9-7f92-ab2b-5ba3b7602e8a";

/// The adapters' recorded stores, moved to `repo` under `home`: `/work/shop` becomes `repo`.
fn install_stores(home: &Path, repo: &Path) {
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

async fn start() -> Harness {
    let harness = Harness::start().await;
    install_stores(
        harness.env.user_home.as_deref().unwrap(),
        &harness.env.repo_root,
    );
    harness
}

fn resume(vendor: &str, id: &str) -> UiCommand {
    UiCommand::Slash {
        command: SlashCommand::Resume {
            target: Some(ResumeTarget::VendorSession(reference(vendor, id))),
        },
    }
}

fn reference(vendor: &str, id: &str) -> VendorSessionRef {
    VendorSessionRef {
        vendor: VendorId(vendor.into()),
        id: id.into(),
    }
}

fn answer(text: &str) -> FakeSession {
    FakeSession::structured(json!({"kind": "answer", "answer_markdown": text, "estimate": null}))
}

async fn notice(harness: &mut Harness, part: &str) -> String {
    match harness
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains(part)))
        .await
    {
        AppEvent::Notice { text, .. } => text,
        _ => unreachable!(),
    }
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

#[tokio::test]
async fn the_picker_lists_the_main_agents_own_sessions() {
    let mut harness = start().await;
    harness
        .send(UiCommand::Slash {
            command: SlashCommand::Resume { target: None },
        })
        .await;
    let AppEvent::View { data } = harness
        .until(|event| matches!(event, AppEvent::View { data } if matches!(data.as_ref(), ViewData::Runs { .. })))
        .await
    else {
        unreachable!()
    };
    let ViewData::Runs { sessions, .. } = *data else {
        unreachable!()
    };
    // Claude Code is the main agent, so Codex's sessions aren't listed.
    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec![EXPORT, LOOP]);
    assert_eq!(sessions[1].title, "Input loop notes");
    assert_eq!(sessions[1].folder, harness.env.repo_root);
    harness.exit().await;
}

#[tokio::test]
async fn picking_a_session_reprints_it_and_the_next_turn_resumes_it() {
    let mut harness = start().await;
    harness
        .agent
        .push(answer("It debounces.").with_vendor_session(LOOP));
    harness.send(resume("claude-code", LOOP)).await;
    let AppEvent::ChatResumed {
        chat,
        title,
        worker,
        earlier,
        turns,
        ..
    } = harness
        .until(|event| matches!(event, AppEvent::ChatResumed { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(chat, ChatId::numbered(1));
    assert_eq!(title, "Input loop notes");
    assert_eq!(worker, Some("claude-code".parse().unwrap()));
    assert!(earlier.is_empty());
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
    let said = notice(&mut harness, "Carrying on").await;
    assert_eq!(
        said,
        "Carrying on Claude Code session 0a1b2c3d. Your next message goes to it through \
         claude's own resume."
    );

    turn(&mut harness, "and the mouse?").await;
    let spec = &harness.agent.specs()[0];
    assert_eq!(spec.resume.as_deref(), Some(LOOP));
    // The session's model stays, since none is set for claude-code.
    assert_eq!(spec.worker.model.as_deref(), Some("claude-opus-5-5"));
    assert!(spec.prompt.iter().all(|part| part.label != "chat so far"));
    // The conversation keeps the session, so `/resume chat-001` carries it on later too.
    let store = ChatStore::open(&harness.env.chat_dir(), &ChatId::numbered(1));
    assert_eq!(store.session().unwrap().vendor_session, LOOP);

    harness.send(resume("claude-code", LOOP)).await;
    notice(&mut harness, "is the conversation on screen").await;
    harness.exit().await;
}

#[tokio::test]
async fn a_short_id_picks_the_one_session_it_starts() {
    let mut harness = start().await;
    harness.send(resume("claude-code", "0a1b2c3d")).await;
    let said = notice(&mut harness, "could be").await;
    assert!(said.contains("could be 2 sessions"), "{said}");

    harness.send(resume("claude-code", "0a1b2c3d-2222")).await;
    let AppEvent::ChatResumed { title, .. } = harness
        .until(|event| matches!(event, AppEvent::ChatResumed { .. }))
        .await
    else {
        unreachable!()
    };
    assert!(title.starts_with("Fix the flaky export test."), "{title}");

    harness.send(resume("claude-code", "ffff")).await;
    let said = notice(&mut harness, "ffff").await;
    assert!(said.contains("Claude Code has no session ffff"), "{said}");
    harness.exit().await;
}

#[tokio::test]
async fn another_clis_session_primes_the_main_agent() {
    let mut harness = start().await;
    harness.agent.push(answer("Same loop."));
    harness.send(resume("codex", CODEX_INPUT)).await;
    harness
        .until(|event| matches!(event, AppEvent::ChatResumed { .. }))
        .await;
    let said = notice(&mut harness, "Carrying on").await;
    assert!(
        said.contains("codex isn't the main agent, so claude-code takes the next message"),
        "{said}"
    );
    turn(&mut harness, "and in vim mode?").await;
    let spec = &harness.agent.specs()[0];
    assert_eq!(spec.resume, None);
    assert_eq!(spec.worker.model, None);
    let primer = spec
        .prompt
        .iter()
        .find(|part| part.label == "chat so far")
        .expect("the session so far primes the main agent");
    assert!(
        primer.text.contains("It reads keys in one loop."),
        "{}",
        primer.text
    );
    harness.exit().await;
}

#[tokio::test]
async fn argonauts_resume_opens_on_a_cli_session() {
    let opening = Opening {
        guided_setup: false,
        first: FirstView::VendorSession(reference("claude-code", EXPORT)),
    };
    let mut harness = Harness::start_with(HarnessOptions {
        opening,
        prepare: Some(Box::new(|repo| {
            let root = repo.root().canonicalize().unwrap();
            install_stores(repo.user_home(), &root);
        })),
        ..HarnessOptions::default()
    })
    .await;
    let AppEvent::ChatResumed { turns, .. } = harness
        .until(|event| matches!(event, AppEvent::ChatResumed { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(turns.len(), 2);
    harness.exit().await;
}
