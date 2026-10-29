//! The main session is the CLI you chose, through the real
//! controller loop with fake CLIs: with a crew workflow in use your message still goes to the
//! main session as you typed it, talking starts nothing, the session hands work over by calling
//! a tool, and your words decide for one request. A CLI that can't take tools keeps the
//! verdict.

use std::time::Duration;

use argonauts_core::app::{AppEvent, ChoiceKind, ConversationEvent, TurnReply, UiCommand};
use argonauts_core::asking::{Ask, AskLine, AskReply};
use argonauts_core::events::run::{RunMode, StartsFrom};
use argonauts_core::input::SlashCommand;
use argonauts_core::session_runner::{Access, SessionSpec};
use argonauts_testkit::FakeSession;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::client::test_support::session_started;
use crate::controller::runs::rig;
use crate::controller::test_support::{Call as ConductorCall, CliOptions, TestDriver, max_wait};
use argonauts_core::RunId;
use argonauts_core::events::run::RunOutcome;
use argonauts_core::events::{RunEvent, RunEventKind, RunLifecycle};
use argonauts_core::names::{RoleName, SessionKey};

const START_WORKFLOW: &str = "argonauts_start_workflow";
const QUESTION: &str = "what does parse_due do when the date has no year?";
const WORK: &str = "ok. add support for dates without a year, across the cli, the store and \
                    the tests";

/// The default settings, where council is the workflow in use, with whatever `config` adds.
fn with_config(config: &'static str) -> CliOptions {
    CliOptions {
        prepare: Some(Box::new(move |repo: &argonauts_testkit::TempRepo| {
            std::fs::write(repo.global_config(), config).unwrap();
        })),
        ..CliOptions::default()
    }
}

/// Claude Code as the main agent: it takes Argonauts Code's tools when driven.
async fn claude_code(config: &'static str) -> TestDriver {
    let driver = TestDriver::start_with(with_config(config)).await;
    driver.directory.taking_tools("claude-code");
    driver
}

fn turn_ended(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Conversation {
            event: ConversationEvent::TurnFinished { .. } | ConversationEvent::TurnFailed { .. }
        }
    )
}

