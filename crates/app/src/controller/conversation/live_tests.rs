//! Your conversation's Claude Code session while it works, through the real controller loop:
//! a message goes in at once, send now and take back name it by its uuid, and the session is
//! joined as a screen only when You should know is on.

use std::time::Duration;

use argonauts_core::app::{AppEvent, AppMode, ConversationEvent, UiCommand};
use argonauts_core::event::AgentEvent;
use argonauts_core::events::context::ContextDetail;
use argonauts_core::events::session::{SubagentKind, SubagentStatus, SubagentUpdate};
use argonauts_core::input::SlashCommand;
use argonauts_core::screen::{ScreenChange, ScreenRequest};
use argonauts_core::session_runner::{RunnerEvent, SessionControl};
use argonauts_testkit::{FakeSession, Hold};

use crate::controller::test_support::{CliOptions, TestDriver, max_wait};

fn init(capabilities: &[&str], plugins: &[&str]) -> RunnerEvent {
    let names = |list: &[&str]| list.iter().map(|&name| name.to_owned()).collect();
    RunnerEvent::Agent(AgentEvent::Init {
        session_id: Some("s-1".into()),
        model: Some("claude-opus-5-5".into()),
        tools: Vec::new(),
        plugins: names(plugins),
        capabilities: names(capabilities),
        version: Some("2.1.289".into()),
    })
}

fn writing() -> RunnerEvent {
    RunnerEvent::Agent(AgentEvent::TextDelta {
        text: "Running the slow test".into(),
        parent: None,
    })
}

/// Direct Captain, whose turns answer in the CLI's own words, so a message can join them.
async fn direct() -> TestDriver {
    TestDriver::start_with(CliOptions {
        prepare: Some(Box::new(|repo: &argonauts_testkit::TempRepo| {
            std::fs::write(
                repo.global_config(),
                "[captain_or_crew]\nforce = \"captain\"\n",
            )
            .unwrap();
        })),
        ..CliOptions::default()
    })
    .await
}

async fn working(driver: &mut TestDriver, events: Vec<RunnerEvent>) {
    driver.agent.push(FakeSession::until_stopped(events));
    driver
        .submit("check release.md, then run ./slow-test.sh")
        .await;
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Conversation {
                    event: ConversationEvent::Agent {
                        event: AgentEvent::TextDelta { .. },
                        ..
                    }
                }
            )
        })
        .await;
}

/// What the session got, once something came.
async fn controls_after(driver: &TestDriver, count: usize) -> Vec<SessionControl> {
    let deadline = tokio::time::Instant::now() + max_wait();
    loop {
        let controls: Vec<SessionControl> = driver
            .agent
            .controls()
            .into_iter()
            .map(|(_, c)| c)
            .collect();
        if controls.len() >= count || tokio::time::Instant::now() >= deadline {
            return controls;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_message_goes_in_at_once_and_send_now_and_take_back_name_it() {
    let mut driver = direct().await;
    working(
        &mut driver,
        vec![init(&["interrupt_send_now_v1"], &[]), writing()],
    )
    .await;
    driver.submit("also check the changelog").await;
    let controls = controls_after(&driver, 1).await;
    let Some(SessionControl::Send { text, uuid, .. }) = controls.first() else {
        panic!("{controls:?}");
    };
    assert_eq!(text, "also check the changelog");
    let uuid = uuid.clone();

    driver.send(UiCommand::SendNow).await;
    driver.send(UiCommand::TakeBackConversation).await;
    let controls = controls_after(&driver, 3).await;
    assert_eq!(
        controls[1..3],
        [
            SessionControl::SendNow { uuid: uuid.clone() },
            SessionControl::TakeBack { uuid }
        ]
    );
    // No second turn waits: the message went into this one.
    driver.send(UiCommand::CancelTurn).await;
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
    driver.exit().await;
}

#[tokio::test]
async fn a_follow_up_typed_during_the_estimate_waits_for_the_next_turn() {
    // The estimate's answer has a schema, read from the session's last result. A follow-up
    // that joined it would be answered instead of the request, whose answer would be lost.
    let mut driver = TestDriver::start().await;
    driver.agent.push(FakeSession::until_stopped(vec![
        init(&["interrupt_send_now_v1"], &[]),
        writing(),
    ]));
    driver.submit("check release.md").await;
    // The screen isn't offered send now for this turn either.
    let shown = driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Conversation {
                    event: ConversationEvent::Agent {
                        event: AgentEvent::Init { .. },
                        ..
                    }
                }
            )
        })
        .await;
    let AppEvent::Conversation {
        event:
            ConversationEvent::Agent {
                event: AgentEvent::Init { capabilities, .. },
                ..
            },
    } = shown
    else {
        unreachable!()
    };
    assert!(capabilities.is_empty(), "{capabilities:?}");
    driver.submit("also check the changelog").await;
    driver.send(UiCommand::SendNow).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let controls = driver.agent.controls();
    assert!(
        !controls.iter().any(|(_, control)| matches!(
            control,
            SessionControl::Send { .. } | SessionControl::SendNow { .. }
        )),
        "{controls:?}"
    );
    driver.send(UiCommand::CancelTurn).await;
    let taken = driver
        .until(|event| matches!(event, AppEvent::TakenBack { .. }))
        .await;
    assert_eq!(
        taken,
        AppEvent::TakenBack {
            texts: vec!["also check the changelog".to_owned()]
        }
    );
    driver.exit().await;
}

