//! On autopilot the conversation asks nothing either: a request that could be a change starts
//! as the rules would pick, and what a request says about the crew applies to that request,
//! each said in one line, through the real controller loop with fake agents. A question already
//! open when autopilot comes on gets the same, in the line it closes with.

use std::sync::Arc;
use std::time::Duration;

use argonauts_core::app::{
    AppChoice, AppEvent, ChoiceKind, ConversationEvent, TurnReply, UiCommand,
};
use argonauts_core::event::AgentEvent;
use argonauts_core::events::run::RunMode;
use argonauts_core::events::session::EndReason;
use argonauts_core::input::SlashCommand;
use argonauts_core::registry::{ModelEntry, ModelRegistry};
use argonauts_core::session_runner::{Access, RunnerEvent, SessionSpec};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_testkit::services::StaticRegistry;
use argonauts_testkit::{FakeRunner, FakeSession, TempRepo};
use serde_json::{Value, json};

use crate::controller::test_support::{CliOptions, TestDriver, WAIT};

fn estimate() -> Value {
    json!({
        "task_type": "feature",
        "summary": "Add due dates to tasks",
        "files_likely": ["src/task.rs"],
        "file_count": 3,
        "risk_labels": [],
        "tests_exist": true,
        "unknowns": []
    })
}

fn reply(kind: &str, how_to_run: Vec<Value>) -> FakeSession {
    FakeSession::structured(json!({
        "kind": kind,
        "answer_markdown": "I'd add a due field, parse --due, and show it in list.",
        "estimate": estimate(),
        "how_to_run": how_to_run,
    }))
}

/// A session whose settings turn autopilot on, with Codex and Antigravity CLI installed too.
async fn on_autopilot() -> TestDriver {
    with_approvals("autopilot = true").await
}

/// Under `[approvals]`: one approval that waits for you makes the session's mode supervised.
const SUPERVISED: &str = "understand.mode = \"supervised\"";

/// A session that asks you, until Shift+Tab says otherwise.
async fn asking_you() -> TestDriver {
    with_approvals(SUPERVISED).await
}

async fn with_approvals(approvals: &'static str) -> TestDriver {
    let options = CliOptions {
        prepare: Some(Box::new(move |repo: &TempRepo| {
            // Captain works in a run of its own here, so a start shows as one.
            let settings = format!("[approvals]\n{approvals}\n[captain]\nkind = \"checked\"\n");
            std::fs::write(repo.global_config(), settings).unwrap();
        })),
        ..CliOptions::default()
    };
    let driver = TestDriver::start_with(options).await;
    driver.directory.add(FakeRunner::cli("codex", "openai"));
    driver
        .directory
        .add(FakeRunner::cli("antigravity-cli", "google"));
    driver
}

/// Waits for a run to start, and gives its mode.
async fn started(driver: &TestDriver) -> RunMode {
    let deadline = tokio::time::Instant::now() + WAIT;
    while driver.conductor.starts().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "no run started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    driver.conductor.starts().remove(0).mode
}

fn choices(driver: &TestDriver) -> Vec<ChoiceKind> {
    driver
        .seen()
        .iter()
        .filter_map(|event| match event {
            AppEvent::Choice { choice } => Some(choice.kind),
            _ => None,
        })
        .collect()
}

/// A request that could be a change opens no "Start work on it?" question on autopilot: the work
/// starts as Argonauts Code would pick, and one line says so.
#[tokio::test]
async fn an_unclear_request_starts_without_asking() {
    let mut driver = on_autopilot().await;
    driver.agent.push(reply("unclear", Vec::new()));
    driver.submit("how should we add due dates?").await;
    let said = driver
        .until(|event| {
            matches!(event, AppEvent::Notice { text, .. } if text.starts_with("Decided on autopilot · "))
        })
        .await;
    let AppEvent::Notice { text, .. } = said else {
        unreachable!()
    };
    assert!(
        text.starts_with("Decided on autopilot · this could be a change request, so "),
        "{text}"
    );
    assert!(text.ends_with(" starts work on it."), "{text}");
    let mode = started(&driver).await;
    assert!(matches!(mode, RunMode::Captain | RunMode::Crew));
    assert!(
        !choices(&driver).contains(&ChoiceKind::StartWork),
        "{:?}",
        choices(&driver)
    );
    driver.exit().await;
}

