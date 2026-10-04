//! Your conversation's Claude Code session while it works, through the real controller loop:
//! a message goes in at once, send now and take back name it by its uuid, and the session is
//! joined as a screen only when You should know is on.

use std::time::Duration;

use argonauts_core::app::{AppEvent, AppMode, ChatEvent, UiCommand};
use argonauts_core::event::AgentEvent;
use argonauts_core::input::SlashCommand;
use argonauts_core::screen::{ScreenChange, ScreenRequest};
use argonauts_core::worker::{SessionControl, WorkerEvent};
use argonauts_testkit::FakeSession;

use crate::controller::test_support::{Harness, HarnessOptions};

fn init(capabilities: &[&str], plugins: &[&str]) -> WorkerEvent {
    let names = |list: &[&str]| list.iter().map(|&name| name.to_owned()).collect();
    WorkerEvent::Agent(AgentEvent::Init {
        session_id: Some("s-1".into()),
        model: Some("claude-opus-5-5".into()),
        tools: Vec::new(),
        plugins: names(plugins),
        capabilities: names(capabilities),
        version: Some("2.1.289".into()),
    })
}

fn writing() -> WorkerEvent {
    WorkerEvent::Agent(AgentEvent::TextDelta {
        text: "Running the slow test".into(),
        parent: None,
    })
}

/// Direct solo, whose turns answer in the CLI's own words, so a message can join them.
async fn direct() -> Harness {
    Harness::start_with(HarnessOptions {
        prepare: Some(Box::new(|repo: &argonauts_testkit::TempRepo| {
            std::fs::write(repo.global_config(), "[triage]\nforce = \"solo\"\n").unwrap();
        })),
        ..HarnessOptions::default()
    })
    .await
}

async fn working(harness: &mut Harness, events: Vec<WorkerEvent>) {
    harness.agent.push(FakeSession::until_stopped(events));
    harness
        .submit("check release.md, then run ./slow-test.sh")
        .await;
    harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Chat {
                    event: ChatEvent::Agent {
                        event: AgentEvent::TextDelta { .. },
                        ..
                    }
                }
            )
        })
        .await;
}

/// What the session got, once something came.
async fn controls_after(harness: &Harness, count: usize) -> Vec<SessionControl> {
    let agent = harness.agent.clone();
    for _ in 0..200 {
        let controls: Vec<SessionControl> = agent.controls().into_iter().map(|(_, c)| c).collect();
        if controls.len() >= count {
            return controls;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    agent.controls().into_iter().map(|(_, c)| c).collect()
}

#[tokio::test]
async fn a_message_goes_in_at_once_and_send_now_and_take_back_name_it() {
    let mut harness = direct().await;
    working(
        &mut harness,
        vec![init(&["interrupt_send_now_v1"], &[]), writing()],
    )
    .await;
    harness.submit("also check the changelog").await;
    let controls = controls_after(&harness, 1).await;
    let Some(SessionControl::Send { text, uuid }) = controls.first() else {
        panic!("{controls:?}");
    };
    assert_eq!(text, "also check the changelog");
    let uuid = uuid.clone();

    harness.send(UiCommand::SendNow).await;
    harness.send(UiCommand::TakeBackChat).await;
    let controls = controls_after(&harness, 3).await;
    assert_eq!(
        controls[1..3],
        [
            SessionControl::SendNow { uuid: uuid.clone() },
            SessionControl::TakeBack { uuid }
        ]
    );
    // No second turn waits: the message went into this one.
    harness.send(UiCommand::CancelTurn).await;
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
    harness.exit().await;
}

#[tokio::test]
async fn a_follow_up_typed_during_the_estimate_waits_for_the_next_turn() {
    // The estimate's answer has a schema, read from the session's last result. A follow-up
    // that joined it would be answered instead of the request, whose answer would be lost.
    let mut harness = Harness::start().await;
    harness.agent.push(FakeSession::until_stopped(vec![
        init(&["interrupt_send_now_v1"], &[]),
        writing(),
    ]));
    harness.submit("check release.md").await;
    // The screen isn't offered send now for this turn either.
    let shown = harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Chat {
                    event: ChatEvent::Agent {
                        event: AgentEvent::Init { .. },
                        ..
                    }
                }
            )
        })
        .await;
    let AppEvent::Chat {
        event:
            ChatEvent::Agent {
                event: AgentEvent::Init { capabilities, .. },
                ..
            },
    } = shown
    else {
        unreachable!()
    };
    assert!(capabilities.is_empty(), "{capabilities:?}");
    harness.submit("also check the changelog").await;
    harness.send(UiCommand::SendNow).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let controls = harness.agent.controls();
    assert!(
        !controls.iter().any(|(_, control)| matches!(
            control,
            SessionControl::Send { .. } | SessionControl::SendNow { .. }
        )),
        "{controls:?}"
    );
    harness.send(UiCommand::CancelTurn).await;
    let taken = harness
        .until(|event| matches!(event, AppEvent::TakenBack { .. }))
        .await;
    assert_eq!(
        taken,
        AppEvent::TakenBack {
            texts: vec!["also check the changelog".to_owned()]
        }
    );
    harness.exit().await;
}

