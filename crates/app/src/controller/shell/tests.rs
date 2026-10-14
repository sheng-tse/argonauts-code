//! `!` commands: the pieces on their own, then through the real controller loop.

use argonauts_core::app::{AppEvent, AppMode, ConversationEvent, ShellEvent, UiCommand};
use argonauts_core::events::session::InputPartKind;
use argonauts_testkit::FakeSession;
use serde_json::json;

use super::*;
use crate::controller::test_support::TestDriver;

fn ran(command: &str, output: &str, exit_code: Option<i32>, stopped: bool) -> ShellRan {
    ShellRan {
        id: 1,
        command: command.to_owned(),
        folder: None,
        output: output.to_owned(),
        exit_code,
        stopped,
    }
}

#[test]
fn a_read_that_cuts_a_character_leaves_it_for_the_next() {
    let mut bytes = "héllo".as_bytes()[..2].to_vec();
    assert_eq!(whole_chars(&mut bytes), "h");
    assert_eq!(bytes, vec![0xc3]);
    bytes.extend_from_slice(&"héllo".as_bytes()[2..]);
    assert_eq!(whole_chars(&mut bytes), "éllo");
    assert!(bytes.is_empty());
    let mut broken = vec![b'a', 0xff, b'b'];
    assert_eq!(whole_chars(&mut broken), "a\u{fffd}b");
}

#[test]
fn progress_bars_settle_to_their_last_state() {
    assert_eq!(settled("one\r\ntwo\n"), "one\ntwo");
    assert_eq!(settled("10%\r50%\r100%\ndone\n"), "100%\ndone");
    assert_eq!(settled("half\r"), "half");
}

#[test]
fn long_output_keeps_its_start_and_end() {
    let mut kept = Kept::default();
    let line = format!("{}\n", "x".repeat(99));
    for _ in 0..1000 {
        kept.push(&line);
    }
    kept.push("the last line\n");
    let text = kept.text();
    assert!(text.len() < KEPT_BYTES + 100, "{}", text.len());
    assert!(text.starts_with("xxx"));
    assert!(text.ends_with("the last line\n"));
    assert!(text.contains("lines cut …]"), "{text}");

    let mut short = Kept::default();
    short.push("hi\n");
    assert_eq!(short.text(), "hi\n");
}

/// What the agent hears of the commands names the folder they ran in, which is a run's worktree
/// for a conversation that carries on one of its agents' sessions.
#[test]
fn the_commands_say_where_they_ran() {
    let worktree = std::path::PathBuf::from("/work/argonauts/worktrees/shop/run-01/task-001");
    let elsewhere = ShellRan {
        folder: Some(worktree.clone()),
        ..ran(
            "pwd",
            "/work/argonauts/worktrees/shop/run-01/task-001\n",
            Some(0),
            false,
        )
    };
    let text = context_text(std::slice::from_ref(&elsewhere)).unwrap();
    assert!(
        text.starts_with(
            "Before this message I ran these commands myself, in \
             /work/argonauts/worktrees/shop/run-01/task-001."
        ),
        "{text}"
    );
    let text = context_text(&[ran("ls", "a\n", Some(0), false), elsewhere]).unwrap();
    assert!(
        text.starts_with(
            "Before this message I ran these commands myself, each in the folder named after it."
        ),
        "{text}"
    );
    assert!(text.contains("\n$ ls\n(in the repo folder)\n"), "{text}");
    assert!(
        text.contains("\n$ pwd\n(in /work/argonauts/worktrees/shop/run-01/task-001)\n"),
        "{text}"
    );
}

#[test]
fn commands_waiting_for_a_message_keep_the_newest_few() {
    let mut held = Vec::new();
    for n in 0..6 {
        let output = "x".repeat(KEPT_BYTES - 100);
        hold(&mut held, ran(&format!("cmd {n}"), &output, Some(0), false));
    }
    let kept: Vec<&str> = held.iter().map(|ran| ran.command.as_str()).collect();
    assert_eq!(kept, ["cmd 3", "cmd 4", "cmd 5"]);

    // One command bigger than the whole limit still goes.
    let mut one = Vec::new();
    hold(
        &mut one,
        ran("cat big", &"y".repeat(HELD_BYTES + 1), Some(0), false),
    );
    assert_eq!(one.len(), 1);
}

#[test]
fn the_main_agent_reads_each_command_with_its_output_and_how_it_ended() {
    assert_eq!(context_part(&[]), None);
    let part = context_part(&[
        ran("echo hi", "hi\n", Some(0), false),
        ran("false", "", Some(1), false),
        ran("sleep 25", "", None, true),
    ])
    .expect("a part");
    assert_eq!(part.kind, InputPartKind::Message);
    assert_eq!(
        part.text,
        "Before this message I ran these commands myself, in the repo folder. Their output \
         is here as context.\n\n$ echo hi\nhi\n\n$ false\n(exit code 1)\n\n$ sleep 25\n\
         (stopped with Ctrl+C)\n"
    );
}