/// Words that set the crew show the crew understood and apply to the request, with no
/// "Keep this crew?" question on autopilot: one line says how to keep it.
#[tokio::test]
async fn words_that_set_the_crew_ask_nothing() {
    let mut driver = on_autopilot().await;
    let this = |says: &str, who: &str| {
        json!({"says": says, "kind": "others", "who": who, "model": "", "effort": "",
               "value": "", "scope": "this_request"})
    };
    driver.agent.push(reply(
        "change",
        vec![this("all the other agents need to be codex", "codex")],
    ));
    driver
        .submit("all the other agents need to be codex. add due dates")
        .await;
    driver
        .until(|event| {
            matches!(event, AppEvent::Notice { text, .. } if text.starts_with("Crew for this request"))
        })
        .await;
    driver
        .until(|event| {
            matches!(event, AppEvent::Notice { text, .. }
                if text == "Decided on autopilot · this applies to this request only. /config keeps it for later ones.")
        })
        .await;
    started(&driver).await;
    assert!(
        !choices(&driver).contains(&ChoiceKind::KeepInstructions),
        "{:?}",
        choices(&driver)
    );
    driver.exit().await;
}

/// Waits for a question of `kind` to open.
async fn asked(driver: &mut TestDriver, kind: ChoiceKind) -> AppChoice {
    let question = driver
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == kind))
        .await;
    let AppEvent::Choice { choice } = question else {
        unreachable!()
    };
    choice
}

/// Shift+Tab to autopilot, then the line `question` closes with.
async fn closed_by_autopilot(driver: &mut TestDriver, question: &AppChoice) -> String {
    driver
        .send(UiCommand::ApprovalMode {
            mode: ApprovalMode::Autopilot,
        })
        .await;
    let closed = driver
        .until(|event| matches!(event, AppEvent::ChoiceClosed { id, .. } if id == &question.id))
        .await;
    let AppEvent::ChoiceClosed { outcome, .. } = closed else {
        unreachable!()
    };
    outcome
}

/// "Start work on it?" open when autopilot comes on closes with what autopilot decided, and
/// the work starts.
#[tokio::test]
async fn autopilot_coming_on_starts_the_work_a_question_offered() {
    let mut driver = asking_you().await;
    driver.agent.push(reply("unclear", Vec::new()));
    driver.submit("how should we add due dates?").await;
    let question = asked(&mut driver, ChoiceKind::StartWork).await;
    assert!(driver.conductor.starts().is_empty());
    let outcome = closed_by_autopilot(&mut driver, &question).await;
    assert!(
        outcome.starts_with("Decided on autopilot · this could be a change request, so "),
        "{outcome}"
    );
    assert!(outcome.ends_with(" starts work on it."), "{outcome}");
    started(&driver).await;
    driver.exit().await;
}

/// "Keep this crew?" open when autopilot comes on closes as kept for its request alone.
#[tokio::test]
async fn autopilot_coming_on_keeps_what_was_said_for_its_request() {
    let mut driver = asking_you().await;
    let others = json!({"says": "all the other agents need to be codex", "kind": "others",
        "who": "codex", "model": "", "effort": "", "value": "", "scope": "this_request"});
    driver.agent.push(reply("change", vec![others]));
    driver
        .submit("all the other agents need to be codex. add due dates")
        .await;
    let question = asked(&mut driver, ChoiceKind::KeepInstructions).await;
    let outcome = closed_by_autopilot(&mut driver, &question).await;
    assert_eq!(
        outcome,
        "Decided on autopilot · this applies to this request only. /config keeps it for later ones."
    );
    driver.exit().await;
}

const FIXED: &str = "Fixed both functions in invoice/money.py.";

/// The Captain workflow on autopilot, where every message is a direct turn in your checkout,
/// with Codex listed to carry the main session's turn on. `config` adds to your settings.
fn direct_on_autopilot(config: &'static str) -> CliOptions {
    direct("autopilot = true", config)
}

/// The Captain workflow with `approvals` as your approval settings.
fn direct(approvals: &'static str, config: &'static str) -> CliOptions {
    CliOptions {
        prepare: Some(Box::new(move |repo: &TempRepo| {
            repo.write("invoice/money.py", "sign = 1\n").unwrap();
            repo.commit_all("money").unwrap();
            let settings = format!(
                "{config}\n[approvals]\n{approvals}\n[captain_or_crew]\nforce = \"captain\"\n"
            );
            std::fs::write(repo.global_config(), settings).unwrap();
        })),
        flags: vec![r#"fallback.main.order=["codex"]"#.to_owned()],
        ..CliOptions::default()
    }
}

/// The main session's turn runs out part way through its edit.
fn runs_out(spec: &SessionSpec) -> FakeSession {
    std::fs::write(spec.workdir.join("invoice/money.py"), "half\n").unwrap();
    FakeSession::ended(EndReason::UsageLimit { resets_at: None })
}

fn turn_ended(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Conversation {
            event: ConversationEvent::TurnFinished { .. } | ConversationEvent::TurnFailed { .. }
        }
    )
}