async fn spec_number(driver: &TestDriver, number: usize) -> SessionSpec {
    let deadline = tokio::time::Instant::now() + max_wait();
    loop {
        if let Some(spec) = driver.agent.specs().into_iter().nth(number) {
            return spec;
        }
        assert!(tokio::time::Instant::now() < deadline, "no session started");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Calls one of the main session's tools as the session's MCP server does.
async fn call(spec: &SessionSpec, name: &str, arguments: Value) -> AskReply {
    let asking = spec.asking.clone().expect("the turn listens");
    let mut stream = TcpStream::connect(&asking.endpoint).await.unwrap();
    let line = AskLine {
        token: asking.token,
        ask: Ask::Tool {
            name: name.to_owned(),
            arguments,
        },
    };
    let text = format!("{}\n", serde_json::to_string(&line).unwrap());
    stream.write_all(text.as_bytes()).await.unwrap();
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).await.unwrap();
    serde_json::from_str(answer.trim()).unwrap()
}

/// Calls the tool on a task of its own, since its answer may wait on the run.
fn calling(
    spec: &SessionSpec,
    name: &'static str,
    arguments: Value,
) -> tokio::task::JoinHandle<AskReply> {
    let spec = spec.clone();
    tokio::spawn(async move { call(&spec, name, arguments).await })
}

/// The run the app started and now follows, once its stream is open.
pub(super) async fn followed_run(driver: &TestDriver) -> RunId {
    let deadline = tokio::time::Instant::now() + max_wait();
    loop {
        let followed = driver
            .conductor
            .calls()
            .into_iter()
            .find_map(|call| match call {
                ConductorCall::Subscribe { run, .. } => Some(run),
                _ => None,
            });
        if let Some(run) = followed {
            return run;
        }
        assert!(tokio::time::Instant::now() < deadline, "no run followed");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The start tool's answer, once the run it started is under way.
async fn handed_over(driver: &TestDriver, spec: &SessionSpec, arguments: Value) -> AskReply {
    let answer = calling(spec, START_WORKFLOW, arguments);
    let run = followed_run(driver).await;
    driver.conductor.push(&run, agent_started(&run, 1)).await;
    answer.await.unwrap()
}

/// The run's first agent started.
pub(super) fn agent_started(run: &RunId, seq: u64) -> RunEvent {
    let key = SessionKey::numbered(1);
    RunEvent {
        run_id: run.clone(),
        ..session_started(seq, &key, None, RoleName::Worker, "codex")
    }
}

fn failed(run: &RunId, seq: u64, detail: &str) -> RunEvent {
    let finished = RunLifecycle::Finished {
        outcome: RunOutcome::Failed,
        detail: Some(detail.to_owned()),
    };
    rig::event(run, seq, RunEventKind::Run(finished))
}

const GIT_REFUSED: &str = "`git add --all -- .argonauts/goal/run-001.md` failed (exit status: \
                           1): The following paths are ignored by one of your .gitignore \
                           files:\n.argonauts\nhint: Use -f if you really want to add them.";

fn setup_of(spec: &SessionSpec) -> Option<&str> {
    spec.prompt
        .iter()
        .map(|part| part.text.as_str())
        .find(|text| text.contains("Argonauts Code setup:"))
}

/// A question with council in use: the main session answers it in your checkout, as its CLI
/// would, and nothing starts. No estimate, no verdict, no run, no question about starting work.
#[tokio::test]
async fn with_a_crew_workflow_a_question_is_answered_and_nothing_starts() {
    let mut driver = claude_code("").await;
    let root = driver.repo.root().to_owned();
    driver.agent.push(FakeSession::completed(
        "It returns an error. parse_due only accepts full ISO dates.",
    ));
    driver.submit(QUESTION).await;
    let ended = driver.until(turn_ended).await;
    assert!(matches!(
        &ended,
        AppEvent::Conversation { event: ConversationEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. } }
            if markdown.starts_with("It returns an error.")
    ));
    let spec = spec_number(&driver, 0).await;
    // The CLI itself: your checkout, your words first, no schema, so no verdict to read.
    assert_eq!(spec.access, Access::Direct);
    assert_eq!(spec.workdir, root);
    assert!(spec.json_schema.is_none());
    assert_eq!(spec.prompt[0].text, QUESTION);
    let asking = spec.asking.as_ref().expect("the turn listens");
    assert_eq!(
        asking.tools,
        [
            START_WORKFLOW,
            "argonauts_set_goal",
            "argonauts_setup",
            "argonauts_guide",
            "argonauts_lookup"
        ]
    );
    let standing = asking.standing.as_deref().expect("standing lines");
    assert!(standing.contains("A question, or talking something over: answer it."));
    assert!(
        standing.contains("say that nothing is changed"),
        "{standing}"
    );
    let setup = setup_of(&spec).expect("the setup in use");
    assert!(setup.contains("the council workflow is in use"), "{setup}");
    assert!(setup.contains("ultracode is off"), "{setup}");

    let events = driver.exit().await;
    assert!(cli_started_nothing(&events));
}

fn cli_started_nothing(events: &[AppEvent]) -> bool {
    !events.iter().any(|event| {
        matches!(event, AppEvent::Run { .. })
            || matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::StartWork)
    })
}

/// Real work: the session calls its tool, and the workflow in use starts through the one entry
/// point, as a crew run with your words first and the session's account after them. One
/// workflow a request.
#[tokio::test]
async fn real_work_starts_the_workflow_in_use_when_the_session_hands_it_over() {
    let driver = claude_code("").await;
    driver.agent.push(
        FakeSession::completed("That touches three parts, so I'm handing it to the crew.")
            .with_delay(Duration::from_secs(3)),
    );
    driver.submit(WORK).await;
    let spec = spec_number(&driver, 0).await;
    assert!(
        driver.conductor.starts().is_empty(),
        "sending starts nothing"
    );

    let account = "Accept dates without a year in parse_due, in the cli, the store and the tests.";
    let started = calling(&spec, START_WORKFLOW, json!({ "request": account }));
    let run = followed_run(&driver).await;
    // The answer waits until the run is under way.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !started.is_finished(),
        "the tool answered before the run began"
    );
    driver.conductor.push(&run, agent_started(&run, 1)).await;
    let started = started.await.unwrap();
    assert!(!started.is_error, "{started:?}");
    assert!(
        started
            .text
            .starts_with("The council workflow started on this."),
        "{started:?}"
    );
    let start = driver.conductor.starts().remove(0);
    assert_eq!(start.mode, RunMode::Crew);
    assert_eq!(
        start.request,
        format!("{WORK}\n\nFrom the conversation, as the main agent put it: {account}")
    );
    assert!(start.conversation_turn.is_some());

    let again = call(&spec, START_WORKFLOW, json!({ "request": account })).await;
    assert!(
        again.text.starts_with("A workflow already started"),
        "{again:?}"
    );
    assert_eq!(driver.conductor.starts().len(), 1);
    driver.exit().await;
}

