//! The conversation through the real controller loop: WORKFLOW's examples 1 and 4, `/ask`,
//! `/captain`, queued turns, Esc, resume, and fallbacks.

mod agent;
mod autopilot;
mod carry;
mod cli;
mod direct;
mod direct_kept_out;
mod direct_outside;
mod in_cli;
mod live;
mod main_session;
mod mention;
mod picture;
mod request;
mod resume;
mod shared;
mod starting;

use std::time::Duration;

use argonauts_core::agents::session_runner::{Access, RunnerEvent, SessionControl};
use argonauts_core::app::{
    AnswerTarget, AppEvent, AppMode, ChoiceKind, ConversationEvent, TurnReply, UiCommand,
};
use argonauts_core::base::names::{AgentId, ChoiceId};
use argonauts_core::input::SlashCommand;
use argonauts_core::models::limits::{
    PlanUsageBoard as _, PlanUsageReading, PlanUsageSource, PlanUsageStatus,
};
use argonauts_core::runs::event::AgentEvent;
use argonauts_core::runs::events::approval::AnswerAction;
use argonauts_core::runs::events::run::RunMode;
use argonauts_core::runs::events::session::EndReason;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::workflows::steps::StepName;
use argonauts_testkit::{FakeRunner, FakeSession, Hold};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::controller::base::test_support::{CliOptions, TestCtx, TestDriver, max_wait};
use crate::controller::conversation::{self, ConversationState};
use crate::controller::{Effect, Internal};
use crate::conversation::store::ConversationStore;
use crate::startup::app_settings::AppSettings;

fn estimate() -> Value {
    json!({
        "task_type": "api-feature",
        "summary": "Add rate limiting after auth",
        "files_likely": ["src/middleware.rs", "src/server.rs"],
        "file_count": 9,
        "risk_labels": ["api"],
        "tests_exist": true,
        "unknowns": ["one instance or several"]
    })
}

fn answer(text: &str) -> FakeSession {
    FakeSession::structured(json!({"kind": "answer", "answer_markdown": text, "estimate": null}))
}

fn reply(kind: &str, text: &str) -> FakeSession {
    FakeSession::structured(json!({"kind": kind, "answer_markdown": text, "estimate": estimate()}))
}

fn conversation_event(event: &AppEvent) -> Option<&ConversationEvent> {
    match event {
        AppEvent::Conversation { event } => Some(event),
        _ => None,
    }
}

fn finished(event: &AppEvent) -> bool {
    matches!(
        conversation_event(event),
        Some(ConversationEvent::TurnFinished { .. } | ConversationEvent::TurnFailed { .. })
    )
}

fn idle(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Mode {
            mode: AppMode::Idle
        }
    )
}

async fn turn_done(driver: &mut TestDriver) -> AppEvent {
    let event = driver.until(finished).await;
    driver.until(idle).await;
    event
}

fn answer_choice(id: ChoiceId, value: &str) -> UiCommand {
    UiCommand::Answer {
        to: AnswerTarget::Choice { id },
        action: AnswerAction::Choose {
            value: value.to_owned(),
        },
        text: None,
        evidence_sha256: None,
        tasks: Vec::new(),
        remember: false,
    }
}

/// Polls state the events don't show, such as what a fake session received.
async fn eventually(what: impl Fn() -> bool) {
    let wait = max_wait();
    let deadline = tokio::time::Instant::now() + wait;
    while !what() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "not within {wait:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn prompt_text(driver: &TestDriver, index: usize) -> String {
    driver.agent.specs()[index]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect()
}

#[tokio::test]
async fn hi_answers_and_nothing_starts() {
    let mut driver = TestDriver::start().await;
    driver
        .agent
        .push(answer("Hi. Ask me anything about this repo."));
    driver.submit("hi").await;
    let mode = driver
        .until(|event| matches!(event, AppEvent::Mode { .. }))
        .await;
    assert!(
        matches!(
            mode,
            AppEvent::Mode {
                mode: AppMode::Answering { .. }
            }
        ),
        "{mode:?}"
    );
    let done = turn_done(&mut driver).await;
    let Some(ConversationEvent::TurnFinished { reply, .. }) = conversation_event(&done) else {
        panic!("{done:?}");
    };
    assert_eq!(
        reply,
        &TurnReply::Answer {
            markdown: "Hi. Ask me anything about this repo.".to_owned()
        }
    );
    let spec = &driver.agent.specs()[0];
    assert_eq!(spec.access, Access::ReadOnly);
    assert_eq!(spec.workdir, driver.repo.root());
    assert_eq!(spec.step, Some(StepName::Estimate));
    assert_eq!(spec.resume, None);
    assert!(spec.json_schema.is_some());
    assert!(prompt_text(&driver, 0).ends_with("hi\n"));
    let events = driver.exit().await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AppEvent::Choice { .. })),
        "{events:#?}"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::Status { status } if status.title == "hi"
    )));
}

