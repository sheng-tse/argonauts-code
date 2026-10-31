//! The workflow view beside its pictures: each test builds the picture's own run, draws it at
//! the picture's width, and compares every cell, its colour included.
//!
//! The pictures are 118 cells wide. The frame stops six cells short of the screen's right
//! edge, as Claude Code's does, so they are drawn on a screen of 124.

use std::time::Instant;

use argonauts_core::app::views::{
    AgentView as AgentData, StatusKind, StatusLine, Tone, WorkflowState,
};
use argonauts_core::app::{UiCommand, ViewData};
use argonauts_core::base::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::workflows::steps::StepName;
use argonauts_core::{RunId, TaskId};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::super::WorkflowView;
use super::super::examples::{self as examples, Merge};
use crate::frame::state::{AppState, RunState, SessionState, TaskRow};
use crate::frame::view::{View, ViewCtx};
use crate::testing::pictures::workflow_pictures::{Picture, dark, picture};

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

/// `view` drawn `width` cells wide and `rows` tall, on the screen `state` holds.
fn drawn_in(view: &mut WorkflowView, width: u16, rows: u16, state: &AppState) -> Buffer {
    let theme = dark();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    let area = Rect::new(0, 0, width, rows);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    buf
}

/// A screen whose names know the models OpenCode's own list names.
fn with_opencode_names() -> AppState {
    let mut state = AppState::default();
    examples::opencode_names(&mut state.names);
    state
}

/// Each logo before a model in `rows`, where the picture draws its image.
fn logos(picture: &mut Picture, rows: &[(usize, &str, &str)]) {
    for (row, model, id) in rows {
        let worker = examples::worker(id);
        let logo = argonauts_core::look::logos::logo_for(&worker.agent, worker.model.as_deref());
        picture.logo(*row, model, &logo);
    }
}

/// Where the view and its pictures part on purpose: it keeps Claude Code's bright rule and
/// frame, its name bold, its description dim under the name, and its keys dim, as every docked view draws
/// them.
fn as_the_view_draws_its_frame(picture: &mut Picture, description: &str, keys: Option<usize>) {
    let dim = dark().style(crate::base::theme::Role::Dim).fg;
    picture.bright_frames();
    let rule = picture.text(0..1)[0].clone();
    picture.restyle(0, &rule, None, false);
    let name = picture.text(1..2)[0].trim().to_owned();
    picture.restyle(1, &name, None, true);
    picture.restyle(2, description, dim, false);
    if let Some(row) = keys {
        picture.restyle(row, "↑↓  select · ", dim, false);
    }
}

/// Every state in its own mark, and each row its mark, its label, its CLI's logo with the
/// model, then its tokens and its time at the right. The keys under the agents offer the peek.
#[test]
fn every_mark_reads_as_its_picture() {
    let mut picture = picture!("workflow_marks");
    as_the_view_draws_its_frame(
        &mut picture,
        "orchestrator workflow · add --due to the todo CLI, each part reviewed on another CLI",
        Some(14),
    );
    logos(
        &mut picture,
        &[
            (6, "Opus 5.5", examples::OPUS),
            (7, "5.6 Luna", examples::LUNA),
            (8, "Sonnet 5.5", examples::SONNET),
            (9, "Qwen3 Coder 30B (64k)", examples::QWEN),
            (10, "Nemotron 3.5 Super VL Free", examples::NEMOTRON),
        ],
    );
    let state = with_opencode_names();
    let mut view = WorkflowView::new(examples::every_mark());
    // It opens on Review, where the work is; Enter takes the keys to its agents, and the second
    // reviewer is picked.
    for code in [KeyCode::Enter, KeyCode::Down] {
        press(&mut view, code);
    }
    picture.assert_drawn(0..15, &drawn_in(&mut view, 120, 15, &state), 0);
}

/// `width` cells of `buf` from column `x`, `rows` of them from row `y`, as a buffer of their own.
fn region(buf: &Buffer, (x, y): (u16, u16), (width, rows): (u16, u16)) -> Buffer {
    let mut part = Buffer::empty(Rect::new(0, 0, width, rows));
    for row in 0..rows {
        for column in 0..width {
            part[(column, row)] = buf[(x + column, y + row)].clone();
        }
    }
    part
}