#[tokio::test]
async fn without_send_now_a_message_waits_for_the_next_turn() {
    let mut driver = TestDriver::start().await;
    working(&mut driver, vec![init(&[], &[]), writing()]).await;
    driver.submit("also check the changelog").await;
    driver.send(UiCommand::SendNow).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let controls = driver.agent.controls();
    assert!(
        !controls.iter().any(|(_, control)| matches!(
            control,
            SessionControl::Send { .. } | SessionControl::SendNow { .. }
        )),
        "{controls:?}"
    );
    driver.send(UiCommand::CancelTurn).await;
    let taken = driver
        .until(|event| matches!(event, AppEvent::TakenBack { .. }))
        .await;
    assert_eq!(
        taken,
        AppEvent::TakenBack {
            texts: vec!["also check the changelog".to_owned()]
        }
    );
    driver.exit().await;
}

const NOTICE_PLUGIN: &str = "cc-plugin-you-should-know@builtin";

async fn screen_requests(options: CliOptions, capabilities: &[&str], plugins: &[&str]) -> usize {
    let mut driver = TestDriver::start_with(options).await;
    working(&mut driver, vec![init(capabilities, plugins), writing()]).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let asked = driver
        .agent
        .controls()
        .iter()
        .filter(|(_, control)| matches!(control, SessionControl::Screen(_)))
        .count();
    driver.send(UiCommand::CancelTurn).await;
    driver.exit().await;
    asked
}

#[tokio::test]
async fn the_main_session_joins_as_a_screen_only_when_you_should_know_is_on() {
    let all = ["interrupt_send_now_v1", "ui_surface_v1"];
    let mut driver = TestDriver::start().await;
    working(&mut driver, vec![init(&all, &[NOTICE_PLUGIN]), writing()]).await;
    let controls = controls_after(&driver, 2).await;
    assert_eq!(
        controls[..2],
        [
            SessionControl::Screen(ScreenRequest::Attach),
            SessionControl::Screen(ScreenRequest::Render)
        ]
    );
    // A press names a button the session drew; one it never drew says so on the notice line
    // and sends nothing.
    driver
        .send(UiCommand::PressNotice {
            plugin: "cc-plugin-you-should-know".into(),
            handle: 7,
        })
        .await;
    driver
        .until(|event| matches!(event, AppEvent::Hint { .. }))
        .await;
    driver.send(UiCommand::CancelTurn).await;
    driver.exit().await;

    assert_eq!(
        screen_requests(CliOptions::default(), &all, &[]).await,
        0,
        "the mod isn't on"
    );
    assert_eq!(
        screen_requests(CliOptions::default(), &all[..1], &[NOTICE_PLUGIN]).await,
        0,
        "the CLI can't take a screen"
    );
    let off = CliOptions {
        flags: vec![r#"ui.notices="off""#.to_owned()],
        ..CliOptions::default()
    };
    assert_eq!(
        screen_requests(off, &all, &[NOTICE_PLUGIN]).await,
        0,
        "ui.notices = off"
    );
}

fn draws_nothing(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Conversation {
            event: ConversationEvent::Agent {
                event: AgentEvent::ScreenChanged {
                    change: ScreenChange::Drawn { drawn },
                },
                ..
            },
        } if drawn.is_empty()
    )
}