/// A run that fails before any agent starts: the tool answers with the failure, as a failure,
/// and never says the run started. The session that heard it says so, so it isn't told again.
#[tokio::test]
async fn a_run_that_fails_at_start_is_answered_as_a_failure() {
    let mut driver = claude_code("").await;
    driver.agent.push(
        FakeSession::completed("The council workflow didn't start: git refused its goal file.")
            .with_delay(Duration::from_secs(2)),
    );
    driver.submit(WORK).await;
    let spec = spec_number(&driver, 0).await;
    let answer = calling(&spec, START_WORKFLOW, json!({ "request": "Add dates." }));
    let run = followed_run(&driver).await;
    driver
        .conductor
        .push(&run, failed(&run, 1, GIT_REFUSED))
        .await;
    let answer = answer.await.unwrap();
    assert!(answer.is_error, "{answer:?}");
    assert!(!answer.text.contains("started on this"), "{answer:?}");
    assert!(
        answer.text.starts_with(
            "The council workflow failed before any agent started: `git add --all -- \
             .argonauts/goal/run-001.md` failed (exit status: 1): The following paths are \
             ignored by one of your .gitignore files. Tell the user in one line"
        ),
        "{answer:?}"
    );
    driver.until(turn_ended).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(driver.agent.specs().len(), 1, "the session was told twice");
    driver.exit().await;
}

