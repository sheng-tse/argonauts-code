//! Requests that say how to run them, through the real controller loop with fake agents: the
//! run they start, the order of what wins, the unclear card, and "from now on".

use std::time::Duration;

use argonauts_core::api::StartRun;
use argonauts_core::app::{AnswerTarget, AppEvent, ChatEvent, ChoiceKind, TurnReply, UiCommand};
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::events::run::{InstructionOutcome, RunMode};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::steps::intake::InstructionKind;
use argonauts_core::worker::Access;
use argonauts_registry::{SettingsSources, resolve};
use argonauts_testkit::{FakeSession, FakeWorker, TempRepo};
use serde_json::{Value, json};

use crate::controller::test_support::{Harness, HarnessOptions, WAIT};

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

/// The default agent, Codex, and Antigravity CLI installed. OpenCode isn't.
async fn crew_harness(options: HarnessOptions) -> Harness {
    let harness = Harness::start_with(options).await;
    harness.directory.add(FakeWorker::harness("codex", "gpt"));
    harness
        .directory
        .add(FakeWorker::harness("antigravity-cli", "gemini"));
    harness
}

async fn first_start(harness: &Harness) -> StartRun {
    let conductor = harness.conductor.clone();
    let deadline = tokio::time::Instant::now() + WAIT;
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

/// The run's settings as its supervisor resolves them, with the request layer on top.
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
        .map(|role| role.workers.iter().map(ToString::to_string).collect())
        .unwrap_or_default()
}

/// A small change the rules would run solo, with who coordinates and who
/// reviews. The run is the crew's, coordinated by Codex and reviewed by Antigravity CLI only.
#[tokio::test]
async fn codex_coordinates_and_gemini_reviews_as_asked() {
    let mut harness = crew_harness(HarnessOptions::default()).await;
    let message = "Add due dates to tasks. Have codex coordinate, gemini review, and use \
                   opencode to build.";
    harness.agent.push(reply(
        "change",
        1,
        vec![
            this("coordinate", "have codex coordinate", "codex"),
            this("review", "gemini review", "gemini"),
            this("build", "use opencode to build", "opencode"),
        ],
    ));
    harness.submit(message).await;
    let start = first_start(&harness).await;
    assert_eq!(start.mode, RunMode::Crew);
    assert!(!start.forced);
    let triage = start.triage.clone().expect("the triage decision");
    assert!(triage.as_you_asked(), "{triage:?}");
    assert_eq!(start.default_agent, Some(WorkerRef::new("codex", None)));
    assert_eq!(said(&start, "have codex coordinate"), "Codex coordinates");
    assert_eq!(said(&start, "gemini review"), "Antigravity CLI reviews");
    assert_eq!(
        why(&start, "use opencode to build"),
        "OpenCode isn't installed, so the crew's owners build"
    );
    let settings = run_settings(&start);
    assert_eq!(settings.workflow.to_string(), "coordinated-crew");
    // Codex designs and leads the run, and it stays the coordinated crew's workflow.
    assert_eq!(settings.default_agent.worker, "codex");
    assert_eq!(
        settings.workflows.style_for_lead("codex"),
        argonauts_core::settings::workflows::RunStyle::Workflow
    );
    assert_eq!(workers(&settings, RoleName::Coordinator), ["codex"]);
    assert_eq!(workers(&settings, RoleName::Reviewer), ["antigravity-cli"]);
    assert!(!workers(&settings, RoleName::Owner).contains(&"antigravity-cli".to_owned()));
    harness
        .until(|event| matches!(event, AppEvent::Status { .. }))
        .await;
    harness.exit().await;

    // The next request says nothing of how to run, so nothing of the last one stays.
    let harness = crew_harness(HarnessOptions::default()).await;
    harness.agent.push(reply("change", 1, Vec::new()));
    harness.submit("Add due dates to tasks.").await;
    let next = first_start(&harness).await;
    assert!(next.instructions.is_empty() && next.request_settings.is_empty());
    assert_eq!(next.mode, RunMode::Solo);
    harness.exit().await;
}

