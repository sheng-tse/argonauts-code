//! The chat through the real controller loop: WORKFLOW's examples 1 and 4, `/ask`, `/solo`,
//! queued turns, Esc, resume, and fallbacks.

use std::time::Duration;

use argonauts_core::app::{
    AnswerTarget, AppEvent, AppMode, ChatEvent, ChoiceKind, TurnReply, UiCommand,
};
use argonauts_core::event::AgentEvent;
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::events::run::RunMode;
use argonauts_core::events::session::EndReason;
use argonauts_core::input::SlashCommand;
use argonauts_core::limits::{QuotaBoard as _, QuotaReading, QuotaSource, QuotaStatus};
use argonauts_core::names::{ChoiceId, VendorId};
use argonauts_core::steps::StepName;
use argonauts_core::worker::{Access, SessionControl, WorkerEvent};
use argonauts_testkit::{FakeSession, FakeWorker};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::app_settings::AppSettings;
use crate::chat_store::ChatStore;
use crate::choices::JUST_ASKING;
use crate::controller::chat::{self, ChatState};
use crate::controller::test_support::{Harness, HarnessOptions, TestCtx, WAIT};
use crate::controller::{Effect, Internal};

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

fn chat_event(event: &AppEvent) -> Option<&ChatEvent> {
    match event {
        AppEvent::Chat { event } => Some(event),
        _ => None,
    }
}

