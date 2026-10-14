//! The main session is the CLI you chose, through the real
//! controller loop with fake CLIs: with a crew workflow in use your message still goes to the
//! main session as you typed it, talking starts nothing, the session hands work over by calling
//! a tool, and your words decide for one request. A CLI that can't take tools keeps the
//! verdict.

use std::time::Duration;

use argonauts_core::app::{AppEvent, ChatEvent, ChoiceKind, TurnReply, UiCommand};
use argonauts_core::asking::{Ask, AskLine, AskReply};
use argonauts_core::events::run::RunMode;
use argonauts_core::input::SlashCommand;
use argonauts_core::worker::{Access, SessionSpec};
use argonauts_testkit::FakeSession;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::client::test_support::session_started;
use crate::controller::runs::rig;
use crate::controller::test_support::{Call as ConductorCall, Harness, HarnessOptions, WAIT};
use argonauts_core::RunId;
use argonauts_core::events::run::RunOutcome;
use argonauts_core::events::{RunEvent, RunEventKind, RunLifecycle};
use argonauts_core::names::{RoleName, SessionKey};

const START_WORKFLOW: &str = "argonauts_start_workflow";
const QUESTION: &str = "what does parse_due do when the date has no year?";
const WORK: &str = "ok. add support for dates without a year, across the cli, the store and \
                    the tests";

/// The default settings, where council is the workflow in use, with whatever `config` adds.
fn with_config(config: &'static str) -> HarnessOptions {
    HarnessOptions {
        prepare: Some(Box::new(move |repo: &argonauts_testkit::TempRepo| {
            std::fs::write(repo.global_config(), config).unwrap();
        })),
        ..HarnessOptions::default()
    }
}

/// Claude Code as the main agent: it takes Argonauts' tools when driven.
async fn claude_code(config: &'static str) -> Harness {
    let harness = Harness::start_with(with_config(config)).await;
    harness.directory.taking_tools("claude-code");
    harness
}

fn turn_ended(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Chat {
            event: ChatEvent::TurnFinished { .. } | ChatEvent::TurnFailed { .. }
        }
    )
}

