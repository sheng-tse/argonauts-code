//! The question a CLI that is down asks, in its words and beside its picture: under the
//! workflow view of the run it pauses, where it draws while it holds the keys.

use std::time::{Duration, Instant};

use argonauts_core::TaskId;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::approval::{
    AgentDownPacket, AnswerAction, AnswerOption, ApprovalKind, ApprovalOpened, ApprovalPacket,
    FreeText, PausedTask,
};
use argonauts_core::names::{AgentId, ApprovalId, RoleName};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use time::macros::datetime;
use time::{OffsetDateTime, UtcOffset};

use super::count_down;
use crate::bottom::{BottomState, QuestionPrompt};
use crate::questions::approval_question_local;
use crate::render::{Clock, RenderCtx};
use crate::state::AppState;
use crate::views::workflow::WorkflowView;
use crate::views::workflow_examples as examples;
use crate::views::workflow_pictures::{buffer_of, dark, picture};
use crate::views::{View, ViewCtx, dock};

const WALL: OffsetDateTime = datetime!(2026-10-10 09:15 UTC);

fn option(key: char, label: &str, action: AnswerAction) -> AnswerOption {
    AnswerOption {
        key,
        label: label.to_owned(),
        action,
        recommended: false,
        detail: None,
    }
}

/// Claude Code is down with `paused` tasks waiting on it, tested again 4m48s from [`WALL`].
fn down(paused: usize) -> AgentDownPacket {
    let titles = [
        "Add the --due flag",
        "Test the --due flag",
        "Document --due",
    ];
    AgentDownPacket {
        agent_id: AgentId("claude-code".into()),
        evidence: "Opus 5.5 overloaded, Sonnet 5.5 failing with server errors".into(),
        test_at: Some(WALL + time::Duration::seconds(288)),
        paused: titles
            .iter()
            .take(paused)
            .enumerate()
            .map(|(at, title)| PausedTask {
                task: TaskId::new(format!("task-00{}", at + 1)).expect("a task id"),
                title: (*title).to_owned(),
                role: RoleName::Worker,
            })
            .collect(),
        fits_all: None,
        kept_by: None,
    }
}

/// The question as the run opens it: about the CLI and no one task, and taking a note for
/// the paused tasks' workers.
fn opened(packet: AgentDownPacket, options: Vec<AnswerOption>) -> ApprovalOpened {
    let paused = match packet.paused.len() {
        1 => "1 task".to_owned(),
        many => format!("{many} tasks"),
    };
    ApprovalOpened {
        id: ApprovalId::numbered(1),
        kind: ApprovalKind::AgentDown,
        title: format!("Claude Code is down · {paused} paused"),
        task: None,
        packet: ApprovalPacket::AgentDown(packet),
        evidence_sha256: "0".repeat(64),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        options,
        free_text: FreeText::Guidance,
        needs_you: true,
    }
}

fn all_three() -> Vec<AnswerOption> {
    vec![
        option('w', "Wait for Claude Code", AnswerAction::Wait),
        option(
            'a',
            "One agent for every paused task",
            AnswerAction::OneForAll,
        ),
        option('e', "Choose per task", AnswerAction::PerTask),
    ]
}

fn asked(opened: &ApprovalOpened) -> QuestionPrompt {
    approval_question_local(1, opened, None, UtcOffset::UTC, &DisplayNames::default())
}

/// Each option's label and what it says after it, at `now`.
fn explained(question: &QuestionPrompt, now: Instant) -> Vec<(String, String)> {
    question
        .options
        .iter()
        .map(|option| {
            let mut said: Vec<String> = Vec::new();
            said.extend(option.model.as_ref().map(|model| model.name.clone()));
            said.extend(option.description.clone());
            said.extend(option.counts_to.as_ref().map(|count| count.text(now)));
            (option.label.clone(), said.join(" | "))
        })
        .collect()
}

#[test]
fn the_title_names_the_cli_and_the_options_say_what_they_mean() {
    let question = asked(&opened(down(2), all_three()));
    assert_eq!(question.title, "Claude Code is down");
    assert_eq!(
        question.detail,
        [
            "Opus 5.5 overloaded, Sonnet 5.5 failing with server errors",
            "Two tasks are paused, their work so far kept.",
        ]
    );
    let now = Instant::now();
    assert_eq!(
        explained(&question, now),
        [
            (
                "Wait for Claude Code".to_owned(),
                "tried again at 9:19 AM".to_owned()
            ),
            // No agent is known to fit them all, so nothing is promised.
            ("One agent for every paused task".to_owned(), String::new()),
            (
                "Choose per task".to_owned(),
                "an agent for each, from every installed CLI".to_owned()
            ),
        ]
    );
    assert_eq!(question.selected, 0);
}

