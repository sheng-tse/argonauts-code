//! A new main model mid-conversation: the same CLI carries its session on, and another CLI
//! starts from a summary, the recent exchanges in full, and the transcript's path.

use std::time::Duration;

use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, AppMode, ChatEvent, UiCommand};
use argonauts_core::events::session::EndReason;
use argonauts_core::settings::SettingScope;
use argonauts_core::worker::SessionControl;
use argonauts_testkit::{FakeSession, FakeWorker};
use serde_json::json;

use crate::controller::test_support::{Harness, HarnessOptions, WAIT};

const SESSION: &str = "claude-session-1";

fn answer(text: &str) -> FakeSession {
    FakeSession::structured(json!({"kind": "answer", "answer_markdown": text, "estimate": null}))
}

async fn turn_done(harness: &mut Harness) {
    harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Chat {
                    event: ChatEvent::TurnFinished { .. } | ChatEvent::TurnFailed { .. }
                }
            )
        })
        .await;
    harness.until(idle).await;
}

fn idle(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Mode {
            mode: AppMode::Idle
        }
    )
}

async fn notice(harness: &mut Harness, part: &str) -> String {
    let event = harness
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains(part)))
        .await;
    match event {
        AppEvent::Notice { text, .. } => text,
        _ => unreachable!(),
    }
}

/// The `Carried over: …` line hung under `Set model to …`.
async fn carried(harness: &mut Harness, part: &str) -> String {
    let event = harness
        .until(|event| matches!(event, AppEvent::Continued { text } if text.contains(part)))
        .await;
    match event {
        AppEvent::Continued { text } => text,
        _ => unreachable!(),
    }
}

async fn set_main(harness: &Harness, worker: &str) {
    harness
        .send(UiCommand::SetSetting {
            key: "main_agent".to_owned(),
            value: format!("\"{worker}\""),
            scope: SettingScope::Session,
        })
        .await;
}

/// One exchange on Claude Code, which leaves its session to resume.
async fn first_exchange(harness: &mut Harness) {
    harness
        .agent
        .push(answer("Use a middleware after auth.").with_vendor_session(SESSION));
    harness.submit("how do we add rate limiting?").await;
    turn_done(harness).await;
}

/// For the summary turn, which only answers.
fn said(text: &str) -> FakeSession {
    FakeSession::structured(json!({"answer_markdown": text}))
}

fn summary_shown(event: &AppEvent) -> bool {
    matches!(event, AppEvent::Folded { .. })
}

#[tokio::test]
async fn another_cli_starts_from_the_summary_the_model_left_wrote() {
    let mut harness = Harness::start().await;
    let codex = harness.directory.add(FakeWorker::harness("codex", "codex"));
    first_exchange(&mut harness).await;

    harness.agent.push(said(
        "**Asked:** rate limiting.\nDone: `nothing` yet.\nOpen: where the middleware goes.",
    ));
    set_main(&harness, "codex").await;
    let said = carried(&mut harness, "Carried over:").await;
    assert_eq!(
        said,
        "Carried over: a summary of 1 exchange, the 1 exchange in full, and the transcript to \
         read"
    );
    let shown = harness.until(summary_shown).await;
    let AppEvent::Folded { title, lines, more } = shown else {
        unreachable!()
    };
    // Claude Code ran its own default, which is named as the status line names it.
    assert_eq!(title, "Summary for Codex · written by Opus 5.5 · 11 words");
    assert_eq!(lines, ["Asked: rate limiting.", "Done: nothing yet."]);
    assert_eq!(more, ["Open: where the middleware goes."]);

    // The model being left wrote it in its own session, with /compact's prompt.
    let summary_spec = &harness.agent.specs()[1];
    assert_eq!(summary_spec.resume.as_deref(), Some(SESSION));
    assert!(
        summary_spec
            .prompt
            .iter()
            .any(|part| part.label == "summary")
    );

    codex.push(answer("Nine files."));
    harness.submit("how many files?").await;
    turn_done(&mut harness).await;
    let spec = &codex.specs()[0];
    assert_eq!(spec.resume, None);
    let primer = &spec.prompt[0];
    assert_eq!(primer.label, "chat so far");
    assert!(
        primer.text.contains("Open: where the middleware goes.")
            && primer.text.contains("how do we add rate limiting?")
            && primer.text.contains("Use a middleware after auth.")
            && primer.text.contains(".jsonl, one JSON event per line"),
        "{}",
        primer.text
    );
    // Nothing of the summary turn went to the screen or the journal as a turn of its own.
    let echoed = harness.seen().iter().any(|event| {
        matches!(event, AppEvent::Chat { event: ChatEvent::TurnStarted { turn, .. } }
            if turn.as_str().starts_with("t-carry-"))
    });
    assert!(!echoed);
    harness.exit().await;
}