/// A notice an earlier session drew stays above the input only while a session joins as a
/// screen. A turn whose session won't, as when you turned the mod off in Claude Code or the
/// conversation went to another CLI, says nothing is drawn there, and so does `/clear`.
#[tokio::test]
async fn a_turn_that_wont_join_and_clear_take_the_notice_away() {
    let mut driver = TestDriver::start().await;
    working(
        &mut driver,
        vec![
            init(&["interrupt_send_now_v1", "ui_surface_v1"], &[]),
            writing(),
        ],
    )
    .await;
    assert!(driver.seen().iter().any(draws_nothing));
    driver.send(UiCommand::CancelTurn).await;
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
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Clear,
        })
        .await;
    driver
        .until(|event| matches!(event, AppEvent::ConversationCleared { .. }))
        .await;
    driver.exit().await;

    // A session that joins draws for itself.
    let mut driver = TestDriver::start().await;
    working(
        &mut driver,
        vec![
            init(
                &["interrupt_send_now_v1", "ui_surface_v1"],
                &[NOTICE_PLUGIN],
            ),
            writing(),
        ],
    )
    .await;
    assert!(!driver.seen().iter().any(draws_nothing));
    driver.send(UiCommand::CancelTurn).await;
    driver.exit().await;
}

#[tokio::test]
async fn context_opening_and_a_watch_reach_the_running_session() {
    let mut driver = TestDriver::start().await;
    driver
        .send(UiCommand::WatchContext {
            detail: Some(ContextDetail::Summary),
        })
        .await;
    working(&mut driver, vec![init(&[], &[]), writing()]).await;
    // The watch set between turns goes to the session as it starts.
    let controls = controls_after(&driver, 1).await;
    assert_eq!(
        controls[0],
        SessionControl::WatchContext(Some(ContextDetail::Summary))
    );
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Context {
                all: false,
                action: Default::default(),
            },
        })
        .await;
    driver
        .until(|event| matches!(event, AppEvent::View { .. }))
        .await;
    driver.send(UiCommand::WatchContext { detail: None }).await;
    let controls = controls_after(&driver, 3).await;
    assert_eq!(
        controls[1..3],
        [
            // Opening the tab counts as Claude Code's own /context does.
            SessionControl::AskContext(ContextDetail::Full),
            SessionControl::WatchContext(None)
        ]
    );
    driver.send(UiCommand::CancelTurn).await;
    driver.exit().await;
}

/// `/context print` while the session works: the session is asked to count itself as its own
/// /context does, and the rows print when its count is in. Here it never answers, so they
/// print as the turn ends, from what the session last said.
#[tokio::test]
async fn context_print_counts_the_working_session_first() {
    use argonauts_core::app::ViewData;
    use argonauts_core::input::ViewAction;

    let mut driver = TestDriver::start().await;
    let filled = RunnerEvent::Agent(AgentEvent::ContextFill {
        used: 48_200,
        size: Some(200_000),
        sent: None,
    });
    working(&mut driver, vec![init(&[], &[]), filled, writing()]).await;
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Context {
                all: false,
                action: ViewAction::Print,
            },
        })
        .await;
    let controls = controls_after(&driver, 1).await;
    assert_eq!(controls[0], SessionControl::AskContext(ContextDetail::Full));
    let printed = |event: &AppEvent| matches!(event, AppEvent::View { data } if matches!(**data, ViewData::ContextPrint { .. }));
    assert!(
        !driver.seen().iter().any(printed),
        "nothing prints before the count"
    );
    driver.send(UiCommand::CancelTurn).await;
    let AppEvent::View { data } = driver.until(printed).await else {
        unreachable!()
    };
    let ViewData::ContextPrint { view } = *data else {
        unreachable!()
    };
    let rows: Vec<(Option<&argonauts_core::RunId>, u64, Option<u64>)> = view
        .rows
        .iter()
        .map(|row| (row.run.as_ref(), row.used, row.window))
        .collect();
    assert_eq!(rows, [(None, 48_200, Some(200_000))]);
    driver.exit().await;
}