#[tokio::test]
async fn a_turns_spend_goes_to_the_usage_ledger_when_it_ends() {
    let mut driver = TestDriver::start().await;
    let spent = argonauts_core::Spend {
        input_tokens: 5000,
        output_tokens: 300,
        cached_tokens: 4000,
        cost_usd: 0.12,
        ..argonauts_core::Spend::default()
    };
    driver
        .agent
        .push(answer("Hi. Ask me anything about this repo.").with_spend(spent));
    driver.submit("hi").await;
    turn_done(&mut driver).await;
    let ledger = crate::usage::usage_ledger::UsageLedger::new(&argonauts_core::Paths::new(
        driver.repo.home(),
    ));
    let lines = ledger.read_since(None);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].spent(), spent);
    assert_eq!(lines[0].run, None);
    driver.exit().await;
}

/// With no model picked, the CLI runs its own default and says which it is, so the ledger, and
/// Stats from it, name that model rather than the CLI.
#[tokio::test]
async fn a_turn_on_the_clis_default_model_is_recorded_under_the_model_it_named() {
    let mut driver = TestDriver::start().await;
    let spent = argonauts_core::Spend {
        input_tokens: 900,
        output_tokens: 40,
        cost_usd: 0.01,
        ..argonauts_core::Spend::default()
    };
    let init = RunnerEvent::Agent(AgentEvent::Init {
        session_id: Some("s-1".into()),
        model: Some("claude-opus-5-5".into()),
        tools: Vec::new(),
        plugins: Vec::new(),
        capabilities: Vec::new(),
        version: None,
    });
    driver.agent.push(
        answer("Hi. Ask me anything about this repo.")
            .with_event(init)
            .with_spend(spent),
    );
    driver.submit("hi").await;
    turn_done(&mut driver).await;
    assert_eq!(driver.agent.specs()[0].model.model, None);
    let ledger = crate::usage::usage_ledger::UsageLedger::new(&argonauts_core::Paths::new(
        driver.repo.home(),
    ));
    let lines = ledger.read_since(None);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].model.as_deref(), Some("claude-opus-5-5"));
    driver.exit().await;
}

#[tokio::test]
async fn an_unclear_request_offers_start_work_and_n_starts_nothing() {
    let mut driver = TestDriver::start().await;
    driver.agent.push(reply(
        "unclear",
        "Put a middleware right after auth. Expect about 9 files, including tests.",
    ));
    driver.submit("how should we add rate limiting?").await;
    let offered = driver
        .until(|event| matches!(event, AppEvent::Choice { .. }))
        .await;
    let AppEvent::Choice { choice } = offered else {
        unreachable!()
    };
    assert_eq!(choice.kind, ChoiceKind::StartWork);
    assert!(
        choice.title.starts_with(
            "This could be a change request. Start work on it? Argonauts Code would pick "
        ),
        "{}",
        choice.title
    );
    let keys: String = choice.options.iter().map(|option| option.key).collect();
    assert_eq!(keys, "scn");
    let unclear = driver
        .seen()
        .iter()
        .find_map(|event| match conversation_event(event) {
            Some(ConversationEvent::TurnFinished {
                reply:
                    TurnReply::Unclear {
                        would_pick,
                        estimate,
                        ..
                    },
                ..
            }) => Some((*would_pick, estimate.is_some())),
            _ => None,
        });
    assert!(matches!(unclear, Some((Some(_), true))), "{unclear:?}");

    // Esc goes on without you: the answer stands and nothing starts.
    driver
        .send(UiCommand::SetAside {
            to: AnswerTarget::Choice {
                id: choice.id.clone(),
            },
        })
        .await;
    let closed = driver
        .until(|event| matches!(event, AppEvent::ChoiceClosed { .. }))
        .await;
    assert_eq!(
        closed,
        AppEvent::ChoiceClosed {
            id: choice.id.clone(),
            outcome: "No · nothing started".to_owned()
        }
    );
    driver.send(answer_choice(choice.id, "captain")).await;
    driver
        .until(|event| {
            matches!(event, AppEvent::Notice { text, .. } if text.contains("already closed"))
        })
        .await;
    assert!(driver.conductor.starts().is_empty());
    driver.exit().await;
}