#[tokio::test]
async fn when_the_model_left_cant_answer_the_new_one_writes_it_from_the_record() {
    let mut harness = Harness::start().await;
    let codex = harness.directory.add(FakeWorker::harness("codex", "codex"));
    first_exchange(&mut harness).await;

    harness
        .agent
        .push(FakeSession::ended(EndReason::UsageLimit {
            resets_at: None,
        }));
    codex.push(said("Asked: rate limiting."));
    set_main(&harness, "codex").await;
    let said = notice(&mut harness, "couldn't write the summary").await;
    assert!(
        said.ends_with("so Codex writes it from the record."),
        "{said}"
    );
    let AppEvent::Folded { title, .. } = harness.until(summary_shown).await else {
        unreachable!()
    };
    assert!(title.contains("written by Codex"), "{title}");
    let record = &codex.specs()[0];
    assert_eq!(record.resume, None);
    let prompt: String = record
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect();
    assert!(
        prompt.contains("Asked:\nhow do we add rate limiting?")
            && prompt.contains("Use a middleware after auth."),
        "{prompt}"
    );
    harness.exit().await;
}

#[tokio::test]
async fn switching_back_before_sending_cancels_the_summary_and_keeps_the_session() {
    let mut harness = Harness::start().await;
    harness.directory.add(FakeWorker::harness("codex", "codex"));
    first_exchange(&mut harness).await;

    harness.agent.push(FakeSession::until_stopped(Vec::new()));
    set_main(&harness, "codex").await;
    carried(&mut harness, "Carried over: a summary").await;
    set_main(&harness, "claude-code").await;
    let said = carried(&mut harness, "Carried over:").await;
    assert_eq!(
        said,
        "Carried over: the whole conversation, in the same Claude Code session"
    );
    // The summary's session is stopped, and once it ends the next turn may resume the session.
    let agent = harness.agent.clone();
    let deadline = tokio::time::Instant::now() + WAIT;
    while !agent.controls().iter().any(|(key, control)| {
        key.as_str().ends_with("t-carry-001") && matches!(control, SessionControl::Stop)
    }) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the summary is never stopped"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    harness.agent.push(answer("Nine files."));
    harness.submit("how many files?").await;
    turn_done(&mut harness).await;
    let specs = harness.agent.specs();
    assert_eq!(specs[2].resume.as_deref(), Some(SESSION));
    assert!(!harness.seen().iter().any(summary_shown));
    let stopped = harness.seen().iter().any(|event| {
        matches!(event, AppEvent::Notice { level: NoteLevel::Warn, text } if text.contains("No summary"))
    });
    assert!(!stopped);
    harness.exit().await;
}

#[tokio::test]
async fn another_model_on_the_same_cli_carries_the_session_on() {
    let mut harness = Harness::start().await;
    first_exchange(&mut harness).await;

    set_main(&harness, "claude-code/claude-sonnet-5").await;
    let said = carried(&mut harness, "Carried over:").await;
    assert_eq!(
        said,
        "Carried over: the whole conversation, in the same Claude Code session"
    );
    harness.agent.push(answer("Nine files."));
    harness.submit("how many files?").await;
    turn_done(&mut harness).await;
    let spec = &harness.agent.specs()[1];
    assert_eq!(spec.resume.as_deref(), Some(SESSION));
    assert_eq!(spec.worker.model.as_deref(), Some("claude-sonnet-5"));
    harness.exit().await;
}

#[tokio::test]
async fn the_recent_ones_only_skip_the_summary_and_cost_nothing() {
    let mut harness = Harness::start_with(HarnessOptions {
        flags: vec![
            r#"switch.carry="recent""#.to_owned(),
            "switch.recent=1".to_owned(),
        ],
        ..HarnessOptions::default()
    })
    .await;
    let codex = harness.directory.add(FakeWorker::harness("codex", "codex"));
    first_exchange(&mut harness).await;
    harness
        .agent
        .push(answer("Ten lines.").with_vendor_session(SESSION));
    harness.submit("how long is it?").await;
    turn_done(&mut harness).await;

    set_main(&harness, "codex").await;
    let said = carried(&mut harness, "Carried over:").await;
    assert_eq!(
        said,
        "Carried over: the last 1 exchange in full and the transcript to read"
    );
    codex.push(answer("Nine files."));
    harness.submit("how many files?").await;
    turn_done(&mut harness).await;
    assert_eq!(harness.agent.specs().len(), 2);
    let primer = &codex.specs()[0].prompt[0].text;
    assert!(
        primer.contains("how long is it?")
            && !primer.contains("how do we add rate limiting?")
            && primer.contains("Left out before them: 1 exchange."),
        "{primer}"
    );
    harness.exit().await;
}
