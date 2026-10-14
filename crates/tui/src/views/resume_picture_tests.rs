//! The resume list, drawn from its picture's rows at the picture's width of 118 columns. The
//! picture writes the app's name in full, which it was first drawn without, in a column five
//! cells wider, so each row's title, last use and length stand five cells further right.

use argonauts_core::names::AgentId;
use time::macros::datetime;

use super::*;
use crate::bridge::transcript::picture_check::assert_matches;

const WIDTH: usize = 118;
const NOW: OffsetDateTime = datetime!(2026-10-10 12:00 UTC);

fn session(cli: &str, id: &str, title: &str, updated: OffsetDateTime, turns: u32) -> CliSession {
    CliSession {
        agent_id: AgentId(cli.into()),
        id: id.into(),
        title: title.into(),
        prompt: None,
        folder: "/work/argonauts-code".into(),
        updated,
        model: None,
        branch: None,
        last_message: None,
        turns,
    }
}

fn picker() -> ResumePicker {
    let conversations = vec![ConversationSummary {
        conversation: ConversationId::numbered(1),
        title: "Add due dates to the tasks CLI".into(),
        updated: datetime!(2026-10-10 10:00 UTC),
        worker: None,
        turns: 42,
        runs: 0,
    }];
    let sessions = vec![
        session(
            "claude-code",
            "0a1b2c3d-0001",
            "README.md review and research",
            datetime!(2026-10-09 09:00 UTC),
            1_240,
        ),
        session(
            "claude-code",
            "0a1b2c3d-0002",
            "Fix the money rounding so the three failing tests pass",
            datetime!(2026-10-08 15:00 UTC),
            18,
        ),
        session(
            "codex",
            "019a5770-0003",
            "Make the rate limiter per API key",
            datetime!(2026-10-08 11:00 UTC),
            9,
        ),
    ];
    let mut view = ResumePicker::listing(Vec::new(), conversations, sessions);
    view.clock = WallClock::pinned(NOW);
    view
}

#[test]
fn the_list_is_its_picture() {
    let theme = Theme::dark();
    let lines = picker().boxed(
        WIDTH,
        4,
        "~/github/argonauts-code",
        &DisplayNames::default(),
        &theme,
    );
    assert_matches(
        "resume",
        &lines,
        include_str!("pictures/resume.txt"),
        include_str!("pictures/resume.html"),
        &theme,
        &[],
    );
    // The picked row's mark and title take the color every picker gives its selection.
    let picked = theme.style(Role::Question).fg;
    let row = &lines[4];
    assert_eq!(row.spans[1].content.as_ref(), "❯ ");
    assert_eq!(row.spans[1].style.fg, picked);
    assert_eq!(row.spans[5].style.fg, picked);
}

#[test]
fn every_row_is_as_wide_as_the_box() {
    for width in [80, 100, 118, 160] {
        let lines = picker().boxed(width, 4, "~/work", &DisplayNames::default(), &Theme::dark());
        for line in &lines {
            assert_eq!(line.width(), width, "{line}");
        }
    }
}

#[test]
fn b_branches_and_a_lists_every_folder() {
    let mut view = picker();
    let branch = view.branch();
    assert!(
        matches!(&branch, ViewOutcome::Send(UiCommand::Slash { command: SlashCommand::Resume { target: Some(ResumeTarget::BranchConversation(id)) } }) if *id == ConversationId::numbered(1)),
        "{branch:?}"
    );
    view.list.selected = 3;
    let branch = view.branch();
    assert!(
        matches!(&branch, ViewOutcome::Send(UiCommand::Slash { command: SlashCommand::Resume { target: Some(ResumeTarget::BranchCliSession(session)) } }) if session.to_string() == "codex:019a5770-0003"),
        "{branch:?}"
    );
    let every = view.other_folders();
    assert!(matches!(
        every,
        ViewOutcome::Request(ViewRequest::SessionsEverywhere)
    ));
    let theme = Theme::dark();
    let title = view.title_line("~/work", 4, &theme).to_string();
    assert_eq!(title, "Resume a conversation · in every folder");
    assert!(
        view.key_line(114, &theme)
            .to_string()
            .contains("a this folder")
    );
    let here = view.other_folders();
    assert!(matches!(here, ViewOutcome::Request(ViewRequest::Runs)));
}

#[test]
fn a_session_its_cli_just_wrote_to_asks_before_it_carries_on() {
    let mut view = picker();
    view.sessions[0].updated = NOW - time::Duration::seconds(40);
    view.rows = view.merged();
    let at = view
        .rows
        .iter()
        .position(|row| matches!(row, Row::Session(0)))
        .unwrap();
    view.list.selected = at;
    assert!(matches!(view.enter(), ViewOutcome::Consumed));
    let keys = view.key_line(114, &Theme::dark()).to_string();
    assert!(
        keys.starts_with("Claude Code wrote to it just now, so it may still be open there"),
        "{keys}"
    );
    let again = view.enter();
    assert!(
        matches!(&again, ViewOutcome::Send(UiCommand::Slash { command: SlashCommand::Resume { target: Some(ResumeTarget::CliSession(session)) } }) if session.id == "0a1b2c3d-0001"),
        "{again:?}"
    );
}

#[test]
fn lengths_and_days_read_as_the_list_writes_them() {
    assert_eq!(counted(0, "turn"), "");
    assert_eq!(counted(1, "turn"), "1 turn");
    assert_eq!(counted(42, "turn"), "42 turns");
    assert_eq!(counted(1_240, "turn"), "1.2k turns");
    assert_eq!(counted(2_030, "turn"), "2k turns");
    let clock = WallClock::pinned(NOW);
    assert_eq!(
        last_used(datetime!(2026-10-10 11:48 UTC), &clock),
        "12m ago"
    );
    assert_eq!(
        last_used(datetime!(2026-10-09 13:00 UTC), &clock),
        "23h ago"
    );
    assert_eq!(
        last_used(datetime!(2026-10-09 09:00 UTC), &clock),
        "yesterday"
    );
    assert_eq!(last_used(datetime!(2026-10-08 23:59 UTC), &clock), "Oct 8");
    assert_eq!(
        last_used(datetime!(2025-12-31 08:00 UTC), &clock),
        "Dec 31, 2025"
    );
}