/// Space on a reviewer that sent the work back peeks at what its row can't show: its verdict and
/// the findings it gave, in a box under the row that says how to close it and open the agent.
#[test]
fn a_peek_reads_as_its_picture() {
    let mut picture = picture!("workflow_peek");
    logos(
        &mut picture,
        &[
            (0, "Opus 5.5", examples::OPUS),
            (1, "5.6 Luna", examples::LUNA),
            (7, "Sonnet 5.5", examples::SONNET),
        ],
    );
    let state = with_opencode_names();
    let mut view = WorkflowView::new(examples::every_mark());
    for code in [KeyCode::Enter, KeyCode::Down] {
        press(&mut view, code);
    }
    let theme = dark();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let asked = view.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), &ctx);
    let crate::frame::view::ViewOutcome::Request(argonauts_core::app::ViewRequest::Agent {
        session,
        ..
    }) = asked
    else {
        panic!("{asked:?}");
    };
    assert_eq!(session.as_str(), "s-004");
    let mut reviewer = examples::reviewer_opened();
    if let Some(review) = reviewer.review.as_mut() {
        review.findings[0].text = "month 13 passes as a date".to_owned();
        review.findings[1].text = "--due has no test".to_owned();
    }
    assert!(view.accept(&ViewData::Agent {
        view: Box::new(reviewer),
    }));
    // The pane's rows start at the pointer's column, 22 cells in.
    let buf = drawn_in(&mut view, 120, 20, &state);
    picture.assert_drawn(0..8, &region(&buf, (22, 6), (89, 8)), 0);
    // Space again closes it, and the next row is back under the reviewer.
    view.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), &ctx);
    let buf = drawn_in(&mut view, 120, 20, &state);
    picture.assert_drawn(7..8, &region(&buf, (22, 8), (89, 1)), 0);
}

/// Enter on a run's row while one of its agents asks you something: the view opens on that
/// agent, and the question comes up under it with Carry on without me its last row.
#[test]
fn a_question_from_an_agent_opens_beside_it_as_its_picture() {
    use crate::base::draw::render::{Clock, RenderCtx};
    use crate::bottom::BottomState;
    use crate::frame::state::OpenApproval;

    let mut picture = picture!("workflow_question_beside");
    // The picture's box is a cell wider than its rule leaves room for, and its totals are dim
    // where the view draws them in the text colour.
    picture.narrow_box(5..11);
    as_the_view_draws_its_frame(
        &mut picture,
        "orchestrator workflow · totals round as the ledger does, each part reviewed on another CLI",
        None,
    );
    picture.restyle(2, "0/2 agents · 3m05s", None, false);
    for title in ["Phases", "Build · 2 agents"] {
        picture.restyle(5, title, None, false);
    }
    // The view lays each phase out by its own labels, where the picture gives Build a column
    // three cells wider.
    picture.reword(6, "build:fix-money        ", "build:fix-money     ");
    picture.reword(7, "build:ledger-tests     ", "build:ledger-tests  ");
    logos(
        &mut picture,
        &[
            (6, "Haiku 5.5", examples::HAIKU),
            (7, "Sonnet 5.5", examples::SONNET),
        ],
    );
    let opened = examples::fix_money_question();
    let mut state = AppState::default();
    let mut run = RunState::new(examples::run());
    // The run knows the task it asks about, which names the agent `build:fix-money`.
    let task = TaskId::new("fix-money").expect("a task id");
    let mut asking = TaskRow::new(task.clone(), "Fix the money rounding");
    asking.item = Some("fix-money".to_owned());
    run.tasks.insert(task, asking);
    run.approvals.push(OpenApproval {
        opened: Box::new(opened.clone()),
        set_aside: false,
        replayed: false,
    });
    state.runs.insert(examples::run(), run);
    let mut view = WorkflowView::new(examples::fix_money());
    // Over a question, the view gives its keys' row to the question's rule.
    let buf = drawn_in(&mut view, 120, 12, &state);
    picture.assert_drawn(0..11, &buf, 0);

    let theme = dark();
    let now = Instant::now();
    let mut question = crate::questions::approval_question_local(
        1,
        &opened,
        state.runs.get(&examples::run()),
        time::UtcOffset::UTC,
        &state.names,
    );
    question.focus();
    let mut bottom = BottomState::new();
    bottom.question = Some(question);
    let mut rows = crate::frame::view::dock::lines(&buf);
    rows.pop();
    let ctx = RenderCtx::new(120, &theme, Clock::still(now)).with_rows(30);
    let drawn = bottom.render_docked(&ctx, 28, false, rows, true);
    let drawn = crate::testing::pictures::workflow_pictures::buffer_of(&drawn.lines, 120);
    // The question's rule and its title are in the question's colour, the title bold, as
    // Claude Code's prompts draw them.
    let asks = dark().style(crate::base::theme::Role::Question).fg;
    let rule = picture.text(11..12)[0].clone();
    picture.restyle(11, &rule, asks, false);
    picture.restyle(12, "Question from build:fix-money", asks, true);
    // Its numbers are dim, and the agent it names is in the text colour.
    let dim = dark().style(crate::base::theme::Role::Dim).fg;
    picture.restyle(13, "build:fix-money", None, false);
    for (row, number) in [(16, "1."), (17, "2."), (18, "3.")] {
        picture.restyle(row, number, dim, false);
    }
    picture.assert_drawn(11..22, &drawn, 11);
}

