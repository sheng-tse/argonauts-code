//! The main agent alone, kept to a goal, through the real controller loop with fake CLIs and
//! real check commands in a temp repository: Argonauts Code keeps the goal on Claude Code, Codex
//! keeps its own, the judge reads a line no command checks, and a pause stops the going round.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use argonauts_core::app::{AppEvent, ConversationEvent, UiCommand};
use argonauts_core::asking::{Ask, AskLine, AskReply};
use argonauts_core::input::SlashCommand;
use argonauts_core::session_runner::SessionSpec;
use argonauts_core::steps::StepName;
use argonauts_testkit::{FakeRunner, FakeSession};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::controller::test_support::{CliOptions, TestDriver};

const TASK: &str = "Write the release note.";
/// A goal one command checks, by its exit code.
const DONE: &str = "`test -f done.txt` holds";

/// The Captain workflow, so what you type goes to the main session in your checkout.
fn alone(config: &'static str) -> CliOptions {
    CliOptions {
        prepare: Some(Box::new(move |repo: &argonauts_testkit::TempRepo| {
            std::fs::write(
                repo.global_config(),
                format!("{config}\n[captain_or_crew]\nforce = \"captain\"\n"),
            )
            .unwrap();
        })),
        ..CliOptions::default()
    }
}

async fn slash(driver: &TestDriver, command: SlashCommand) {
    driver.send(UiCommand::Slash { command }).await;
}

async fn set_goal(driver: &mut TestDriver, condition: &str) {
    slash(
        driver,
        SlashCommand::Goal {
            text: Some(condition.to_owned()),
        },
    )
    .await;
    notice(driver, "Goal set").await;
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

fn noticed(driver: &TestDriver, part: &str) -> bool {
    driver
        .seen()
        .iter()
        .any(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains(part)))
}

/// Everything a session was sent, as one text.
fn sent(spec: &SessionSpec) -> String {
    spec.prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A main agent that answers `first` and changes nothing, then writes `done.txt` and answers
/// `then`.
fn done_on_the_second_turn(
    first: &'static str,
    then: &'static str,
) -> impl Fn(&SessionSpec) -> FakeSession + Send + Sync + 'static {
    let turns = Arc::new(AtomicU32::new(0));
    move |spec: &SessionSpec| {
        if turns.fetch_add(1, Ordering::SeqCst) == 0 {
            return FakeSession::completed(first);
        }
        std::fs::write(spec.workdir.join("done.txt"), "done\n").unwrap();
        FakeSession::completed(then)
    }
}

/// On Claude Code, which has no goal of its own, Argonauts Code keeps it: after each turn it runs
/// the check in your checkout and sends back what still fails, to the same session.
#[tokio::test]
async fn alone_on_claude_code_argonauts_keeps_the_goal() {
    let mut driver = TestDriver::start_with(alone("")).await;
    driver.agent.respond(done_on_the_second_turn(
        "Drafted the note.",
        "Wrote done.txt.",
    ));
    let worker = driver.agent.clone();
    set_goal(&mut driver, DONE).await;
    driver.submit(TASK).await;

    // The screen says who keeps it, then what each check found.
    notice(&mut driver, "Goal · kept by Argonauts Code").await;
    let checking = notice(&mut driver, "Checking the goal").await;
    assert_eq!(checking, "Checking the goal · checked by test -f done.txt");
    let failed = notice(&mut driver, "✗").await;
    assert!(failed.starts_with("✗ `test -f done.txt` holds"), "{failed}");
    notice(&mut driver, "Goal · try 2 · sent back what still fails").await;
    let met = notice(&mut driver, "Goal met").await;
    assert_eq!(met, "Goal met · 2 tries · kept by Argonauts Code");
    assert!(noticed(&driver, "✓ `test -f done.txt` holds"));
    let conductor = driver.conductor.clone();
    driver.exit().await;

    let specs = worker.specs();
    assert_eq!(specs.len(), 2, "two tries, two turns of the same session");
    let first = sent(&specs[0]);
    assert!(first.starts_with(TASK), "{first}");
    assert!(
        first.contains("This request is worked as a goal"),
        "{first}"
    );
    assert!(
        first.contains("- `test -f done.txt` holds (checked by test -f done.txt)"),
        "{first}"
    );
    // The next try reads only what still fails, in the session that did the work.
    let second = sent(&specs[1]);
    assert!(second.starts_with("The goal doesn't hold yet"), "{second}");
    assert!(second.contains("- `test -f done.txt` holds"), "{second}");
    assert!(!second.contains("worked as a goal"), "told once: {second}");
    assert!(
        conductor.starts().is_empty(),
        "no run: the main agent works alone"
    );
}

