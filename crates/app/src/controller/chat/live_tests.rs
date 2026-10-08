//! Your conversation's Claude Code session while it works, through the real controller loop:
//! a message goes in at once, send now and take back name it by its uuid, and the session is
//! joined as a screen only when You should know is on.

use std::time::Duration;

use argonauts_core::app::{AppEvent, AppMode, ChatEvent, UiCommand};
use argonauts_core::event::AgentEvent;
use argonauts_core::events::context::ContextDetail;
use argonauts_core::events::session::{SubagentKind, SubagentStatus, SubagentUpdate};
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
            std::fs::write(repo.global_config(), "[solo_or_crew]\nforce = \"solo\"\n").unwrap();
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

#[tokio::test]
async fn context_opening_and_a_watch_reach_the_running_session() {
    let mut harness = Harness::start().await;
    harness
        .send(UiCommand::WatchContext {
            detail: Some(ContextDetail::Summary),
        })
        .await;
    working(&mut harness, vec![init(&[], &[]), writing()]).await;
    // The watch set between turns goes to the session as it starts.
    let controls = controls_after(&harness, 1).await;
    assert_eq!(
        controls[0],
        SessionControl::WatchContext(Some(ContextDetail::Summary))
    );
    harness
        .send(UiCommand::Slash {
            command: SlashCommand::Context {
                all: false,
                action: Default::default(),
            },
        })
        .await;
    harness
        .until(|event| matches!(event, AppEvent::View { .. }))
        .await;
    harness.send(UiCommand::WatchContext { detail: None }).await;
    let controls = controls_after(&harness, 3).await;
    assert_eq!(
        controls[1..3],
        [
            SessionControl::AskContext(ContextDetail::Summary),
            SessionControl::WatchContext(None)
        ]
    );
    harness.send(UiCommand::CancelTurn).await;
    harness.exit().await;
}

/// `x` on a subagent of the conversation's own session: the session is asked to stop that one
/// and goes on. With no turn running there's nothing to ask, and you're told.
#[tokio::test]
async fn stopping_a_subagent_asks_the_session_and_leaves_the_turn_running() {
    let mut harness = direct().await;
    working(&mut harness, vec![init(&[], &[]), writing()]).await;
    harness
        .send(UiCommand::StopSubagent {
            id: "toolu_a".into(),
        })
        .await;
    let controls = controls_after(&harness, 1).await;
    assert_eq!(
        controls,
        [SessionControl::StopSubagent {
            id: "toolu_a".into()
        }]
    );
    harness.send(UiCommand::CancelTurn).await;
    harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Chat {
                    event: ChatEvent::TurnCancelled { .. }
                }
            )
        })
        .await;
    harness
        .send(UiCommand::StopSubagent {
            id: "toolu_a".into(),
        })
        .await;
    let told = harness
        .until(|event| matches!(event, AppEvent::Notice { .. }))
        .await;
    let AppEvent::Notice { text, .. } = told else {
        unreachable!()
    };
    assert_eq!(
        text,
        "That subagent isn't running, so there's nothing to stop."
    );
    harness.exit().await;
}

