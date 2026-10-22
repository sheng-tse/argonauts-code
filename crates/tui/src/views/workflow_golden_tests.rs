//! The workflow view beside its pictures: each test builds the picture's own run, draws it at
//! the picture's width, and compares every cell, its colour included.
//!
//! The pictures are 118 cells wide. The frame stops six cells short of the screen's right
//! edge, as Claude Code's does, so they are drawn on a screen of 124.

use std::time::Instant;

use argonauts_core::app::{UiCommand, ViewData};
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::steps::StepName;
use argonauts_core::views::{AgentView as AgentData, StatusKind, StatusLine, Tone, WorkflowState};
use argonauts_core::{RunId, TaskId};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::workflow::WorkflowView;
use super::workflow_examples::{self as examples, Merge};
use super::workflow_pictures::{Picture, dark, picture};
use super::{View, ViewCtx};
use crate::state::{AppState, RunState, SessionState, TaskRow};

const SCREEN: u16 = 124;

/// The frame's rows above its box: the rule, the name, the description, the status line, and
/// a blank row.
const ABOVE_BOX: u16 = 5;

fn drawn(view: &mut WorkflowView, rows: u16) -> Buffer {
    let theme = dark();
    let state = AppState::default();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let area = Rect::new(0, 0, SCREEN, rows);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    buf
}

fn press(view: &mut WorkflowView, code: KeyCode) {
    let theme = dark();
    let state = AppState::default();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx);
}

#[test]
fn a_run_mid_review_reads_as_its_picture() {
    let mut picture = picture!("workflow_running");
    // The status line colours the model it names as that model's row does.
    picture.colour_as_its_row(3, &["GPT 5.6 Luna"]);
    let mut view = WorkflowView::new(examples::running());
    // It opens on Build, where the worker is on its second round.
    press(&mut view, KeyCode::Down);
    picture.assert_drawn(0..12, &drawn(&mut view, 12), 0);
}

#[test]
fn a_long_list_scrolls_with_claude_codes_mark() {
    let mut picture = picture!("workflow_big");
    // The picture ends its mark with `↓` on the list's last row. Claude Code draws an arrow
    // only where rows are hidden that way, so no row hides below the sixth of six.
    let mark = picture.text(11..12)[0].chars().count() - 4;
    picture.blank(11, mark);
    let mut view = WorkflowView::new(examples::big());
    // The fourth reviewer picked, then back to the phases: the list stays where it was.
    press(&mut view, KeyCode::Enter);
    for _ in 0..3 {
        press(&mut view, KeyCode::Down);
    }
    press(&mut view, KeyCode::Esc);
    picture.assert_drawn(0..13, &drawn(&mut view, 13), 0);
}

#[test]
fn a_fallback_at_work_reads_as_its_picture() {
    let mut picture = picture!("workflow_fallback");
    picture.in_place_of(6, "Opus 5.5", None);
    // The status line colours the model that does the job as its row does, and leaves the
    // one it stands in for dim.
    picture.colour_as_its_row(3, &["Sonnet 5.5"]);
    let mut view = WorkflowView::new(examples::fallback());
    press(&mut view, KeyCode::Down);
    picture.assert_drawn(0..12, &drawn(&mut view, 12), 0);
}

#[test]
fn a_run_paused_on_a_cli_that_is_down_reads_as_its_picture() {
    let mut picture = picture!("workflow_cli_down");
    build_in_its_own_column(&mut picture);
    // The picture says paused, which now only a pause you asked for reads: these tasks wait
    // for their CLI.
    picture.reword(3, "2 tasks paused", "2 tasks wait for it");
    picture.reword(
        6,
        "paused · work so far kept",
        "waits for Claude Code · work so far kept",
    );
    let mut view = WorkflowView::new(examples::cli_down());
    // The question about the CLI takes the keys' row and the rows under it.
    picture.assert_drawn(0..11, &drawn(&mut view, 12), 0);
}