/// On Codex the goal is Codex's own: the turn is told to set it with its goal tool, a turn
/// that ends with the goal still active is carried on without a check in between, and
/// Argonauts Code runs the check once Codex says it's done.
#[tokio::test]
async fn alone_on_codex_the_goal_is_codexs_own_and_argonauts_still_checks() {
    let mut driver = TestDriver::start_with(alone("crew.main = \"codex\"\n")).await;
    let codex = driver
        .directory
        .add(
            FakeRunner::cli("codex", "openai").respond_with(done_on_the_second_turn(
                "Drafted the note.\ngoal: active · the file is still to write",
                "Wrote done.txt.\ngoal: complete · done.txt is there",
            )),
        );
    set_goal(&mut driver, DONE).await;
    driver.submit(TASK).await;

    notice(
        &mut driver,
        "Goal · kept by Codex's own goal · checked by Argonauts Code",
    )
    .await;
    let says = notice(&mut driver, "Codex says").await;
    assert_eq!(
        says,
        "Codex says · goal active · \"the file is still to write\""
    );
    let met = notice(&mut driver, "Goal met").await;
    assert_eq!(
        met,
        "Goal met · 1 try · kept by Codex's own goal · checked by Argonauts Code"
    );
    // One check, after Codex said it was done.
    let checks = driver
        .seen()
        .iter()
        .filter(|event| {
            matches!(event, AppEvent::Notice { text, .. } if text.starts_with("Checking the goal"))
        })
        .count();
    assert_eq!(checks, 1);
    driver.exit().await;

    let specs = codex.specs();
    assert_eq!(specs.len(), 2);
    let first = sent(&specs[0]);
    assert!(first.contains("with your own goal tool"), "{first}");
    assert!(
        first.contains("End your answer with one last line"),
        "{first}"
    );
    let second = sent(&specs[1]);
    assert!(
        second.starts_with("Continue working toward your goal"),
        "{second}"
    );
}

/// A line no command checks goes to the judge: an agent from another provider than the main
/// agent's reads your checkout against it, and its "not yet" goes back as the next try.
#[tokio::test]
async fn the_judge_reads_a_line_no_command_checks() {
    let mut driver = TestDriver::start_with(alone("")).await;
    driver
        .agent
        .respond(|_spec: &SessionSpec| FakeSession::completed("Wrote the note."));
    let worker = driver.agent.clone();
    let verdicts = Arc::new(AtomicU32::new(0));
    let asked = Arc::clone(&verdicts);
    let judge = driver
        .directory
        .add(
            FakeRunner::cli("codex", "openai").respond_with(move |spec: &SessionSpec| {
                assert_eq!(spec.step, Some(StepName::CheckGoal));
                let met = asked.fetch_add(1, Ordering::SeqCst) > 0;
                FakeSession::structured(json!({
                    "verdicts": [{
                        "id": "G1",
                        "verdict": if met { "met" } else { "not_yet" },
                        "evidence": if met { "NOTES.md names --dry-run" } else { "" },
                        "missing": if met { "" } else { "the note never names --dry-run" },
                    }],
                    "overall": if met { "met" } else { "not_yet" },
                    "reason": if met { "" } else { "the flag isn't named" },
                }))
            }),
        );
    set_goal(&mut driver, "the release note names the --dry-run flag").await;
    assert!(noticed(&driver, "Goal set · checked by the judge"));
    driver.submit(TASK).await;

    let failed = notice(&mut driver, "✗").await;
    assert_eq!(
        failed,
        "✗ the release note names the --dry-run flag · the note never names --dry-run"
    );
    let met = notice(&mut driver, "Goal met").await;
    assert_eq!(met, "Goal met · 2 tries · kept by Argonauts Code");
    driver.exit().await;

    // The judge read twice, read-only, and what it found missing went back to the worker.
    let read = judge.specs();
    assert_eq!(read.len(), 2);
    assert!(sent(&read[0]).contains("You are the judge of a goal"));
    assert!(sent(&read[0]).contains("the release note names the --dry-run flag"));
    let second = sent(&worker.specs()[1]);
    assert!(
        second.contains("the note never names --dry-run"),
        "{second}"
    );
}