fn finished(event: &AppEvent) -> bool {
    matches!(
        chat_event(event),
        Some(ChatEvent::TurnFinished { .. } | ChatEvent::TurnFailed { .. })
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

async fn turn_done(harness: &mut Harness) -> AppEvent {
    let event = harness.until(finished).await;
    harness.until(idle).await;
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
        cards: Vec::new(),
        remember: false,
    }
}

/// Polls state the events don't show, such as what a fake session received.
async fn eventually(what: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !what() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "not within {WAIT:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn prompt_text(harness: &Harness, index: usize) -> String {
    harness.agent.specs()[index]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect()
}

#[tokio::test]
async fn hi_answers_and_nothing_starts() {
    let mut harness = Harness::start().await;
    harness
        .agent
        .push(answer("Hi. Ask me anything about this repo."));
    harness.submit("hi").await;
    let mode = harness
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
    let done = turn_done(&mut harness).await;
    let Some(ChatEvent::TurnFinished { reply, .. }) = chat_event(&done) else {
        panic!("{done:?}");
    };
    assert_eq!(
        reply,
        &TurnReply::Answer {
            markdown: "Hi. Ask me anything about this repo.".to_owned()
        }
    );
    let spec = &harness.agent.specs()[0];
    assert_eq!(spec.access, Access::ReadOnly);
    assert_eq!(spec.workdir, harness.repo.root());
    assert_eq!(spec.step, Some(StepName::TriageEstimate));
    assert_eq!(spec.resume, None);
    assert!(spec.json_schema.is_some());
    assert!(prompt_text(&harness, 0).ends_with("hi\n"));
    let events = harness.exit().await;
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
async fn an_unclear_request_offers_start_work_and_n_starts_nothing() {
    let mut harness = Harness::start().await;
    harness.agent.push(reply(
        "unclear",
        "Put a middleware right after auth. Expect about 9 files, including tests.",
    ));
    harness.submit("how should we add rate limiting?").await;
    let offered = harness
        .until(|event| matches!(event, AppEvent::Choice { .. }))
        .await;
    let AppEvent::Choice { choice } = offered else {
        unreachable!()
    };
    assert_eq!(choice.kind, ChoiceKind::StartWork);
    assert!(
        choice
            .title
            .starts_with("This could be a change request. Start work on it? Triage would pick "),
        "{}",
        choice.title
    );
    let keys: String = choice.options.iter().map(|option| option.key).collect();
    assert_eq!(keys, "scn");
    let unclear = harness
        .seen()
        .iter()
        .find_map(|event| match chat_event(event) {
            Some(ChatEvent::TurnFinished {
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

    // Esc sets it aside; it stays answerable.
    harness
        .send(UiCommand::SetAside {
            to: AnswerTarget::Choice {
                id: choice.id.clone(),
            },
        })
        .await;
    harness.send(answer_choice(choice.id.clone(), "no")).await;
    let closed = harness
        .until(|event| matches!(event, AppEvent::ChoiceClosed { .. }))
        .await;
    assert_eq!(
        closed,
        AppEvent::ChoiceClosed {
            id: choice.id.clone(),
            outcome: JUST_ASKING.to_owned()
        }
    );
    harness.send(answer_choice(choice.id, "solo")).await;
    harness
        .until(|event| {
            matches!(event, AppEvent::Notice { text, .. } if text.contains("already closed"))
        })
        .await;
    assert!(harness.conductor.starts().is_empty());
    harness.exit().await;
}

#[tokio::test]
async fn solo_skips_the_estimate() {
    let mut harness = Harness::start().await;
    harness
        .send(UiCommand::Slash {
            command: SlashCommand::Solo {
                task: "fix the typo in the README".to_owned(),
            },
        })
        .await;
    harness
        .until(|event| {
            matches!(event, AppEvent::Status { status } if status.title == "fix the typo in the README")
        })
        .await;
    let agent = harness.agent.clone();
    let conductor = harness.conductor.clone();
    let events = harness.exit().await;
    assert!(agent.specs().is_empty());
    assert!(
        !events.iter().any(|event| chat_event(event).is_some()),
        "{events:#?}"
    );
    // Starting belongs to the runs part; whatever it sent is the forced request.
    for start in conductor.starts() {
        assert!(start.forced);
        assert_eq!(start.mode, RunMode::Solo);
        assert_eq!(start.estimate, None);
    }
}

/// `/solo` while triage still reads the same request: the triage reply starts nothing more.
async fn solo_during_triage(kind: &str) -> Vec<AppEvent> {
    let mut harness = Harness::start().await;
    harness
        .agent
        .push(reply(kind, "Add a middleware after auth.").with_delay(Duration::from_millis(300)));
    harness.submit("add rate limiting").await;
    harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Mode {
                    mode: AppMode::Answering { .. }
                }
            )
        })
        .await;
    harness
        .send(UiCommand::Slash {
            command: SlashCommand::Solo {
                task: "add rate limiting".to_owned(),
            },
        })
        .await;
    harness.until(finished).await;
    let conductor = harness.conductor.clone();
    let events = harness.exit().await;
    let starts = conductor.starts();
    assert_eq!(starts.len(), 1, "{starts:#?}");
    assert!(starts[0].forced);
    assert_eq!(starts[0].mode, RunMode::Solo);
    events
}

#[tokio::test]
async fn solo_while_triage_reads_a_change_starts_one_run() {
    solo_during_triage("change").await;
}

#[tokio::test]
async fn solo_while_triage_reads_an_unclear_request_offers_nothing() {
    let events = solo_during_triage("unclear").await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AppEvent::Choice { .. })),
        "{events:#?}"
    );
}