async fn spec_number(harness: &Harness, number: usize) -> SessionSpec {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if let Some(spec) = harness.agent.specs().into_iter().nth(number) {
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
pub(super) async fn followed_run(harness: &Harness) -> RunId {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let followed = harness
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
async fn handed_over(harness: &Harness, spec: &SessionSpec, arguments: Value) -> AskReply {
    let answer = calling(spec, START_WORKFLOW, arguments);
    let run = followed_run(harness).await;
    harness.conductor.push(&run, agent_started(&run, 1)).await;
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
        .find(|text| text.contains("Argonauts setup:"))
}

/// A question with council in use: the main session answers it in your checkout, as its CLI
/// would, and nothing starts. No estimate, no verdict, no run, no question about starting work.
#[tokio::test]
async fn with_a_crew_workflow_a_question_is_answered_and_nothing_starts() {
    let mut harness = claude_code("").await;
    let root = harness.repo.root().to_owned();
    harness.agent.push(FakeSession::completed(
        "It returns an error. parse_due only accepts full ISO dates.",
    ));
    harness.submit(QUESTION).await;
    let ended = harness.until(turn_ended).await;
    assert!(matches!(
        &ended,
        AppEvent::Chat { event: ChatEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. } }
            if markdown.starts_with("It returns an error.")
    ));
    let spec = spec_number(&harness, 0).await;
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

    let events = harness.exit().await;
    assert!(harness_started_nothing(&events));
}

fn harness_started_nothing(events: &[AppEvent]) -> bool {
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
    let harness = claude_code("").await;
    harness.agent.push(
        FakeSession::completed("That touches three parts, so I'm handing it to the crew.")
            .with_delay(Duration::from_secs(3)),
    );
    harness.submit(WORK).await;
    let spec = spec_number(&harness, 0).await;
    assert!(
        harness.conductor.starts().is_empty(),
        "sending starts nothing"
    );

    let account = "Accept dates without a year in parse_due, in the cli, the store and the tests.";
    let started = calling(&spec, START_WORKFLOW, json!({ "request": account }));
    let run = followed_run(&harness).await;
    // The answer waits until the run is under way.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !started.is_finished(),
        "the tool answered before the run began"
    );
    harness.conductor.push(&run, agent_started(&run, 1)).await;
    let started = started.await.unwrap();
    assert!(!started.is_error, "{started:?}");
    assert!(
        started
            .text
            .starts_with("The council workflow started on this."),
        "{started:?}"
    );
    let start = harness.conductor.starts().remove(0);
    assert_eq!(start.mode, RunMode::Crew);
    assert_eq!(
        start.request,
        format!("{WORK}\n\nFrom the conversation, as the main agent put it: {account}")
    );
    assert!(start.chat_turn.is_some());

    let again = call(&spec, START_WORKFLOW, json!({ "request": account })).await;
    assert!(
        again.text.starts_with("A workflow already started"),
        "{again:?}"
    );
    assert_eq!(harness.conductor.starts().len(), 1);
    harness.exit().await;
}

/// A run that fails before any agent starts: the tool answers with the failure, as a failure,
/// and never says the run started. The session that heard it says so, so it isn't told again.
#[tokio::test]
async fn a_run_that_fails_at_start_is_answered_as_a_failure() {
    let mut harness = claude_code("").await;
    harness.agent.push(
        FakeSession::completed("The council workflow didn't start: git refused its goal file.")
            .with_delay(Duration::from_secs(2)),
    );
    harness.submit(WORK).await;
    let spec = spec_number(&harness, 0).await;
    let answer = calling(&spec, START_WORKFLOW, json!({ "request": "Add dates." }));
    let run = followed_run(&harness).await;
    harness
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
    harness.until(turn_ended).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(harness.agent.specs().len(), 1, "the session was told twice");
    harness.exit().await;
}

/// A run that fails before it began doesn't stand as this request's workflow: a second call
/// in the same turn starts one, rather than hearing that one already started.
#[tokio::test]
async fn after_a_failed_start_the_session_may_start_again() {
    let harness = claude_code("").await;
    harness.agent.push(
        FakeSession::completed("It failed, so I tried again.").with_delay(Duration::from_secs(3)),
    );
    harness.submit(WORK).await;
    let spec = spec_number(&harness, 0).await;
    let answer = calling(&spec, START_WORKFLOW, json!({ "request": "Add dates." }));
    let run = followed_run(&harness).await;
    harness
        .conductor
        .push(&run, failed(&run, 1, GIT_REFUSED))
        .await;
    assert!(answer.await.unwrap().is_error);

    let again = calling(&spec, START_WORKFLOW, json!({ "request": "Add dates." }));
    let second = tokio::time::timeout(WAIT, async {
        loop {
            let other = harness
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
    harness
        .conductor
        .push(&second, agent_started(&second, 1))
        .await;
    let again = again.await.unwrap();
    assert!(
        !again.text.starts_with("A workflow already started"),
        "{again:?}"
    );
    assert_eq!(harness.conductor.starts().len(), 2);
    harness.exit().await;
}

/// A run that parks for its CLIs before any agent starts isn't said to have started: the tool
/// says what it waits for and that nothing runs until then.
#[tokio::test]
async fn a_run_that_waits_for_its_clis_is_not_said_to_have_started() {
    use argonauts_core::events::run::ParkReason;

    let harness = claude_code("").await;
    harness.agent.push(
        FakeSession::completed("It waits for a CLI to come back.")
            .with_delay(Duration::from_secs(2)),
    );
    harness.submit(WORK).await;
    let spec = spec_number(&harness, 0).await;
    let answer = calling(&spec, START_WORKFLOW, json!({ "request": "Add dates." }));
    let run = followed_run(&harness).await;
    let parked = RunLifecycle::Parked {
        reason: ParkReason::AllVendorsOut { until: None },
        waiting_on: Vec::new(),
    };
    harness
        .conductor
        .push(&run, rig::event(&run, 1, RunEventKind::Run(parked)))
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
    harness.exit().await;
}

/// A run that fails after it began: the session is told in a turn of its own, which shows no
/// message of yours, reads only, and says so in one line.
#[tokio::test]
async fn a_run_that_fails_later_is_told_to_the_session() {
    let mut harness = claude_code("").await;
    harness
        .agent
        .push(FakeSession::completed("Handed to the crew.").with_delay(Duration::from_secs(2)));
    let said = "The council run failed: git refused its goal file.";
    harness
        .agent
        .push(FakeSession::structured(json!({ "answer_markdown": said })));
    harness.submit(WORK).await;
    let spec = spec_number(&harness, 0).await;
    let answer = calling(&spec, START_WORKFLOW, json!({ "request": "Add dates." }));
    let run = followed_run(&harness).await;
    harness.conductor.push(&run, agent_started(&run, 1)).await;
    assert!(!answer.await.unwrap().is_error);
    harness.until(turn_ended).await;

    harness
        .conductor
        .push(&run, failed(&run, 2, GIT_REFUSED))
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
    assert!(
        matches!(
            &started,
            AppEvent::Chat {
                event: ChatEvent::TurnStarted {
                    continues: true,
                    read_only: true,
                    ..
                }
            }
        ),
        "{started:?}"
    );
    let told = spec_number(&harness, 1).await;
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
    let ended = harness.until(turn_ended).await;
    assert!(matches!(
        &ended,
        AppEvent::Chat { event: ChatEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. } }
            if markdown == said
    ));
    harness.exit().await;
}

/// A step a setup kept for this request goes with the workflow the request starts, so the
/// runner places it as it does the steps `always.toml` lists.
#[tokio::test]
async fn a_step_kept_for_this_request_goes_with_the_workflow_it_starts() {
    let harness = claude_code("").await;
    harness.agent.push(
        FakeSession::completed("Kept, and handed to the crew.").with_delay(Duration::from_secs(3)),
    );
    harness
        .submit("just for this one, run the benchmarks before sign-off. add due dates")
        .await;
    let spec = spec_number(&harness, 0).await;
    let kept = call(
        &spec,
        "argonauts_setup",
        json!({
            "asked": "Run the benchmarks before sign-off, for this request.",
            "steps": [{"name": "Benchmark", "role": "workers",
                       "does": "Run the benchmarks.", "when": "before sign-off"}],
            "keep": "request",
            "keep_says": "just for this one",
        }),
    )
    .await;
    assert!(!kept.is_error, "{kept:?}");
    let started = handed_over(&harness, &spec, json!({ "request": "Add due dates." })).await;
    assert!(!started.is_error, "{started:?}");
    let deadline = tokio::time::Instant::now() + WAIT;
    while harness.conductor.starts().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "no run started");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let start = harness.conductor.starts().remove(0);
    let steps: Vec<&str> = start
        .required_steps
        .iter()
        .map(|step| step.name.as_str())
        .collect();
    assert_eq!(steps, ["Benchmark"]);
    harness.exit().await;
}

/// "Just do it yourself" keeps one request with the session whatever is in use, and the words
/// must be yours: the same call for a message that never said so starts the workflow.
#[tokio::test]
async fn your_words_keep_one_request_with_the_main_session() {
    let harness = claude_code("").await;
    harness
        .agent
        .push(FakeSession::completed("Done.").with_delay(Duration::from_secs(3)));
    harness
        .submit("Just do it yourself: add due dates across the cli and the store.")
        .await;
    let spec = spec_number(&harness, 0).await;
    let how_to_run = json!([{"says": "Just do it yourself", "kind": "mode", "value": "solo"}]);
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
    assert!(harness.conductor.starts().is_empty());
    harness.exit().await;
}

/// The setup travels with your message once, and again only when it changed: a resumed
/// session keeps its instructions from the first request, so nothing else would tell it.
#[tokio::test]
async fn the_setup_is_said_once_and_again_when_it_changes() {
    let mut harness = claude_code("").await;
    harness.agent.push(FakeSession::completed("One."));
    harness.agent.push(FakeSession::completed("Two."));
    harness.agent.push(FakeSession::completed("Three."));
    harness.submit("first").await;
    harness.until(turn_ended).await;
    harness.submit("second").await;
    harness.until(turn_ended).await;
    let first = spec_number(&harness, 0).await;
    let second = spec_number(&harness, 1).await;
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
    harness
        .send(UiCommand::Submit {
            text: "third, with ultracode".to_owned(),
            workflow_requested: true,
            pictures: Vec::new(),
        })
        .await;
    harness
        .until(|event| matches!(event, AppEvent::Attached { .. }))
        .await;
    let started = harness.conductor.starts();
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(started[0].request, "third, with ultracode");
    assert_eq!(started[0].mode, RunMode::Crew);
    harness.exit().await;
}

/// With solo in use nothing leaves the main session unless you ask: the call alone is refused.
#[tokio::test]
async fn in_solo_the_session_s_own_judgment_starts_nothing() {
    let harness = claude_code("[solo_or_crew]\nforce = \"solo\"\n").await;
    harness
        .agent
        .push(FakeSession::completed("Done.").with_delay(Duration::from_secs(3)));
    harness.submit(WORK).await;
    let spec = spec_number(&harness, 0).await;
    let setup = setup_of(&spec).expect("the setup in use");
    assert!(setup.contains("solo is in use"), "{setup}");
    let refused = call(&spec, START_WORKFLOW, json!({ "request": WORK })).await;
    assert_eq!(
        refused.text,
        "The user didn't ask for a workflow, so do it yourself."
    );
    assert!(harness.conductor.starts().is_empty());
    harness.exit().await;
}

/// A CLI that can't take tools when driven keeps the verdict: with a crew workflow in use its
/// reply is read for answer, change or unclear, as before.
#[tokio::test]
async fn a_cli_without_tools_keeps_the_verdict() {
    let mut harness = Harness::start_with(with_config("")).await;
    harness.agent.push(FakeSession::structured(json!({
        "kind": "answer",
        "answer_markdown": "It returns an error."
    })));
    harness.submit(QUESTION).await;
    harness.until(turn_ended).await;
    let spec = spec_number(&harness, 0).await;
    assert_eq!(spec.access, Access::ReadOnly);
    assert!(spec.json_schema.is_some(), "the verdict's schema");
    assert!(spec.asking.is_none());
    assert_eq!(setup_of(&spec), None);
    harness.exit().await;
}

/// `/plan <task>` with a workflow in use: the workflow plans first, and its plan waits for you.
#[tokio::test]
async fn slash_plan_has_the_workflow_plan_first() {
    let harness = claude_code("").await;
    harness
        .agent
        .push(FakeSession::completed("Handing it over.").with_delay(Duration::from_secs(3)));
    harness
        .send(UiCommand::Slash {
            command: SlashCommand::Plan {
                task: Some(WORK.to_owned()),
            },
        })
        .await;
    let spec = spec_number(&harness, 0).await;
    let started = handed_over(&harness, &spec, json!({ "request": WORK })).await;
    assert!(!started.is_error, "{started:?}");
    let deadline = tokio::time::Instant::now() + WAIT;
    while harness.conductor.starts().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "no run started");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let start = harness.conductor.starts().remove(0);
    assert!(
        start
            .overrides
            .iter()
            .any(|set| set == "approvals.plan.mode=\"supervised\""),
        "{:?}",
        start.overrides
    );
    harness.exit().await;
}