/// Enter on a run's row while a CLI it uses is down: the view opens on the first agent the CLI
/// holds, and the question about the CLI comes up under it, its options in the run's order and
/// Carry on without me the last.
#[test]
fn a_cli_that_is_down_opens_beside_the_agents_it_holds_as_its_picture() {
    use crate::base::draw::render::{Clock, RenderCtx};
    use crate::bottom::BottomState;
    use crate::frame::state::OpenApproval;

    let mut picture = picture!("workflow_cli_down_beside");
    picture.narrow_box(5..10);
    as_the_view_draws_its_frame(
        &mut picture,
        "orchestrator workflow · cache the rates for an hour, each part reviewed on another CLI",
        None,
    );
    picture.restyle(2, "0/3 agents · 1m02s", None, false);
    for title in ["Phases", "Build · 3 agents"] {
        picture.restyle(5, title, None, false);
    }
    // As in the question an agent asks, the view lays Build out by its own labels.
    picture.reword(6, "build:rate-cache          ", "build:rate-cache       ");
    picture.reword(7, "build:cache-expiry        ", "build:cache-expiry     ");
    picture.reword(8, "build:cache-tests         ", "build:cache-tests      ");
    logos(
        &mut picture,
        &[
            (6, "Opus 5.5", examples::OPUS),
            (7, "Sonnet 5.5", examples::SONNET),
            (8, "5.6 Luna", examples::LUNA),
        ],
    );
    let opened = examples::cache_rates_down();
    let mut state = AppState::default();
    let mut run = RunState::new(examples::run());
    run.approvals.push(OpenApproval {
        opened: Box::new(opened.clone()),
        set_aside: false,
        replayed: false,
    });
    state.runs.insert(examples::run(), run);
    let mut view = WorkflowView::new(examples::cache_rates());
    let buf = drawn_in(&mut view, 120, 11, &state);
    picture.assert_drawn(0..10, &buf, 0);

    let theme = dark();
    let now = Instant::now();
    let mut question = crate::questions::approval_question_local(
        1,
        &opened,
        state.runs.get(&examples::run()),
        time::UtcOffset::UTC,
        &state.names,
    );
    question.focus();
    let mut bottom = BottomState::new();
    bottom.question = Some(question);
    let mut rows = crate::frame::view::dock::lines(&buf);
    rows.pop();
    let ctx = RenderCtx::new(120, &theme, Clock::still(now)).with_rows(30);
    let drawn = bottom.render_docked(&ctx, 28, false, rows, true);
    let drawn = crate::testing::pictures::workflow_pictures::buffer_of(&drawn.lines, 120);
    // The question keeps Claude Code's prompt colours, as the agent's question does: its rule
    // and bold title in the question's colour and its numbers dim. What is paused is dim under
    // what went wrong, as when the question stands alone.
    let asks = dark().style(crate::base::theme::Role::Question).fg;
    let dim = dark().style(crate::base::theme::Role::Dim).fg;
    let rule = picture.text(10..11)[0].clone();
    picture.restyle(10, &rule, asks, false);
    picture.restyle(11, "Claude Code is down", asks, true);
    picture.restyle(
        13,
        "Two tasks wait for it, their work so far kept.",
        dim,
        false,
    );
    for (row, number) in [(15, "1."), (16, "2."), (18, "3."), (20, "4.")] {
        picture.restyle(row, number, dim, false);
    }
    picture.assert_drawn(10..23, &drawn, 10);
    // The picture leaves out the keys the agent's question shows under its options; they are
    // there all the same.
    let keys: String = (0..120).map(|x| drawn[(x, 23)].symbol()).collect();
    assert_eq!(
        keys.trim_end(),
        "  Enter to choose · Tab to add a note · Esc back to the list · shift+tab to cycle modes"
    );
}