#[tokio::test]
async fn forced_runs_and_triaged_changes_carry_what_start_needs() {
    let (mut parts, _) = TestCtx::new(HarnessOptions::default());
    let mut chat = ChatState::new(&parts.ctx).unwrap();
    let effects = chat::start_forced(
        &mut parts.ctx,
        &mut chat,
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
    assert_eq!(request.chat_turn, None);
    assert!(parts.agent.specs().is_empty());

    parts
        .agent
        .push(reply("change", "Add a middleware after auth."));
    chat::new_request(&mut parts.ctx, &mut chat, "add rate limiting".into())
        .await
        .unwrap();
    let mut effects = Vec::new();
    while effects.is_empty() {
        let message = tokio::time::timeout(WAIT, parts.internal.recv())
            .await
            .expect("the turn ends")
            .expect("the channel stays open");
        let Internal::Chat(update) = message else {
            continue;
        };
        effects = chat::on_update(&mut parts.ctx, &mut chat, update)
            .await
            .unwrap();
    }
    let [Effect::StartRun(request)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert!(!request.forced);
    let triage = request.triage.as_ref().expect("the triage decision");
    assert_eq!(request.mode, triage.mode);
    assert_eq!(request.estimate.as_ref().map(|e| e.file_count), Some(9));
    assert!(request.chat_turn.is_some());
    assert_eq!(parts.ctx.mode, AppMode::Idle);
}

#[tokio::test]
async fn text_while_answering_queues_the_next_turn() {
    let mut harness = Harness::start().await;
    harness
        .agent
        .push(answer("first answer").with_delay(Duration::from_millis(300)));
    harness.agent.push(answer("second answer"));
    harness.submit("first question").await;
    harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Mode {
                    mode: AppMode::Answering { .. }
                }
            )
        })
        .await;
    harness.submit("second question").await;
    // The second starts only once the first finished.
    let first = harness.until(finished).await;
    let second_started = harness
        .until(|event| matches!(chat_event(event), Some(ChatEvent::TurnStarted { .. })))
        .await;
    let Some(ChatEvent::TurnStarted { text, .. }) = chat_event(&second_started) else {
        unreachable!()
    };
    assert_eq!(text, "second question");
    let second = turn_done(&mut harness).await;
    let turn_of = |event: &AppEvent| match chat_event(event) {
        Some(ChatEvent::TurnFinished { turn, .. }) => Some(turn.clone()),
        _ => None,
    };
    assert_ne!(turn_of(&first), turn_of(&second));
    assert!(prompt_text(&harness, 1).ends_with("second question\n"));
    harness.exit().await;
}

#[tokio::test]
async fn turn_started_carries_the_commit_uncommitted_paths_and_settings_hash() {
    let mut harness = Harness::start_with(HarnessOptions {
        prepare: Some(Box::new(|repo| {
            repo.write("notes.txt", "draft\n").unwrap();
        })),
        ..HarnessOptions::default()
    })
    .await;
    harness.agent.push(answer("Two files."));
    harness.submit("what changed?").await;
    let started = harness
        .until(|event| matches!(chat_event(event), Some(ChatEvent::TurnStarted { .. })))
        .await;
    let Some(ChatEvent::TurnStarted {
        commit,
        uncommitted,
        settings_sha256,
        read_only,
        worker,
        ..
    }) = chat_event(&started).cloned()
    else {
        unreachable!()
    };
    assert_eq!(commit, harness.repo.head().unwrap());
    assert!(
        uncommitted.contains(&"notes.txt".to_owned()),
        "{uncommitted:?}"
    );
    assert!(read_only);
    assert_eq!(worker.worker, "claude-code");
    let settings = AppSettings::load(&harness.env, Vec::new(), true).unwrap();
    assert_eq!(settings_sha256.as_deref(), Some(settings.sha256()));
    assert!(prompt_text(&harness, 0).contains("uncommitted file"));
    turn_done(&mut harness).await;

    let chat_dir = harness.env.chat_dir();
    let journal = ChatStore::open(&chat_dir, &argonauts_core::names::ChatId::numbered(1));
    let recorded = journal.events().unwrap();
    assert!(
        matches!(recorded.first(), Some(ChatEvent::TurnStarted { commit: c, .. }) if *c == commit)
    );
    assert!(matches!(
        recorded.last(),
        Some(ChatEvent::TurnFinished { .. })
    ));
    harness.exit().await;

    let mut including = Harness::start_with(HarnessOptions {
        flags: vec!["intake.include_uncommitted=true".to_owned()],
        prepare: Some(Box::new(|repo| {
            repo.write("notes.txt", "draft\n").unwrap();
        })),
        ..HarnessOptions::default()
    })
    .await;
    including.agent.push(answer("ok"));
    including.submit("what changed?").await;
    let started = including
        .until(|event| matches!(chat_event(event), Some(ChatEvent::TurnStarted { .. })))
        .await;
    assert!(matches!(
        chat_event(&started),
        Some(ChatEvent::TurnStarted { uncommitted, .. }) if uncommitted.is_empty()
    ));
    including.exit().await;
}