fn answer(text: &str) -> FakeSession {
    FakeSession::structured(json!({"kind": "answer", "answer_markdown": text, "estimate": null}))
}

/// How long a started shell may take to say something. Starting your shell and stopping it
/// are slow while every other test runs beside this one.
const SHELL_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

fn shell(command: &str) -> UiCommand {
    UiCommand::Shell {
        command: command.to_owned(),
    }
}

fn finished(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Shell {
            event: ShellEvent::Finished { .. }
        }
    )
}

/// Everything the `!` commands printed so far, in order.
fn printed(driver: &TestDriver) -> String {
    driver
        .seen()
        .iter()
        .filter_map(|event| match event {
            AppEvent::Shell {
                event: ShellEvent::Output { text, .. },
            } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn prompt_text(driver: &TestDriver, index: usize) -> String {
    driver.agent.specs()[index]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect()
}

async fn turn_done(driver: &mut TestDriver) {
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
async fn a_command_runs_in_the_repo_and_waits_for_your_next_message() {
    let mut driver = TestDriver::start().await;
    driver
        .send(shell("echo hi; echo oops >&2; touch made-here"))
        .await;
    let started = driver
        .until(|event| matches!(event, AppEvent::Shell { .. }))
        .await;
    assert_eq!(
        started,
        AppEvent::Shell {
            event: ShellEvent::Started {
                id: 1,
                command: "echo hi; echo oops >&2; touch made-here".into()
            }
        }
    );
    let end = driver.until(finished).await;
    assert_eq!(
        end,
        AppEvent::Shell {
            event: ShellEvent::Finished {
                id: 1,
                exit_code: Some(0),
                stopped: false
            }
        }
    );
    let output = printed(&driver);
    assert!(
        output.contains("hi\n") && output.contains("oops\n"),
        "{output:?}"
    );
    assert!(driver.repo.root().join("made-here").exists());
    assert!(
        driver.agent.specs().is_empty(),
        "a command never starts a turn by itself"
    );

    driver.agent.push(answer("It printed hi."));
    driver.agent.push(answer("Nothing new."));
    driver.submit("what did that print?").await;
    turn_done(&mut driver).await;
    let spec = &driver.agent.specs()[0];
    let part = spec
        .prompt
        .iter()
        .find(|part| part.label == "shell commands")
        .expect("the commands reach the main agent");
    assert_eq!(part.kind, InputPartKind::Message);
    assert!(
        part.text
            .contains("\n$ echo hi; echo oops >&2; touch made-here\n"),
        "{}",
        part.text
    );
    let prompt = prompt_text(&driver, 0);
    assert!(prompt.ends_with("what did that print?\n"), "{prompt}");
    assert!(
        prompt.find("$ echo hi") < prompt.find("what did that print?"),
        "the commands come before your message"
    );
    let echoed = driver.seen().iter().any(|event| {
        matches!(
            event,
            AppEvent::Conversation { event: ConversationEvent::TurnStarted { text, .. } }
                if text == "what did that print?"
        )
    });
    assert!(echoed, "your message shows as you typed it");

    driver.submit("and now?").await;
    turn_done(&mut driver).await;
    assert!(
        !prompt_text(&driver, 1).contains("$ echo hi"),
        "the main agent hears of a command once"
    );
    driver.exit().await;
}

#[tokio::test]
async fn ctrl_c_stops_a_command_and_one_runs_at_a_time() {
    let mut driver = TestDriver::start().await;
    driver.send(shell("sleep 30")).await;
    driver
        .until_within(SHELL_WAIT, |event| matches!(event, AppEvent::Shell { .. }))
        .await;
    driver.send(shell("echo second")).await;
    driver
        .until(
            |event| matches!(event, AppEvent::Notice { text, .. } if text == super::STILL_RUNNING),
        )
        .await;
    let started = std::time::Instant::now();
    driver.send(UiCommand::StopShell).await;
    let end = driver.until_within(SHELL_WAIT, finished).await;
    assert!(
        matches!(
            end,
            AppEvent::Shell {
                event: ShellEvent::Finished {
                    id: 1,
                    stopped: true,
                    ..
                }
            }
        ),
        "{end:?}"
    );
    assert!(
        started.elapsed() < SHELL_WAIT,
        "the stop ended it, well before its 30 seconds were up"
    );

    // A failing command says how it ended, and the next one gets its own number.
    driver.send(shell("exit 3")).await;
    let end = driver.until_within(SHELL_WAIT, finished).await;
    assert_eq!(
        end,
        AppEvent::Shell {
            event: ShellEvent::Finished {
                id: 2,
                exit_code: Some(3),
                stopped: false
            }
        }
    );
    driver.agent.push(answer("Noted."));
    driver.submit("why did those end?").await;
    turn_done(&mut driver).await;
    let prompt = prompt_text(&driver, 0);
    assert!(
        prompt.contains("$ sleep 30\n(stopped with Ctrl+C)\n"),
        "{prompt}"
    );
    assert!(prompt.contains("$ exit 3\n(exit code 3)\n"), "{prompt}");
    driver.exit().await;
}

#[tokio::test]
async fn a_fallback_hears_of_the_commands_too() {
    use argonauts_core::events::session::EndReason;
    use argonauts_testkit::FakeRunner;

    let mut driver = TestDriver::start_with(crate::controller::test_support::CliOptions {
        flags: vec![r#"fallback.main.order=["codex"]"#.to_owned()],
        ..Default::default()
    })
    .await;
    let codex = driver
        .directory
        .add(FakeRunner::cli("codex", "openai").then(answer("From codex.")));
    driver
        .agent
        .push(FakeSession::ended(EndReason::RateLimited));
    driver.send(shell("echo hi")).await;
    driver.until(finished).await;
    driver.submit("what did that print?").await;
    turn_done(&mut driver).await;
    let first = prompt_text(&driver, 0);
    let retry: String = codex.specs()[0]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect();
    assert!(first.contains("$ echo hi\nhi\n"), "{first}");
    assert!(retry.contains("$ echo hi\nhi\n"), "{retry}");
    driver.exit().await;
}

/// Polls state the events don't show, such as what the fake conductor received.
async fn eventually(what: impl Fn() -> bool) {
    use crate::controller::test_support::WAIT;
    let deadline = tokio::time::Instant::now() + WAIT;
    while !what() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "not within {WAIT:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

fn guidance_sent(driver: &TestDriver) -> Vec<String> {
    use crate::controller::test_support::Call;
    driver
        .conductor
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::Guidance { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

/// During a run your message goes to the run as guidance, and the commands go with it. What
/// you ran before the run started was for the conversation, and stays out of it.
#[tokio::test]
async fn in_a_run_the_commands_go_with_your_guidance_cleaned_of_escapes() {
    use argonauts_core::input::SlashCommand;

    let mut driver =
        TestDriver::start_with(crate::controller::conversation::checked_captain()).await;
    driver.send(shell("echo before")).await;
    driver.until(finished).await;
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Captain {
                task: "fix the flaky test".to_owned(),
            },
        })
        .await;
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Mode {
                    mode: AppMode::Working { .. }
                }
            )
        })
        .await;
    driver.submit("keep going").await;
    eventually(|| guidance_sent(&driver).len() == 1).await;
    assert_eq!(guidance_sent(&driver), ["keep going"]);

    driver
        .send(shell(
            "printf 'FAILED test_x\\033]0;owned\\007\\033[31m!\\033[0m\\n'",
        ))
        .await;
    driver.until(finished).await;
    driver.submit("the test fails, see above").await;
    eventually(|| guidance_sent(&driver).len() == 2).await;
    let sent = guidance_sent(&driver).remove(1);
    assert!(sent.starts_with("the test fails, see above\n\n"), "{sent}");
    assert!(sent.contains("FAILED test_x!\n"), "{sent:?}");
    assert!(!sent.contains(['\u{1b}', '\u{7}']), "{sent:?}");
    assert!(!sent.contains("echo before"), "{sent}");

    driver.submit("thanks").await;
    eventually(|| guidance_sent(&driver).len() == 3).await;
    assert_eq!(guidance_sent(&driver)[2], "thanks", "heard of once");
    driver.exit().await;
}

/// A message you typed while the main agent answered went out after a command you ran later, which
/// it isn't about: the command waits for your next message.
#[tokio::test]
async fn a_message_typed_before_a_command_ended_goes_without_it() {
    let mut driver = TestDriver::start().await;
    driver
        .agent
        .push(answer("One.").with_delay(std::time::Duration::from_millis(1500)));
    driver.agent.push(answer("Two."));
    driver.agent.push(answer("Three."));
    driver.submit("first").await;
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Conversation {
                    event: ConversationEvent::TurnStarted { .. }
                }
            )
        })
        .await;
    driver.submit("second").await;
    driver.send(shell("echo later")).await;
    turns_finished(&mut driver, 2).await;
    assert!(
        !prompt_text(&driver, 1).contains("$ echo later"),
        "{}",
        prompt_text(&driver, 1)
    );

    driver.submit("third").await;
    turns_finished(&mut driver, 3).await;
    assert!(prompt_text(&driver, 2).contains("$ echo later\nlater\n"));
    driver.exit().await;
}

async fn turns_finished(driver: &mut TestDriver, count: usize) {
    let finished = |driver: &TestDriver| {
        driver
            .seen()
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    AppEvent::Conversation {
                        event: ConversationEvent::TurnFinished { .. }
                    }
                )
            })
            .count()
    };
    while finished(driver) < count {
        driver.next().await;
    }
}

#[tokio::test]
async fn a_new_conversation_leaves_the_commands_behind() {
    use argonauts_core::input::SlashCommand;

    let mut driver = TestDriver::start().await;
    driver.send(shell("echo old")).await;
    driver.until(finished).await;
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Clear,
        })
        .await;
    driver.agent.push(answer("Fresh."));
    driver.submit("hello").await;
    turn_done(&mut driver).await;
    assert!(!prompt_text(&driver, 0).contains("echo old"));
    driver.exit().await;
}
