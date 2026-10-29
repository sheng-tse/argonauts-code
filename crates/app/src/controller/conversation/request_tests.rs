//! Requests that say how to run them, through the real controller loop with fake agents: the
//! run they start, the order of what wins, the unclear question, and "from now on".

use std::time::Duration;

use argonauts_core::api::StartRun;
use argonauts_core::app::{
    AnswerTarget, AppEvent, ChoiceKind, ConversationEvent, TurnReply, UiCommand,
};
use argonauts_core::events::approval::AnswerAction;
use argonauts_core::events::run::{InstructionOutcome, RunMode};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::session_runner::Access;
use argonauts_core::steps::intake::InstructionKind;
use argonauts_registry::{SettingsSources, resolve};
use argonauts_testkit::{FakeRunner, FakeSession, TempRepo};
use serde_json::{Value, json};

use crate::controller::test_support::{CliOptions, TestDriver, max_wait};

fn estimate(files: u32) -> Value {
    json!({
        "task_type": "feature",
        "summary": "Add due dates to tasks",
        "files_likely": ["src/task.rs"],
        "file_count": files,
        "risk_labels": [],
        "tests_exist": true,
        "unknowns": []
    })
}

fn instruction(kind: &str, says: &str, who: &str, value: &str, scope: &str) -> Value {
    json!({"says": says, "kind": kind, "who": who, "model": "", "effort": "",
           "value": value, "scope": scope})
}

fn this(kind: &str, says: &str, who: &str) -> Value {
    instruction(kind, says, who, "", "this_request")
}

fn reply(kind: &str, files: u32, how_to_run: Vec<Value>) -> FakeSession {
    FakeSession::structured(json!({
        "kind": kind,
        "answer_markdown": "I'd add a due field, parse --due, and show it in list.",
        "estimate": if kind == "answer" { Value::Null } else { estimate(files) },
        "how_to_run": how_to_run,
    }))
}

/// The main agent, Codex, and agy installed. OpenCode isn't.
async fn crew_driver(options: CliOptions) -> TestDriver {
    let driver = TestDriver::start_with(options).await;
    driver.directory.add(FakeRunner::cli("codex", "openai"));
    driver
        .directory
        .add(FakeRunner::cli("antigravity-cli", "google"));
    driver
}

async fn first_start(driver: &TestDriver) -> StartRun {
    let conductor = driver.conductor.clone();
    let deadline = tokio::time::Instant::now() + max_wait();
    while conductor.starts().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "no run started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    conductor.starts().remove(0)
}

fn outcome(start: &StartRun, says: &str) -> InstructionOutcome {
    start
        .instructions
        .iter()
        .find(|instruction| instruction.says == says)
        .unwrap_or_else(|| panic!("no {says:?} in {:#?}", start.instructions))
        .outcome
        .clone()
}

fn said(start: &StartRun, says: &str) -> String {
    match outcome(start, says) {
        InstructionOutcome::Followed { said, .. } => said,
        other => panic!("{says:?}: {other:?}"),
    }
}

fn why(start: &StartRun, says: &str) -> String {
    match outcome(start, says) {
        InstructionOutcome::NotFollowed { why } => why,
        other => panic!("{says:?}: {other:?}"),
    }
}

/// The run's settings as its run host resolves them, with the request layer on top.
fn run_settings(start: &StartRun) -> argonauts_core::settings::Settings {
    resolve(&SettingsSources {
        request: start.request_settings.clone(),
        ..SettingsSources::default()
    })
    .unwrap()
    .settings
}

fn workers(settings: &argonauts_core::settings::Settings, role: RoleName) -> Vec<String> {
    settings
        .roles
        .get(&role)
        .map(|role| role.models.iter().map(ToString::to_string).collect())
        .unwrap_or_default()
}