#[tokio::test]
async fn turn_two_resumes_turn_ones_vendor_session() {
    let mut harness = Harness::start().await;
    harness
        .agent
        .push(answer("one").with_vendor_session("vendor-7"));
    harness
        .agent
        .push(answer("two").with_vendor_session("vendor-7"));
    harness.submit("first").await;
    turn_done(&mut harness).await;
    harness.submit("second").await;
    turn_done(&mut harness).await;
    let specs = harness.agent.specs();
    assert_eq!(specs[0].resume, None);
    assert_eq!(specs[1].resume.as_deref(), Some("vendor-7"));
    assert_eq!(specs[0].worker, specs[1].worker);
    assert_ne!(specs[0].key, specs[1].key);
    let store = ChatStore::open(
        &harness.env.chat_dir(),
        &argonauts_core::names::ChatId::numbered(1),
    );
    assert_eq!(
        store.session().map(|session| session.vendor_session),
        Some("vendor-7".to_owned())
    );
    harness.exit().await;
}

#[tokio::test]
async fn a_fresh_session_is_primed_with_the_chat_so_far() {
    let (mut parts, _) = TestCtx::new(HarnessOptions::default());
    let mut chat = ChatState::new(&parts.ctx).unwrap();
    parts
        .agent
        .push(answer("Use a middleware.").with_vendor_session("v-1"));
    parts
        .agent
        .push(answer("Nine files.").with_vendor_session("v-2"));
    run_turn(&mut parts, &mut chat, "how do we add rate limiting?").await;
    chat::fresh_session(&mut parts.ctx, &mut chat).unwrap();
    run_turn(&mut parts, &mut chat, "how many files?").await;
    let specs = parts.agent.specs();
    assert_eq!(specs[1].resume, None);
    let primer = &specs[1].prompt[0];
    assert_eq!(primer.label, "chat so far");
    assert!(
        primer.text.contains("how do we add rate limiting?"),
        "{}",
        primer.text
    );
    assert!(primer.text.contains("Use a middleware."), "{}", primer.text);
}

async fn run_turn(parts: &mut TestCtx, chat: &mut ChatState, text: &str) {
    chat::new_request(&mut parts.ctx, chat, text.to_owned())
        .await
        .unwrap();
    loop {
        let message = tokio::time::timeout(WAIT, parts.internal.recv())
            .await
            .expect("the turn ends")
            .expect("the channel stays open");
        let Internal::Chat(update) = message else {
            continue;
        };
        let done = matches!(update, chat::TurnUpdate::Done { .. });
        chat::on_update(&mut parts.ctx, chat, update).await.unwrap();
        if done {
            return;
        }
    }
}

#[tokio::test]
async fn cancel_turn_stops_the_step_session() {
    let mut harness = Harness::start().await;
    harness
        .agent
        .push(FakeSession::until_stopped(vec![WorkerEvent::Agent(
            AgentEvent::TextDelta {
                text: "Looking at the build".to_owned(),
                parent: None,
            },
        )]));
    harness.agent.push(answer("Done."));
    harness.submit("explain the build").await;
    harness
        .until(|event| {
            matches!(
                chat_event(event),
                Some(ChatEvent::Agent {
                    event: AgentEvent::TextDelta { .. },
                    ..
                })
            )
        })
        .await;
    harness.submit("and the tests?").await;
    harness.send(UiCommand::CancelTurn).await;
    harness
        .until(|event| matches!(chat_event(event), Some(ChatEvent::TurnCancelled { .. })))
        .await;
    harness.until(idle).await;
    let taken = harness
        .until(|event| matches!(event, AppEvent::TakenBack { .. }))
        .await;
    assert_eq!(
        taken,
        AppEvent::TakenBack {
            texts: vec!["and the tests?".to_owned()]
        }
    );
    let agent = harness.agent.clone();
    eventually(|| {
        agent
            .controls()
            .iter()
            .any(|(_, control)| *control == SessionControl::Stop)
    })
    .await;

    // The chat goes on as usual.
    harness.submit("hi").await;
    let done = turn_done(&mut harness).await;
    assert!(matches!(
        chat_event(&done),
        Some(ChatEvent::TurnFinished {
            reply: TurnReply::Answer { .. },
            ..
        })
    ));
    harness.exit().await;
}