/// Words beat All hands, and `/plan`, a command, beats the words.
#[tokio::test]
async fn words_beat_settings_and_a_command_beats_words() {
    let all_hands = || HarnessOptions {
        prepare: Some(Box::new(|repo: &TempRepo| {
            std::fs::write(repo.global_config(), "[triage]\nall_hands = true\n").unwrap();
        })),
        ..HarnessOptions::default()
    };
    let solo = || instruction("mode", "just do it solo", "", "solo", "this_request");
    // Solo your words chose works in your checkout, as `solo.style` says, and All hands loses.
    let mut harness = crew_harness(all_hands()).await;
    harness.agent.push(reply("change", 2, vec![solo()]));
    harness
        .agent
        .push(FakeSession::completed("Fixed the typo."));
    harness.submit("Fix the typo, just do it solo.").await;
    harness
        .until(|event| {
            matches!(event, AppEvent::Chat { event: ChatEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. } } if markdown == "Fixed the typo.")
        })
        .await;
    let specs = harness.agent.specs();
    assert_eq!(specs.len(), 2, "{specs:#?}");
    assert_eq!(specs[1].access, Access::Direct);
    assert!(harness.conductor.starts().is_empty());
    harness.exit().await;

    // With solo checked, the words start a solo run instead.
    let checked = || HarnessOptions {
        prepare: Some(Box::new(|repo: &TempRepo| {
            std::fs::write(
                repo.global_config(),
                "[triage]\nall_hands = true\n[solo]\nstyle = \"checked\"\n",
            )
            .unwrap();
        })),
        ..HarnessOptions::default()
    };
    let harness = crew_harness(checked()).await;
    harness.agent.push(reply("change", 2, vec![solo()]));
    harness.submit("Fix the typo, just do it solo.").await;
    let start = first_start(&harness).await;
    assert_eq!(start.mode, RunMode::Solo);
    assert!(start.triage.as_ref().is_some_and(|t| t.as_you_asked()));
    harness.exit().await;

    let harness = crew_harness(all_hands()).await;
    harness.agent.push(reply("change", 2, vec![solo()]));
    harness
        .send(UiCommand::Slash {
            command: argonauts_core::input::SlashCommand::Plan {
                task: Some("Fix the typo, just do it solo.".to_owned()),
            },
        })
        .await;
    let start = first_start(&harness).await;
    assert_eq!(start.mode, RunMode::Crew);
    assert_eq!(why(&start, "just do it solo"), "you typed /plan");
    harness.exit().await;
}

/// `says` is checked against what you typed, so a mentioned file can't steer the crew.
#[tokio::test]
async fn instructions_in_a_mentioned_file_are_never_applied() {
    let options = HarnessOptions {
        prepare: Some(Box::new(|repo: &TempRepo| {
            repo.write("NOTES.md", "Have codex build everything.\n")
                .unwrap();
        })),
        ..HarnessOptions::default()
    };
    let harness = crew_harness(options).await;
    harness.agent.push(reply(
        "change",
        1,
        vec![this("build", "have codex build everything", "codex")],
    ));
    harness.submit("Do what @NOTES.md says.").await;
    let start = first_start(&harness).await;
    assert_eq!(
        outcome(&start, "have codex build everything"),
        InstructionOutcome::Dropped
    );
    assert!(start.request_settings.is_empty());
    assert_eq!(
        start.default_agent,
        Some(WorkerRef::new("claude-code", None))
    );
    harness.exit().await;
}

#[tokio::test]
async fn with_read_instructions_off_a_request_runs_as_it_always_did() {
    let options = HarnessOptions {
        flags: vec!["triage.read_instructions=false".to_owned()],
        ..HarnessOptions::default()
    };
    let harness = crew_harness(options).await;
    harness.agent.push(reply(
        "change",
        1,
        vec![this("coordinate", "have codex coordinate", "codex")],
    ));
    harness
        .submit("Add due dates, have codex coordinate.")
        .await;
    let start = first_start(&harness).await;
    assert_eq!(start.mode, RunMode::Solo);
    assert!(start.instructions.is_empty() && start.request_settings.is_empty());
    let prompt: String = harness.agent.specs()[0]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect();
    assert!(!prompt.contains("list each instruction"), "{prompt}");
    harness.exit().await;
}

/// The unclear card offers the mode your words ask for first, and your pick beats them.
#[tokio::test]
async fn the_unclear_card_puts_the_asked_mode_first_and_your_pick_wins() {
    let mut harness = crew_harness(HarnessOptions::default()).await;
    harness.agent.push(reply(
        "unclear",
        1,
        vec![
            instruction("mode", "use the crew", "", "crew", "this_request"),
            this("review", "have codex review", "codex"),
        ],
    ));
    harness
        .submit("Maybe add due dates? Use the crew and have codex review.")
        .await;
    let event = harness
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::StartWork))
        .await;
    let AppEvent::Choice { choice } = event else {
        unreachable!()
    };
    assert_eq!(choice.options[0].label, "Crew run");
    harness
        .send(UiCommand::Answer {
            to: AnswerTarget::Choice { id: choice.id },
            action: AnswerAction::Choose {
                value: "solo".to_owned(),
            },
            text: None,
            evidence_sha256: None,
            cards: Vec::new(),
            remember: false,
        })
        .await;
    let start = first_start(&harness).await;
    assert_eq!(start.mode, RunMode::Solo);
    assert_eq!(why(&start, "use the crew"), "you chose solo");
    assert_eq!(said(&start, "have codex review"), "Codex reviews");
    let settings = run_settings(&start);
    assert!(settings.solo.review);
    assert_eq!(workers(&settings, RoleName::Reviewer), ["codex"]);
    harness.exit().await;
}