/// With `fallback.mode = "ask"` a turn whose agent ran out would ask who carries it on. On
/// autopilot the first agent that may take it carries on, and no question opens.
#[tokio::test]
async fn a_main_session_that_runs_out_is_carried_on_without_asking() {
    let options = direct_on_autopilot("[fallback]\nmode = \"ask\"\n");
    let mut driver = TestDriver::start_with(options).await;
    let codex = driver
        .directory
        .add(FakeRunner::cli("codex", "openai").then(FakeSession::completed(FIXED)));
    driver.agent.respond(runs_out);
    driver.submit("Fix invoice/money.py").await;
    let ended = driver.until(turn_ended).await;
    assert!(
        matches!(
            &ended,
            AppEvent::Conversation {
                event: ConversationEvent::TurnFinished {
                    reply: TurnReply::Answer { markdown },
                    ..
                }
            } if markdown == FIXED
        ),
        "{ended:?}"
    );
    assert_eq!(codex.specs()[0].access, Access::Direct);
    assert!(
        !choices(&driver).contains(&ChoiceKind::Fallback),
        "{:?}",
        choices(&driver)
    );
    driver.exit().await;
}

/// An agent billed to an API key is yours to allow, so on autopilot a turn with no other left
/// ends there, with one line that says why, and nothing is asked.
#[tokio::test]
async fn a_paid_agent_never_carries_a_turn_on_by_itself() {
    let registry = StaticRegistry::default();
    registry.add(
        serde_json::from_value::<ModelEntry>(json!({
            "id": "codex",
            "provider": "openai",
            "kind": "cli",
            "access": "api-key",
            "cost": { "unit": "usd", "input_per_mtok": 1.0, "output_per_mtok": 4.0 },
        }))
        .unwrap(),
    );
    let mut options = direct_on_autopilot("");
    options.registry = Some(Arc::new(registry) as Arc<dyn ModelRegistry>);
    let mut driver = TestDriver::start_with(options).await;
    let codex = driver
        .directory
        .add(FakeRunner::cli("codex", "openai").then(FakeSession::completed(FIXED)));
    driver.agent.respond(runs_out);
    driver.submit("Fix invoice/money.py").await;
    let ended = driver.until(turn_ended).await;
    assert!(
        matches!(
            ended,
            AppEvent::Conversation {
                event: ConversationEvent::TurnFailed { .. }
            }
        ),
        "{ended:?}"
    );
    assert!(codex.specs().is_empty());
    let seen = driver.seen();
    assert!(
        seen.iter()
            .any(|event| matches!(event, AppEvent::Notice { text, .. }
            if text.starts_with("Decided on autopilot · only an agent billed to an API key"))),
        "{seen:#?}"
    );
    assert!(
        !choices(&driver).contains(&ChoiceKind::Fallback),
        "{:?}",
        choices(&driver)
    );
    driver.exit().await;
}

/// "Who carries it on?" open when autopilot comes on goes to the first agent that may take
/// the turn, which finishes it.
#[tokio::test]
async fn autopilot_coming_on_carries_on_a_turn_that_waited_for_your_pick() {
    let options = direct(SUPERVISED, "[fallback]\nmode = \"ask\"\n");
    let mut driver = TestDriver::start_with(options).await;
    let codex = driver
        .directory
        .add(FakeRunner::cli("codex", "openai").then(FakeSession::completed(FIXED)));
    driver.agent.respond(runs_out);
    driver.submit("Fix invoice/money.py").await;
    let question = asked(&mut driver, ChoiceKind::Fallback).await;
    assert!(codex.specs().is_empty(), "nothing carried on before");
    let outcome = closed_by_autopilot(&mut driver, &question).await;
    assert!(
        outcome.starts_with("Decided on autopilot · ") && outcome.ends_with(" carries it on"),
        "{outcome}"
    );
    let ended = driver.until(turn_ended).await;
    assert!(
        matches!(
            &ended,
            AppEvent::Conversation {
                event: ConversationEvent::TurnFinished {
                    reply: TurnReply::Answer { markdown },
                    ..
                }
            } if markdown == FIXED
        ),
        "{ended:?}"
    );
    assert_eq!(codex.specs()[0].access, Access::Direct);
    driver.exit().await;
}

