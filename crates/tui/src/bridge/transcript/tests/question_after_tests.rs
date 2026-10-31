//! How a question answered in your place says so, how a run that ended without merging says
//! why, and how a CLI that turned out signed out says so, drawn from `question_after` at its
//! width of 118 columns. The picture's other rows are its captions and the question it
//! answered.

use std::ops::Range;

use argonauts_core::RunId;
use argonauts_core::app::{AppEvent, ConversationEvent, SignedOut};
use argonauts_core::base::names::{MessageId, ModelRef, RoleName, TurnId};
use argonauts_core::runs::events::run::{RunLifecycle, RunOutcome};
use argonauts_core::runs::events::shared::{AnsweredForYou, BlackboardEvent};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_testkit::pictures::picture;
use ratatui::text::Line;
use time::macros::datetime;

use super::live_rows_tests::Screen;
use crate::testing::pictures::picture_check::assert_matches;

const PICTURE: &str = include_str!("../../../../tests/golden/transcript/question_after.txt");
const COLOURS: &str = include_str!("../../../../tests/golden/transcript/question_after.html");
const WIDTH: u16 = 118;

/// The picture's `rows`, as text and as a page of their cells alone.
fn picture_rows(rows: Range<usize>) -> (String, String) {
    let text: Vec<&str> = PICTURE.lines().collect();
    let text = text[rows.clone()].join("\n");
    let html = picture::page(&picture::grid(COLOURS)[rows]);
    (text, html)
}

fn run_event(kind: RunEventKind) -> AppEvent {
    AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 0,
            ts: datetime!(2026-10-14 05:49:05 UTC),
            run_id: RunId::new("run-012").expect("a run id"),
            task_id: None,
            session: None,
            source: "argonauts-code".into(),
            kind,
        }),
    }
}

/// What prints, without the blank rows around it.
fn drawn(screen: &mut Screen) -> Vec<Line<'static>> {
    let mut lines = screen.lines(WIDTH);
    let blank = |line: &Line<'_>| line.spans.iter().all(|span| span.content.trim().is_empty());
    while lines.first().is_some_and(blank) {
        lines.remove(0);
    }
    while lines.last().is_some_and(blank) {
        lines.pop();
    }
    lines
}

/// A question answered in your place says who answered, in which part, what and why, and what
/// the task does next.
#[test]
fn a_question_answered_in_your_place_says_who_and_why() {
    let mut screen = Screen::on_macos();
    screen.app(run_event(RunEventKind::Blackboard(
        BlackboardEvent::AnsweredForYou(AnsweredForYou {
            question: MessageId::numbered(4),
            asker: "build:fix-the-money".into(),
            answer: "Block the merge".into(),
            by: Some(ModelRef::new("claude-code", Some("claude-opus-5-5".into()))),
            role: Some(RoleName::Main),
            why: Some("the sign-off already decides, so a \"not ready\" stops it".into()),
            finished: true,
        }),
    )));
    let (want, colours) = picture_rows(6..9);
    assert_matches(
        "question_after_answered",
        &drawn(&mut screen),
        &want,
        &colours,
        &Screen::theme(),
        &[],
    );
}

/// The run's end line gives the reason its end recorded, and under it what held the task, that
/// the branch is kept, and how to carry the run on.
#[test]
fn a_run_that_ended_without_merging_says_why() {
    let mut screen = Screen::on_macos();
    screen.app(run_event(RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Failed,
        detail: Some(
            "Nothing was merged: build:fix-the-money stopped before its checks passed\n\
             it was blocked on a question"
                .into(),
        ),
    })));
    let (want, colours) = picture_rows(11..13);
    assert_matches(
        "question_after_end",
        &drawn(&mut screen),
        &want,
        &colours,
        &Screen::theme(),
        &[],
    );
}

/// A CLI whose sign-in the server revoked says so in one line: why, the command that signs it
/// in again at full strength among dim words, and who answers meanwhile. Nothing fails.
#[test]
fn a_signed_out_cli_says_how_to_sign_in_and_who_answers_meanwhile() {
    let mut screen = Screen::on_macos();
    screen.ask("Should the judge's \"not ready\" block the merge?");
    let asked = screen.lines(WIDTH).len();
    screen.send(ConversationEvent::FellBack {
        turn: TurnId::numbered(1),
        from: ModelRef::new("claude-code", Some("haiku".into())),
        to: ModelRef::new("codex", Some("gpt-6-luna".into())),
        why: "is signed out (its sign-in was revoked)".into(),
        told: "your request and the transcript so far".into(),
        signed_out: Some(SignedOut {
            why: "its sign-in was revoked".into(),
            login: Some("claude /login".into()),
        }),
    });
    let mut lines = screen.lines(WIDTH).split_off(asked);
    let blank = |line: &Line<'_>| line.spans.iter().all(|span| span.content.trim().is_empty());
    lines.retain(|line| !blank(line));
    let (want, colours) = picture_rows(14..15);
    assert_matches(
        "question_after_signed_out",
        &lines,
        &want,
        &colours,
        &Screen::theme(),
        &[],
    );
}

/// Signed out with no cause given and no command known, the line still says who answers.
#[test]
fn a_signed_out_cli_with_nothing_more_to_say_keeps_to_its_one_line() {
    let mut screen = Screen::on_macos();
    screen.ask("hi");
    let asked = screen.lines(WIDTH).len();
    screen.send(ConversationEvent::FellBack {
        turn: TurnId::numbered(1),
        from: ModelRef::new("ollama", None),
        to: ModelRef::new("codex", None),
        why: "is signed out".into(),
        told: "your request".into(),
        signed_out: Some(SignedOut {
            why: String::new(),
            login: None,
        }),
    });
    let text = screen.text(WIDTH);
    let rows: Vec<&str> = text
        .lines()
        .skip(asked)
        .filter(|row| !row.trim().is_empty())
        .collect();
    assert_eq!(rows, ["⏺ Ollama is signed out · Codex answers meanwhile"]);
}