#[test]
fn a_setting_that_keeps_the_tasks_here_makes_waiting_your_own_answer() {
    let mut packet = down(2);
    packet.kept_by = Some("same_cli".into());
    packet.fits_all = Some("antigravity-cli/gemini-3.6-pro".parse().expect("a model"));
    // Whatever order the options come in, the pointer starts on waiting.
    let mut options = all_three();
    options.rotate_left(1);
    let opened = opened(packet.clone(), options);
    let mut question = asked(&opened);
    assert_eq!(
        question.options[question.selected].label,
        "Wait for Claude Code"
    );
    assert_eq!(
        question.detail[1],
        "Two tasks are paused, their work so far kept. same_cli keeps them on Claude Code."
    );

    let now = Instant::now();
    count_down(&mut question, &packet, &opened.options, now, WALL);
    let said = |question: &QuestionPrompt, at: Instant| explained(question, at)[2].1.clone();
    assert_eq!(said(&question, now), "your setting | tried again in 4m48s");
    let later = now + Duration::from_secs(60);
    assert_eq!(
        said(&question, later),
        "your setting | tried again in 3m48s"
    );
    // The time shown changes as its second runs out, and not before.
    assert_eq!(
        question.next_change(now),
        Some(now + Duration::from_secs(1))
    );
    let part = now + Duration::from_millis(400);
    assert_eq!(
        question.next_change(part),
        Some(now + Duration::from_secs(1))
    );
    // Once the test is due it reads as any moment, and nothing more changes.
    let due = now + Duration::from_secs(288);
    assert_eq!(
        said(&question, due),
        "your setting | tried again in a moment"
    );
    assert_eq!(question.next_change(due + Duration::from_secs(1)), None);

    assert_eq!(
        explained(&question, now)[0],
        (
            "One agent for every paused task".to_owned(),
            "Gemini 3.6 Pro | on Antigravity CLI fits both".to_owned()
        )
    );
}

#[test]
fn the_words_follow_how_many_tasks_wait() {
    let fits = |paused: usize| {
        let mut packet = down(paused);
        packet.fits_all = Some("codex/gpt-5.6".parse().expect("a model"));
        let question = asked(&opened(packet, all_three()));
        let said = explained(&question, Instant::now());
        (
            question.detail.get(1).cloned(),
            said[1].1.clone(),
            said[2].1.clone(),
        )
    };
    assert_eq!(
        fits(1),
        (
            Some("One task is paused, its work so far kept.".to_owned()),
            "GPT 5.6 | on Codex can take it".to_owned(),
            "an agent of your choice, from every installed CLI".to_owned(),
        )
    );
    assert_eq!(
        fits(3),
        (
            Some("Three tasks are paused, their work so far kept.".to_owned()),
            "GPT 5.6 | on Codex fits all 3".to_owned(),
            "an agent for each, from every installed CLI".to_owned(),
        )
    );
    assert_eq!(fits(0).0, None);
}

/// An agent with no model named goes by its CLI's name, said once.
#[test]
fn an_agent_that_names_no_model_is_named_once() {
    let mut packet = down(1);
    packet.fits_all = Some("codex".parse().expect("an agent"));
    let question = asked(&opened(packet, all_three()));
    assert_eq!(
        explained(&question, Instant::now())[1].1,
        "Codex | can take it"
    );
}

#[test]
fn an_explanation_the_event_carries_is_kept() {
    let mut options = all_three();
    options[0].detail = Some("until Claude Code answers again".into());
    let packet = down(2);
    let opened = opened(packet.clone(), options);
    let mut question = asked(&opened);
    let now = Instant::now();
    count_down(&mut question, &packet, &opened.options, now, WALL);
    assert_eq!(
        explained(&question, now)[0].1,
        "until Claude Code answers again"
    );
}

/// The workflow view of a run paused on Claude Code, and under it the question, on a screen
/// of 124 by 24: the view takes twelve rows, and the question's rule takes its keys' row.
#[test]
fn under_the_workflow_view_the_question_reads_as_its_picture() {
    let mut picture = picture!("workflow_cli_down");
    // The picture spells the count as Claude Code does. Every screen here writes `44k`.
    picture.reword(6, "44.0k", "44k");
    // Esc means carry on without me, as it does on every question a run asks: whoever asked
    // then decides, and here the tasks go on waiting. The picture calls it setting the
    // question aside, and ends the row there.
    picture.reword(
        19,
        "Esc to set aside",
        "Esc to carry on without me · shift+tab to cycle modes",
    );

    let theme = dark();
    let state = AppState::default();
    let now = Instant::now();
    let mut view = WorkflowView::new(examples::cli_down());
    let area = Rect::new(0, 0, 124, 12);
    let mut buf = Buffer::empty(area);
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now,
    };
    view.render(area, &mut buf, &ctx);
    let mut rows = dock::lines(&buf);
    rows.pop();

    let mut packet = down(2);
    packet.kept_by = Some("same_cli".into());
    packet.fits_all = Some("antigravity-cli/gemini-3.6-pro".parse().expect("a model"));
    let opened = opened(packet.clone(), all_three());
    let mut question = approval_question_local(1, &opened, None, UtcOffset::UTC, &state.names);
    count_down(&mut question, &packet, &opened.options, now, WALL);
    question.focus();
    let mut bottom = BottomState::new();
    bottom.question = Some(question);

    let ctx = RenderCtx::new(124, &theme, Clock::still(now)).with_rows(24);
    let drawn = bottom.render_docked(&ctx, 22, false, rows, true);
    assert_eq!(drawn.lines.len(), 20);
    picture.assert_drawn(0..20, &buffer_of(&drawn.lines, 124), 0);
}