/// Build's rows in the column Build's own labels give them, where the picture gives every
/// phase the widest label of the whole workflow.
fn build_in_its_own_column(picture: &mut Picture) {
    let width = "build:due-tests  ".len();
    picture.label_column(6, "build:due-flag", width);
    picture.label_column(7, "build:due-tests", width);
}

/// The wait under the header loses a second as each runs out, while every agent is paused,
/// and reads `in a moment` once it is due.
#[test]
fn the_wait_under_the_header_counts_down() {
    let theme = dark();
    let state = AppState::default();
    let mut view = WorkflowView::new(examples::cli_down());
    let status_at = |view: &mut WorkflowView, now: Instant| -> String {
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now,
        };
        let area = Rect::new(0, 0, SCREEN, 12);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, &ctx);
        let row: String = (0..SCREEN).map(|x| buf[(x, 3)].symbol()).collect();
        row.trim_end().to_owned()
    };
    let t0 = Instant::now();
    let at = |secs: u64| t0 + std::time::Duration::from_secs(secs);
    assert_eq!(
        status_at(&mut view, t0),
        " ◆ Claude Code is down · 2 tasks wait for it · tried again in 4m48s"
    );
    assert_eq!(view.next_change(t0), Some(at(1)));
    assert!(status_at(&mut view, at(60)).ends_with(" · tried again in 3m48s"));
    assert!(status_at(&mut view, at(287)).ends_with(" · tried again in 1s"));
    assert!(status_at(&mut view, at(288)).ends_with(" · tried again in a moment"));
    assert!(status_at(&mut view, at(900)).ends_with(" · tried again in a moment"));
}

/// The picture's run as the screen holds it: `build:due-flag` is session 1, building a task
/// that Sonnet 5.5 has.
fn building_due_flag(view: &WorkflowView) -> (AppState, RunId, TaskId) {
    let run = view.current().run.clone();
    let task = TaskId::new("task-due-flag").unwrap();
    let worker: ModelRef = "claude-code/claude-sonnet-5-5".parse().unwrap();
    let mut held = RunState::new(run.clone());
    let mut session = SessionState::new(
        SessionKey::numbered(1),
        worker.clone(),
        "claude-code",
        RoleName::Worker,
        time::OffsetDateTime::now_utc(),
    );
    session.step = Some(StepName::Build);
    session.task = Some(task.clone());
    held.sessions.insert(session.key.clone(), session);
    let mut row = TaskRow::new(task.clone(), "Add the --due flag");
    row.worker = Some(worker);
    held.tasks.insert(task.clone(), row);
    let mut state = AppState::default();
    state.attached = Some(run.clone());
    state.runs.insert(run.clone(), held);
    (state, run, task)
}

/// `r` on a running agent asks once more, as `x` does, and the second `r` has the run start
/// the agent's task over with the worker it has.
#[test]
fn r_twice_starts_a_running_agents_task_over() {
    let theme = dark();
    let mut view = WorkflowView::new(examples::running());
    let (state, run, task) = building_due_flag(&view);
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let key = |view: &mut WorkflowView, code: KeyCode| {
        view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
    };
    let keys = |view: &mut WorkflowView| -> String {
        let area = Rect::new(0, 0, SCREEN, 12);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, &ctx);
        let row: String = (0..SCREEN).map(|x| buf[(x, 11)].symbol()).collect();
        row.trim().to_owned()
    };
    // Into the agents of the phase it opens on, where the first one is at work.
    key(&mut view, KeyCode::Enter);
    assert!(matches!(
        key(&mut view, KeyCode::Char('r')),
        super::ViewOutcome::Consumed
    ));
    assert_eq!(
        keys(&mut view),
        "r again to restart build:due-flag · esc to keep"
    );
    // Esc keeps the agent, and the keys are Claude Code's again.
    assert!(matches!(
        key(&mut view, KeyCode::Esc),
        super::ViewOutcome::Consumed
    ));
    assert!(keys(&mut view).starts_with("↑↓  select · ⏎ open"));

    key(&mut view, KeyCode::Char('r'));
    let outcome = key(&mut view, KeyCode::Char('r'));
    let super::ViewOutcome::Send(UiCommand::Reassign {
        run: asked,
        task: started_over,
        to,
    }) = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!((asked, started_over), (run, task));
    assert_eq!(to.to_string(), "claude-code/claude-sonnet-5-5");

    // The agent beside it has finished, so there's nothing of it to restart.
    key(&mut view, KeyCode::Down);
    let outcome = key(&mut view, KeyCode::Char('r'));
    let super::ViewOutcome::Notice(said) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(
        said,
        "build:due-tests isn't running, so there's nothing to restart"
    );
}