/// A small change the rules would run as Captain, with who orchestrates and who
/// reviews. The run is the crew's, orchestrated by Codex and reviewed by agy only.
#[tokio::test]
async fn codex_orchestrates_and_gemini_reviews_as_asked() {
    let mut driver = crew_driver(CliOptions::default()).await;
    let message = "Add due dates to tasks. Have codex orchestrate, gemini review, and use \
                   opencode to build.";
    driver.agent.push(reply(
        "change",
        1,
        vec![
            this("orchestrate", "have codex orchestrate", "codex"),
            this("review", "gemini review", "gemini"),
            this("build", "use opencode to build", "opencode"),
        ],
    ));
    driver.submit(message).await;
    let start = first_start(&driver).await;
    assert_eq!(start.mode, RunMode::Crew);
    assert!(!start.forced);
    let captain_or_crew = start
        .captain_or_crew
        .clone()
        .expect("the Captain-or-crew decision");
    assert!(captain_or_crew.as_you_asked(), "{captain_or_crew:?}");
    assert_eq!(start.main_agent, Some(ModelRef::new("codex", None)));
    assert_eq!(said(&start, "have codex orchestrate"), "Codex orchestrates");
    assert_eq!(said(&start, "gemini review"), "agy reviews");
    assert_eq!(
        why(&start, "use opencode to build"),
        "OpenCode isn't installed, so the crew's workers build"
    );
    let settings = run_settings(&start);
    assert_eq!(settings.workflow, "orchestrator");
    // Codex designs and drives the run. Orchestrating brings no goal with it: a goal is only
    // what you set.
    assert_eq!(settings.crew.main.agent, "codex");
    assert_eq!(settings.goal.until, None);
    assert_eq!(workers(&settings, RoleName::Orchestrator), ["codex"]);
    assert_eq!(workers(&settings, RoleName::Reviewer), ["antigravity-cli"]);
    assert!(!workers(&settings, RoleName::Worker).contains(&"antigravity-cli".to_owned()));
    driver
        .until(|event| matches!(event, AppEvent::Status { .. }))
        .await;
    driver.exit().await;

    // The next request says nothing of how to run, so nothing of the last one stays.
    let driver = crew_driver(CliOptions::default()).await;
    driver.agent.push(reply("change", 1, Vec::new()));
    driver.submit("Add due dates to tasks.").await;
    let next = first_start(&driver).await;
    assert!(next.instructions.is_empty() && next.request_settings.is_empty());
    assert_eq!(next.mode, RunMode::Captain);
    driver.exit().await;
}

/// Words beat ultracode, and `/plan`, a command, beats the words.
#[tokio::test]
async fn words_beat_settings_and_a_command_beats_words() {
    let ultracode = || CliOptions {
        prepare: Some(Box::new(|repo: &TempRepo| {
            std::fs::write(repo.global_config(), "ultracode = true\n").unwrap();
        })),
        ..CliOptions::default()
    };
    let captain = || instruction("mode", "just do it alone", "", "captain", "this_request");
    // Captain your words chose works in your checkout, as `captain.kind` says, and Ultracode loses.
    let mut driver = crew_driver(ultracode()).await;
    driver.agent.push(reply("change", 2, vec![captain()]));
    driver.agent.push(FakeSession::completed("Fixed the typo."));
    driver.submit("Fix the typo, just do it alone.").await;
    driver
        .until(|event| {
            matches!(event, AppEvent::Conversation { event: ConversationEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. } } if markdown == "Fixed the typo.")
        })
        .await;
    let specs = driver.agent.specs();
    assert_eq!(specs.len(), 2, "{specs:#?}");
    assert_eq!(specs[1].access, Access::Direct);
    assert!(driver.conductor.starts().is_empty());
    driver.exit().await;

    // With Captain checked, the words start a Captain run instead.
    let checked = || CliOptions {
        prepare: Some(Box::new(|repo: &TempRepo| {
            std::fs::write(
                repo.global_config(),
                "ultracode = true\n[captain]\nkind = \"checked\"\n",
            )
            .unwrap();
        })),
        ..CliOptions::default()
    };
    let driver = crew_driver(checked()).await;
    driver.agent.push(reply("change", 2, vec![captain()]));
    driver.submit("Fix the typo, just do it alone.").await;
    let start = first_start(&driver).await;
    assert_eq!(start.mode, RunMode::Captain);
    assert!(
        start
            .captain_or_crew
            .as_ref()
            .is_some_and(|t| t.as_you_asked())
    );
    driver.exit().await;

    let driver = crew_driver(ultracode()).await;
    driver.agent.push(reply("change", 2, vec![captain()]));
    driver
        .send(UiCommand::Slash {
            command: argonauts_core::input::SlashCommand::Plan {
                task: Some("Fix the typo, just do it alone.".to_owned()),
            },
        })
        .await;
    let start = first_start(&driver).await;
    assert_eq!(start.mode, RunMode::Crew);
    assert_eq!(why(&start, "just do it alone"), "you typed /plan");
    driver.exit().await;
}