/// With Ultracode on, a one-file change that the rules would give one agent goes to the crew,
/// whose main agent then designs its workflow. A question is still just answered, and `/captain`
/// still runs one task Captain.
#[tokio::test]
async fn ultracode_sends_every_change_to_the_crew() {
    let ultracode = || CliOptions {
        prepare: Some(Box::new(|repo: &argonauts_testkit::TempRepo| {
            std::fs::write(
                repo.global_config(),
                "ultracode = true\n[captain]\nkind = \"checked\"\n",
            )
            .unwrap();
        })),
        ..CliOptions::default()
    };
    let small = json!({
        "task_type": "docs",
        "summary": "Fix the install command in the README",
        "files_likely": ["README.md"],
        "file_count": 1,
        "risk_labels": [],
        "tests_exist": true,
        "unknowns": []
    });
    let mut driver = TestDriver::start_with(ultracode()).await;
    driver.agent.push(answer("Hi."));
    driver.submit("hi").await;
    turn_done(&mut driver).await;
    assert!(
        driver.conductor.starts().is_empty(),
        "a question is answered"
    );

    driver.agent.push(FakeSession::structured(json!({
        "kind": "change",
        "answer_markdown": "The README's install line names the wrong package.",
        "estimate": small
    })));
    driver.submit("fix the install command in the README").await;
    driver.until(finished).await;
    let conductor = driver.conductor.clone();
    eventually(|| conductor.starts().len() == 1).await;
    let start = conductor.starts().remove(0);
    assert_eq!(start.mode, RunMode::Crew);
    assert!(!start.forced);
    let captain_or_crew = start.captain_or_crew.expect("the Captain-or-crew decision");
    assert_eq!(
        (
            captain_or_crew.rule.as_str(),
            captain_or_crew.reason.as_str()
        ),
        ("ultracode", "Ultracode is on")
    );
    driver.exit().await;

    let driver = TestDriver::start_with(ultracode()).await;
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Captain {
                task: "fix the install command in the README".to_owned(),
            },
        })
        .await;
    let conductor = driver.conductor.clone();
    eventually(|| conductor.starts().len() == 1).await;
    let start = conductor.starts().remove(0);
    assert_eq!((start.mode, start.forced), (RunMode::Captain, true));
    driver.exit().await;
}

/// Checked Captain, which `/captain` runs as a run in a worktree.
pub(crate) fn checked_captain() -> CliOptions {
    CliOptions {
        prepare: Some(Box::new(|repo: &argonauts_testkit::TempRepo| {
            std::fs::write(repo.global_config(), "[captain]\nkind = \"checked\"\n").unwrap();
        })),
        ..CliOptions::default()
    }
}

#[tokio::test]
async fn checked_captain_skips_the_estimate() {
    let mut driver = TestDriver::start_with(checked_captain()).await;
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Captain {
                task: "fix the typo in the README".to_owned(),
            },
        })
        .await;
    driver
        .until(|event| {
            matches!(event, AppEvent::Status { status } if status.title == "fix the typo in the README")
        })
        .await;
    let agent = driver.agent.clone();
    let conductor = driver.conductor.clone();
    let events = driver.exit().await;
    assert!(agent.specs().is_empty());
    assert!(
        !events
            .iter()
            .any(|event| conversation_event(event).is_some()),
        "{events:#?}"
    );
    // Starting belongs to the runs part; whatever it sent is the forced request.
    for start in conductor.starts() {
        assert!(start.forced);
        assert_eq!(start.mode, RunMode::Captain);
        assert_eq!(start.estimate, None);
    }
}

/// `/captain` while the estimate still reads the same request: its reply starts nothing more.
async fn captain_during_the_estimate(kind: &str) -> Vec<AppEvent> {
    let mut driver = TestDriver::start_with(checked_captain()).await;
    let estimate = Hold::new();
    driver
        .agent
        .push(reply(kind, "Add a middleware after auth.").held_after(0, &estimate));
    driver.submit("add rate limiting").await;
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Mode {
                    mode: AppMode::Answering { .. }
                }
            )
        })
        .await;
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Captain {
                task: "add rate limiting".to_owned(),
            },
        })
        .await;
    driver.taken().await;
    estimate.release();
    driver.until(finished).await;
    let conductor = driver.conductor.clone();
    let events = driver.exit().await;
    let starts = conductor.starts();
    assert_eq!(starts.len(), 1, "{starts:#?}");
    assert!(starts[0].forced);
    assert_eq!(starts[0].mode, RunMode::Captain);
    events
}

#[tokio::test]
async fn captain_while_the_estimate_reads_a_change_starts_one_run() {
    captain_during_the_estimate("change").await;
}

#[tokio::test]
async fn captain_while_the_estimate_reads_an_unclear_request_offers_nothing() {
    let events = captain_during_the_estimate("unclear").await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AppEvent::Choice { .. })),
        "{events:#?}"
    );
}