/// `x` on a subagent of the conversation's own session: the session is asked to stop that one
/// and goes on. With no turn running there's nothing to ask, and you're told.
#[tokio::test]
async fn stopping_a_subagent_asks_the_session_and_leaves_the_turn_running() {
    let mut driver = direct().await;
    working(&mut driver, vec![init(&[], &[]), writing()]).await;
    driver
        .send(UiCommand::StopSubagent {
            id: "toolu_a".into(),
        })
        .await;
    let controls = controls_after(&driver, 1).await;
    assert_eq!(
        controls,
        [SessionControl::StopSubagent {
            id: "toolu_a".into()
        }]
    );
    driver.send(UiCommand::CancelTurn).await;
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Conversation {
                    event: ConversationEvent::TurnCancelled { .. }
                }
            )
        })
        .await;
    driver
        .send(UiCommand::StopSubagent {
            id: "toolu_a".into(),
        })
        .await;
    let told = driver
        .until(|event| matches!(event, AppEvent::Notice { .. }))
        .await;
    let AppEvent::Notice { text, .. } = told else {
        unreachable!()
    };
    assert_eq!(
        text,
        "That subagent isn't running, so there's nothing to stop."
    );
    driver.exit().await;
}

/// Enter on a subagent's row: the main agent gets your words with how to pass them on, as
/// its own command, so a name whose first word reads like an agent's is never a mention, and
/// the conversation shows only `@name` and your words.
#[tokio::test]
async fn a_message_for_a_subagent_goes_to_the_main_agent_whatever_its_name() {
    let mut driver = direct().await;
    driver.agent.push(FakeSession::until_stopped(vec![
        init(&["interrupt_send_now_v1"], &[]),
        writing(),
    ]));
    driver
        .send(UiCommand::MessageSubagent {
            name: "Count slowly".into(),
            text: "stop at 5".into(),
        })
        .await;
    let started = driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Conversation {
                    event: ConversationEvent::TurnStarted { .. }
                }
            )
        })
        .await;
    let AppEvent::Conversation {
        event: ConversationEvent::TurnStarted { text, .. },
    } = started
    else {
        unreachable!()
    };
    assert_eq!(text, "@Count slowly stop at 5");
    let prompt: String = driver
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
    driver
        .send(UiCommand::MessageSubagent {
            name: "Explore the auth flow".into(),
            text: "skip the tests".into(),
        })
        .await;
    let controls = controls_after(&driver, 1).await;
    let Some(SessionControl::Send { text, .. }) = controls.first() else {
        panic!("{controls:?}");
    };
    assert!(
        text.starts_with("For your subagent \"Explore the auth flow\": skip the tests"),
        "{text}"
    );
    assert!(
        !driver.seen().iter().any(|event| matches!(
            event,
            AppEvent::Notice { text, .. } if text.contains("mention")
        )),
        "never read as a mention"
    );
    driver.exit().await;
}

fn background(running: &[&str]) -> RunnerEvent {
    RunnerEvent::Agent(AgentEvent::BackgroundTasks {
        running: running.iter().map(|&task| task.to_owned()).collect(),
        ended: running.is_empty(),
    })
}