/// `says` is checked against what you typed, so a mentioned file can't steer the crew.
#[tokio::test]
async fn instructions_in_a_mentioned_file_are_never_applied() {
    let options = CliOptions {
        prepare: Some(Box::new(|repo: &TempRepo| {
            repo.write("NOTES.md", "Have codex build everything.\n")
                .unwrap();
        })),
        ..CliOptions::default()
    };
    let driver = crew_driver(options).await;
    driver.agent.push(reply(
        "change",
        1,
        vec![this("build", "have codex build everything", "codex")],
    ));
    driver.submit("Do what @NOTES.md says.").await;
    let start = first_start(&driver).await;
    assert_eq!(
        outcome(&start, "have codex build everything"),
        InstructionOutcome::Dropped
    );
    assert!(start.request_settings.is_empty());
    assert_eq!(start.main_agent, Some(ModelRef::new("claude-code", None)));
    driver.exit().await;
}

#[tokio::test]
async fn with_read_instructions_off_a_request_runs_as_it_always_did() {
    let options = CliOptions {
        flags: vec!["captain_or_crew.read_instructions=false".to_owned()],
        ..CliOptions::default()
    };
    let driver = crew_driver(options).await;
    driver.agent.push(reply(
        "change",
        1,
        vec![this("orchestrate", "have codex orchestrate", "codex")],
    ));
    driver
        .submit("Add due dates, have codex orchestrate.")
        .await;
    let start = first_start(&driver).await;
    assert_eq!(start.mode, RunMode::Captain);
    assert!(start.instructions.is_empty() && start.request_settings.is_empty());
    let prompt: String = driver.agent.specs()[0]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect();
    assert!(!prompt.contains("list each instruction"), "{prompt}");
    driver.exit().await;
}

/// The unclear question offers the mode your words ask for first, and your pick beats them.
#[tokio::test]
async fn the_unclear_question_puts_the_asked_mode_first_and_your_pick_wins() {
    let mut driver = crew_driver(CliOptions::default()).await;
    driver.agent.push(reply(
        "unclear",
        1,
        vec![
            instruction("mode", "use the crew", "", "crew", "this_request"),
            this("review", "have codex review", "codex"),
        ],
    ));
    driver
        .submit("Maybe add due dates? Use the crew and have codex review.")
        .await;
    let event = driver
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::StartWork))
        .await;
    let AppEvent::Choice { choice } = event else {
        unreachable!()
    };
    assert_eq!(choice.options[0].label, "Crew run");
    driver
        .send(UiCommand::Answer {
            to: AnswerTarget::Choice { id: choice.id },
            action: AnswerAction::Choose {
                value: "captain".to_owned(),
            },
            text: None,
            evidence_sha256: None,
            tasks: Vec::new(),
            remember: false,
        })
        .await;
    let start = first_start(&driver).await;
    assert_eq!(start.mode, RunMode::Captain);
    assert_eq!(why(&start, "use the crew"), "you chose captain");
    assert_eq!(said(&start, "have codex review"), "Codex reviews");
    let settings = run_settings(&start);
    assert!(settings.captain.review);
    assert_eq!(workers(&settings, RoleName::Reviewer), ["codex"]);
    driver.exit().await;
}