#[tokio::test]
async fn forced_runs_and_estimated_changes_carry_what_start_needs() {
    // With words left unread, `/crew` starts at once, as it always did.
    let (mut parts, _) = TestCtx::new(CliOptions {
        prepare: Some(Box::new(|repo: &argonauts_testkit::TempRepo| {
            std::fs::write(
                repo.global_config(),
                "[captain_or_crew]\nread_instructions = false\n",
            )
            .unwrap();
        })),
        ..CliOptions::default()
    });
    let mut conversation = ConversationState::new(&parts.ctx).unwrap();
    let effects = conversation::start_forced(
        &mut parts.ctx,
        &mut conversation,
        RunMode::Crew,
        " add auth ".into(),
    )
    .await
    .unwrap();
    let Some(Effect::StartRun(request)) = effects.last() else {
        panic!("{effects:?}");
    };
    assert_eq!(request.request, "add auth");
    assert_eq!(request.mode, RunMode::Crew);
    assert!(request.forced);
    assert_eq!(request.estimate, None);
    assert_eq!(request.conversation_turn, None);
    assert!(parts.agent.specs().is_empty());

    parts
        .agent
        .push(reply("change", "Add a middleware after auth."));
    conversation::new_request(
        &mut parts.ctx,
        &mut conversation,
        "add rate limiting".into(),
    )
    .await
    .unwrap();
    let mut effects = Vec::new();
    while effects.is_empty() {
        let message = tokio::time::timeout(max_wait(), parts.internal.recv())
            .await
            .expect("the turn ends")
            .expect("the channel stays open");
        let Internal::Conversation(update) = message else {
            continue;
        };
        effects = conversation::on_update(&mut parts.ctx, &mut conversation, update)
            .await
            .unwrap();
    }
    let [Effect::StartRun(request)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert!(!request.forced);
    let captain_or_crew = request
        .captain_or_crew
        .as_ref()
        .expect("the Captain-or-crew decision");
    assert_eq!(request.mode, captain_or_crew.mode);
    assert_eq!(request.estimate.as_ref().map(|e| e.file_count), Some(9));
    assert!(request.conversation_turn.is_some());
    assert_eq!(parts.ctx.mode, AppMode::Idle);
}

#[tokio::test]
async fn text_while_answering_queues_the_next_turn() {
    let mut driver = TestDriver::start().await;
    let answering = Hold::new();
    driver
        .agent
        .push(answer("first answer").held_after(0, &answering));
    driver.agent.push(answer("second answer"));
    driver.submit("first question").await;
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Mode {
                    mode: AppMode::Answering { .. }
                }
            )
        })
        .await;
    driver.submit("second question").await;
    driver.taken().await;
    answering.release();
    // The second starts only once the first finished.
    let first = driver.until(finished).await;
    let second_started = driver
        .until(|event| {
            matches!(
                conversation_event(event),
                Some(ConversationEvent::TurnStarted { .. })
            )
        })
        .await;
    let Some(ConversationEvent::TurnStarted { text, .. }) = conversation_event(&second_started)
    else {
        unreachable!()
    };
    assert_eq!(text, "second question");
    let second = turn_done(&mut driver).await;
    let turn_of = |event: &AppEvent| match conversation_event(event) {
        Some(ConversationEvent::TurnFinished { turn, .. }) => Some(turn.clone()),
        _ => None,
    };
    assert_ne!(turn_of(&first), turn_of(&second));
    assert!(prompt_text(&driver, 1).ends_with("second question\n"));
    driver.exit().await;
}