/// A run that fails before it began doesn't stand as this request's workflow: a second call
/// in the same turn starts one, rather than hearing that one already started.
#[tokio::test]
async fn after_a_failed_start_the_session_may_start_again() {
    let driver = claude_code("").await;
    driver.agent.push(
        FakeSession::completed("It failed, so I tried again.").with_delay(Duration::from_secs(3)),
    );
    driver.submit(WORK).await;
    let spec = spec_number(&driver, 0).await;
    let answer = calling(&spec, START_WORKFLOW, json!({ "request": "Add dates." }));
    let run = followed_run(&driver).await;
    driver
        .conductor
        .push(&run, failed(&run, 1, GIT_REFUSED))
        .await;
    assert!(answer.await.unwrap().is_error);

    let again = calling(&spec, START_WORKFLOW, json!({ "request": "Add dates." }));
    let second = tokio::time::timeout(max_wait(), async {
        loop {
            let other = driver
                .conductor
                .calls()
                .into_iter()
                .find_map(|call| match call {
                    ConductorCall::Subscribe { run: other, .. } if other != run => Some(other),
                    _ => None,
                });
            if let Some(other) = other {
                break other;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a second run started");
    driver
        .conductor
        .push(&second, agent_started(&second, 1))
        .await;
    let again = again.await.unwrap();
    assert!(
        !again.text.starts_with("A workflow already started"),
        "{again:?}"
    );
    assert_eq!(driver.conductor.starts().len(), 2);
    driver.exit().await;
}

/// A run that waits for its CLIs before any agent starts isn't said to have started: the tool
/// says what it waits for and that nothing runs until then.
#[tokio::test]
async fn a_run_that_waits_for_its_clis_is_not_said_to_have_started() {
    use argonauts_core::events::run::WaitReason;

    let driver = claude_code("").await;
    driver.agent.push(
        FakeSession::completed("It waits for a CLI to come back.")
            .with_delay(Duration::from_secs(2)),
    );
    driver.submit(WORK).await;
    let spec = spec_number(&driver, 0).await;
    let answer = calling(&spec, START_WORKFLOW, json!({ "request": "Add dates." }));
    let run = followed_run(&driver).await;
    let waiting = RunLifecycle::Waiting {
        reason: WaitReason::AllAgentsOut { until: None },
        waiting_on: Vec::new(),
    };
    driver
        .conductor
        .push(&run, rig::event(&run, 1, RunEventKind::Run(waiting)))
        .await;
    let answer = answer.await.unwrap();
    assert!(!answer.is_error, "{answer:?}");
    assert!(!answer.text.contains("started on this"), "{answer:?}");
    assert!(
        answer.text.starts_with(
            "The council workflow is waiting: every agent's CLI is out of plan usage or down \
             until one comes back; nothing runs until then."
        ),
        "{answer:?}"
    );
    driver.exit().await;
}

/// A run that fails after it began: the session is told in a turn of its own, which shows no
/// message of yours, reads only, and says so in one line.
#[tokio::test]
async fn a_run_that_fails_later_is_told_to_the_session() {
    let mut driver = claude_code("").await;
    driver
        .agent
        .push(FakeSession::completed("Passed to the crew.").with_delay(Duration::from_secs(2)));
    let said = "The council run failed: git refused its goal file.";
    driver
        .agent
        .push(FakeSession::structured(json!({ "answer_markdown": said })));
    driver.submit(WORK).await;
    let spec = spec_number(&driver, 0).await;
    let answer = calling(&spec, START_WORKFLOW, json!({ "request": "Add dates." }));
    let run = followed_run(&driver).await;
    driver.conductor.push(&run, agent_started(&run, 1)).await;
    assert!(!answer.await.unwrap().is_error);
    driver.until(turn_ended).await;

    driver
        .conductor
        .push(&run, failed(&run, 2, GIT_REFUSED))
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
    assert!(
        matches!(
            &started,
            AppEvent::Conversation {
                event: ConversationEvent::TurnStarted {
                    continues: true,
                    read_only: true,
                    ..
                }
            }
        ),
        "{started:?}"
    );
    let told = spec_number(&driver, 1).await;
    let prompt: String = told.prompt.iter().map(|part| part.text.as_str()).collect();
    assert!(
        prompt.contains(
            "run-001, the run started for the user's request, failed: `git add --all -- \
             .argonauts/goal/run-001.md` failed (exit status: 1)"
        ),
        "{prompt}"
    );
    assert!(prompt.contains("Tell the user in one line"), "{prompt}");
    assert!(
        told.asking
            .as_ref()
            .is_none_or(|asking| asking.tools.is_empty())
    );
    let ended = driver.until(turn_ended).await;
    assert!(matches!(
        &ended,
        AppEvent::Conversation { event: ConversationEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. } }
            if markdown == said
    ));
    driver.exit().await;
}

/// A step a setup kept for this request goes with the workflow the request starts, so the
/// runner places it as it does the steps `always.toml` lists.
#[tokio::test]
async fn a_step_kept_for_this_request_goes_with_the_workflow_it_starts() {
    let driver = claude_code("").await;
    driver.agent.push(
        FakeSession::completed("Kept, and passed to the crew.").with_delay(Duration::from_secs(3)),
    );
    driver
        .submit("just for this one, run the benchmarks before sign-off. add due dates")
        .await;
    let spec = spec_number(&driver, 0).await;
    let kept = call(
        &spec,
        "argonauts_setup",
        json!({
            "asked": "Run the benchmarks before sign-off, for this request.",
            "steps": [{"name": "Benchmark", "role": "worker",
                       "does": "Run the benchmarks.", "when": "before sign-off"}],
            "keep": "request",
            "keep_says": "just for this one",
        }),
    )
    .await;
    assert!(!kept.is_error, "{kept:?}");
    let started = handed_over(&driver, &spec, json!({ "request": "Add due dates." })).await;
    assert!(!started.is_error, "{started:?}");
    let deadline = tokio::time::Instant::now() + max_wait();
    while driver.conductor.starts().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "no run started");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let start = driver.conductor.starts().remove(0);
    let steps: Vec<&str> = start
        .required_steps
        .iter()
        .map(|step| step.name.as_str())
        .collect();
    assert_eq!(steps, ["Benchmark"]);
    driver.exit().await;
}

/// "Just do it yourself" keeps one request with the session whatever is in use, and the words
/// must be yours: the same call for a message that never said so starts the workflow.
#[tokio::test]
async fn your_words_keep_one_request_with_the_main_session() {
    let driver = claude_code("").await;
    driver
        .agent
        .push(FakeSession::completed("Done.").with_delay(Duration::from_secs(3)));
    driver
        .submit("Just do it yourself: add due dates across the cli and the store.")
        .await;
    let spec = spec_number(&driver, 0).await;
    let how_to_run = json!([{"says": "Just do it yourself", "kind": "mode", "value": "captain"}]);
    let kept = call(
        &spec,
        START_WORKFLOW,
        json!({ "request": "add due dates", "how_to_run": how_to_run }),
    )
    .await;
    assert_eq!(
        kept.text,
        "The user asked you to do this one yourself, so do it here."
    );
    assert!(driver.conductor.starts().is_empty());
    driver.exit().await;
}

/// The setup travels with your message once, and again only when it changed: a resumed
/// session keeps its instructions from the first request, so nothing else would tell it.
#[tokio::test]
async fn the_setup_is_said_once_and_again_when_it_changes() {
    let mut driver = claude_code("").await;
    driver.agent.push(FakeSession::completed("One."));
    driver.agent.push(FakeSession::completed("Two."));
    driver.agent.push(FakeSession::completed("Three."));
    driver.submit("first").await;
    driver.until(turn_ended).await;
    driver.submit("second").await;
    driver.until(turn_ended).await;
    let first = spec_number(&driver, 0).await;
    let second = spec_number(&driver, 1).await;
    assert!(setup_of(&first).is_some());
    assert!(
        second.resume.is_some(),
        "the conversation resumes its session"
    );
    assert_eq!(setup_of(&second), None, "nothing changed");
    assert_eq!(
        first.asking.as_ref().unwrap().standing,
        second.asking.as_ref().unwrap().standing
    );

    // The typed word `ultracode` sends one request to the workflow in use, whatever its size.
    driver
        .send(UiCommand::Submit {
            text: "third, with ultracode".to_owned(),
            workflow_requested: true,
            pictures: Vec::new(),
        })
        .await;
    driver
        .until(|event| matches!(event, AppEvent::Attached { .. }))
        .await;
    let started = driver.conductor.starts();
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(started[0].request, "third, with ultracode");
    assert_eq!(started[0].mode, RunMode::Crew);
    driver.exit().await;
}

/// With the keyword trigger off, a prompt that says `ultracode` is a turn like any other.
#[tokio::test]
async fn with_the_trigger_off_the_word_starts_no_workflow() {
    let mut driver = claude_code("[workflows]\nultracode_trigger = false\n").await;
    driver.agent.push(FakeSession::completed("Done."));
    driver
        .send(UiCommand::Submit {
            text: "tidy the README, ultracode".to_owned(),
            workflow_requested: true,
            pictures: Vec::new(),
        })
        .await;
    driver.until(turn_ended).await;
    assert!(driver.conductor.starts().is_empty());
    let spec = spec_number(&driver, 0).await;
    assert!(
        spec.prompt
            .iter()
            .any(|part| part.text.contains("tidy the README, ultracode"))
    );
    assert!(spec.preferences.workflow_word_off, "Claude Code's own too");
    driver.exit().await;
}

/// Thinking, fast mode, and the language you set go with each turn of the session you talk to,
/// for its CLI to take as settings of its own; nothing of them by default.
#[tokio::test]
async fn your_thinking_speed_and_language_go_with_each_turn() {
    let mut driver =
        claude_code("[main_session]\nthinking = false\nfast = true\nlanguage = \"japanese\"\n")
            .await;
    driver.agent.push(FakeSession::completed("Done."));
    driver.submit(WORK).await;
    driver.until(turn_ended).await;
    let spec = spec_number(&driver, 0).await;
    let preferences = &spec.preferences;
    assert!(preferences.thinking_off && preferences.fast);
    assert_eq!(preferences.language.as_deref(), Some("japanese"));
    assert!(!preferences.workflow_word_off);
    driver.exit().await;

    let mut driver = claude_code("").await;
    driver.agent.push(FakeSession::completed("Done."));
    driver.submit(WORK).await;
    driver.until(turn_ended).await;
    assert!(spec_number(&driver, 0).await.preferences.is_default());
    driver.exit().await;
}

/// With Captain in use nothing leaves the main session unless you ask: the call alone is refused.
#[tokio::test]
async fn in_captain_the_session_s_own_judgment_starts_nothing() {
    let driver = claude_code("[captain_or_crew]\nforce = \"captain\"\n").await;
    driver
        .agent
        .push(FakeSession::completed("Done.").with_delay(Duration::from_secs(3)));
    driver.submit(WORK).await;
    let spec = spec_number(&driver, 0).await;
    let setup = setup_of(&spec).expect("the setup in use");
    assert!(setup.contains("Captain is in use"), "{setup}");
    let refused = call(&spec, START_WORKFLOW, json!({ "request": WORK })).await;
    assert!(
        refused.text.starts_with("No workflow started.")
            && refused.text.contains("passed no instruction")
            && refused
                .text
                .ends_with("If they don't, do the work yourself."),
        "{}",
        refused.text
    );
    assert!(driver.conductor.starts().is_empty());
    driver.exit().await;
}

/// A CLI that can't take tools when driven keeps the verdict: with a crew workflow in use its
/// reply is read for answer, change or unclear, as before.
#[tokio::test]
async fn a_cli_without_tools_keeps_the_verdict() {
    let mut driver = TestDriver::start_with(with_config("")).await;
    driver.agent.push(FakeSession::structured(json!({
        "kind": "answer",
        "answer_markdown": "It returns an error."
    })));
    driver.submit(QUESTION).await;
    driver.until(turn_ended).await;
    let spec = spec_number(&driver, 0).await;
    assert_eq!(spec.access, Access::ReadOnly);
    assert!(spec.json_schema.is_some(), "the verdict's schema");
    assert!(spec.asking.is_none());
    assert_eq!(setup_of(&spec), None);
    driver.exit().await;
}

/// `/plan <task>` with a workflow in use: the workflow plans first, and its plan waits for you.
#[tokio::test]
async fn slash_plan_has_the_workflow_plan_first() {
    let driver = claude_code("").await;
    driver
        .agent
        .push(FakeSession::completed("Handing it over.").with_delay(Duration::from_secs(3)));
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Plan {
                task: Some(WORK.to_owned()),
            },
        })
        .await;
    let spec = spec_number(&driver, 0).await;
    let started = handed_over(&driver, &spec, json!({ "request": WORK })).await;
    assert!(!started.is_error, "{started:?}");
    let deadline = tokio::time::Instant::now() + max_wait();
    while driver.conductor.starts().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "no run started");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let start = driver.conductor.starts().remove(0);
    assert!(
        start
            .overrides
            .iter()
            .any(|set| set == "approvals.plan.mode=\"supervised\""),
        "{:?}",
        start.overrides
    );
    driver.exit().await;
}