#[tokio::test]
async fn a_turn_after_esc_never_resumes_a_session_still_stopping() {
    let mut harness = Harness::start().await;
    harness
        .agent
        .push(answer("one").with_vendor_session("vendor-7"));
    // Stop is seen only after the pause, so the step outlives the cancel for a while.
    harness.agent.push(
        answer("two")
            .with_vendor_session("vendor-7")
            .with_delay(Duration::from_millis(400)),
    );
    harness.agent.push(answer("three"));
    harness.submit("first").await;
    turn_done(&mut harness).await;
    harness.submit("second").await;
    harness
        .until(|event| matches!(chat_event(event), Some(ChatEvent::TurnStarted { .. })))
        .await;
    harness.send(UiCommand::CancelTurn).await;
    harness.until(idle).await;
    harness.submit("third").await;
    turn_done(&mut harness).await;
    let specs = harness.agent.specs();
    assert_eq!(specs[1].resume.as_deref(), Some("vendor-7"));
    assert_eq!(specs[2].resume, None);
    // A fresh session is primed with the chat so far instead.
    assert!(prompt_text(&harness, 2).contains("one"));
    harness.exit().await;
}

#[tokio::test]
async fn ask_answers_and_never_starts_a_run() {
    let mut harness = Harness::start().await;
    harness.agent.push(FakeSession::structured(
        json!({"answer_markdown": "The tests live in tests/."}),
    ));
    harness
        .send(UiCommand::Slash {
            command: SlashCommand::Ask {
                question: "where are the tests?".to_owned(),
            },
        })
        .await;
    let done = turn_done(&mut harness).await;
    assert!(matches!(
        chat_event(&done),
        Some(ChatEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. })
            if markdown == "The tests live in tests/."
    ));
    assert_eq!(harness.agent.specs()[0].step, Some(StepName::ChatAnswer));
    assert!(harness.conductor.starts().is_empty());
    harness.exit().await;
}

fn out_of_quota(vendor: &str) -> QuotaReading {
    QuotaReading {
        vendor: VendorId(vendor.to_owned()),
        status: QuotaStatus::Rejected,
        used: Some(1.0),
        resets_at: None,
        window: None,
        source: QuotaSource::RateLimitEvent,
        observed_at: OffsetDateTime::now_utc(),
        detail: None,
    }
}

fn with_codex_fallback() -> HarnessOptions {
    HarnessOptions {
        flags: vec![r#"roles.default.handoff.fallback=["codex"]"#.to_owned()],
        ..HarnessOptions::default()
    }
}

#[tokio::test]
async fn an_out_default_agent_hands_the_request_to_its_fallback() {
    let mut harness = Harness::start_with(with_codex_fallback()).await;
    let codex = harness
        .directory
        .add(FakeWorker::harness("codex", "openai").then(answer("From codex.")));
    harness.quota.observe(out_of_quota("claude-code"));
    harness.submit("hi").await;
    let started = harness
        .until(|event| matches!(chat_event(event), Some(ChatEvent::TurnStarted { .. })))
        .await;
    assert!(matches!(
        chat_event(&started),
        Some(ChatEvent::TurnStarted { worker, .. }) if worker.worker == "codex"
    ));
    turn_done(&mut harness).await;
    assert!(harness.agent.specs().is_empty());
    assert_eq!(codex.specs().len(), 1);
    harness.exit().await;
}

#[tokio::test]
async fn a_vendor_failure_mid_turn_retries_with_the_fallback() {
    let mut harness = Harness::start_with(with_codex_fallback()).await;
    let codex = harness
        .directory
        .add(FakeWorker::harness("codex", "openai").then(answer("From codex.")));
    harness
        .agent
        .push(FakeSession::ended(EndReason::RateLimited));
    harness.submit("hi").await;
    let failed = harness.until(finished).await;
    assert!(matches!(
        chat_event(&failed),
        Some(ChatEvent::TurnFailed { error, .. }) if error.contains("rate limited")
    ));
    let done = turn_done(&mut harness).await;
    assert!(matches!(
        chat_event(&done),
        Some(ChatEvent::TurnFinished { reply: TurnReply::Answer { markdown }, .. })
            if markdown == "From codex."
    ));
    assert_eq!(codex.specs().len(), 1);
    harness.exit().await;
}