#[tokio::test]
async fn turn_started_carries_the_commit_uncommitted_paths_and_settings_hash() {
    let mut driver = TestDriver::start_with(CliOptions {
        prepare: Some(Box::new(|repo| {
            repo.write("notes.txt", "draft\n").unwrap();
        })),
        ..CliOptions::default()
    })
    .await;
    driver.agent.push(answer("Two files."));
    driver.submit("what changed?").await;
    let started = driver
        .until(|event| {
            matches!(
                conversation_event(event),
                Some(ConversationEvent::TurnStarted { .. })
            )
        })
        .await;
    let Some(ConversationEvent::TurnStarted {
        commit,
        uncommitted,
        settings_sha256,
        read_only,
        worker,
        ..
    }) = conversation_event(&started).cloned()
    else {
        unreachable!()
    };
    assert_eq!(commit, driver.repo.head().unwrap());
    assert!(
        uncommitted.contains(&"notes.txt".to_owned()),
        "{uncommitted:?}"
    );
    assert!(read_only);
    assert_eq!(worker.agent, "claude-code");
    let flags = crate::controller::base::test_support::quiet_local_servers(Vec::new());
    let settings = AppSettings::load(&driver.env, flags, true).unwrap();
    assert_eq!(settings_sha256.as_deref(), Some(settings.sha256()));
    assert!(prompt_text(&driver, 0).contains("uncommitted file"));
    turn_done(&mut driver).await;

    let conversation_dir = driver.env.conversation_dir();
    let journal = ConversationStore::open(
        &conversation_dir,
        &argonauts_core::base::names::ConversationId::numbered(1),
    );
    let recorded = journal.events().unwrap();
    assert!(
        matches!(recorded.first(), Some(ConversationEvent::TurnStarted { commit: c, .. }) if *c == commit)
    );
    assert!(matches!(
        recorded.last(),
        Some(ConversationEvent::TurnFinished { .. })
    ));
    driver.exit().await;

    let mut including = TestDriver::start_with(CliOptions {
        flags: vec!["intake.include_uncommitted=true".to_owned()],
        prepare: Some(Box::new(|repo| {
            repo.write("notes.txt", "draft\n").unwrap();
        })),
        ..CliOptions::default()
    })
    .await;
    including.agent.push(answer("ok"));
    including.submit("what changed?").await;
    let started = including
        .until(|event| {
            matches!(
                conversation_event(event),
                Some(ConversationEvent::TurnStarted { .. })
            )
        })
        .await;
    assert!(matches!(
        conversation_event(&started),
        Some(ConversationEvent::TurnStarted { uncommitted, .. }) if uncommitted.is_empty()
    ));
    including.exit().await;
}

#[tokio::test]
async fn turn_two_resumes_turn_ones_cli_session() {
    let mut driver = TestDriver::start().await;
    driver.agent.push(answer("one").with_cli_session("cli-7"));
    driver.agent.push(answer("two").with_cli_session("cli-7"));
    driver.submit("first").await;
    turn_done(&mut driver).await;
    driver.submit("second").await;
    turn_done(&mut driver).await;
    let specs = driver.agent.specs();
    assert_eq!(specs[0].resume, None);
    assert_eq!(specs[1].resume.as_deref(), Some("cli-7"));
    assert_eq!(specs[0].model, specs[1].model);
    assert_ne!(specs[0].key, specs[1].key);
    let store = ConversationStore::open(
        &driver.env.conversation_dir(),
        &argonauts_core::base::names::ConversationId::numbered(1),
    );
    assert_eq!(
        store.session().map(|session| session.cli_session),
        Some("cli-7".to_owned())
    );
    driver.exit().await;
}

#[tokio::test]
async fn a_session_that_cant_resume_is_primed_with_the_conversation_so_far() {
    let (mut parts, _) = TestCtx::new(CliOptions {
        flags: vec!["sessions.conversation_resumes=false".to_owned()],
        ..CliOptions::default()
    });
    let mut conversation = ConversationState::new(&parts.ctx).unwrap();
    parts
        .agent
        .push(answer("Use a middleware.").with_cli_session("v-1"));
    parts
        .agent
        .push(answer("Nine files.").with_cli_session("v-2"));
    run_turn(
        &mut parts,
        &mut conversation,
        "how do we add rate limiting?",
    )
    .await;
    run_turn(&mut parts, &mut conversation, "how many files?").await;
    let specs = parts.agent.specs();
    assert_eq!(specs[1].resume, None);
    let primer = &specs[1].prompt[0];
    assert_eq!(primer.label, "conversation so far");
    assert!(
        primer.text.contains("how do we add rate limiting?"),
        "{}",
        primer.text
    );
    assert!(primer.text.contains("Use a middleware."), "{}", primer.text);
    let store = ConversationStore::open(
        &parts.env.conversation_dir(),
        &argonauts_core::base::names::ConversationId::numbered(1),
    );
    assert!(
        primer
            .text
            .contains(&store.journal_path().display().to_string()),
        "{}",
        primer.text
    );
}

async fn run_turn(parts: &mut TestCtx, conversation: &mut ConversationState, text: &str) {
    conversation::new_request(&mut parts.ctx, conversation, text.to_owned())
        .await
        .unwrap();
    loop {
        let message = tokio::time::timeout(max_wait(), parts.internal.recv())
            .await
            .expect("the turn ends")
            .expect("the channel stays open");
        let Internal::Conversation(update) = message else {
            continue;
        };
        let done = matches!(update, conversation::TurnUpdate::Done { .. });
        conversation::on_update(&mut parts.ctx, conversation, update)
            .await
            .unwrap();
        if done {
            return;
        }
    }
}