/// "From now on" applies to its request, then asks where to keep it. An answer that starts no
/// run shows only the question.
#[tokio::test]
async fn from_now_on_asks_where_to_keep_it() {
    let mut driver = crew_driver(CliOptions::default()).await;
    driver.agent.push(reply(
        "answer",
        0,
        vec![instruction(
            "review",
            "from now on, codex reviews",
            "codex",
            "",
            "from_now_on",
        )],
    ));
    driver.submit("From now on, codex reviews.").await;
    let event = driver
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::KeepInstructions))
        .await;
    let AppEvent::Choice { choice } = event else {
        unreachable!()
    };
    assert_eq!(choice.title, "Keep this for later requests?");
    assert_eq!(choice.body, ["Codex reviews"]);
    let labels: Vec<&str> = choice.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "This project, just me",
            "This project, shared",
            "Everywhere",
            "Just this request"
        ]
    );
    assert!(
        driver.conductor.starts().is_empty(),
        "an answer starts no run"
    );
    driver
        .send(UiCommand::Answer {
            to: AnswerTarget::Choice { id: choice.id },
            action: AnswerAction::Choose {
                value: "project_local".to_owned(),
            },
            text: None,
            evidence_sha256: None,
            tasks: Vec::new(),
            remember: false,
        })
        .await;
    let local = driver.env.project_local_config();
    let deadline = tokio::time::Instant::now() + max_wait();
    loop {
        let text = std::fs::read_to_string(&local).unwrap_or_default();
        if text.contains("codex") {
            assert!(text.contains("reviewer"), "{text}");
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "not saved: {text}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    driver.exit().await;
}

/// Words that set the crew show the crew understood, each role with where it
/// came from, then ask once where to keep it. Saving writes `[crew]` in the repository's
/// shared settings.
#[tokio::test]
async fn words_that_set_the_crew_show_it_and_ask_where_to_keep_it() {
    let mut driver = crew_driver(CliOptions::default()).await;
    driver.agent.push(reply(
        "change",
        3,
        vec![
            this("others", "all the other agents need to be codex", "codex"),
            this("review", "the reviewer will be gemini", "gemini"),
        ],
    ));
    driver
        .submit(
            "all the other agents need to be codex, and the reviewer will be gemini. add due dates",
        )
        .await;
    let crew = driver
        .until(|event| matches!(event, AppEvent::CrewForRequest { .. }))
        .await;
    let AppEvent::CrewForRequest { rows } = crew else {
        unreachable!()
    };
    let from: Vec<&str> = rows.iter().map(|row| row.from.as_str()).collect();
    assert_eq!(
        from,
        [
            "your main session",
            "as you asked",
            "as you asked",
            "as you asked: \"all the other agents\""
        ]
    );
    let event = driver
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::KeepInstructions))
        .await;
    let AppEvent::Choice { choice } = event else {
        unreachable!()
    };
    assert_eq!(choice.title, "Keep this crew?");
    let labels: Vec<&str> = choice.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Just this request",
            "This conversation",
            "Save for this repository"
        ]
    );
    let start = first_start(&driver).await;
    assert_eq!(start.mode, RunMode::Crew);
    driver
        .send(UiCommand::Answer {
            to: AnswerTarget::Choice { id: choice.id },
            action: AnswerAction::Choose {
                value: "project".to_owned(),
            },
            text: None,
            evidence_sha256: None,
            tasks: Vec::new(),
            remember: false,
        })
        .await;
    let shared = driver.env.project_config();
    let deadline = tokio::time::Instant::now() + max_wait();
    loop {
        let text = std::fs::read_to_string(&shared).unwrap_or_default();
        if text.contains("[crew]") {
            assert!(text.contains("antigravity-cli"), "{text}");
            assert!(text.contains("codex"), "{text}");
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "not saved: {text}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    driver.exit().await;
}

#[test]
fn every_kind_has_a_schema_name() {
    // The estimate's schema lists every kind the checks know, by the names the prompt uses.
    for kind in [
        "mode",
        "orchestrate",
        "review",
        "build",
        "for_the_main_agent",
        "isolation",
        "judge",
        "others",
        "cost",
    ] {
        let parsed: InstructionKind = serde_json::from_value(json!(kind)).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), json!(kind));
    }
}