#[tokio::test]
async fn without_send_now_a_message_waits_for_the_next_turn() {
    let mut harness = Harness::start().await;
    working(&mut harness, vec![init(&[], &[]), writing()]).await;
    harness.submit("also check the changelog").await;
    harness.send(UiCommand::SendNow).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let controls = harness.agent.controls();
    assert!(
        !controls.iter().any(|(_, control)| matches!(
            control,
            SessionControl::Send { .. } | SessionControl::SendNow { .. }
        )),
        "{controls:?}"
    );
    harness.send(UiCommand::CancelTurn).await;
    let taken = harness
        .until(|event| matches!(event, AppEvent::TakenBack { .. }))
        .await;
    assert_eq!(
        taken,
        AppEvent::TakenBack {
            texts: vec!["also check the changelog".to_owned()]
        }
    );
    harness.exit().await;
}

const HEADS_UP: &str = "cc-plugin-you-should-know@builtin";

async fn screen_requests(
    options: HarnessOptions,
    capabilities: &[&str],
    plugins: &[&str],
) -> usize {
    let mut harness = Harness::start_with(options).await;
    working(&mut harness, vec![init(capabilities, plugins), writing()]).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let asked = harness
        .agent
        .controls()
        .iter()
        .filter(|(_, control)| matches!(control, SessionControl::Screen(_)))
        .count();
    harness.send(UiCommand::CancelTurn).await;
    harness.exit().await;
    asked
}

#[tokio::test]
async fn the_main_session_joins_as_a_screen_only_when_you_should_know_is_on() {
    let all = ["interrupt_send_now_v1", "ui_surface_v1"];
    let mut harness = Harness::start().await;
    working(&mut harness, vec![init(&all, &[HEADS_UP]), writing()]).await;
    let controls = controls_after(&harness, 2).await;
    assert_eq!(
        controls[..2],
        [
            SessionControl::Screen(ScreenRequest::Attach),
            SessionControl::Screen(ScreenRequest::Render)
        ]
    );
    // A press names a button the session drew; one it never drew says so on the notice line
    // and sends nothing.
    harness
        .send(UiCommand::PressHeadsUp {
            plugin: "cc-plugin-you-should-know".into(),
            handle: 7,
        })
        .await;
    harness
        .until(|event| matches!(event, AppEvent::Hint { .. }))
        .await;
    harness.send(UiCommand::CancelTurn).await;
    harness.exit().await;

    assert_eq!(
        screen_requests(HarnessOptions::default(), &all, &[]).await,
        0,
        "the mod isn't on"
    );
    assert_eq!(
        screen_requests(HarnessOptions::default(), &all[..1], &[HEADS_UP]).await,
        0,
        "the CLI can't take a screen"
    );
    let off = HarnessOptions {
        flags: vec![r#"ui.heads_up="off""#.to_owned()],
        ..HarnessOptions::default()
    };
    assert_eq!(
        screen_requests(off, &all, &[HEADS_UP]).await,
        0,
        "ui.heads_up = off"
    );
}

fn draws_nothing(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Chat {
            event: ChatEvent::Agent {
                event: AgentEvent::ScreenChanged {
                    change: ScreenChange::Drawn { drawn },
                },
                ..
            },
        } if drawn.is_empty()
    )
}

/// A heads-up an earlier session drew stays above the input only while a session joins as a
/// screen. A turn whose session won't, as when you turned the mod off in Claude Code or the
/// conversation went to another CLI, says nothing is drawn there, and so does `/clear`.
#[tokio::test]
async fn a_turn_that_wont_join_and_clear_take_the_heads_up_away() {
    let mut harness = Harness::start().await;
    working(
        &mut harness,
        vec![
            init(&["interrupt_send_now_v1", "ui_surface_v1"], &[]),
            writing(),
        ],
    )
    .await;
    assert!(harness.seen().iter().any(draws_nothing));
    harness.send(UiCommand::CancelTurn).await;
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
    harness
        .send(UiCommand::Slash {
            command: SlashCommand::Clear,
        })
        .await;
    harness
        .until(|event| matches!(event, AppEvent::ChatCleared))
        .await;
    harness.exit().await;

    // A session that joins draws for itself.
    let mut harness = Harness::start().await;
    working(
        &mut harness,
        vec![
            init(&["interrupt_send_now_v1", "ui_surface_v1"], &[HEADS_UP]),
            writing(),
        ],
    )
    .await;
    assert!(!harness.seen().iter().any(draws_nothing));
    harness.send(UiCommand::CancelTurn).await;
    harness.exit().await;
}