#[tokio::test]
async fn cancel_turn_stops_the_step_session() {
    let mut driver = TestDriver::start().await;
    driver
        .agent
        .push(FakeSession::until_stopped(vec![RunnerEvent::Agent(
            AgentEvent::TextDelta {
                text: "Looking at the build".to_owned(),
                parent: None,
            },
        )]));
    driver.agent.push(answer("Done."));
    driver.submit("explain the build").await;
    driver
        .until(|event| {
            matches!(
                conversation_event(event),
                Some(ConversationEvent::Agent {
                    event: AgentEvent::TextDelta { .. },
                    ..
                })
            )
        })
        .await;
    driver.submit("and the tests?").await;
    driver.send(UiCommand::CancelTurn).await;
    driver
        .until(|event| {
            matches!(
                conversation_event(event),
                Some(ConversationEvent::TurnCancelled { .. })
            )
        })
        .await;
    driver.until(idle).await;
    let taken = driver
        .until(|event| matches!(event, AppEvent::TakenBack { .. }))
        .await;
    assert_eq!(
        taken,
        AppEvent::TakenBack {
            texts: vec!["and the tests?".to_owned()]
        }
    );
    let agent = driver.agent.clone();
    eventually(|| {
        agent
            .controls()
            .iter()
            .any(|(_, control)| *control == SessionControl::Stop)
    })
    .await;

    // The conversation goes on as usual.
    driver.submit("hi").await;
    let done = turn_done(&mut driver).await;
    assert!(matches!(
        conversation_event(&done),
        Some(ConversationEvent::TurnFinished {
            reply: TurnReply::Answer { .. },
            ..
        })
    ));
    driver.exit().await;
}

#[tokio::test]
async fn a_turn_after_esc_never_resumes_a_session_still_stopping() {
    let mut driver = TestDriver::start().await;
    driver.agent.push(answer("one").with_cli_session("cli-7"));
    // Held, the session takes the stop in but acts on it only once released, so the step
    // outlives the cancel.
    let stopping = Hold::new();
    driver.agent.push(
        answer("two")
            .with_cli_session("cli-7")
            .held_after(0, &stopping),
    );
    driver.agent.push(answer("three"));
    driver.submit("first").await;
    turn_done(&mut driver).await;
    driver.submit("second").await;
    driver
        .until(|event| {
            matches!(
                conversation_event(event),
                Some(ConversationEvent::TurnStarted { .. })
            )
        })
        .await;
    driver.send(UiCommand::CancelTurn).await;
    driver.until(idle).await;
    driver.submit("third").await;
    turn_done(&mut driver).await;
    stopping.release();
    let specs = driver.agent.specs();
    assert_eq!(specs[1].resume.as_deref(), Some("cli-7"));
    assert_eq!(specs[2].resume, None);
    // A fresh session is primed with the conversation so far instead.
    assert!(prompt_text(&driver, 2).contains("one"));
    driver.exit().await;
}

#[tokio::test]
async fn ask_answers_and_never_starts_a_run() {
    let mut driver = TestDriver::start().await;
    driver.agent.push(FakeSession::structured(
        json!({"answer_markdown": "The tests live in tests/."}),
    ));
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Ask {
                question: "where are the tests?".to_owned(),
            },
        })
        .await;
    let done = turn_done(&mut driver).await;
    assert!(matches!(
        conversation_event(&done),
        Some(ConversationEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. })
            if markdown == "The tests live in tests/."
    ));
    assert_eq!(
        driver.agent.specs()[0].step,
        Some(StepName::ConversationAnswer)
    );
    assert!(driver.conductor.starts().is_empty());
    driver.exit().await;
}

fn out_of_plan_usage(agent_id: &str) -> PlanUsageReading {
    PlanUsageReading {
        agent_id: AgentId(agent_id.to_owned()),
        status: PlanUsageStatus::Rejected,
        used: Some(1.0),
        resets_at: None,
        window: None,
        source: PlanUsageSource::RateLimitEvent,
        observed_at: OffsetDateTime::now_utc(),
        detail: None,
        wrap_up: false,
    }
}