/// An agent a script's own step called has no task to give its worker again. The second
/// `r` has the run stop it and ask it again, and the script waits for the new answer. A
/// session a step of the run's own started without a task, as a planner is, still says why
/// it can't start over.
#[test]
fn r_twice_starts_an_agent_a_script_called_over() {
    let theme = dark();
    let (mut state, run, _) = building_due_flag(&WorkflowView::new(examples::running()));
    let called = SessionKey::numbered(1);
    let session = |state: &mut AppState, change: &dyn Fn(&mut SessionState)| {
        let held = state.runs.get_mut(&run).expect("the run");
        change(held.sessions.get_mut(&called).expect("the session"));
    };
    session(&mut state, &|session| {
        session.step = None;
        session.task = None;
        session.workflow_step = Some(argonauts_core::workflow::StepRef {
            workflow: argonauts_core::names::WorkflowId::numbered(1),
            phase: 0,
            path: vec![0],
        });
    });
    let twice = |state: &AppState| {
        let ctx = ViewCtx {
            theme: &theme,
            state,
            now: Instant::now(),
        };
        let mut view = WorkflowView::new(examples::running());
        let mut key =
            |code: KeyCode| view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx);
        key(KeyCode::Enter);
        key(KeyCode::Char('r'));
        key(KeyCode::Char('r'))
    };
    let outcome = twice(&state);
    let super::ViewOutcome::Send(UiCommand::RestartAgent {
        run: asked,
        session: started_over,
    }) = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!((asked, started_over), (run.clone(), called.clone()));

    session(&mut state, &|session| {
        session.step = Some(StepName::PlanToTasks)
    });
    let outcome = twice(&state);
    let super::ViewOutcome::Notice(said) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(said, "build:due-flag has no task of its own to start again");
}

#[test]
fn a_merged_run_reads_as_its_picture() {
    let picture = picture!("workflow_done");
    let mut view = WorkflowView::new(examples::done(Merge::Clean));
    picture.assert_drawn(0..12, &drawn(&mut view, 12), 0);
}

/// A folder without git: code wrote the work into your folder and no model wrote a commit
/// message, so the Merge phase has no agent and isn't listed. The view opens on the last phase
/// that is, Sign-off.
#[test]
fn a_run_in_a_folder_without_git_reads_as_its_picture() {
    let picture = picture!("workflow_done_no_git");
    let mut view = WorkflowView::new(examples::done(Merge::NoGit));
    picture.assert_drawn(0..12, &drawn(&mut view, 12), 0);
}

/// The picture of that run's questions and closing rows draws the same box.
#[test]
fn the_box_of_a_folder_without_git_reads_as_its_picture() {
    let wanted = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/no_git_fixes.txt"
    ));
    let rows: Vec<&str> = wanted.lines().collect();
    let top = rows
        .iter()
        .position(|row| row.starts_with(" ╭ Phases "))
        .expect("the run's box");
    let picture = picture!("workflow_done_no_git");
    let box_rows: Vec<String> = rows[top..top + 6]
        .iter()
        .map(|row| row.trim_end().to_owned())
        .collect();
    assert_eq!(box_rows, picture.text(5..11));
    let mut view = WorkflowView::new(examples::done(Merge::NoGit));
    picture.assert_drawn(5..11, &drawn(&mut view, ABOVE_BOX + 7), ABOVE_BOX);
}