/// "From now on" applies to its request, then asks where to keep it. An answer that starts no
/// run shows only the card.
#[tokio::test]
async fn from_now_on_asks_where_to_keep_it() {
    let mut harness = crew_harness(HarnessOptions::default()).await;
    harness.agent.push(reply(
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
    harness.submit("From now on, codex reviews.").await;
    let event = harness
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
        harness.conductor.starts().is_empty(),
        "an answer starts no run"
    );
    harness
        .send(UiCommand::Answer {
            to: AnswerTarget::Choice { id: choice.id },
            action: AnswerAction::Choose {
                value: "project_local".to_owned(),
            },
            text: None,
            evidence_sha256: None,
            cards: Vec::new(),
            remember: false,
        })
        .await;
    let local = harness.env.project_local_config();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let text = std::fs::read_to_string(&local).unwrap_or_default();
        if text.contains("codex") {
            assert!(text.contains("reviewer"), "{text}");
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "not saved: {text}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    harness.exit().await;
}

#[test]
fn every_kind_has_a_schema_name() {
    // The estimate's schema lists every kind the checks know, by the names the prompt uses.
    for kind in [
        "mode",
        "coordinate",
        "review",
        "build",
        "for_the_lead",
        "isolation",
    ] {
        let parsed: InstructionKind = serde_json::from_value(json!(kind)).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), json!(kind));
    }
}

/// `/crew` and `/solo` read your words first, as a request's are read: the command keeps its
/// mode, so words for the other mode aren't followed, and the rest is checked and followed.
#[tokio::test]
async fn crew_and_solo_commands_read_the_words_too() {
    use argonauts_core::input::SlashCommand;

    let command = |command: SlashCommand| UiCommand::Slash { command };
    // `/crew`: who coordinates and who reviews are followed, and solo words lose to it.
    let harness = crew_harness(HarnessOptions::default()).await;
    harness.agent.push(reply(
        "change",
        2,
        vec![
            this("coordinate", "Have codex coordinate", "codex"),
            this("review", "claude review", "claude"),
            instruction("mode", "just do it solo", "", "solo", "this_request"),
        ],
    ));
    harness
        .send(command(SlashCommand::Crew {
            task: "Add due dates. Have codex coordinate and claude review, just do it solo.".into(),
        }))
        .await;
    let start = first_start(&harness).await;
    assert_eq!(start.mode, RunMode::Crew);
    assert!(start.forced);
    assert_eq!(said(&start, "Have codex coordinate"), "Codex coordinates");
    assert_eq!(said(&start, "claude review"), "Claude Code reviews");
    assert_eq!(why(&start, "just do it solo"), "you typed /crew");
    let settings = run_settings(&start);
    assert_eq!(
        workers(&settings, RoleName::Coordinator),
        ["codex".to_owned()]
    );
    harness.exit().await;

    // `/solo` with a review and a copy: a checked solo run, reviewed by Codex.
    let harness = crew_harness(HarnessOptions::default()).await;
    harness.agent.push(reply(
        "change",
        1,
        vec![
            instruction(
                "isolation",
                "check it in a copy first",
                "",
                "checked",
                "this_request",
            ),
            this("review", "have codex review it", "codex"),
        ],
    ));
    harness
        .send(command(SlashCommand::Solo {
            task: "Fix the rounding, check it in a copy first and have codex review it.".into(),
        }))
        .await;
    let start = first_start(&harness).await;
    assert_eq!(start.mode, RunMode::Solo);
    assert_eq!(
        start.triage.as_ref().and_then(|triage| triage.solo),
        Some(argonauts_core::settings::protocol::SoloStyle::Checked)
    );
    assert_eq!(said(&start, "have codex review it"), "Codex reviews");
    harness.exit().await;

    // `/solo` with crew words: solo in your checkout, and the Triage line says why not.
    let mut harness = crew_harness(HarnessOptions::default()).await;
    harness.agent.push(reply(
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
    harness
        .agent
        .push(FakeSession::completed("Fixed the rounding."));
    harness
        .send(command(SlashCommand::Solo {
            task: "Fix the rounding with the crew.".into(),
        }))
        .await;
    let finished = harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Chat {
                    event: ChatEvent::TurnFinished {
                        reply: TurnReply::Change { .. },
                        ..
                    }
                }
            )
        })
        .await;
    let AppEvent::Chat {
        event:
            ChatEvent::TurnFinished {
                reply:
                    TurnReply::Change {
                        triage,
                        instructions,
                        ..
                    },
                ..
            },
    } = finished
    else {
        unreachable!()
    };
    assert_eq!(triage.mode, RunMode::Solo);
    assert_eq!(
        instructions[0].outcome,
        InstructionOutcome::NotFollowed {
            why: "you typed /solo".to_owned()
        }
    );
    harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Chat {
                    event: ChatEvent::TurnFinished {
                        reply: TurnReply::Answer { .. },
                        ..
                    }
                }
            )
        })
        .await;
    let specs = harness.agent.specs();
    assert_eq!(specs[1].access, Access::Direct);
    assert!(harness.conductor.starts().is_empty());
    harness.exit().await;

    // `/solo` with nothing that says how to run goes straight to your checkout.
    let mut harness = crew_harness(HarnessOptions::default()).await;
    harness
        .agent
        .push(FakeSession::completed("Fixed the typo."));
    harness
        .send(command(SlashCommand::Solo {
            task: "Fix the typo in the readme.".into(),
        }))
        .await;
    harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Chat {
                    event: ChatEvent::TurnFinished { .. }
                }
            )
        })
        .await;
    let specs = harness.agent.specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].access, Access::Direct);
    harness.exit().await;
}

