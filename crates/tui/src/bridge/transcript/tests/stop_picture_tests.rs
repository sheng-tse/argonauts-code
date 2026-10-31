//! A run that can't go on, drawn from its pictures at their width of 118 columns: the line a
//! model its CLI no longer lists says, the item of a run that stopped fully, and the item of a
//! run you paused.

use argonauts_core::RunId;
use argonauts_core::agents::session_runner::{Access, RunnerKind};
use argonauts_core::app::AppEvent;
use argonauts_core::base::names::{AgentId, RoleName, SessionKey};
use argonauts_core::runs::events::run::{RunLifecycle, RunStop, WaitReason};
use argonauts_core::runs::events::session::{EndReason, SessionEnd, SessionEvent, SessionStarted};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::{Note, NoteLevel, Spend};
use time::macros::datetime;

use super::live_rows_tests::Screen;
use crate::testing::pictures::picture_check::assert_matches;

const WIDTH: u16 = 118;

fn run_event(kind: RunEventKind) -> AppEvent {
    AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 0,
            ts: datetime!(2026-10-15 00:35 UTC),
            run_id: RunId::new("run-20261014-055235-ccb4").expect("a run id"),
            task_id: None,
            session: None,
            source: "argonauts-code".into(),
            kind,
        }),
    }
}

#[test]
fn a_model_its_cli_no_longer_lists_is_said_in_one_line() {
    let mut screen = Screen::on_macos();
    screen.app(run_event(RunEventKind::Note(Note {
        level: NoteLevel::Warn,
        text: "propose:p3 runs on Gemini 3.8 Flash (Low): agy no longer offers \
               Gemini 3.6 Flash (Low)"
            .into(),
    })));
    let lines = screen.lines(WIDTH);
    assert_matches(
        "waiting_nearest",
        &lines,
        include_str!("../../../../tests/golden/transcript/waiting_nearest.txt"),
        include_str!("../../../../tests/golden/transcript/waiting_nearest.html"),
        &Screen::theme(),
        &[],
    );
}

#[test]
fn a_run_that_cant_go_on_says_why_what_was_tried_and_how_to_carry_on() {
    let stop = RunStop {
        why: "a council needs two proposals, and only propose:p1 can make one".into(),
        agents: vec![
            "propose:p2 · Codex crashed twice as it started: \"Failed to synchronize managed \
             preferences\""
                .into(),
            "propose:p3 · agy is signed out".into(),
        ],
        tried: vec![
            "OpenCode has no model set".into(),
            "you said no to a second proposal from Claude Code".into(),
        ],
        kept: vec!["propose:p1".into()],
        waits_for: vec!["Codex".into(), "agy".into()],
    };
    let mut screen = Screen::on_macos();
    screen.app(run_event(RunEventKind::Run(RunLifecycle::Waiting {
        reason: WaitReason::NoFallback {
            detail: "nobody to fall back to as the task's worker".into(),
            until: None,
            stop: Some(Box::new(stop)),
        },
        waiting_on: Vec::new(),
    })));
    let lines = screen.lines(WIDTH);
    assert_matches(
        "waiting_stopped",
        &lines,
        include_str!("../../../../tests/golden/transcript/waiting_stopped.txt"),
        include_str!("../../../../tests/golden/transcript/waiting_stopped.html"),
        &Screen::theme(),
        &[],
    );
}

/// A session of `label` that the pause stopped, as its start and its end.
fn paused_session(n: u32, label: &str) -> [AppEvent; 2] {
    let session = SessionKey::numbered(n);
    let on = |kind: RunEventKind| {
        let AppEvent::Run { mut event } = run_event(kind) else {
            unreachable!("a run event")
        };
        event.session = Some(session.clone());
        AppEvent::Run { event }
    };
    let started = SessionStarted {
        agent_id: AgentId("claude-code".to_owned()),
        worker: "claude-code/haiku".parse().expect("a model"),
        provider: "anthropic".to_owned(),
        kind: RunnerKind::Cli,
        role: RoleName::Worker,
        step: None,
        access: Access::ReadOnly,
        lifetime: SessionLifetime::Task,
        resumed_from: None,
        follow_key: None,
        workflow_step: None,
        label: Some(label.to_owned()),
        asked: None,
        asked_by: None,
        asked_why: None,
        forked: false,
        seat: None,
        billing: None,
    };
    let end = SessionEnd {
        cli_session: None,
        exit_code: None,
        signal: None,
        duration_ms: 20_000,
        reason: EndReason::Paused,
        spend: Spend::default(),
        tool_calls: 0,
        result_text: None,
        structured: None,
        last_message: None,
        raw_log: None,
    };
    [
        on(RunEventKind::Session(SessionEvent::Started(Box::new(
            started,
        )))),
        on(RunEventKind::Session(SessionEvent::Ended(Box::new(end)))),
    ]
}

#[test]
fn only_a_pause_you_asked_for_says_paused() {
    let mut screen = Screen::on_macos();
    for (n, label) in [(1, "propose:p1"), (2, "propose:p2"), (3, "propose:p3")] {
        for event in paused_session(n, label) {
            screen.seen(event);
        }
    }
    let before = screen.lines(WIDTH).len();
    screen.seen(run_event(RunEventKind::Run(RunLifecycle::Paused {
        sessions_stopped: 3,
        committed: Vec::new(),
    })));
    let lines = screen.lines(WIDTH);
    // The pause's own item, after what the sessions printed and any blank row between.
    let first = (before..lines.len())
        .find(|&row| {
            lines[row]
                .spans
                .iter()
                .any(|span| !span.content.trim().is_empty())
        })
        .unwrap_or(lines.len());
    let lines = &lines[first..];
    assert_matches(
        "waiting_paused",
        lines,
        include_str!("../../../../tests/golden/transcript/waiting_paused.txt"),
        include_str!("../../../../tests/golden/transcript/waiting_paused.html"),
        &Screen::theme(),
        &[],
    );
}