#[test]
fn the_merge_phase_reads_as_its_pictures_in_each_case() {
    let picture = picture!("merge_cases");
    for (merge, rows) in [
        (Merge::Clean, 1..7),
        (Merge::Conflict, 9..16),
        (Merge::BranchMoved, 18..26),
    ] {
        let mut view = WorkflowView::new(examples::done(merge));
        let frame = ABOVE_BOX + rows.len() as u16 + 1;
        picture.assert_drawn(rows, &drawn(&mut view, frame), ABOVE_BOX);
    }
}

/// Enter on the selected agent, and its answer arriving.
/// A finding is blocking or a note now, in the colours `high` and `low` had, and its column is
/// as wide as `blocking` in every section.
fn blocking_and_notes(picture: &mut Picture, high: usize, low: usize) {
    picture.reword(high, "high  ", "blocking  ");
    picture.reword(low, "low   ", "note      ");
}

fn open(view: &mut WorkflowView, agent: AgentData) {
    press(view, KeyCode::Enter);
    assert!(view.accept(&ViewData::Agent {
        view: Box::new(agent),
    }));
}

#[test]
fn a_worker_sent_back_opens_as_its_picture() {
    let mut picture = picture!("workflow_agent_worker");
    blocking_and_notes(&mut picture, 8, 9);
    // Claude Code leaves a blank line between an open agent's sections.
    let spaced = picture.space_sections(0, &["Findings to fix", "Activity", "Outcome"]);
    let mut view = WorkflowView::new(examples::running());
    press(&mut view, KeyCode::Enter);
    open(&mut view, examples::worker_opened());
    let frame = ABOVE_BOX + 18 + spaced as u16;
    picture.assert_drawn(0..18 + spaced, &drawn(&mut view, frame), ABOVE_BOX);
}

#[test]
fn the_reviewer_that_sent_it_back_opens_as_its_picture() {
    let mut picture = picture!("workflow_agent_reviewer");
    blocking_and_notes(&mut picture, 12, 13);
    // The outcome counts what blocks and what is a note, and reads the verdict in its colour.
    picture.reword(11, "2 findings", "1 blocking · 1 note");
    let verdict = picture.text(11..12)[0]
        .chars()
        .collect::<Vec<_>>()
        .windows("sent it back".chars().count())
        .position(|window| window.iter().copied().eq("sent it back".chars()))
        .expect("the verdict");
    let warn = dark().style(crate::theme::Role::Warn).fg;
    for column in verdict..verdict + "sent it back".len() {
        picture.recolour(11, column, warn);
    }
    // A blank line between the sections, and no stop or restart once it's done.
    let spaced = picture.space_sections(0, &["Activity", "Outcome"]);
    picture.reword(
        15 + spaced,
        "m message · x stop · r restart · p pause",
        "m message · p pause",
    );
    let mut view = WorkflowView::new(examples::running());
    press(&mut view, KeyCode::Down);
    press(&mut view, KeyCode::Enter);
    press(&mut view, KeyCode::Down);
    open(&mut view, examples::reviewer_opened());
    let frame = ABOVE_BOX + 16 + spaced as u16;
    picture.assert_drawn(0..16 + spaced, &drawn(&mut view, frame), ABOVE_BOX);
}

#[test]
fn the_header_says_how_a_run_ended_in_its_colour() {
    let mut data = examples::done(Merge::Clean);
    data.state = WorkflowState::Failed;
    data.status_lines =
        vec![StatusLine::new(StatusKind::Ended, "Failed · the final check broke").toned(Tone::Bad)];
    let mut view = WorkflowView::new(data);
    let buf = drawn(&mut view, 12);
    let row = |y: u16| -> String { (0..SCREEN).map(|x| buf[(x, y)].symbol()).collect() };
    assert!(row(2).trim_end().ends_with("6/6 agents · 6m20s · failed"));
    assert!(row(3).starts_with(" ✘ Failed · the final check broke"));
    let theme = dark();
    let danger = theme.style(crate::theme::Role::Danger).fg.unwrap();
    assert_eq!(buf[(1, 3)].fg, danger);
    assert_eq!(buf[(3, 3)].fg, danger);
}