/// Captain, with the main agent on a Claude Code whose CLI forks its sessions.
const FORKING: &str = "[captain_or_crew]\nforce = \"captain\"\n\n[crew]\nmain = \
                       \"claude-code/opus-5-5\"\n";
const FORKING_MAIN: &str = "claude-code/opus-5-5";
const WORKED_OUT: &str =
    "Run what we just worked out as an orchestrator workflow, with Codex building it.";

/// A fake CLI with a fork of its own.
fn forking(worker: &str) -> argonauts_testkit::FakeRunner {
    use argonauts_core::session_runner::{RunnerCapabilities, SessionRunner as _};
    let runner = argonauts_testkit::FakeRunner::cli(worker, "anthropic");
    let capabilities = RunnerCapabilities {
        fork: true,
        ..runner.capabilities()
    };
    runner.with_capabilities(capabilities)
}

/// The `n`th session `runner` started.
async fn spec_of(runner: &argonauts_testkit::FakeRunner, n: usize) -> SessionSpec {
    let deadline = tokio::time::Instant::now() + max_wait();
    loop {
        if let Some(spec) = runner.specs().into_iter().nth(n) {
            return spec;
        }
        assert!(tokio::time::Instant::now() < deadline, "no session started");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A conversation with one turn behind it, whose session the main agent's CLI can copy, and
/// the start tool's call in its second turn, whose words ask for the orchestrator's workflow
/// with Codex building, so the run is Argonauts Code's own rather than Claude Code's.
async fn worked_out(config: &'static str) -> (TestDriver, tokio::task::JoinHandle<AskReply>) {
    let mut driver = claude_code(config).await;
    let main = driver.directory.add(forking(FORKING_MAIN));
    driver
        .directory
        .add(argonauts_testkit::FakeRunner::cli("codex", "openai"));
    main.push(
        FakeSession::completed("Due dates go in the store, and the cli prints them.")
            .with_cli_session("main-session-1"),
    );
    driver.submit("How should due dates work?").await;
    driver.until(turn_ended).await;
    main.push(FakeSession::completed("Starting it.").with_delay(Duration::from_secs(3)));
    driver.submit(WORKED_OUT).await;
    let spec = spec_of(&main, 1).await;
    let how_to_run = json!([
        {"says": "as an orchestrator workflow", "kind": "mode", "value": "crew"},
        {"says": "with Codex building it", "kind": "build", "who": "codex"},
    ]);
    let arguments = json!({ "request": "Add due dates.", "how_to_run": how_to_run });
    (driver, calling(&spec, START_WORKFLOW, arguments))
}

/// An orchestrator on the main session's CLI is a copy of the session, made after the start
/// tool answered: the answer comes as soon as the run is taken, before any agent started, and
/// the line it gives the session says the orchestrator carries the conversation.
#[tokio::test]
async fn an_orchestrator_that_copies_the_session_hears_the_answer_first() {
    let (driver, answer) = worked_out(FORKING).await;
    let run = followed_run(&driver).await;
    let answer = tokio::time::timeout(max_wait(), answer)
        .await
        .expect("answered before any agent started")
        .unwrap();
    assert!(!answer.is_error, "{answer:?}");
    assert!(
        answer.text.contains(
            "Starting a workflow: Orchestrator, **Opus 5.5 on Claude Code**, carrying this \
             conversation; **"
        ) && answer.text.contains("Codex** builds."),
        "{answer:?}"
    );
    // The run's request names the session the orchestrator copies, where it ran.
    let starts_from = driver
        .conductor
        .starts()
        .last()
        .and_then(|start| start.starts_from.clone());
    match starts_from {
        Some(StartsFrom::Fork(fork)) => {
            assert_eq!(fork.cli_session, "main-session-1");
            assert_eq!(fork.worker.agent, "claude-code");
            assert_eq!(fork.folder, driver.repo.root());
        }
        other => panic!("a fork of the main session, not {other:?}"),
    }
    // The run went on as any other, its failures told later.
    driver.conductor.push(&run, agent_started(&run, 1)).await;
    driver.exit().await;
}

/// A main agent that answers with a change rather than through the start tool, as Codex's
/// does, starts a crew run whose orchestrator copies the main session all the same.
#[tokio::test]
async fn a_crew_run_from_a_change_reply_copies_the_main_session_too() {
    const CREW: &str = "[captain_or_crew]\nforce = \"crew\"\n\n[crew]\nmain = \
                        \"claude-code/opus-5-5\"\n";
    let mut driver = TestDriver::start_with(with_config(CREW)).await;
    let main = driver.directory.add(forking(FORKING_MAIN));
    main.push(
        FakeSession::completed("Due dates go in the store, and the cli prints them.")
            .with_cli_session("main-session-1"),
    );
    driver.submit("How should due dates work?").await;
    driver.until(turn_ended).await;
    // The second turn carries the same session on, as the CLI resumes it.
    main.push(
        FakeSession::structured(json!({
            "kind": "change",
            "answer_markdown": "Adding due dates.",
            "estimate": {
                "task_type": "api-feature",
                "summary": "Add due dates",
                "files_likely": ["src/store.js", "src/cli.js"],
                "file_count": 2,
                "risk_labels": [],
                "tests_exist": true,
                "unknowns": []
            }
        }))
        .with_cli_session("main-session-1"),
    );
    driver.submit("Add due dates.").await;
    let _run = followed_run(&driver).await;
    let starts_from = driver
        .conductor
        .starts()
        .last()
        .and_then(|start| start.starts_from.clone());
    match starts_from {
        Some(StartsFrom::Fork(fork)) => {
            assert_eq!(fork.cli_session, "main-session-1");
            assert_eq!(fork.worker.agent, "claude-code");
        }
        other => panic!("a fork of the main session, not {other:?}"),
    }
    driver.exit().await;
}

/// With `crew.orchestrator_context = "brief"` the orchestrator starts from the brief, so the
/// answer waits for the run to be under way, as it always did.
#[tokio::test]
async fn an_orchestrator_from_the_brief_waits_for_the_run() {
    const BRIEF: &str = "[captain_or_crew]\nforce = \"captain\"\n\n[crew]\nmain = \
                         \"claude-code/opus-5-5\"\norchestrator_context = \"brief\"\n";
    let (driver, answer) = worked_out(BRIEF).await;
    let run = followed_run(&driver).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !answer.is_finished(),
        "the tool answered before the run began"
    );
    driver.conductor.push(&run, agent_started(&run, 1)).await;
    let answer = answer.await.unwrap();
    assert!(
        answer.text.contains(
            "Starting a workflow: Orchestrator, **Opus 5.5 on Claude Code**, starting from the \
             brief; **"
        ),
        "{answer:?}"
    );
    let starts_from = driver
        .conductor
        .starts()
        .last()
        .and_then(|start| start.starts_from.clone());
    assert!(
        matches!(&starts_from, Some(StartsFrom::Brief(brief)) if brief.why.is_none()),
        "{starts_from:?}"
    );
    driver.exit().await;
}