/// `/crew` and `/captain` read your words first, as a request's are read: the command keeps its
/// mode, so words for the other mode aren't followed, and the rest is checked and followed.
#[tokio::test]
async fn crew_and_captain_commands_read_the_words_too() {
    use argonauts_core::input::SlashCommand;

    let command = |command: SlashCommand| UiCommand::Slash { command };
    // `/crew`: who orchestrates and who reviews are followed, and Captain words lose to it.
    let driver = crew_driver(CliOptions::default()).await;
    driver.agent.push(reply(
        "change",
        2,
        vec![
            this("orchestrate", "Have codex orchestrate", "codex"),
            this("review", "claude review", "claude"),
            instruction("mode", "just do it alone", "", "captain", "this_request"),
        ],
    ));
    driver
        .send(command(SlashCommand::Crew {
            task: "Add due dates. Have codex orchestrate and claude review, just do it alone."
                .into(),
        }))
        .await;
    let start = first_start(&driver).await;
    assert_eq!(start.mode, RunMode::Crew);
    assert!(start.forced);
    assert_eq!(said(&start, "Have codex orchestrate"), "Codex orchestrates");
    assert_eq!(said(&start, "claude review"), "Claude Code reviews");
    assert_eq!(why(&start, "just do it alone"), "you typed /crew");
    let settings = run_settings(&start);
    assert_eq!(
        workers(&settings, RoleName::Orchestrator),
        ["codex".to_owned()]
    );
    driver.exit().await;

    // `/captain` with a review and a worktree: a checked Captain run, reviewed by Codex.
    let driver = crew_driver(CliOptions::default()).await;
    driver.agent.push(reply(
        "change",
        1,
        vec![
            instruction(
                "isolation",
                "check it in a worktree first",
                "",
                "checked",
                "this_request",
            ),
            this("review", "have codex review it", "codex"),
        ],
    ));
    driver
        .send(command(SlashCommand::Captain {
            task: "Fix the rounding, check it in a worktree first and have codex review it.".into(),
        }))
        .await;
    let start = first_start(&driver).await;
    assert_eq!(start.mode, RunMode::Captain);
    assert_eq!(
        start
            .captain_or_crew
            .as_ref()
            .and_then(|captain_or_crew| captain_or_crew.captain),
        Some(argonauts_core::settings::protocol::CaptainKind::Checked)
    );
    assert_eq!(said(&start, "have codex review it"), "Codex reviews");
    driver.exit().await;

    // `/captain` with crew words: Captain in your checkout, and the start line says why not.
    let mut driver = crew_driver(CliOptions::default()).await;
    driver.agent.push(reply(
        "change",
        1,
        vec![instruction(
            "mode",
            "with the crew",
            "",
            "crew",
            "this_request",
        )],
    ));
    driver
        .agent
        .push(FakeSession::completed("Fixed the rounding."));
    driver
        .send(command(SlashCommand::Captain {
            task: "Fix the rounding with the crew.".into(),
        }))
        .await;
    let finished = driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Conversation {
                    event: ConversationEvent::TurnFinished {
                        reply: TurnReply::Change { .. },
                        ..
                    }
                }
            )
        })
        .await;
    let AppEvent::Conversation {
        event:
            ConversationEvent::TurnFinished {
                reply:
                    TurnReply::Change {
                        captain_or_crew,
                        instructions,
                        ..
                    },
                ..
            },
    } = finished
    else {
        unreachable!()
    };
    assert_eq!(captain_or_crew.mode, RunMode::Captain);
    assert_eq!(
        instructions[0].outcome,
        InstructionOutcome::NotFollowed {
            why: "you typed /captain".to_owned()
        }
    );
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Conversation {
                    event: ConversationEvent::TurnFinished {
                        reply: TurnReply::Answer { .. },
                        ..
                    }
                }
            )
        })
        .await;
    let specs = driver.agent.specs();
    assert_eq!(specs[1].access, Access::Direct);
    assert!(driver.conductor.starts().is_empty());
    driver.exit().await;

    // `/captain` with nothing that says how to run goes straight to your checkout.
    let mut driver = crew_driver(CliOptions::default()).await;
    driver.agent.push(FakeSession::completed("Fixed the typo."));
    driver
        .send(command(SlashCommand::Captain {
            task: "Fix the typo in the readme.".into(),
        }))
        .await;
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
    let specs = driver.agent.specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].access, Access::Direct);
    driver.exit().await;
}

/// `/workflow` reads your words as `/crew` does, and its run stays a workflow: who orchestrates
/// is followed, and words for Captain lose to the command.
#[tokio::test]
async fn workflow_reads_the_words_and_stays_a_workflow() {
    use argonauts_core::input::SlashCommand;

    let driver = crew_driver(CliOptions::default()).await;
    driver.agent.push(reply(
        "change",
        1,
        vec![
            this("orchestrate", "Have codex orchestrate", "codex"),
            instruction("mode", "just do it alone", "", "captain", "this_request"),
        ],
    ));
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::WorkflowRun {
                task: Some("Fix the rounding. Have codex orchestrate, just do it alone.".into()),
            },
        })
        .await;
    let start = first_start(&driver).await;
    assert_eq!((start.mode, start.forced), (RunMode::Crew, true));
    assert_eq!(said(&start, "Have codex orchestrate"), "Codex orchestrates");
    assert_eq!(why(&start, "just do it alone"), "you typed /workflow");
    let settings = run_settings(&start);
    assert_eq!(
        workers(&settings, RoleName::Orchestrator),
        ["codex".to_owned()]
    );
    // Codex is the run's own main agent, so it's Codex that designs the workflow.
    assert_eq!(settings.crew.main.agent, "codex");
    driver.exit().await;
}