/// Enter on a subagent's row: the main agent gets your words with how to pass them on, as
/// its own command, so a name whose first word reads like an agent's is never a mention, and
/// the chat shows only `@name` and your words.
#[tokio::test]
async fn a_message_for_a_subagent_goes_to_the_main_agent_whatever_its_name() {
    let mut harness = direct().await;
    harness.agent.push(FakeSession::until_stopped(vec![
        init(&["interrupt_send_now_v1"], &[]),
        writing(),
    ]));
    harness
        .send(UiCommand::MessageSubagent {
            name: "Count slowly".into(),
            text: "stop at 5".into(),
        })
        .await;
    let started = harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Chat {
                    event: ChatEvent::TurnStarted { .. }
                }
            )
        })
        .await;
    let AppEvent::Chat {
        event: ChatEvent::TurnStarted { text, .. },
    } = started
    else {
        unreachable!()
    };
    assert_eq!(text, "@Count slowly stop at 5");
    let prompt: String = harness
        .agent
        .specs()
        .last()
        .map(|spec| spec.prompt.iter().map(|part| part.text.as_str()).collect())
        .unwrap_or_default();
    assert!(
        prompt.contains("For your subagent \"Count slowly\": stop at 5")
            && prompt.contains("SendMessage"),
        "{prompt}"
    );

    // While that turn runs, the next goes in at once.
    harness
        .send(UiCommand::MessageSubagent {
            name: "Explore the auth flow".into(),
            text: "skip the tests".into(),
        })
        .await;
    let controls = controls_after(&harness, 1).await;
    let Some(SessionControl::Send { text, .. }) = controls.first() else {
        panic!("{controls:?}");
    };
    assert!(
        text.starts_with("For your subagent \"Explore the auth flow\": skip the tests"),
        "{text}"
    );
    assert!(
        !harness.seen().iter().any(|event| matches!(
            event,
            AppEvent::Notice { text, .. } if text.contains("mention")
        )),
        "never read as a mention"
    );
    harness.exit().await;
}

fn background(running: &[&str]) -> WorkerEvent {
    WorkerEvent::Agent(AgentEvent::BackgroundTasks {
        running: running.iter().map(|&task| task.to_owned()).collect(),
        ended: running.is_empty(),
    })
}

/// One of the session's own subagents, going or done.
fn subagent(status: SubagentStatus) -> WorkerEvent {
    WorkerEvent::Agent(AgentEvent::Subagent {
        update: SubagentUpdate {
            id: "toolu_a".into(),
            parent: None,
            kind: SubagentKind::Subagent,
            label: Some("Count slowly".into()),
            prompt: None,
            status,
            last_message: None,
            log_uri: None,
            steps_visible: true,
            tokens: None,
        },
    })
}

/// Esc while the conversation's own session runs subagents: only the main agent's turn is
/// asked to stop. The turn stays for them, so one can still be stopped by
/// itself. With nothing in the background, or only a command the session left running, which
/// has no row to stop it from, Esc stops the session whole, as before.
#[tokio::test]
async fn esc_stops_the_main_agents_turn_alone_while_its_own_subagents_run() {
    let mut harness = direct().await;
    working(
        &mut harness,
        vec![
            init(&[], &[]),
            subagent(SubagentStatus::Running),
            background(&["b1"]),
            writing(),
        ],
    )
    .await;
    harness.send(UiCommand::CancelTurn).await;
    harness
        .send(UiCommand::StopSubagent {
            id: "toolu_a".into(),
        })
        .await;
    let controls = controls_after(&harness, 2).await;
    assert_eq!(
        controls,
        [
            SessionControl::StopTurn,
            SessionControl::StopSubagent {
                id: "toolu_a".into()
            }
        ]
    );
    let cancelled = harness.seen().iter().any(|event| {
        matches!(
            event,
            AppEvent::Chat {
                event: ChatEvent::TurnCancelled { .. }
            }
        )
    });
    assert!(!cancelled, "the turn goes on for its subagents");
    harness.exit().await;

    let alone = vec![init(&[], &[]), writing()];
    let with_a_command = vec![
        init(&[], &[]),
        subagent(SubagentStatus::Done),
        background(&["bash-1"]),
        writing(),
    ];
    for events in [alone, with_a_command] {
        let mut harness = direct().await;
        working(&mut harness, events).await;
        harness.send(UiCommand::CancelTurn).await;
        harness
            .until(|event| {
                matches!(
                    event,
                    AppEvent::Chat {
                        event: ChatEvent::TurnCancelled { .. }
                    }
                )
            })
            .await;
        assert_eq!(controls_after(&harness, 1).await, [SessionControl::Stop]);
        harness.exit().await;
    }
}