fn with_codex_fallback() -> CliOptions {
    CliOptions {
        flags: vec![r#"fallback.main.order=["codex"]"#.to_owned()],
        ..CliOptions::default()
    }
}

#[tokio::test]
async fn an_out_main_agent_hands_the_request_to_its_fallback() {
    let mut driver = TestDriver::start_with(with_codex_fallback()).await;
    let codex = driver
        .directory
        .add(FakeRunner::cli("codex", "openai").then(answer("From codex.")));
    driver.plan_usage.observe(out_of_plan_usage("claude-code"));
    driver.submit("hi").await;
    let started = driver
        .until(|event| {
            matches!(
                conversation_event(event),
                Some(ConversationEvent::TurnStarted { .. })
            )
        })
        .await;
    assert!(matches!(
        conversation_event(&started),
        Some(ConversationEvent::TurnStarted { worker, .. }) if worker.agent == "codex"
    ));
    turn_done(&mut driver).await;
    assert!(driver.agent.specs().is_empty());
    assert_eq!(codex.specs().len(), 1);
    driver.exit().await;
}

#[tokio::test]
async fn a_agent_failure_mid_turn_retries_with_the_fallback() {
    let mut driver = TestDriver::start_with(with_codex_fallback()).await;
    let codex = driver
        .directory
        .add(FakeRunner::cli("codex", "openai").then(answer("From codex.")));
    driver
        .agent
        .push(FakeSession::ended(EndReason::RateLimited));
    driver.submit("hi").await;
    let failed = driver.until(finished).await;
    assert!(matches!(
        conversation_event(&failed),
        Some(ConversationEvent::TurnFailed { error, .. }) if error.contains("rate limited")
    ));
    let done = turn_done(&mut driver).await;
    assert!(matches!(
        conversation_event(&done),
        Some(ConversationEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. })
            if markdown == "From codex."
    ));
    assert_eq!(codex.specs().len(), 1);
    driver.exit().await;
}

/// A sign-in the server revoked fails nothing: the question goes on with the fallback, and
/// its one line says the CLI is signed out and how to sign it in again.
#[tokio::test]
async fn a_revoked_sign_in_falls_back_and_says_so() {
    let mut driver = TestDriver::start_with(with_codex_fallback()).await;
    let codex = driver
        .directory
        .add(FakeRunner::cli("codex", "openai").then(answer("From codex.")));
    driver.agent.push(FakeSession::ended(EndReason::Auth {
        message: "Failed to authenticate: OAuth token revoked. Please log in again or contact \
                  your administrator."
            .into(),
    }));
    driver.submit("hi").await;
    let fell_back = driver
        .until(|event| {
            matches!(
                conversation_event(event),
                Some(ConversationEvent::FellBack { .. } | ConversationEvent::TurnFailed { .. })
            )
        })
        .await;
    let Some(ConversationEvent::FellBack { to, signed_out, .. }) = conversation_event(&fell_back)
    else {
        panic!("the turn fell back: {fell_back:?}");
    };
    assert_eq!(to.agent, "codex");
    assert_eq!(
        signed_out,
        &Some(argonauts_core::app::SignedOut {
            why: "its sign-in was revoked".into(),
            login: Some("claude /login".into()),
        })
    );
    let done = turn_done(&mut driver).await;
    assert!(matches!(
        conversation_event(&done),
        Some(ConversationEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. })
            if markdown == "From codex."
    ));
    assert_eq!(codex.specs().len(), 1);
    driver.exit().await;
}

/// With the main session's mode set to ask, a turn that only answers asks who answers, where
/// it used to go to the next agent by itself. Your pick reads the turn from its start, and
/// Stop leaves it where it failed.
#[tokio::test]
async fn an_answer_turn_asks_who_answers_when_the_main_sessions_mode_is_ask() {
    for pick in ["codex", "stop"] {
        let mut options = with_codex_fallback();
        options.flags.push(r#"fallback.main.mode="ask""#.to_owned());
        let mut driver = TestDriver::start_with(options).await;
        let codex = driver
            .directory
            .add(FakeRunner::cli("codex", "openai").then(answer("From codex.")));
        driver
            .agent
            .push(FakeSession::ended(EndReason::RateLimited));
        driver.submit("hi").await;
        let question = driver
            .until(|event| {
                matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::Fallback)
            })
            .await;
        let AppEvent::Choice { choice } = question else {
            unreachable!()
        };
        assert!(
            choice.title.ends_with("Who answers instead?"),
            "{}",
            choice.title
        );
        let keys: Vec<char> = choice.options.iter().map(|option| option.key).collect();
        assert_eq!(keys, ['1', 's'], "{pick}");
        assert!(codex.specs().is_empty(), "nobody read it before you chose");

        driver
            .send(UiCommand::Answer {
                to: AnswerTarget::Choice { id: choice.id },
                action: AnswerAction::Choose {
                    value: pick.to_owned(),
                },
                text: None,
                evidence_sha256: None,
                tasks: Vec::new(),
                remember: false,
            })
            .await;
        if pick == "stop" {
            let closed = driver
                .until(|event| matches!(event, AppEvent::ChoiceClosed { .. }))
                .await;
            assert!(
                matches!(&closed, AppEvent::ChoiceClosed { outcome, .. } if outcome == "Stopped"),
                "{closed:?}"
            );
            assert!(codex.specs().is_empty());
        } else {
            let done = turn_done(&mut driver).await;
            assert!(matches!(
                conversation_event(&done),
                Some(ConversationEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. })
                    if markdown == "From codex."
            ));
            assert_eq!(codex.specs().len(), 1);
        }
        driver.exit().await;
    }
}

