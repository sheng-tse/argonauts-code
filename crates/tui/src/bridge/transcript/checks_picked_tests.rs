//! A run's checks as the conversation shows them, compared cell for cell, colours included, with
//! their pictures in `tests/golden/transcript`: the checks the main agent picked, under the
//! workflow's row with who picked them and where they run, then a task's Checks row running them
//! (`checks_picked`); the line under your message when you ask to keep them (`checks_kept`); the
//! Checks row of a run with none, saying why (`checks_held_back`); a
//! merged run's recap, saying what its checks did (`recap_checks`); and the lines of a run whose
//! reviewer fell back to its builder's own provider on autopilot (`fallback_kept`), and of one
//! whose task nobody but its builder could review (`nobody_left_kept`).

use std::path::PathBuf;

use argonauts_core::app::AppEvent;
use argonauts_core::events::run::{OverrideRecord, RunLifecycle, RunMode, RunNamed, RunStarted};
use argonauts_core::events::task::{CheckKind, CheckOutcome, TaskEvent, TaskState};
use argonauts_core::events::workflow::{DeclaredPhase, WorkflowDeclared, WorkflowEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::WorkflowId;
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::workflow::StandardPhase;
use argonauts_core::{Note, NoteLevel, RunId, TaskId};
use serde_json::json;
use time::macros::datetime;

use super::live_rows_tests::Screen;
use super::picture_check::assert_matches;
use crate::state::{RunState, WorkflowState};

const WIDTH: u16 = 118;

/// The run's slug as the picture spells it, and as the Workflow line draws it: as words.
const SLUG: &str = "fix-money-rounding";
const SLUG_WORDS: &str = "Fix money rounding";

fn run_id() -> RunId {
    RunId::new("run-01").expect("a valid run id")
}

fn event(task: Option<&TaskId>, kind: RunEventKind) -> AppEvent {
    AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 0,
            ts: datetime!(2026-10-09 10:42 UTC),
            run_id: run_id(),
            task_id: task.cloned(),
            session: None,
            source: "argonauts-code".into(),
            kind,
        }),
    }
}

/// A run the main agent started with the tests it picked, since none were set.
fn started() -> RunStarted {
    let tests = json!([{"name": "tests", "command": ["python3", "-m", "unittest", "-q"]}]);
    RunStarted {
        request: "Fix the money rounding".into(),
        title: "Fix the money rounding".into(),
        mode: RunMode::Crew,
        forced: false,
        workflow: "orchestrator".into(),
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        repo_root: PathBuf::from("/Users/you/money"),
        base_branch: Some("main".into()),
        base_commit: "6fc02994c".into(),
        uncommitted: vec![],
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: json!({"checks": {"run": tests, "full_suite": []}}),
        overrides: vec![OverrideRecord {
            key: "checks.run".into(),
            value: tests.to_string(),
            from: "request".into(),
        }],
        cli_versions: Default::default(),
        version: "0.2.0".into(),
        outside_agent: None,
        instructions: Vec::new(),
        required_steps: Vec::new(),
        starts_from: None,
    }
}