/// A turn stopped alone ends when its session does. What the main agent said once its
/// subagents were done is shown as its words, and starts no work: you stopped that request.
#[tokio::test]
async fn a_turn_stopped_alone_starts_no_work_when_its_session_ends() {
    use argonauts_core::app::TurnReply;
    use argonauts_core::event::ErrorCategory;
    use serde_json::json;

    let change = json!({
        "kind": "change",
        "answer_markdown": "The README's install line names the wrong package.",
        "estimate": {
            "task_type": "docs",
            "summary": "Fix the install command in the README",
            "files_likely": ["README.md"],
            "file_count": 1,
            "risk_labels": [],
            "tests_exist": true,
            "unknowns": []
        }
    });
    let mut harness = Harness::start_with(HarnessOptions {
        prepare: Some(Box::new(|repo: &argonauts_testkit::TempRepo| {
            std::fs::write(
                repo.global_config(),
                "ultracode = true\n[solo]\nkind = \"checked\"\n",
            )
            .unwrap();
        })),
        ..HarnessOptions::default()
    })
    .await;
    let mut session = FakeSession::structured(change);
    let verdict = session.events.pop().expect("the result");
    session.events = vec![
        init(&[], &[]),
        subagent(SubagentStatus::Running),
        background(&["b1"]),
        writing(),
        WorkerEvent::Agent(AgentEvent::Error {
            category: ErrorCategory::Interrupted,
            message: "interrupted".into(),
            retryable: false,
        }),
        WorkerEvent::Agent(AgentEvent::Result {
            text: None,
            structured: None,
            is_error: true,
        }),
        subagent(SubagentStatus::Done),
        background(&[]),
        verdict,
    ];
    // Slow enough that Esc lands while the main agent still writes.
    harness
        .agent
        .push(session.with_delay(Duration::from_millis(200)));
    harness
        .submit("fix the install command in the README")
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
    harness.send(UiCommand::CancelTurn).await;
    let finished = harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Chat {
                    event: ChatEvent::TurnFinished { .. } | ChatEvent::TurnFailed { .. }
                }
            )
        })
        .await;
    let AppEvent::Chat {
        event: ChatEvent::TurnFinished { reply, .. },
    } = finished
    else {
        panic!("{finished:?}");
    };
    assert_eq!(
        reply,
        TurnReply::Answer {
            markdown: "The README's install line names the wrong package.".into()
        }
    );
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
    assert!(harness.conductor.starts().is_empty(), "no run was started");
    assert_eq!(
        controls_after(&harness, 1).await,
        [SessionControl::StopTurn]
    );
    harness.exit().await;
}

/// A turn stopped alone whose session ends with nothing more ends as any turn Esc stops: it
/// is cancelled, nothing failed, and no fallback takes the request.
#[tokio::test]
async fn a_turn_stopped_alone_with_nothing_after_it_ends_cancelled() {
    use argonauts_core::event::ErrorCategory;
    use argonauts_core::events::session::EndReason;

    let mut harness = direct().await;
    let mut session = FakeSession::ended(EndReason::Stopped);
    session.events = vec![
        init(&[], &[]),
        subagent(SubagentStatus::Running),
        background(&["b1"]),
        writing(),
        WorkerEvent::Agent(AgentEvent::Error {
            category: ErrorCategory::Interrupted,
            message: "interrupted".into(),
            retryable: false,
        }),
        WorkerEvent::Agent(AgentEvent::Result {
            text: None,
            structured: None,
            is_error: true,
        }),
    ];
    harness
        .agent
        .push(session.with_delay(Duration::from_millis(200)));
    harness.submit("count in the background").await;
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
    harness.send(UiCommand::CancelTurn).await;
    let ended = harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Chat {
                    event: ChatEvent::TurnCancelled { .. }
                        | ChatEvent::TurnFailed { .. }
                        | ChatEvent::TurnFinished { .. }
                }
            )
        })
        .await;
    assert!(
        matches!(
            ended,
            AppEvent::Chat {
                event: ChatEvent::TurnCancelled { .. }
            }
        ),
        "{ended:?}"
    );
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
    assert_eq!(harness.agent.specs().len(), 1, "no fallback took it");
    harness.exit().await;
}