fn asked_who_answers(event: &AppEvent) -> bool {
    matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::Fallback)
}

/// On autopilot nobody is asked, with the main session's mode set to ask too: the first agent
/// that may take a turn that only answers reads it from its start.
#[tokio::test]
async fn on_autopilot_an_answer_turn_goes_to_the_next_agent_unasked() {
    let mut options = with_codex_fallback();
    options.flags.push(r#"fallback.main.mode="ask""#.to_owned());
    options.flags.push("approvals.autopilot=true".to_owned());
    let mut driver = TestDriver::start_with(options).await;
    let codex = driver
        .directory
        .add(FakeRunner::cli("codex", "openai").then(answer("From codex.")));
    driver
        .agent
        .push(FakeSession::ended(EndReason::RateLimited));
    driver.submit("hi").await;
    let failed = driver.until(finished).await;
    assert!(matches!(
        conversation_event(&failed),
        Some(ConversationEvent::TurnFailed { error, .. }) if error.contains("rate limited")
    ));
    let done = turn_done(&mut driver).await;
    assert!(matches!(
        conversation_event(&done),
        Some(ConversationEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. })
            if markdown == "From codex."
    ));
    assert_eq!(codex.specs().len(), 1);
    assert!(
        !driver.seen().iter().any(asked_who_answers),
        "autopilot asks nobody who answers"
    );
    driver.exit().await;
}

/// "Who answers instead?" open when autopilot comes on goes to the first agent that may take
/// the turn, which reads it from its start.
#[tokio::test]
async fn autopilot_coming_on_has_the_next_agent_answer_a_turn_that_waited_for_your_pick() {
    let mut options = with_codex_fallback();
    options.flags.push(r#"fallback.main.mode="ask""#.to_owned());
    let mut driver = TestDriver::start_with(options).await;
    let codex = driver
        .directory
        .add(FakeRunner::cli("codex", "openai").then(answer("From codex.")));
    driver
        .agent
        .push(FakeSession::ended(EndReason::RateLimited));
    driver.submit("hi").await;
    let AppEvent::Choice { choice } = driver.until(asked_who_answers).await else {
        unreachable!()
    };
    assert!(codex.specs().is_empty(), "nobody read it before autopilot");

    driver
        .send(UiCommand::ApprovalMode {
            mode: ApprovalMode::Autopilot,
        })
        .await;
    let closed = driver
        .until(|event| matches!(event, AppEvent::ChoiceClosed { id, .. } if id == &choice.id))
        .await;
    let AppEvent::ChoiceClosed { outcome, .. } = closed else {
        unreachable!()
    };
    assert!(
        outcome.starts_with("Decided on autopilot · ") && outcome.ends_with(" answers"),
        "{outcome}"
    );
    let done = turn_done(&mut driver).await;
    assert!(matches!(
        conversation_event(&done),
        Some(ConversationEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. })
            if markdown == "From codex."
    ));
    assert_eq!(codex.specs().len(), 1);
    driver.exit().await;
}

/// With the internet gone, a turn the network ended stays the main agent's: it fails with its
/// CLI's words and goes to no fallback, which would meet the same network.
#[tokio::test]
async fn a_turn_the_network_ended_while_it_is_down_goes_to_no_fallback() {
    let mut driver = TestDriver::start_with(with_codex_fallback()).await;
    let codex = driver
        .directory
        .add(FakeRunner::cli("codex", "openai").then(answer("From codex.")));
    driver
        .health
        .set_network(argonauts_core::models::limits::NetState::Down {
            since: OffsetDateTime::now_utc(),
        });
    driver.agent.push(FakeSession::ended(EndReason::CliError {
        category: argonauts_core::runs::event::ErrorCategory::Network,
        message: "Unable to connect to API (ConnectionRefused)".into(),
    }));
    driver.submit("hi").await;
    let failed = driver.until(finished).await;
    assert!(matches!(
        conversation_event(&failed),
        Some(ConversationEvent::TurnFailed { error, .. }) if error.contains("Unable to connect")
    ));
    let events = driver.exit().await;
    assert!(!events.iter().any(|event| matches!(
        conversation_event(event),
        Some(ConversationEvent::TurnStarted { worker, .. }) if worker.agent == "codex"
    )));
    assert!(codex.specs().is_empty());
}