/// An agent held on another CLI isn't what a CLI that is down asks about, so the view passes it
/// over for the first agent the down CLI holds.
#[test]
fn a_cli_that_is_down_opens_on_an_agent_it_holds() {
    use crate::frame::state::OpenApproval;

    let mut state = AppState::default();
    let mut run = RunState::new(examples::run());
    run.approvals.push(OpenApproval {
        opened: Box::new(examples::cache_rates_down()),
        set_aside: false,
        replayed: false,
    });
    state.runs.insert(examples::run(), run);
    let mut data = examples::cache_rates();
    if let Some(argonauts_core::app::views::WorkflowRow::Agent(first)) =
        data.phases[0].rows.first_mut()
    {
        first.worker = examples::worker(examples::LUNA);
    }
    let mut view = WorkflowView::new(data);
    let buf = drawn_in(&mut view, 120, 11, &state);
    let pointed: Vec<String> = (6..9)
        .map(|y| (0..120).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .filter(|row| row.contains('❯'))
        .collect();
    assert_eq!(pointed.len(), 1, "{pointed:?}");
    assert!(pointed[0].contains("❯‖ build:cache-expiry"), "{pointed:?}");
}

/// An agent's question opens the view on the agent whose task asked it, past an agent of an
/// earlier phase that ran the same model and whose task the screen doesn't know.
#[test]
fn a_question_opens_on_the_agent_whose_task_asked_it() {
    use crate::frame::state::OpenApproval;
    use argonauts_core::app::views::{RowStatus, WorkflowRow};

    let mut run = RunState::new(examples::run());
    let mut asker = SessionState::new(
        SessionKey::numbered(1),
        examples::worker(examples::HAIKU),
        "claude-code",
        RoleName::Worker,
        time::OffsetDateTime::now_utc(),
    );
    asker.task = Some(TaskId::new("fix-money").expect("a task id"));
    run.sessions.insert(asker.key.clone(), asker);
    run.approvals.push(OpenApproval {
        opened: Box::new(examples::fix_money_question()),
        set_aside: false,
        replayed: false,
    });
    let mut state = AppState::default();
    state.runs.insert(examples::run(), run);
    let mut data = examples::fix_money();
    let mut understand = data.phases[0].clone();
    understand.name = "Understand".to_owned();
    understand.status = RowStatus::Done;
    understand.count = None;
    let reader = examples::agent(
        9,
        "read:ledger",
        examples::HAIKU,
        RowStatus::Done,
        4_000,
        20,
    );
    understand.rows = vec![WorkflowRow::Agent(reader)];
    data.phases.insert(0, understand);
    let mut view = WorkflowView::new(data);
    let buf = drawn_in(&mut view, 120, 12, &state);
    let pointed: Vec<String> = (6..10)
        .map(|y| (0..120).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .filter(|row| row.contains('❯'))
        .collect();
    assert_eq!(pointed.len(), 1, "{pointed:?}");
    assert!(pointed[0].contains("❯◆ build:fix-money"), "{pointed:?}");
}

#[test]
fn a_run_mid_review_reads_as_its_picture() {
    let picture = picture!("workflow_running");
    let mut view = WorkflowView::new(examples::running());
    // It opens on Build, where the worker is on its second round.
    press(&mut view, KeyCode::Down);
    picture.assert_drawn(0..12, &drawn(&mut view, 12), 0);
}

#[test]
fn a_long_list_scrolls_with_claude_codes_mark() {
    // Claude Code draws an arrow on the list's mark only where rows are hidden that way, so
    // no row hides below the sixth of six.
    let picture = picture!("workflow_big");
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
    let picture = picture!("workflow_fallback");
    let mut view = WorkflowView::new(examples::fallback());
    press(&mut view, KeyCode::Down);
    picture.assert_drawn(0..12, &drawn(&mut view, 12), 0);
}

#[test]
fn a_run_paused_on_a_cli_that_is_down_reads_as_its_picture() {
    let picture = picture!("workflow_cli_down");
    let mut view = WorkflowView::new(examples::cli_down());
    // The question about the CLI takes the keys' row and the rows under it.
    picture.assert_drawn(0..11, &drawn(&mut view, 12), 0);
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
        crate::frame::view::ViewOutcome::Consumed
    ));
    assert_eq!(
        keys(&mut view),
        "r again to restart build:due-flag · esc to keep"
    );
    // Esc keeps the agent, and the keys are Claude Code's again.
    assert!(matches!(
        key(&mut view, KeyCode::Esc),
        crate::frame::view::ViewOutcome::Consumed
    ));
    assert!(keys(&mut view).starts_with("↑↓  select · space peek · enter open"));

    key(&mut view, KeyCode::Char('r'));
    let outcome = key(&mut view, KeyCode::Char('r'));
    let crate::frame::view::ViewOutcome::Send(UiCommand::Reassign {
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
    let crate::frame::view::ViewOutcome::Notice(said) = outcome else {
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
        session.workflow_step = Some(argonauts_core::workflows::workflow::StepRef {
            workflow: argonauts_core::base::names::WorkflowId::numbered(1),
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
    let crate::frame::view::ViewOutcome::Send(UiCommand::RestartAgent {
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
    let crate::frame::view::ViewOutcome::Notice(said) = outcome else {
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

/// A run that merged at once didn't wait for its commit message: the writer's row skipped and
/// dim, why in its model's place with no numbers or time, and neither the Merge phase nor its
/// pane counting it.
#[test]
fn a_commit_message_not_needed_reads_as_the_clean_merge_with_its_writer_skipped() {
    let picture = picture!("skipped_merge_writer");
    let mut view = WorkflowView::new(examples::done(Merge::AtOnce));
    picture.assert_drawn(1..7, &drawn(&mut view, ABOVE_BOX + 7), ABOVE_BOX);
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
    // Claude Code leaves a blank line between an open agent's sections.
    let picture = picture!("workflow_agent_worker");
    let mut view = WorkflowView::new(examples::running());
    press(&mut view, KeyCode::Enter);
    open(&mut view, examples::worker_opened());
    picture.assert_drawn(0..21, &drawn(&mut view, ABOVE_BOX + 21), ABOVE_BOX);
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
    let warn = dark().style(crate::base::theme::Role::Warn).fg;
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

/// How a run ended is its status line's mark, in that state's colour, before words in the text
/// colour.
#[test]
fn the_header_says_how_a_run_ended_in_its_mark() {
    let mut data = examples::done(Merge::Clean);
    data.state = WorkflowState::Failed;
    data.status_lines =
        vec![StatusLine::new(StatusKind::Ended, "Failed · the final check broke").toned(Tone::Bad)];
    let mut view = WorkflowView::new(data);
    let buf = drawn(&mut view, 12);
    let row = |y: u16| -> String { (0..SCREEN).map(|x| buf[(x, y)].symbol()).collect() };
    assert!(row(2).trim_end().ends_with("6/6 agents · 6m20s · failed"));
    assert!(row(3).starts_with(" × Failed · the final check broke"));
    let theme = dark();
    let danger = theme.style(crate::base::theme::Role::Danger).fg.unwrap();
    assert_eq!(buf[(1, 3)].fg, danger);
    assert_eq!(buf[(3, 3)].fg, ratatui::style::Color::Reset);
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
    let warn = dark().style(crate::base::theme::Role::Warn).fg.unwrap();
    assert_eq!(buf[(1, 3)].fg, warn);
    assert_eq!(buf[(3, 3)].fg, ratatui::style::Color::Reset);
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
    // Inside Claude Code's own workflow, a model of another CLI says which, and the reviewer
    // that sent the change back is marked so.
    assert!(reviewer.contains(" 5.6 Luna on Codex "), "{rows:#?}");
    assert!(reviewer.contains("« review:due-dates"), "{rows:#?}");
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
    let picture = picture!("workflow_frame_after");
    let mut view = WorkflowView::new(examples::todo_tui());
    // It opens on Look, which runs; Build opened, with the keys in its agents.
    press(&mut view, KeyCode::Up);
    press(&mut view, KeyCode::Up);
    press(&mut view, KeyCode::Enter);
    let buf = drawn(&mut view, ROWS);
    picture.assert_drawn(0..9, &buf, 0);
    picture.assert_drawn(9..11, &buf, ROWS - 2);
}