/// One of the session's own subagents, going or done.
fn subagent(status: SubagentStatus) -> RunnerEvent {
    RunnerEvent::Agent(AgentEvent::Subagent {
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
    let mut driver = direct().await;
    working(
        &mut driver,
        vec![
            init(&[], &[]),
            subagent(SubagentStatus::Running),
            background(&["b1"]),
            writing(),
        ],
    )
    .await;
    driver.send(UiCommand::CancelTurn).await;
    driver
        .send(UiCommand::StopSubagent {
            id: "toolu_a".into(),
        })
        .await;
    let controls = controls_after(&driver, 2).await;
    assert_eq!(
        controls,
        [
            SessionControl::StopTurn,
            SessionControl::StopSubagent {
                id: "toolu_a".into()
            }
        ]
    );
    let cancelled = driver.seen().iter().any(|event| {
        matches!(
            event,
            AppEvent::Conversation {
                event: ConversationEvent::TurnCancelled { .. }
            }
        )
    });
    assert!(!cancelled, "the turn goes on for its subagents");
    driver.exit().await;

    let alone = vec![init(&[], &[]), writing()];
    let with_a_command = vec![
        init(&[], &[]),
        subagent(SubagentStatus::Done),
        background(&["bash-1"]),
        writing(),
    ];
    for events in [alone, with_a_command] {
        let mut driver = direct().await;
        working(&mut driver, events).await;
        driver.send(UiCommand::CancelTurn).await;
        driver
            .until(|event| {
                matches!(
                    event,
                    AppEvent::Conversation {
                        event: ConversationEvent::TurnCancelled { .. }
                    }
                )
            })
            .await;
        assert_eq!(controls_after(&driver, 1).await, [SessionControl::Stop]);
        driver.exit().await;
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
    let mut driver = TestDriver::start_with(CliOptions {
        prepare: Some(Box::new(|repo: &argonauts_testkit::TempRepo| {
            std::fs::write(
                repo.global_config(),
                "ultracode = true\n[captain]\nkind = \"checked\"\n",
            )
            .unwrap();
        })),
        ..CliOptions::default()
    })
    .await;
    let mut session = FakeSession::structured(change);
    let verdict = session.events.pop().expect("the result");
    session.events = vec![
        init(&[], &[]),
        subagent(SubagentStatus::Running),
        background(&["b1"]),
        writing(),
        RunnerEvent::Agent(AgentEvent::Error {
            category: ErrorCategory::Interrupted,
            message: "interrupted".into(),
            retryable: false,
        }),
        RunnerEvent::Agent(AgentEvent::Result {
            text: None,
            structured: None,
            is_error: true,
        }),
        subagent(SubagentStatus::Done),
        background(&[]),
        verdict,
    ];
    // Held once it writes, so Esc lands while the main agent still does.
    let writes = Hold::new();
    driver.agent.push(session.held_after(4, &writes));
    driver.submit("fix the install command in the README").await;
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Conversation {
                    event: ConversationEvent::Agent {
                        event: AgentEvent::TextDelta { .. },
                        ..
                    }
                }
            )
        })
        .await;
    driver.send(UiCommand::CancelTurn).await;
    // It goes on once it has taken the stop in.
    controls_after(&driver, 1).await;
    writes.release();
    let finished = driver
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
    let AppEvent::Conversation {
        event: ConversationEvent::TurnFinished { reply, .. },
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
    assert!(driver.conductor.starts().is_empty(), "no run was started");
    assert_eq!(controls_after(&driver, 1).await, [SessionControl::StopTurn]);
    driver.exit().await;
}

/// A turn stopped alone whose session ends with nothing more ends as any turn Esc stops: it
/// is cancelled, nothing failed, and no fallback takes the request.
#[tokio::test]
async fn a_turn_stopped_alone_with_nothing_after_it_ends_cancelled() {
    use argonauts_core::event::ErrorCategory;
    use argonauts_core::events::session::EndReason;

    let mut driver = direct().await;
    let mut session = FakeSession::ended(EndReason::Stopped);
    session.events = vec![
        init(&[], &[]),
        subagent(SubagentStatus::Running),
        background(&["b1"]),
        writing(),
        RunnerEvent::Agent(AgentEvent::Error {
            category: ErrorCategory::Interrupted,
            message: "interrupted".into(),
            retryable: false,
        }),
        RunnerEvent::Agent(AgentEvent::Result {
            text: None,
            structured: None,
            is_error: true,
        }),
    ];
    let writes = Hold::new();
    driver.agent.push(session.held_after(4, &writes));
    driver.submit("count in the background").await;
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Conversation {
                    event: ConversationEvent::Agent {
                        event: AgentEvent::TextDelta { .. },
                        ..
                    }
                }
            )
        })
        .await;
    driver.send(UiCommand::CancelTurn).await;
    // It goes on once it has taken the stop in.
    controls_after(&driver, 1).await;
    writes.release();
    let ended = driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Conversation {
                    event: ConversationEvent::TurnCancelled { .. }
                        | ConversationEvent::TurnFailed { .. }
                        | ConversationEvent::TurnFinished { .. }
                }
            )
        })
        .await;
    assert!(
        matches!(
            ended,
            AppEvent::Conversation {
                event: ConversationEvent::TurnCancelled { .. }
            }
        ),
        "{ended:?}"
    );
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
    assert_eq!(driver.agent.specs().len(), 1, "no fallback took it");
    driver.exit().await;
}