/// agy's offer to run what it was refused, open when autopilot comes on, gets a yes: agy goes
/// on for the one turn that runs commands without asking.
#[tokio::test]
async fn autopilot_coming_on_lets_agy_run_what_it_was_refused() {
    let options = direct(SUPERVISED, "crew.main = \"antigravity-cli\"\n");
    let mut driver = TestDriver::start_with(options).await;
    let refused = FakeSession::completed("I couldn't run the tests: that needs approval.")
        .with_event(RunnerEvent::Agent(AgentEvent::ToolStarted {
            id: "s2".into(),
            name: "run_command".into(),
            input: json!({"CommandLine": "python -m pytest -q"}),
            parent: None,
        }))
        .with_event(RunnerEvent::Agent(AgentEvent::ToolFinished {
            id: "s2".into(),
            ok: false,
            output: Some("permission check failed for unsandboxed \"python -m pytest -q\"".into()),
            parent: None,
        }))
        .with_cli_session("conv-1");
    let agy = driver.directory.add(
        FakeRunner::cli("antigravity-cli", "google")
            .then(refused)
            .then(FakeSession::completed("6 passed.")),
    );
    driver.submit("Run the tests").await;
    driver.until(turn_ended).await;
    let question = asked(&mut driver, ChoiceKind::RefusedCommands).await;
    let outcome = closed_by_autopilot(&mut driver, &question).await;
    assert_eq!(
        outcome,
        "Decided on autopilot · Antigravity CLI continues, running commands without asking"
    );
    driver.until(turn_ended).await;
    driver.exit().await;
    let resumed = agy.specs().remove(1);
    assert_eq!(resumed.resume.as_deref(), Some("conv-1"));
    assert_eq!(
        resumed.prompt[0].text,
        "You may run the commands you were refused."
    );
}

const PLAN: &str = "1. Read the sign first.\n2. Apply it once.";

/// The main session plans read-only, then fixes the file in your checkout.
fn plans_then_fixes(spec: &SessionSpec) -> FakeSession {
    if spec.access == Access::Direct {
        std::fs::write(spec.workdir.join("invoice/money.py"), "negative = True\n").unwrap();
        FakeSession::completed(FIXED)
    } else {
        FakeSession::completed(PLAN).with_cli_session("plan-1")
    }
}

fn fixed(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Conversation {
            event: ConversationEvent::TurnFinished {
                reply: TurnReply::Answer { markdown },
                ..
            }
        } if markdown == FIXED
    )
}

async fn plan_first(driver: &TestDriver) {
    let command = SlashCommand::Plan { task: None };
    driver.send(UiCommand::Slash { command }).await;
    driver.submit("Fix invoice/money.py").await;
}

/// The session that planned went ahead with its plan, in your checkout.
fn went_ahead(driver: &TestDriver) {
    let specs = driver.agent.specs();
    assert_eq!(specs.len(), 2, "the plan, then the work");
    assert_eq!(specs[1].access, Access::Direct);
    assert_eq!(specs[1].resume.as_deref(), Some("plan-1"));
    assert_eq!(specs[1].prompt[0].text, "Go ahead with this plan.");
}

/// Plan first in the Captain workflow on autopilot: the session plans, then goes ahead with
/// its plan by itself. No "Go ahead with this plan?" waits, and one line says so.
#[tokio::test]
async fn a_plan_made_first_goes_ahead_without_asking() {
    let mut driver = TestDriver::start_with(direct_on_autopilot("")).await;
    driver.agent.respond(plans_then_fixes);
    plan_first(&driver).await;
    // Turning plan first on says what it means here.
    driver
        .until(|event| {
            matches!(event, AppEvent::Notice { text, .. }
                if text.starts_with("Plan first is on")
                    && text.contains("On autopilot Captain goes ahead with its plan"))
        })
        .await;
    driver
        .until(|event| {
            matches!(event, AppEvent::Notice { text, .. }
                if text == "Decided on autopilot · Claude Code goes ahead with its plan in your checkout.")
        })
        .await;
    driver.until(fixed).await;
    went_ahead(&driver);
    assert!(
        !choices(&driver).contains(&ChoiceKind::GoAhead),
        "{:?}",
        choices(&driver)
    );
    driver.exit().await;
}

/// "Go ahead with this plan?" open when autopilot comes on closes as going ahead, and the
/// session that planned carries the plan out.
#[tokio::test]
async fn autopilot_coming_on_goes_ahead_with_a_plan_that_waited() {
    let mut driver = TestDriver::start_with(direct(SUPERVISED, "")).await;
    driver.agent.respond(plans_then_fixes);
    plan_first(&driver).await;
    let question = asked(&mut driver, ChoiceKind::GoAhead).await;
    assert_eq!(driver.agent.specs().len(), 1, "nothing changed before");
    let outcome = closed_by_autopilot(&mut driver, &question).await;
    assert_eq!(
        outcome,
        "Decided on autopilot · Claude Code goes ahead with its plan"
    );
    driver.until(fixed).await;
    went_ahead(&driver);
    driver.exit().await;
}