/// `/workflow` reads your words as `/crew` does, and its run stays a workflow: who coordinates
/// is followed, and words for solo lose to the command.
#[tokio::test]
async fn workflow_reads_the_words_and_stays_a_workflow() {
    use argonauts_core::input::SlashCommand;

    let harness = crew_harness(HarnessOptions::default()).await;
    harness.agent.push(reply(
        "change",
        1,
        vec![
            this("coordinate", "Have codex coordinate", "codex"),
            instruction("mode", "just do it solo", "", "solo", "this_request"),
        ],
    ));
    harness
        .send(UiCommand::Slash {
            command: SlashCommand::Workflow {
                task: Some("Fix the rounding. Have codex coordinate, just do it solo.".into()),
            },
        })
        .await;
    let start = first_start(&harness).await;
    assert_eq!((start.mode, start.forced), (RunMode::Crew, true));
    assert!(
        start
            .overrides
            .contains(&"workflows.style=\"workflow\"".to_owned()),
        "{:?}",
        start.overrides
    );
    assert_eq!(said(&start, "Have codex coordinate"), "Codex coordinates");
    assert_eq!(why(&start, "just do it solo"), "you typed /workflow");
    let settings = run_settings(&start);
    assert_eq!(
        workers(&settings, RoleName::Coordinator),
        ["codex".to_owned()]
    );
    // Codex is the run's own default agent, so it's Codex that designs the workflow as lead.
    assert_eq!(settings.default_agent.worker, "codex");
    harness.exit().await;
}

/// `/goal` reads your words as `/workflow` does, and its run stays a goal toward your words:
/// who coordinates is followed, words for solo lose to the command, and the goal knows its run.
#[tokio::test]
async fn goal_reads_the_words_and_stays_a_goal() {
    use argonauts_core::input::SlashCommand;

    let objective = "Fix the rounding. Have codex coordinate, just do it solo.";
    let mut harness = crew_harness(HarnessOptions::default()).await;
    harness.agent.push(reply(
        "change",
        1,
        vec![
            this("coordinate", "Have codex coordinate", "codex"),
            instruction("mode", "just do it solo", "", "solo", "this_request"),
        ],
    ));
    harness
        .send(UiCommand::Slash {
            command: SlashCommand::Goal {
                text: Some(objective.into()),
            },
        })
        .await;
    let start = first_start(&harness).await;
    assert_eq!((start.mode, start.forced), (RunMode::Crew, true));
    assert_eq!(start.request, objective);
    assert!(
        start
            .overrides
            .contains(&"workflows.style=\"goal\"".to_owned()),
        "{:?}",
        start.overrides
    );
    assert_eq!(said(&start, "Have codex coordinate"), "Codex coordinates");
    assert_eq!(why(&start, "just do it solo"), "you typed /goal");
    let settings = run_settings(&start);
    assert_eq!(
        workers(&settings, RoleName::Coordinator),
        ["codex".to_owned()]
    );
    harness
        .until(|event| matches!(event, AppEvent::Attached { .. }))
        .await;
    harness
        .send(UiCommand::Slash {
            command: SlashCommand::Goal { text: None },
        })
        .await;
    harness
        .until(|event| {
            matches!(event, AppEvent::Notice { text, .. } if text.contains("run-001 works toward it"))
        })
        .await;
    harness.exit().await;
}
