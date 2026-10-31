//! The lines a request leaves in the conversation before and while its work goes on, compared
//! cell for cell, colours included, with their pictures in `tests/golden/transcript`: the crew
//! your words set, each role with where it came from (`crew_for_request`); the start line that
//! says what was asked, "as you asked" once (`start_as_you_asked`); and the line a command that
//! wrote outside the project leaves under its call (`wrote_outside`).

use std::path::PathBuf;

use argonauts_core::app::{AppEvent, CrewRow};
use argonauts_core::runs::event::AgentEvent;
use argonauts_core::runs::events::run::{
    CheckedInstruction, InstructionOutcome, RunLifecycle, RunMode, RunStarted,
};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::workflows::steps::intake::InstructionKind;
use argonauts_core::{NoteLevel, RunId};
use serde_json::json;
use time::macros::datetime;

use super::live_rows_tests::Screen;
use crate::testing::pictures::picture_check::assert_matches;

const WIDTH: u16 = 118;

/// The crew your words set for a request, as the app sends it: each role, its models in full,
/// and where they came from.
fn crew() -> Vec<CrewRow> {
    let all = "Opus 5.5 on Claude Code, GPT 5.6 Luna on Codex, agy";
    [
        (
            "orchestrator",
            "Haiku 5.5 on Claude Code",
            "your main session",
        ),
        ("workers", all, "your settings"),
        ("reviewers", "GPT 5.6 Luna on Codex", "as you asked"),
        ("judge", all, "your settings"),
    ]
    .into_iter()
    .map(|(role, who, from)| CrewRow {
        role: role.into(),
        who: who.into(),
        from: from.into(),
    })
    .collect()
}

#[test]
fn the_crew_your_words_set_shows_with_where_each_role_came_from() {
    let mut screen = Screen::new();
    screen.ask("have codex review it, and add due dates to tasks");
    screen.app(AppEvent::CrewForRequest { rows: crew() });
    let lines = screen.lines(WIDTH);
    let start = lines
        .iter()
        .position(|line| line.to_string().contains("Crew for this request"))
        .expect("the crew's lines");
    assert_matches(
        "crew_for_request",
        &lines[start..],
        include_str!("../../../../tests/golden/transcript/crew_for_request.txt"),
        include_str!("../../../../tests/golden/transcript/crew_for_request.html"),
        &Screen::theme(),
        &[],
    );
}

fn followed(says: &str, kind: InstructionKind, said: &str) -> CheckedInstruction {
    CheckedInstruction {
        says: says.into(),
        kind,
        outcome: InstructionOutcome::Followed {
            said: said.into(),
            note: None,
        },
    }
}

/// A run's start line says what your words asked once: words that keep a crew to one CLI end
/// on "as you asked" themselves, and the line starts with it.
#[test]
fn a_runs_start_line_says_as_you_asked_once() {
    let mut screen = Screen::new();
    let started = RunStarted {
        request: "Add due dates to tasks".into(),
        title: "Add due dates to tasks".into(),
        mode: RunMode::Crew,
        forced: false,
        workflow: "orchestrator".into(),
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        repo_root: PathBuf::from("/Users/you/code/tasks-cli"),
        base_branch: Some("main".into()),
        base_commit: "6fc02994c".into(),
        uncommitted: vec![],
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: json!({}),
        overrides: Vec::new(),
        cli_versions: Default::default(),
        version: "0.2.0".into(),
        outside_agent: None,
        instructions: vec![
            followed(
                "use only claude",
                InstructionKind::Crew,
                "Crew: Claude Code, reviews stay within Claude (Opus 5.5 builds, Sonnet 5 \
                 reviews), as you asked",
            ),
            followed(
                "have codex orchestrate",
                InstructionKind::Orchestrate,
                "Codex orchestrates",
            ),
        ],
        required_steps: Vec::new(),
        starts_from: None,
    };
    screen.app(AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 0,
            ts: datetime!(2026-10-09 10:42 UTC),
            run_id: RunId::new("run-01").expect("a run id"),
            task_id: None,
            session: None,
            source: "argonauts-code".into(),
            kind: RunEventKind::Run(RunLifecycle::Started(Box::new(started))),
        }),
    });
    let lines = screen.lines(WIDTH);
    let start = lines
        .iter()
        .position(|line| line.to_string().contains("As you asked"))
        .expect("the start line");
    let text = lines[start].to_string();
    assert_eq!(text.matches("as you asked").count(), 0, "{text}");
    assert_matches(
        "start_as_you_asked",
        &lines[start..],
        include_str!("../../../../tests/golden/transcript/start_as_you_asked.txt"),
        include_str!("../../../../tests/golden/transcript/start_as_you_asked.html"),
        &Screen::theme(),
        &[],
    );
}

/// A command that wrote a file outside the project leaves one quiet line under its call once it
/// ends, as a file tool's write does.
#[test]
fn a_command_that_wrote_outside_leaves_its_line_under_the_call() {
    let mut screen = Screen::new();
    screen.ask("save the release notes to my downloads");
    screen.agent(AgentEvent::ToolStarted {
        id: "c1".into(),
        name: "Bash".into(),
        input: json!({ "command": "cp NOTES.md ~/Downloads/notes.md" }),
        parent: None,
    });
    screen.agent(AgentEvent::ToolFinished {
        id: "c1".into(),
        ok: true,
        output: None,
        parent: None,
    });
    screen.app(AppEvent::Notice {
        level: NoteLevel::Quiet,
        text: "wrote ~/Downloads/notes.md, outside the project".into(),
    });
    let lines = screen.lines(WIDTH);
    let start = lines
        .iter()
        .position(|line| line.to_string().contains("Bash("))
        .expect("the command's call");
    assert_matches(
        "wrote_outside",
        &lines[start..],
        include_str!("../../../../tests/golden/transcript/wrote_outside.txt"),
        include_str!("../../../../tests/golden/transcript/wrote_outside.html"),
        &Screen::theme(),
        &[],
    );
}
