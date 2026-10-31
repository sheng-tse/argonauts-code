//! The resume list, drawn from its picture's rows at the picture's width of 118 columns. The
//! picture writes the app's name in full, which it was first drawn without, in a column five
//! cells wider, so each row's title, last use and length stand five cells further right.

use argonauts_core::base::names::AgentId;
use time::macros::datetime;

use super::*;
use crate::testing::pictures::picture_check::assert_matches;

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
        needs_you: false,
    }];
    let sessions = vec![
        session(
            "claude-code",
            "0a1b2c3d-0001",
            "Due dates review and research",
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
    let mut view = ResumePicker::listing(conversations, sessions);
    view.clock = WallClock::pinned(NOW);
    view
}

#[test]
fn the_list_is_its_picture() {
    let theme = Theme::dark();
    let lines = picker().boxed(
        WIDTH,
        4,
        "~/code/tasks-cli",
        &DisplayNames::default(),
        &theme,
    );
    assert_matches(
        "resume",
        &lines,
        include_str!("../../../tests/golden/views/resume.txt"),
        include_str!("../../../tests/golden/views/resume.html"),
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
    assert!(matches!(
        here,
        ViewOutcome::Request(ViewRequest::ResumeList)
    ));
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

/// Where the terminal draws logos, each CLI's session has the CLI's logo before its name in
/// the `from` column, and Argonauts Code's own conversations have none.
#[test]
fn each_clis_session_has_its_logo_before_the_name() {
    use ratatui::buffer::Buffer;

    use crate::base::kit::logo::golden::{count, pictured, with_stand_ins};
    use crate::testing::pictures::golden::{Picture, Region, assert_matches, keep};

    let theme = pictured(&crate::testing::pictures::golden::theme());
    let lines = picker().boxed(
        WIDTH,
        4,
        "~/code/tasks-cli",
        &DisplayNames::default(),
        &theme,
    );
    let mut buf = Buffer::empty(Rect::new(0, 0, WIDTH as u16, lines.len() as u16));
    for (y, line) in lines.iter().enumerate() {
        buf.set_line(0, y as u16, line, WIDTH as u16);
    }
    assert_eq!(count(&buf), 3);
    let shown = with_stand_ins(&buf);
    keep("resume_logos", &shown);
    let picture = Picture::screen("views", "resume_logos");
    assert_matches(&picture, &shown, &Region::whole(&picture, (0, 0)), &theme);
}

/// The list once it says where each session carries on: the day after the first picture's,
/// with an OpenCode session among the rest.
const LATER: OffsetDateTime = datetime!(2026-10-11 12:00 UTC);
const CLAUDE_ID: &str = "00000000-0000-4000-8000-000000000001";
const CODEX_ID: &str = "019a5770-1c2d-7e3f-9a4b-5c6d7e8f9a0b";

fn in_cli(cli: &str, agent_id: &str, id: &str, command: &str) -> InItsCli {
    InItsCli {
        session: CliSessionRef {
            agent_id: AgentId(agent_id.into()),
            id: id.into(),
        },
        conversation: None,
        cli: cli.into(),
        command: command.into(),
        local_model: false,
        also: None,
        made_there: 0,
    }
}

fn picker_in_cli() -> ResumePicker {
    let conversations = vec![ConversationSummary {
        conversation: ConversationId::numbered(4),
        title: "Add due dates to the tasks CLI".into(),
        updated: datetime!(2026-10-11 11:48 UTC),
        worker: None,
        turns: 44,
        runs: 0,
        needs_you: false,
    }];
    let sessions = vec![
        session(
            "claude-code",
            "0a1b2c3d-0001",
            "Due dates review and research",
            datetime!(2026-10-10 09:00 UTC),
            1_240,
        ),
        session(
            "opencode",
            "ses_0a1b2c3d",
            "Port the CSV importer to streaming",
            datetime!(2026-10-09 15:00 UTC),
            0,
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
            CODEX_ID,
            "Make the rate limiter per API key",
            datetime!(2026-10-08 11:00 UTC),
            9,
        ),
    ];
    let mut held = in_cli(
        "Claude Code",
        "claude-code",
        CLAUDE_ID,
        &format!("claude --resume {CLAUDE_ID}"),
    );
    held.conversation = Some(ConversationId::numbered(4));
    held.made_there = 2;
    let mut view = ResumePicker::listing(conversations, sessions).with_in_cli(vec![
        held,
        in_cli(
            "Codex",
            "codex",
            CODEX_ID,
            &format!("codex resume {CODEX_ID}"),
        ),
    ]);
    view.clock = WallClock::pinned(LATER);
    view
}

fn boxed_in_cli(view: &ResumePicker) -> Vec<Line<'static>> {
    view.boxed(
        WIDTH,
        5,
        "~/code/tasks-cli",
        &DisplayNames::default(),
        &Theme::dark(),
    )
}

#[test]
fn the_row_under_the_list_names_the_cli_that_holds_the_conversation() {
    let view = picker_in_cli();
    assert_matches(
        "resume_in_cli",
        &boxed_in_cli(&view),
        include_str!("../../../tests/golden/views/resume_in_cli.txt"),
        include_str!("../../../tests/golden/views/resume_in_cli.html"),
        &Theme::dark(),
        &[],
    );
}

#[test]
fn a_codex_session_reads_the_same_way() {
    let mut view = picker_in_cli();
    view.list.selected = 4;
    assert_matches(
        "resume_in_codex",
        &boxed_in_cli(&view),
        include_str!("../../../tests/golden/views/resume_in_codex.txt"),
        include_str!("../../../tests/golden/views/resume_in_codex.html"),
        &Theme::dark(),
        &[],
    );
}

#[test]
fn a_local_models_session_prints_its_whole_command_and_c_copies_it() {
    let command =
        format!("codex resume --oss --local-provider ollama --model qwen3-coder:30b {CODEX_ID}");
    let mut view = picker_in_cli();
    for cli in &mut view.in_cli {
        if cli.session.id == CODEX_ID {
            cli.command = command.clone();
            cli.local_model = true;
        }
    }
    view.list.selected = 4;
    let lines = boxed_in_cli(&view);
    assert_matches(
        "resume_local",
        &lines,
        include_str!("../../../tests/golden/views/resume_local.txt"),
        include_str!("../../../tests/golden/views/resume_local.html"),
        &Theme::dark(),
        &[],
    );
    assert!(matches!(view.copy_command(), ViewOutcome::Copy(copied) if copied == command));
    // The conversation's command has no flags to keep, so c copies nothing there.
    view.list.selected = 0;
    assert!(matches!(view.copy_command(), ViewOutcome::Consumed));
}

#[test]
fn o_opens_the_session_picked_in_its_cli() {
    let mut view = picker_in_cli();
    let opened = view.open_in_cli();
    assert!(
        matches!(&opened, ViewOutcome::Send(UiCommand::OpenInItsCli { session, conversation: Some(conversation) }) if session.id == CLAUDE_ID && *conversation == ConversationId::numbered(4)),
        "{opened:?}"
    );
    view.list.selected = 4;
    let opened = view.open_in_cli();
    assert!(
        matches!(&opened, ViewOutcome::Send(UiCommand::OpenInItsCli { session, conversation: None }) if session.id == CODEX_ID),
        "{opened:?}"
    );
    // OpenCode's session has no command the app knows, so no row and no o.
    view.list.selected = 2;
    assert!(matches!(view.open_in_cli(), ViewOutcome::Consumed));
    let keys = view.key_line(114, &Theme::dark()).to_string();
    assert!(!keys.contains("o opens"), "{keys}");
}

#[test]
fn every_row_under_a_list_with_commands_is_as_wide_as_the_box() {
    for width in [80, 100, 118, 160] {
        let view = picker_in_cli();
        let lines = view.boxed(width, 5, "~/work", &DisplayNames::default(), &Theme::dark());
        for line in &lines {
            assert_eq!(line.width(), width, "{line}");
        }
    }
}