/// The Captain workflow, where every message is the main session's own turn in your checkout,
/// with Codex and agy installed beside the main agent.
async fn captain_driver() -> TestDriver {
    let options = CliOptions {
        prepare: Some(Box::new(|repo: &TempRepo| {
            std::fs::write(
                repo.global_config(),
                "[captain_or_crew]\nforce = \"captain\"\n",
            )
            .unwrap();
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

/// In the Captain workflow on a main session that can't start a run itself, words that plainly
/// ask for a workflow hand the request to the crew, with who reviews followed.
#[tokio::test]
async fn captain_hands_a_request_that_asks_for_a_workflow_to_the_crew() {
    let driver = captain_driver().await;
    driver.agent.push(reply(
        "change",
        1,
        vec![this("review", "with Codex as the reviewer", "codex")],
    ));
    driver
        .submit("Use a workflow for this, with Codex as the reviewer. Fix the money rounding.")
        .await;
    let start = first_start(&driver).await;
    assert_eq!((start.mode, start.forced), (RunMode::Crew, true));
    assert_eq!(said(&start, "with Codex as the reviewer"), "Codex reviews");
    let settings = run_settings(&start);
    assert_eq!(workers(&settings, RoleName::Reviewer), ["codex"]);
    // The main session never took the request as a turn of its own.
    let specs = driver.agent.specs();
    assert_eq!(specs.len(), 1, "only the read of how to run it");
    assert_ne!(specs[0].access, Access::Direct);
    driver.exit().await;
}

/// There, naming a reviewer without asking for a workflow keeps Captain doing the work,
/// checked in a worktree, with that reviewer reading the change before it lands. A request
/// that names nobody stays the main session's own turn in your checkout.
#[tokio::test]
async fn captain_keeps_the_work_and_has_the_reviewer_you_name_check_it() {
    use argonauts_core::settings::protocol::CaptainKind;

    let driver = captain_driver().await;
    driver.agent.push(reply(
        "change",
        1,
        vec![this("review", "with Codex as the reviewer", "codex")],
    ));
    driver
        .submit("Fix the money rounding with Codex as the reviewer.")
        .await;
    let start = first_start(&driver).await;
    assert_eq!((start.mode, start.forced), (RunMode::Captain, false));
    assert_eq!(said(&start, "with Codex as the reviewer"), "Codex reviews");
    let decision = start.captain_or_crew.clone().expect("the decision");
    assert_eq!(decision.captain, Some(CaptainKind::Checked));
    let settings = run_settings(&start);
    assert!(
        settings.captain.review,
        "the change is reviewed before it lands"
    );
    assert_eq!(workers(&settings, RoleName::Reviewer), ["codex"]);
    driver.exit().await;

    let mut driver = captain_driver().await;
    driver
        .agent
        .push(FakeSession::completed("Fixed the rounding."));
    driver.submit("Fix the money rounding.").await;
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
    let specs = driver.agent.specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].access, Access::Direct);
    assert!(driver.conductor.starts().is_empty());
    driver.exit().await;
}

/// "Keep going until its tests pass": the request's own words set a goal for that request, on
/// whichever CLI is the main agent, and the start line states it back before work starts. The
/// conversation takes no goal from it.
#[tokio::test]
async fn the_requests_own_words_set_a_goal_for_that_request() {
    let driver = crew_driver(CliOptions::default()).await;
    let says = "keep going until its tests pass";
    driver.agent.push(reply(
        "change",
        4,
        vec![instruction(
            "goal",
            says,
            "",
            "its tests pass",
            "this_request",
        )],
    ));
    driver
        .submit("Fix the export, and keep going until its tests pass.")
        .await;
    let start = first_start(&driver).await;
    // No check command is set up here, so the judge reads it.
    assert_eq!(
        said(&start, says),
        "goal: its tests pass · checked by the judge"
    );
    let settings = run_settings(&start);
    assert_eq!(settings.goal.until.as_deref(), Some("its tests pass"));
    assert!(
        !start
            .overrides
            .iter()
            .any(|set| set.starts_with("goal.until")),
        "{:?}",
        start.overrides
    );
    driver.exit().await;
}