fn screen() -> Screen {
    let mut screen = Screen::new();
    let id = WorkflowId::numbered(1);
    let mut run = RunState::new(run_id());
    run.workflow = Some("orchestrator".into());
    run.workflows.push(WorkflowState {
        id: id.clone(),
        name: "orchestrator".into(),
        from: None,
        description: String::new(),
        phases: vec!["Build".into(), "Review".into(), "Merge".into()],
        parent: None,
        steps: Vec::new(),
    });
    run.named = Some(RunNamed {
        name: SLUG.into(),
        description: "Fix the money rounding".into(),
        style: RunStyle::Workflow,
        based_on: None,
        done_when: Vec::new(),
    });
    screen.state.runs.insert(run_id(), run);
    screen.app(event(
        None,
        RunEventKind::Run(RunLifecycle::Started(Box::new(started()))),
    ));
    let phase = |index: u32, title: &str| DeclaredPhase {
        index,
        name: title.into(),
        detail: String::new(),
        standard: StandardPhase::Do,
    };
    screen.app(event(
        None,
        RunEventKind::Workflow(WorkflowEvent::Declared(Box::new(WorkflowDeclared {
            id,
            name: "orchestrator".into(),
            description: String::new(),
            phases: vec![phase(0, "Build"), phase(1, "Review"), phase(2, "Merge")],
            required: Vec::new(),
        }))),
    ));
    let task = TaskId::new("fix-rounding").expect("a task id");
    screen.app(event(
        Some(&task),
        RunEventKind::Check(CheckOutcome {
            name: "tests".into(),
            kind: CheckKind::Command,
            passed: true,
            command: Some(
                ["python3", "-m", "unittest", "-q"]
                    .map(str::to_owned)
                    .to_vec(),
            ),
            exit_code: Some(0),
            duration_ms: 30,
            summary: "12 passed".into(),
            output_tail: String::new(),
            violations: Vec::new(),
            sandboxed: true,
            skipped: false,
        }),
    ));
    screen.app(event(
        Some(&task),
        RunEventKind::Task(TaskEvent::State {
            from: Some(TaskState::Checking),
            to: TaskState::InReview,
            reason: String::new(),
        }),
    ));
    screen
}

#[test]
fn checks_the_main_agent_picked_read_as_the_picture_draws_them() {
    let lines = screen().lines(WIDTH);
    let start = lines
        .iter()
        .position(|line| line.to_string().contains("Workflow("))
        .expect("the workflow's row");
    assert_matches(
        "checks_picked",
        &lines[start..],
        &include_str!("../../../tests/golden/transcript/checks_picked.txt")
            .replace(SLUG, SLUG_WORDS),
        &include_str!("../../../tests/golden/transcript/checks_picked.html")
            .replace(SLUG, SLUG_WORDS),
        &Screen::theme(),
        &[],
    );
}

/// Asking to keep a run's checks leaves one line under your message, naming the key alone and
/// where it went: this project, just for you, since checks make sense in one repository.
#[test]
fn checks_you_ask_to_keep_say_kept_under_your_message() {
    let mut screen = Screen::new();
    screen.ask("keep these checks");
    screen.app(AppEvent::SettingChanged {
        key: "checks.run".into(),
        value: r#"[{ name = "tests", command = ["python3", "-m", "unittest", "-q"] }]"#.into(),
        scope: SettingScope::ProjectLocal,
        previous: None,
        reset: false,
        why: None,
        kept: true,
    });
    let lines = screen.lines(WIDTH);
    let start = lines
        .iter()
        .position(|line| line.to_string().starts_with("❯ keep"))
        .expect("your message");
    assert_matches(
        "checks_kept",
        &lines[start..start + 2],
        include_str!("../../../tests/golden/transcript/checks_kept.txt"),
        include_str!("../../../tests/golden/transcript/checks_kept.html"),
        &Screen::theme(),
        &[],
    );
}

/// Checks a project file you haven't trusted names: the run leaves them out, and its marker
/// says where they wait.
#[test]
fn a_task_with_checks_held_back_says_which_file_holds_them() {
    let mut screen = Screen::new();
    let task = TaskId::new("fix-rounding").expect("a task id");
    let mut started = started();
    started.overrides.clear();
    started.settings = json!({"checks": {"run": [], "full_suite": []}});
    screen.app(event(
        None,
        RunEventKind::Run(RunLifecycle::Started(Box::new(started))),
    ));
    screen.app(event(
        Some(&task),
        RunEventKind::Check(CheckOutcome {
            name: "checks.run".into(),
            kind: CheckKind::Command,
            passed: true,
            command: None,
            exit_code: None,
            duration_ms: 0,
            summary: "checks.run is set in .argonauts/config.toml, a file you haven't trusted \
                      · /setup"
                .into(),
            output_tail: String::new(),
            violations: Vec::new(),
            sandboxed: false,
            skipped: true,
        }),
    ));
    screen.app(event(
        Some(&task),
        RunEventKind::Task(TaskEvent::State {
            from: Some(TaskState::Checking),
            to: TaskState::InReview,
            reason: String::new(),
        }),
    ));
    let lines = screen.lines(WIDTH);
    let start = lines
        .iter()
        .position(|line| line.to_string().contains("Checks("))
        .expect("the Checks row");
    assert_matches(
        "checks_held_back",
        &lines[start..],
        include_str!("../../../tests/golden/transcript/checks_held_back.txt"),
        include_str!("../../../tests/golden/transcript/checks_held_back.html"),
        &Screen::theme(),
        &[],
    );
}