/// A run kept on its branch, not merged, ends on a warning, as the conversation says it.
#[test]
fn a_run_kept_on_its_branch_ends_on_a_warning() {
    let mut data = examples::done(Merge::Clean);
    data.status_lines = vec![
        StatusLine::new(
            StatusKind::Ended,
            "Kept on its branch, not merged · 4 files +105 \u{2212}4",
        )
        .toned(Tone::Warn),
    ];
    let mut view = WorkflowView::new(data);
    let buf = drawn(&mut view, 12);
    let row = |y: u16| -> String { (0..SCREEN).map(|x| buf[(x, y)].symbol()).collect() };
    assert!(
        row(3).starts_with(" ◆ Kept on its branch, not merged · 4 files +105 \u{2212}4"),
        "{}",
        row(3)
    );
    let warn = dark().style(crate::theme::Role::Warn).fg.unwrap();
    assert_eq!(buf[(1, 3)].fg, warn);
    assert_eq!(buf[(3, 3)].fg, warn);
}

/// A run that Claude Code's own workflow runs, as the app builds it from such a run's events:
/// it draws in the one view, with its phases on the left and the open phase's agents on the
/// right, and the reviewer it asked says what it decided.
#[test]
fn claude_codes_own_workflow_draws_in_the_same_view() {
    let data = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/own_workflow.view.json"
    )))
    .expect("the run's view");
    let mut view = WorkflowView::new(data);
    let buf = drawn(&mut view, 12);
    let rows: Vec<String> = (0..12)
        .map(|y| (0..SCREEN).map(|x| buf[(x, y)].symbol()).collect())
        .collect();
    assert_eq!(rows[1].trim_end(), " Add due dates", "{rows:#?}");
    // The description names the mode and the workflow that really runs.
    assert!(
        rows[2].starts_with(" Captain · Claude Code's workflow add-due-dates · "),
        "{rows:#?}"
    );
    assert!(rows[2].contains("2/3 agents · "), "{rows:#?}");
    let top = rows
        .iter()
        .position(|row| row.contains("╭ Phases ─"))
        .unwrap_or_else(|| panic!("{rows:#?}"));
    assert!(rows[top].contains("┬ Review · 2 agents ─"), "{rows:#?}");
    assert!(rows[top + 1].contains("✔ Build"), "{rows:#?}");
    let reviewer = rows
        .iter()
        .find(|row| row.contains("review:due-dates"))
        .unwrap_or_else(|| panic!("{rows:#?}"));
    // Inside Claude Code's own workflow, a model of another CLI says which.
    assert!(reviewer.contains("GPT 5.6 Luna on Codex · "), "{rows:#?}");
    assert!(reviewer.contains(" · sent it back"), "{rows:#?}");
    let keys = rows.last().unwrap().trim_end();
    assert!(
        keys.starts_with(" ↑↓  select · enter opens its agents"),
        "{keys}"
    );
}

/// Claude Code's own workflow view in this frame, its box, divider and pane titles in the bright
/// text colour. The picture leaves out the box's empty rows, so its bottom edge and the keys
/// under it are checked where the screen draws them.
#[test]
fn the_frame_is_bright_as_claude_codes_is() {
    const ROWS: u16 = 14;
    let mut picture = picture!("workflow_frame_after");
    // The picture's rows are Claude Code's, with one space after the longest label and no
    // times. The screen's rows keep their own two spaces and each agent's time, here none.
    let dim = dark().style(crate::theme::Role::Dim).fg;
    for row in 5..8 {
        picture.reword(row, "Opus 5.5", " Opus 5.5");
        let edge = picture.text(row..row + 1)[0].chars().count() - 1;
        picture.write(row, edge - 3, "0s", dim);
    }
    let mut view = WorkflowView::new(examples::ui_kit());
    // It opens on Look, which runs; Build opened, with the keys in its agents.
    press(&mut view, KeyCode::Up);
    press(&mut view, KeyCode::Up);
    press(&mut view, KeyCode::Enter);
    let buf = drawn(&mut view, ROWS);
    picture.assert_drawn(0..9, &buf, 0);
    picture.assert_drawn(9..11, &buf, ROWS - 2);
}