/// `/goal pause` while the main agent works: the check that comes back sends nothing round,
/// and the next request isn't kept to the goal until it's resumed.
#[tokio::test]
async fn a_paused_goal_sends_nothing_round() {
    let mut driver = TestDriver::start_with(alone("")).await;
    driver
        .agent
        .respond(|_spec: &SessionSpec| FakeSession::completed("Drafted the note."));
    let worker = driver.agent.clone();
    set_goal(&mut driver, DONE).await;
    slash(
        &driver,
        SlashCommand::Goal {
            text: Some("pause".to_owned()),
        },
    )
    .await;
    notice(&mut driver, "Goal paused").await;
    driver.submit(TASK).await;
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
    let events = driver.exit().await;
    assert_eq!(worker.specs().len(), 1, "one turn, and nothing sent back");
    assert!(!sent(&worker.specs()[0]).contains("worked as a goal"));
    assert!(!events.iter().any(
        |event| matches!(event, AppEvent::Notice { text, .. } if text.contains("Checking the goal"))
    ));
}

/// Asks the turn's listener as the session's MCP server does, and waits for the answer.
async fn ask(spec: &SessionSpec, ask: Ask) -> AskReply {
    let asking = spec.asking.clone().expect("the turn listens for questions");
    let mut stream = TcpStream::connect(&asking.endpoint).await.unwrap();
    let line = AskLine {
        token: asking.token,
        ask,
    };
    let text = format!("{}\n", serde_json::to_string(&line).unwrap());
    stream.write_all(text.as_bytes()).await.unwrap();
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).await.unwrap();
    serde_json::from_str(answer.trim()).unwrap()
}

/// A call of `argonauts_set_goal` with these arguments, as the session's server sends it.
fn goal_tool(arguments: serde_json::Value) -> Ask {
    Ask::Tool {
        name: argonauts_mcp::session::SET_GOAL_TOOL.to_owned(),
        arguments,
    }
}

/// "Keep going until done.txt is there": the main session sets the goal with its tool, for
/// this request alone. The answer states it back, the turn's work is checked, and the next
/// request has no goal.
#[tokio::test]
async fn the_main_session_sets_a_goal_for_one_request_with_its_tool() {
    let mut driver = TestDriver::start_with(alone("")).await;
    // The tool goes to a CLI that takes Argonauts Code's tools when driven.
    driver.directory.taking_tools("claude-code");
    driver.agent.respond(|spec: &SessionSpec| {
        std::fs::write(spec.workdir.join("done.txt"), "done\n").unwrap();
        FakeSession::completed("Wrote done.txt.").with_delay(std::time::Duration::from_millis(300))
    });
    let worker = driver.agent.clone();
    driver
        .submit("Write the release note, and keep going until done.txt is there.")
        .await;
    let deadline = tokio::time::Instant::now() + crate::controller::test_support::WAIT;
    let spec = loop {
        if let Some(spec) = worker.specs().into_iter().next() {
            break spec;
        }
        assert!(tokio::time::Instant::now() < deadline, "no session started");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    // A call with nothing to check comes back as an error the session can act on.
    let refused = ask(&spec, goal_tool(json!({"condition": ""}))).await;
    assert!(refused.is_error, "{refused:?}");
    assert!(
        refused.text.contains("a goal needs a condition"),
        "{refused:?}"
    );
    let reply = ask(&spec, goal_tool(json!({"condition": DONE}))).await;
    assert!(!reply.is_error, "{reply:?}");
    assert!(
        reply
            .text
            .starts_with("Goal set for this request · checked by test -f done.txt."),
        "{}",
        reply.text
    );
    let met = notice(&mut driver, "Goal met").await;
    assert_eq!(met, "Goal met · 1 try · kept by Argonauts Code");
    assert!(noticed(
        &driver,
        "Goal set for this request · checked by test -f done.txt"
    ));
    // It covered that request alone.
    slash(&driver, SlashCommand::Goal { text: None }).await;
    notice(&mut driver, "No goal is set").await;

    driver.exit().await;
}