/// A merged run's recap says the checks pass only when they ran and passed, and otherwise what
/// didn't run or what failed, wrapped at the terminal's width as the picture draws it.
#[test]
fn a_merged_runs_recap_says_what_its_checks_did() {
    let mut screen = Screen::new();
    for text in [
        "Money rounding now uses Decimal. All 3 tasks merged, and the checks pass: tests ran on \
         each task and on the merge. Ask for another change, or /resume an older run.",
        "Money rounding now uses Decimal. All 3 tasks merged. No checks ran, since none are set: \
         /config checks.run sets them. Ask for another change, or /resume an older run.",
        "Money rounding now uses Decimal. All 3 tasks merged. The final check failed (tests) and \
         you merged anyway. Ask for another change, or /resume an older run.",
    ] {
        screen.app(AppEvent::Recap {
            text: text.into(),
            model: None,
        });
    }
    let lines = screen.lines(120);
    let start = lines
        .iter()
        .position(|line| line.to_string().contains("recap:"))
        .expect("a recap");
    assert_matches(
        "recap_checks",
        &lines[start..],
        include_str!("../../../tests/golden/transcript/recap_checks.txt"),
        include_str!("../../../tests/golden/transcript/recap_checks.html"),
        &Screen::theme(),
        &[],
    );
}

/// On autopilot, a reviewer of the builder's own provider stands in for one that is out: its
/// line says the work stays on its branch, and autopilot's answer names the fallback and the
/// task it reviewed.
#[test]
fn a_fallback_from_the_builders_provider_keeps_the_branch_and_names_the_task() {
    let mut screen = Screen::new();
    let task = TaskId::numbered(1);
    let note = |level, text: &str| {
        event(
            Some(&task),
            RunEventKind::Note(Note {
                level,
                text: text.into(),
            }),
        )
    };
    screen.app(note(
        NoteLevel::Warn,
        "GPT 6 Luna on Codex is out (overloaded), and nobody outside the builder's provider is \
         left, so Sonnet 5 on Claude Code takes a reviewer on autopilot · its work stays on its \
         branch for you",
    ));
    screen.app(note(
        NoteLevel::Info,
        "Autopilot answered \"Merge this run into main?\" with \"Keep the branch\" instead of \
         asking you, since Sonnet 5 on Claude Code, a fallback from its builder's own provider, \
         reviewed task-001.",
    ));
    let lines = screen.lines(120);
    let start = lines
        .iter()
        .position(|line| line.to_string().contains("is out"))
        .expect("the fallback's line");
    assert_matches(
        "fallback_kept",
        &lines[start..],
        include_str!("../../../tests/golden/transcript/fallback_kept.txt"),
        include_str!("../../../tests/golden/transcript/fallback_kept.html"),
        &Screen::theme(),
        &[],
    );
}

/// On autopilot, a task nobody but its builder could review goes to the merge unreviewed, and
/// autopilot's answer keeps the branch and names the task.
#[test]
fn a_task_nobody_but_its_builder_could_review_keeps_the_branch_and_names_it() {
    let mut screen = Screen::new();
    let task = TaskId::numbered(1);
    screen.app(event(
        Some(&task),
        RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text: "Autopilot answered \"Merge this run into main?\" with \"Keep the branch\" \
                   instead of asking you, since nobody but its builder is left to review \
                   task-001."
                .into(),
        }),
    ));
    let lines = screen.lines(120);
    let start = lines
        .iter()
        .position(|line| line.to_string().contains("Autopilot answered"))
        .expect("autopilot's answer");
    assert_matches(
        "nobody_left_kept",
        &lines[start..],
        include_str!("../../../tests/golden/transcript/nobody_left_kept.txt"),
        include_str!("../../../tests/golden/transcript/nobody_left_kept.html"),
        &Screen::theme(),
        &[],
    );
}
