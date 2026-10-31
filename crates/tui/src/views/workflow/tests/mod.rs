mod captain;
mod golden;
mod inner;
pub(crate) mod nested;
mod row;
mod view;

use argonauts_core::Spend;
use argonauts_core::app::views::{
    AgentRow, AgentView as AgentData, NestedWorkflowRow, PhaseCount, RowNote, StatusKind, Tone,
};
use argonauts_core::settings::ui::CliInternals;
use argonauts_core::workflows::workflow::StandardPhase;
use time::OffsetDateTime;

use super::*;
use crate::base::stability::{assert_frames, buffer_text};
use crate::base::theme::Glyphs;
use crate::frame::state::{AppState, RunState};

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn run() -> RunId {
    RunId::new("run-01").unwrap()
}

fn agent(session: &str, label: &str, status: RowStatus) -> AgentRow {
    AgentRow {
        session: SessionKey::parse(session).unwrap(),
        label: label.into(),
        worker: "claude-code/opus".parse().unwrap(),
        status,
        tokens: 381_000,
        spent: Spend::default(),
        elapsed_ms: 362_000,
        subagents: 0,
        context: None,
        fallback_for: None,
        answered: false,
        tries: Vec::new(),
        note: None,
        inner: None,
        left_out: false,
        not_needed: false,
        fallback_why: None,
    }
}

fn phase(name: &str, status: RowStatus, rows: Vec<WorkflowRow>) -> PhaseRow {
    let done = rows
        .iter()
        .filter(|row| row_status(row) == RowStatus::Done)
        .count() as u32;
    PhaseRow {
        name: name.into(),
        standard: StandardPhase::Do,
        status,
        detail: String::new(),
        try_number: None,
        count: PhaseCount::of(done, rows.len() as u32),
        rows,
        note: None,
    }
}

fn workflow(id: u32, name: &str, rows: Vec<WorkflowRow>) -> WorkflowData {
    WorkflowData {
        run: run(),
        id: WorkflowId::numbered(id),
        name: name.into(),
        description: "Builds the kit".into(),
        agents_done: 1,
        agents_total: 3,
        elapsed_ms: 60_000,
        spent: Spend::default(),
        phases: vec![phase("Build", RowStatus::Running, rows)],
        style: None,
        status_lines: Vec::new(),
        notes: Vec::new(),
        required: Vec::new(),
        goals: Vec::new(),
        worker: None,
        state: Default::default(),
    }
}

fn parent() -> WorkflowData {
    workflow(
        1,
        "ui-kit",
        vec![
            WorkflowRow::Agent(agent("s-001", "build:a", RowStatus::Done)),
            WorkflowRow::Agent(agent("s-002", "build:b", RowStatus::Running)),
            WorkflowRow::Workflow(NestedWorkflowRow {
                id: WorkflowId::numbered(2),
                name: "build:c".into(),
                status: RowStatus::Running,
                agents_done: 0,
                agents_total: 2,
                workers: vec!["codex/gpt-5.6".parse().unwrap()],
                elapsed_ms: 720_000,
                parent: None,
                worker: None,
                phases: Vec::new(),
                rows: vec![WorkflowRow::Agent(agent(
                    "s-004",
                    "test:a",
                    RowStatus::Running,
                ))],
            }),
        ],
    )
}

/// Understand done, Fix running, Review and Merge still to come.
fn fixing() -> WorkflowData {
    let mut fix_slugify = agent("s-006", "fix:slugify", RowStatus::Done);
    fix_slugify.worker = "claude-code/claude-haiku-4-5".parse().unwrap();
    fix_slugify.tokens = 18_100;
    fix_slugify.elapsed_ms = 130_000;
    let mut fix_median = agent("s-007", "fix:median", RowStatus::Running);
    fix_median.worker = "codex/gpt-5.6-luna".parse().unwrap();
    fix_median.tokens = 31_200;
    fix_median.elapsed_ms = 151_000;
    let read = |n: usize| {
        WorkflowRow::Agent(agent(
            &format!("s-00{n}"),
            &format!("read:{n}"),
            RowStatus::Done,
        ))
    };
    let mut data = workflow(1, "fix-slugify-median", Vec::new());
    data.description = "Fix slugify's dashes and median's even and empty lists, with tests".into();
    data.agents_done = 6;
    data.agents_total = 7;
    data.elapsed_ms = 271_000;
    data.phases = vec![
        phase("Understand", RowStatus::Done, (1..=5).map(read).collect()),
        phase(
            "Fix",
            RowStatus::Running,
            vec![
                WorkflowRow::Agent(fix_slugify),
                WorkflowRow::Agent(fix_median),
            ],
        ),
        phase("Review", RowStatus::Waiting, Vec::new()),
        phase("Merge", RowStatus::Waiting, Vec::new()),
    ];
    data
}

fn ctx_with<'a>(theme: &'a Theme, state: &'a AppState) -> ViewCtx<'a> {
    ViewCtx {
        theme,
        state,
        now: Instant::now(),
    }
}

fn press(view: &mut WorkflowView, code: KeyCode) -> ViewOutcome {
    let theme = theme();
    let state = AppState::default();
    view.handle_key(
        KeyEvent::new(code, KeyModifiers::NONE),
        &ctx_with(&theme, &state),
    )
}

fn press_in(view: &mut WorkflowView, code: KeyCode, state: &AppState) -> ViewOutcome {
    let theme = theme();
    view.handle_key(
        KeyEvent::new(code, KeyModifiers::NONE),
        &ctx_with(&theme, state),
    )
}

fn draw_with(view: &mut WorkflowView, width: u16, theme: &Theme, state: &AppState) -> String {
    let area = Rect::new(0, 0, width, 14);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx_with(theme, state));
    assert_frames(&buf);
    buffer_text(&buf)
}

fn screen(view: &mut WorkflowView) -> String {
    draw_with(view, 80, &theme(), &AppState::default())
}

fn row_with<'a>(text: &'a str, label: &str) -> &'a str {
    text.lines()
        .find(|line| line.contains(label))
        .unwrap_or_default()
}

/// A run on screen and going, so stopping and pausing act.
fn going() -> AppState {
    let mut state = AppState::default();
    state.attached = Some(run());
    state.runs.insert(run(), RunState::new(run()));
    state
}

/// A review Claude Code's workflow asked Astra for through Argonauts Code is Astra's row:
/// its tokens while Astra reads, and once answered its mark says what Astra decided.
#[test]
fn a_step_a_role_answered_is_marked_by_what_it_decided() {
    let mut store = agent("s-002", "review:store", RowStatus::Running);
    store.worker = "astra".parse().unwrap();
    store.tokens = 41_200;
    store.answered = true;
    let mut cli = agent("s-003", "review:cli", RowStatus::Done);
    cli.worker = "astra".parse().unwrap();
    cli.answered = true;
    cli.note = Some(RowNote::new("approved", Tone::Good));
    let mut data = workflow(1, "add-due-dates", Vec::new());
    data.phases = vec![phase(
        "Review",
        RowStatus::Running,
        vec![WorkflowRow::Agent(store), WorkflowRow::Agent(cli)],
    )];
    let mut view = WorkflowView::new(data);
    let text = draw_with(&mut view, 100, &theme(), &going());
    let store = row_with(&text, "review:store");
    assert!(
        store.contains(" astra ") && store.contains("41.2k tok"),
        "{text}"
    );
    assert!(store.contains("⣾ review:store"), "{text}");
    let cli = row_with(&text, "review:cli");
    assert!(
        cli.contains("✔ review:cli") && cli.contains("381.0k tok"),
        "{text}"
    );
    assert!(!cli.contains("approved"), "{text}");
}

/// The open view as Claude Code's: a rule six columns short, the name
/// and the description with its totals inside it, numbered phases with counts only once
/// started, `phase:target` rows with the model and tokens, and the time flush right. No
/// tokens or spend in the header: they are on the run's row in the bottom list.
#[test]
fn the_view_reads_like_claude_codes() {
    let mut view = WorkflowView::new(fixing());
    let text = draw_with(&mut view, 100, &theme(), &going());
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0].trim_end().chars().count(), 94, "{text}");
    assert!(lines[0].starts_with("──"), "{text}");
    assert_eq!(lines[1].trim_end(), " fix-slugify-median", "{text}");
    // The totals end under the box's right border.
    assert!(
        lines[2].trim_end().ends_with("6/7 agents · 4m31s"),
        "{text}"
    );
    assert_eq!(lines[2].trim_end().chars().count(), 93, "{text}");
    assert!(lines[3].trim().is_empty(), "{text}");
    assert!(
        lines[4].starts_with(" ╭ Phases ────────────┬ Fix · 2 agents ─"),
        "{text}"
    );
    assert!(lines[4].trim_end().ends_with('╮'), "{text}");
    assert_eq!(lines[4].trim_end().chars().count(), 93, "{text}");
    assert!(lines[5].starts_with(" │   ✔ Understand 5/5 │"), "{text}");
    assert!(lines[6].starts_with(" │ ❯ ⣾ Fix        1/2 │"), "{text}");
    assert!(lines[7].starts_with(" │   3 Review         │"), "{text}");
    assert!(lines[8].starts_with(" │   4 Merge          │"), "{text}");
    let slugify = row_with(&text, "fix:slugify");
    assert!(
        slugify.contains("│  ✔ fix:slugify  ●  Haiku 4.5 "),
        "{text}"
    );
    assert!(slugify.contains(" 18.1k tok "), "{text}");
    assert!(slugify.trim_end().ends_with("2m10s │"), "{text}");
    let median = row_with(&text, "fix:median");
    assert!(median.contains("⣾ fix:median   ●  GPT 5.6 Luna "), "{text}");
    assert!(median.contains(" 31.2k tok "), "{text}");
    assert!(!text.contains("0/0"), "{text}");
    assert!(!text.contains("task-"), "{text}");
    assert!(!text.contains('$'), "{text}");
    let footer = lines.last().unwrap_or(&"").trim_end();
    assert_eq!(
        footer,
        " ↑↓  select · enter opens its agents · x stop workflow · p pause · esc back · s save"
    );
}

#[test]
fn the_frame_holds_at_every_width() {
    for width in [80, 100, 120, 200] {
        let mut view = WorkflowView::new(fixing());
        let text = draw_with(&mut view, width, &theme(), &going());
        assert!(text.contains("fix:slugify"), "{width}: {text}");
        let mut kit = WorkflowView::new(kit());
        draw_with(&mut kit, width, &theme(), &going());
    }
}

/// Every row of a pane starts its model and ends its time in the same cells, with long and
/// CJK labels and a crew from three CLIs, at every width the snapshots use.
#[test]
fn rows_keep_their_columns_at_every_width() {
    let mut data = fixing();
    let mut cjk = agent("s-008", "修复:导出测试", RowStatus::Running);
    cjk.worker = "antigravity-cli/gemini-3.6-flash-low".parse().unwrap();
    cjk.tokens = 7_100;
    cjk.elapsed_ms = 58_000;
    data.phases[1].rows.push(WorkflowRow::Agent(cjk));
    let cell_of = |row: &str, needle: &str| {
        let at = row.find(needle)?;
        Some(crate::base::text::str_width(&row[..at]))
    };
    for width in [80, 100, 120, 200] {
        let text = draw_with(
            &mut WorkflowView::new(data.clone()),
            width,
            &theme(),
            &going(),
        );
        let rows = [
            ("fix:slugify", "Haiku"),
            ("fix:median", "GPT 5.6 Luna"),
            ("修复:导出测试", "Gemini 3.6 Flash"),
        ];
        let models: Vec<Option<usize>> = rows
            .iter()
            .map(|(label, model)| cell_of(row_with(&text, label), model))
            .collect();
        assert!(
            models.iter().all(|at| at.is_some() && *at == models[0]),
            "{width}: {text}"
        );
        // The time ends one cell before the right border.
        let ends: Vec<Option<usize>> = rows
            .iter()
            .map(|(label, _)| {
                let row = row_with(&text, label);
                let border = row.rfind(" │")?;
                let time = row[..border].trim_end_matches(' ');
                (time.len() == border && time.ends_with('s'))
                    .then(|| crate::base::text::str_width(time))
            })
            .collect();
        assert!(
            ends.iter().all(|end| end.is_some() && *end == ends[0]),
            "{width}: {text}"
        );
        assert!(!text.contains("0/0"), "{width}: {text}");
    }
}

/// A row names its model alone after its CLI's logo, and the opened agent names the CLI. A
/// narrow pane lets go of the tokens before the model.
#[test]
fn a_row_names_its_model_and_the_opened_agent_its_cli() {
    let wide = draw_with(&mut WorkflowView::new(fixing()), 100, &theme(), &going());
    let slugify = row_with(&wide, "fix:slugify");
    assert!(slugify.contains("fix:slugify  ●  Haiku 4.5 "), "{wide}");
    assert!(slugify.contains(" 18.1k tok "), "{wide}");
    assert!(!wide.contains("on Claude Code"), "{wide}");
    let narrow = draw_with(&mut WorkflowView::new(fixing()), 80, &theme(), &going());
    assert!(
        row_with(&narrow, "fix:median").contains("5.6 Luna "),
        "{narrow}"
    );
}

/// A phase still to come says `Not started yet`, as Claude Code's does, and nothing else.
#[test]
fn a_phase_still_to_come_has_not_started() {
    let mut view = WorkflowView::new(fixing());
    press(&mut view, KeyCode::Down);
    let text = draw_with(&mut view, 100, &theme(), &going());
    assert!(text.contains("┬ Review · 0 agents ─"), "{text}");
    assert!(
        row_with(&text, "Understand").contains("│ Not started yet"),
        "{text}"
    );
}

/// Claude Code's keys for each pane, the same whichever row is picked while the run goes,
/// so the line holds still. `p` says what it does now.
#[test]
fn the_keys_hold_still_for_each_pane() {
    let state = going();
    let mut view = WorkflowView::new(fixing());
    let footer = |view: &mut WorkflowView, state: &AppState| {
        let text = draw_with(view, 124, &theme(), state);
        text.lines().last().unwrap_or_default().trim().to_owned()
    };
    let phases = "↑↓  select · enter opens its agents · x stop workflow · p pause · esc back · \
                  s save";
    assert_eq!(footer(&mut view, &state), phases);
    press_in(&mut view, KeyCode::Right, &state);
    let agents = "↑↓  select · space peek · enter open · m message · x stop · r restart · \
                  p pause · esc back · s save";
    // The first row is done and the second is running: the keys are the same.
    assert_eq!(footer(&mut view, &state), agents);
    press_in(&mut view, KeyCode::Down, &state);
    assert_eq!(footer(&mut view, &state), agents);
    // Paused, p resumes.
    let mut paused = state.clone();
    if let Some(run) = paused.runs.get_mut(&run()) {
        run.waiting = Some(WaitReason::Paused);
    }
    assert!(footer(&mut view, &paused).contains("p resume"));
    assert!(matches!(
        press_in(&mut view, KeyCode::Char('p'), &paused),
        ViewOutcome::Send(UiCommand::Resume { run: Some(_) })
    ));
    // Once the workflow ends there is nothing to stop or pause, as in Claude Code's view,
    // and the key pressed anyway says why.
    let mut done = fixing();
    for phase in &mut done.phases {
        phase.status = RowStatus::Done;
    }
    let mut ended = state.clone();
    if let Some(run) = ended.runs.get_mut(&run()) {
        run.finished = Some(argonauts_core::runs::events::run::RunOutcome::Merged);
    }
    let mut view = WorkflowView::new(done);
    assert_eq!(
        footer(&mut view, &ended),
        "↑↓  select · enter opens its agents · esc back · s save"
    );
    assert!(matches!(
        press_in(&mut view, KeyCode::Char('x'), &ended),
        ViewOutcome::Notice(_)
    ));
}

/// `p` while the run is still pausing takes the pause back, since nothing waits yet to
/// resume from; once it waits, `p` resumes it; and on a workflow a CLI runs itself, `p`
/// says that CLI pauses it from its own screen.
#[test]
fn p_takes_a_pause_back_resumes_a_paused_run_and_leaves_a_clis_own_workflow_alone() {
    let mut pausing = going();
    if let Some(run) = pausing.runs.get_mut(&run()) {
        run.pausing = true;
    }
    let mut view = WorkflowView::new(fixing());
    let footer = |view: &mut WorkflowView, state: &AppState| {
        let text = draw_with(view, 124, &theme(), state);
        text.lines().last().unwrap_or_default().trim().to_owned()
    };
    assert!(footer(&mut view, &pausing).contains("p resume"));
    assert!(matches!(
        press_in(&mut view, KeyCode::Char('p'), &pausing),
        ViewOutcome::Send(UiCommand::CancelPause)
    ));
    let mut paused = going();
    if let Some(run) = paused.runs.get_mut(&run()) {
        run.waiting = Some(WaitReason::Paused);
    }
    assert!(matches!(
        press_in(&mut view, KeyCode::Char('p'), &paused),
        ViewOutcome::Send(UiCommand::Resume { run: Some(_) })
    ));
    assert!(matches!(
        press_in(&mut view, KeyCode::Char('p'), &going()),
        ViewOutcome::Send(UiCommand::Pause { now: false })
    ));
    // A run of an earlier session that you paused opens from its listing, and resumes
    // alike.
    let mut listed = AppState::default();
    listed
        .background
        .push(argonauts_core::runs::api::RunSummary {
            run: run(),
            title: "Fix the money rounding".into(),
            mode: argonauts_core::runs::events::run::RunMode::Crew,
            workflow: "council".into(),
            state: argonauts_core::runs::api::RunState::Waiting,
            stopped_at: "paused".into(),
            updated: time::OffsetDateTime::UNIX_EPOCH,
            spent: argonauts_core::Spend::default(),
            sessions_to_resume: 0,
            waiting: Vec::new(),
            live: false,
            remind: false,
            name: None,
            shape_known: true,
        });
    assert!(footer(&mut view, &listed).contains("p resume"));
    assert!(matches!(
        press_in(&mut view, KeyCode::Char('p'), &listed),
        ViewOutcome::Send(UiCommand::Resume { run: Some(_) })
    ));

    let mut own = fixing();
    own.id = argonauts_core::app::views::cli_workflow_run_id("w1");
    own.worker = Some("claude-code".to_owned());
    let mut view = WorkflowView::new(own);
    let ViewOutcome::Notice(said) = press_in(&mut view, KeyCode::Char('p'), &going()) else {
        panic!("p says why it can't");
    };
    assert_eq!(
        said,
        "Claude Code pauses its own workflow from its own screen"
    );
}

/// A stopped run has nothing left to stop or pause, as a done or failed one hasn't.
#[test]
fn a_stopped_runs_keys_offer_no_stop_or_pause() {
    let state = going();
    let mut stopped = fixing();
    stopped.state = WorkflowState::Stopped;
    let mut view = WorkflowView::new(stopped);
    let text = draw_with(&mut view, 124, &theme(), &state);
    let keys = text.lines().last().unwrap_or_default().trim().to_owned();
    assert!(!keys.contains("x stop"), "{keys}");
    assert!(!keys.contains("p pause"), "{keys}");
}

/// On a screen too narrow for the whole line, the phases' keys give up saving and pausing
/// first, and keep what Enter opens and how to stop.
#[test]
fn a_narrow_screen_keeps_what_enter_opens() {
    let state = going();
    let mut view = WorkflowView::new(fixing());
    let text = draw_with(&mut view, 80, &theme(), &state);
    assert_eq!(
        text.lines().last().unwrap_or_default().trim(),
        "↑↓  select · enter opens its agents · x stop workflow · esc back"
    );
}

/// Under 64 columns the view draws one pane, as Claude Code's does: the phases, and once
/// Enter goes to them, the open phase's agents across the whole box, with room for what
/// each row says.
#[test]
fn a_screen_under_64_columns_draws_one_pane() {
    let state = going();
    let mut view = WorkflowView::new(fixing());
    let wide = draw_with(&mut view, 64, &theme(), &state);
    assert!(wide.contains("╭ Phases ─") && wide.contains("┬"), "{wide}");

    let phases = draw_with(&mut view, 63, &theme(), &state);
    let top = phases
        .lines()
        .find(|line| line.contains('╭'))
        .unwrap_or_else(|| panic!("{phases}"));
    assert!(top.contains("╭ Phases ─"), "{phases}");
    assert!(!phases.contains('┬') && !phases.contains('┴'), "{phases}");
    // Enter is the only way to a phase's agents here, so the keys name it beside stopping,
    // and on a narrower screen still it outlasts every key but the way out.
    let keys = |text: &str| text.lines().last().unwrap_or_default().trim().to_owned();
    assert_eq!(
        keys(&phases),
        "↑↓  select · enter opens · x stop workflow · esc back"
    );
    assert_eq!(
        keys(&draw_with(&mut view, 44, &theme(), &state)),
        "↑↓  select · enter opens · esc back"
    );
    // Its rows have the whole box: the cursor, the phase, and its count at the far end.
    let fix = row_with(&phases, "⣾ Fix");
    assert!(
        fix.contains("❯") && fix.trim_end().ends_with("1/2 │"),
        "{phases}"
    );

    press_in(&mut view, KeyCode::Enter, &state);
    let agents = draw_with(&mut view, 63, &theme(), &state);
    let top = agents
        .lines()
        .find(|line| line.contains('╭'))
        .unwrap_or_else(|| panic!("{agents}"));
    assert!(top.contains("╭ Fix · "), "{agents}");
    assert!(!agents.contains("╭ Phases"), "{agents}");
    assert!(!agents.contains('┬'), "{agents}");
    // Esc goes back to the phases.
    press_in(&mut view, KeyCode::Esc, &state);
    let back = draw_with(&mut view, 63, &theme(), &state);
    assert!(back.contains("╭ Phases ─"), "{back}");
}

/// `r` says why when there's nothing to restart: an agent that has finished, or one with
/// no task of its own.
#[test]
fn r_says_why_when_there_is_nothing_to_restart() {
    let state = going();
    let mut view = WorkflowView::new(fixing());
    press_in(&mut view, KeyCode::Right, &state);
    match press_in(&mut view, KeyCode::Char('r'), &state) {
        ViewOutcome::Notice(why) => assert!(why.contains("isn't running"), "{why}"),
        other => panic!("a finished agent restarted: {other:?}"),
    }
    press_in(&mut view, KeyCode::Down, &state);
    // The screen holds no task for it, so there's nothing to start again.
    match press_in(&mut view, KeyCode::Char('r'), &state) {
        ViewOutcome::Notice(why) => assert!(why.contains("no task of its own"), "{why}"),
        other => panic!("an agent without a task restarted: {other:?}"),
    }
}

#[test]
fn x_stops_the_workflow_or_the_agent_after_asking() {
    let state = going();
    let mut view = WorkflowView::new(fixing());
    assert!(matches!(
        press_in(&mut view, KeyCode::Char('x'), &state),
        ViewOutcome::Consumed
    ));
    let text = draw_with(&mut view, 100, &theme(), &state);
    assert!(
        text.contains("x again to stop the workflow · esc to keep"),
        "{text}"
    );
    assert!(matches!(
        press_in(&mut view, KeyCode::Char('x'), &state),
        ViewOutcome::Send(UiCommand::Stop)
    ));
    // On the agents, the selected running agent.
    press_in(&mut view, KeyCode::Right, &state);
    press_in(&mut view, KeyCode::Down, &state);
    press_in(&mut view, KeyCode::Char('x'), &state);
    let outcome = press_in(&mut view, KeyCode::Char('x'), &state);
    assert!(
        matches!(&outcome, ViewOutcome::Send(UiCommand::StopAgent { session, .. }) if session.as_str() == "s-007"),
        "{outcome:?}"
    );
    // Esc between the two keeps it.
    press_in(&mut view, KeyCode::Char('x'), &state);
    assert!(matches!(
        press_in(&mut view, KeyCode::Esc, &state),
        ViewOutcome::Consumed
    ));
    assert!(view.confirm.is_none());
}

/// A run beside a resumed conversation isn't the one on screen, and `x x` still stops it,
/// by name as `/stop <run>` does.
#[test]
fn x_stops_a_workflow_whose_run_is_not_on_screen() {
    let state = AppState::default();
    let mut view = WorkflowView::new(fixing());
    press_in(&mut view, KeyCode::Char('x'), &state);
    let outcome = press_in(&mut view, KeyCode::Char('x'), &state);
    assert!(
        matches!(
            &outcome,
            ViewOutcome::Send(UiCommand::Slash {
                command: argonauts_core::input::SlashCommand::Stop { run: Some(stopped) },
            }) if *stopped == run()
        ),
        "{outcome:?}"
    );
}

#[test]
fn f_filters_the_agents_and_the_title_says_so() {
    let state = going();
    let mut view = WorkflowView::new(fixing());
    // The filter is the agents' key, as in Claude Code.
    press_in(&mut view, KeyCode::Right, &state);
    press_in(&mut view, KeyCode::Char('f'), &state);
    let text = draw_with(&mut view, 100, &theme(), &state);
    assert!(text.contains("Fix · showing 1 running"), "{text}");
    assert!(!text.contains("fix:slugify"), "{text}");
    assert!(text.contains("fix:median"), "{text}");
    for _ in 0..3 {
        press_in(&mut view, KeyCode::Char('f'), &state);
    }
    let text = draw_with(&mut view, 100, &theme(), &state);
    assert!(text.contains("Fix · 2 agents"), "{text}");
}

#[test]
fn space_closes_the_view_from_the_phases_and_peeks_from_the_agents() {
    let mut view = WorkflowView::new(fixing());
    assert!(matches!(
        press(&mut view, KeyCode::Char(' ')),
        ViewOutcome::Close
    ));
    press(&mut view, KeyCode::Right);
    assert!(matches!(
        press(&mut view, KeyCode::Char(' ')),
        ViewOutcome::Request(ViewRequest::Agent { .. })
    ));
    // Space again closes the peek, and Esc then leaves the agents.
    assert!(matches!(
        press(&mut view, KeyCode::Char(' ')),
        ViewOutcome::Consumed
    ));
    press(&mut view, KeyCode::Char(' '));
    assert!(matches!(
        press(&mut view, KeyCode::Esc),
        ViewOutcome::Consumed
    ));
    assert_eq!(view.top().side, Side::Right);
    assert!(matches!(
        press(&mut view, KeyCode::Esc),
        ViewOutcome::Consumed
    ));
    assert_eq!(view.top().side, Side::Left);
}

#[test]
fn s_saves_the_workflow() {
    let mut view = WorkflowView::new(fixing());
    assert!(matches!(
        press(&mut view, KeyCode::Char('s')),
        ViewOutcome::Send(UiCommand::SaveWorkflow { workflow: None, .. })
    ));
    // Tab isn't a key here, as in Claude Code.
    assert!(matches!(
        press(&mut view, KeyCode::Tab),
        ViewOutcome::Ignored
    ));
}

/// The status lines sit under the description: what waits for you, the latest note, or
/// how the run ended.
#[test]
fn status_lines_sit_under_the_description() {
    let mut data = fixing();
    data.status_lines = vec![StatusLine::new(
        StatusKind::Note,
        "fix:slugify passed its tests, so review:slugify started",
    )];
    let mut view = WorkflowView::new(data);
    let text = draw_with(&mut view, 100, &theme(), &going());
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[3].starts_with(" › fix:slugify passed its tests"),
        "{text}"
    );
    assert!(lines[4].trim().is_empty(), "{text}");
    assert!(lines[5].starts_with(" ╭ Phases"), "{text}");
}

/// When a phase is added above the cursor, the cursor stays on its phase.
#[test]
fn the_cursor_stays_on_its_phase_when_one_is_added() {
    let mut view = WorkflowView::new(fixing());
    assert_eq!(view.top().phase().map(|p| p.name.as_str()), Some("Fix"));
    let mut more = fixing();
    more.phases
        .insert(1, phase("Plan", RowStatus::Done, Vec::new()));
    assert!(view.accept_workflow(&more));
    assert_eq!(view.top().phase().map(|p| p.name.as_str()), Some("Fix"));
}

#[test]
fn vim_keys_move_open_and_go_up_only_when_on() {
    let off = list::keys_state(false);
    let on = list::keys_state(true);
    let mut view = WorkflowView::new(parent());
    for c in ['j', 'k', 'g', 'G', 'h'] {
        assert!(
            matches!(
                press_in(&mut view, KeyCode::Char(c), &off),
                ViewOutcome::Ignored
            ),
            "{c}"
        );
    }
    assert_eq!(view.top().side, Side::Left);
    press_in(&mut view, KeyCode::Char('l'), &on);
    assert_eq!((view.top().side, view.top().row), (Side::Right, 0));
    press_in(&mut view, KeyCode::Char('j'), &on);
    assert_eq!(view.top().row, 1);
    press_in(&mut view, KeyCode::Char('G'), &on);
    assert_eq!(
        view.top().row,
        3,
        "the last row is the agent inside build:c"
    );
    press_in(&mut view, KeyCode::Char('g'), &on);
    assert_eq!(view.top().row, 0);
    // `h` goes back a level as ← does, and closes from the phases.
    press_in(&mut view, KeyCode::Char('h'), &on);
    assert_eq!(view.top().side, Side::Left);
    assert!(matches!(
        press_in(&mut view, KeyCode::Char('h'), &on),
        ViewOutcome::Close
    ));
    press(&mut view, KeyCode::Right);
    press(&mut view, KeyCode::End);
    assert_eq!(view.top().row, 3);
    press(&mut view, KeyCode::Home);
    assert_eq!(view.top().row, 0);
}

/// A workflow inside opens and folds in place: it is open while it runs, ← folds it with
/// the cursor on it, → or Enter opens it again, and Esc goes back as ← does elsewhere,
/// from the agents to the phases, then out of the view.
#[test]
fn a_workflow_inside_opens_and_folds_in_place() {
    let mut view = WorkflowView::new(parent());
    press(&mut view, KeyCode::Right);
    press(&mut view, KeyCode::End);
    assert!(matches!(view.top().row(), Some(WorkflowRow::Agent(agent)) if agent.label == "test:a"));
    let text = screen(&mut view);
    assert!(text.contains("▾ build:c"), "{text}");
    assert!(
        text.contains("│ ❯  ⣾ test:a"),
        "two cells further in: {text}"
    );
    press(&mut view, KeyCode::Left);
    assert!(matches!(view.top().row(), Some(WorkflowRow::Workflow(_))));
    let text = screen(&mut view);
    assert!(text.contains("▸ build:c"), "{text}");
    assert!(!text.contains("test:a"), "{text}");
    assert!(matches!(
        press(&mut view, KeyCode::Enter),
        ViewOutcome::Consumed
    ));
    assert!(screen(&mut view).contains("test:a"));
    assert!(matches!(
        press(&mut view, KeyCode::Esc),
        ViewOutcome::Consumed
    ));
    assert_eq!(view.top().side, Side::Left);
    assert!(matches!(press(&mut view, KeyCode::Esc), ViewOutcome::Close));
}

/// When a workflow inside folds itself above the cursor, the cursor stays on its row.
#[test]
fn the_cursor_stays_on_its_row_when_a_workflow_above_it_folds() {
    let with_more = |status: RowStatus| {
        let mut data = parent();
        if let WorkflowRow::Workflow(nested) = &mut data.phases[0].rows[2] {
            nested.status = status;
        }
        for (session, label) in [("s-005", "build:d"), ("s-006", "build:e")] {
            data.phases[0].rows.push(WorkflowRow::Agent(agent(
                session,
                label,
                RowStatus::Running,
            )));
        }
        data
    };
    let on = |view: &WorkflowView| match view.top().row() {
        Some(WorkflowRow::Agent(agent)) => agent.label.clone(),
        other => format!("{other:?}"),
    };
    let mut view = WorkflowView::new(with_more(RowStatus::Running));
    press(&mut view, KeyCode::Right);
    press(&mut view, KeyCode::End);
    press(&mut view, KeyCode::Up);
    assert_eq!(on(&view), "build:d");
    assert!(view.accept_workflow(&with_more(RowStatus::Done)));
    assert!(
        !screen(&mut view).contains("test:a"),
        "the workflow inside folded"
    );
    assert_eq!(on(&view), "build:d");
}

/// Once a workflow inside ends it folds by itself, unless the cursor is in it.
#[test]
fn a_workflow_inside_folds_itself_once_it_ends() {
    let ended = |mut data: WorkflowData| {
        if let WorkflowRow::Workflow(nested) = &mut data.phases[0].rows[2] {
            nested.status = RowStatus::Done;
        }
        data
    };
    let mut view = WorkflowView::new(parent());
    assert!(view.accept_workflow(&ended(parent())));
    assert!(!screen(&mut view).contains("test:a"));

    let mut view = WorkflowView::new(parent());
    press(&mut view, KeyCode::Right);
    press(&mut view, KeyCode::End);
    assert!(view.accept_workflow(&ended(parent())));
    assert!(screen(&mut view).contains("test:a"));
}

#[test]
fn agent_answer_accepted_by_open_view() {
    let mut view = WorkflowView::new(parent());
    // An answer nobody asked for, for an agent, stays with the runtime.
    let data = AgentData {
        run: run(),
        row: agent("s-002", "build:b", RowStatus::Running),
        prompt: "Build the history cells.".into(),
        activity: Vec::new(),
        tool_calls: 0,
        diff: None,
        subagents: None,
        outcome: None,
        goal: None,
        plan: None,
        facts: Vec::new(),
        to_fix: None,
        asked_for: None,
        earlier: None,
        moved: None,
        given: None,
        prompt_about: None,
        review: None,
        started_from: None,
    };
    let answer = ViewData::Agent {
        view: Box::new(data.clone()),
    };
    assert!(!view.accept(&answer));

    press(&mut view, KeyCode::Right);
    press(&mut view, KeyCode::Down);
    let outcome = press(&mut view, KeyCode::Enter);
    assert!(
        matches!(&outcome, ViewOutcome::Request(ViewRequest::Agent { session, .. }) if session.as_str() == "s-002"),
        "{outcome:?}"
    );
    assert!(view.accept(&answer));
    assert_eq!(
        view.agent().map(|agent| agent.data.row.label.as_str()),
        Some("build:b")
    );
    let text = screen(&mut view);
    // Two agents of its own and the nested workflow's two, as the workflow view counts.
    assert!(text.contains("Build · 4 agents"), "{text}");
    assert!(text.contains("⣾ Running"), "{text}");
    assert!(text.contains("│ Prompt "), "{text}");

    // Another run's answer isn't this view's.
    let mut other = data;
    other.run = RunId::new("run-02").unwrap();
    assert!(!view.accept(&ViewData::Agent {
        view: Box::new(other)
    }));

    // `s` in the agent's detail saves the workflow, as in Claude Code.
    assert!(matches!(
        press(&mut view, KeyCode::Char('s')),
        ViewOutcome::Send(UiCommand::SaveWorkflow { .. })
    ));
    // Esc leaves the agent for the workflow's frame.
    assert!(matches!(
        press(&mut view, KeyCode::Esc),
        ViewOutcome::Consumed
    ));
    assert!(view.agent().is_none());
}

fn kit() -> WorkflowData {
    let mut rows = Vec::new();
    for (session, label, by) in [
        ("s-002", "build:a-terminal-bottom", "claude-code/opus"),
        ("s-003", "build:b-history-cells", "codex/gpt-5.6"),
        ("s-005", "build:d-plain-mode", "agy/gemini-3.6"),
    ] {
        let mut row = agent(session, label, RowStatus::Done);
        row.worker = by.parse().unwrap();
        rows.push(WorkflowRow::Agent(row));
    }
    if let WorkflowRow::Agent(row) = &mut rows[1] {
        row.subagents = 2;
    }
    rows.insert(2, parent().phases[0].rows[2].clone());
    workflow(1, "todo-tui", rows)
}

#[test]
fn ascii_frames_join_and_keep_their_columns() {
    let mut ascii = Theme::dark();
    ascii.glyphs = Glyphs::ASCII;
    let state = AppState::default();
    let text = draw_with(&mut WorkflowView::new(kit()), 100, &ascii, &state);
    let box_drawing = |c: char| ('\u{2500}'..='\u{257F}').contains(&c);
    assert!(!text.chars().any(box_drawing), "{text}");
    assert!(text.contains("+ Phases ---"), "{text}");
    assert!(!text.contains(['↑', '↓', '→']), "{text}");
    assert!(text.contains("up/down select"), "{text}");
    // The model sits in one column, after the widest label.
    let models = [
        ("build:a", "Opus"),
        ("build:b", "GPT 5.6"),
        ("build:d", "Gemini"),
    ]
    .map(|(label, model)| row_with(&text, label).find(model));
    assert!(
        models.iter().all(|at| at.is_some() && *at == models[0]),
        "{text}"
    );
    let mut buf = Buffer::empty(Rect::new(0, 0, 100, 14));
    WorkflowView::new(kit()).render(buf.area, &mut buf, &ctx_with(&ascii, &state));
    assert!(
        crate::base::stability::check_with(&buf, 2).is_empty(),
        "{text}"
    );
}

#[test]
fn hidden_cli_internals_leave_out_subagents() {
    let mut state = AppState::default();
    let text = draw_with(&mut WorkflowView::new(kit()), 120, &theme(), &state);
    assert!(text.contains("+2 subagents"), "{text}");
    let mut ui = argonauts_core::settings::ui::UiSettings::default();
    ui.panel.subagents = CliInternals::Hidden;
    state.ui = Some(ui);
    let text = draw_with(&mut WorkflowView::new(kit()), 120, &theme(), &state);
    assert!(!text.contains("subagent"), "{text}");
}

/// `ui.workflow.row_cost` adds what a row cost after its tokens; without it no row shows
/// spend.
#[test]
fn row_cost_is_an_opt_in() {
    let mut data = fixing();
    if let WorkflowRow::Agent(row) = &mut data.phases[1].rows[0] {
        row.spent.cost_usd = 0.12;
    }
    let mut state = going();
    let text = draw_with(&mut WorkflowView::new(data.clone()), 100, &theme(), &state);
    assert!(!text.contains("$0.12"), "{text}");
    let mut ui = argonauts_core::settings::ui::UiSettings::default();
    ui.workflow.row_cost = true;
    state.ui = Some(ui);
    let text = draw_with(&mut WorkflowView::new(data), 100, &theme(), &state);
    assert!(
        row_with(&text, "fix:slugify").contains("Haiku 4.5 · $0.12"),
        "{text}"
    );
}

#[test]
fn pause_acts_only_on_the_run_on_screen() {
    let mut view = WorkflowView::new(parent());
    let outcome = press(&mut view, KeyCode::Char('p'));
    assert!(
        matches!(&outcome, ViewOutcome::Notice(text) if text.starts_with("Only the run on screen")),
        "{outcome:?}"
    );
    let outcome = press_in(&mut view, KeyCode::Char('p'), &going());
    assert!(
        matches!(outcome, ViewOutcome::Send(UiCommand::Pause { now: false })),
        "{outcome:?}"
    );
}

#[test]
fn the_shown_level_or_agent_is_what_refreshes() {
    let refresh = crate::views::follow::Refresh::default();
    let mut view = WorkflowView::new(parent()).following(refresh.follower());
    screen(&mut view);
    assert_eq!(
        view.shown(),
        Shown::Workflow {
            run: run(),
            id: WorkflowId::numbered(1),
            root: true,
        }
    );
    // An agent's own workflow is asked for by its id.
    let mut agent_id = workflow(1, "probe", Vec::new());
    agent_id.id = argonauts_core::app::views::cli_workflow_run_id("w4d273qmn");
    let view = WorkflowView::new(agent_id);
    assert!(matches!(view.shown(), Shown::Workflow { root: false, .. }));
}

/// A workflow an agent's own CLI runs is drawn the same way, but only read: its agents
/// open, and its CLI stops and saves it.
#[test]
fn a_agents_own_workflow_only_reads() {
    let mut data = fixing();
    data.id = argonauts_core::app::views::cli_workflow_run_id("w4d273qmn");
    let mut view = WorkflowView::new(data);
    let state = going();
    let keys = |view: &mut WorkflowView| {
        let text = draw_with(view, 100, &theme(), &state);
        text.lines().last().unwrap_or_default().trim().to_owned()
    };
    assert_eq!(
        keys(&mut view),
        "↑↓  select · enter opens its agents · esc back"
    );
    press_in(&mut view, KeyCode::Right, &state);
    assert_eq!(
        keys(&mut view),
        "↑↓  select · space peek · enter open · esc back"
    );
    press_in(&mut view, KeyCode::Left, &state);
    assert!(matches!(
        press_in(&mut view, KeyCode::Char('x'), &state),
        ViewOutcome::Notice(_)
    ));
    assert!(matches!(
        press_in(&mut view, KeyCode::Char('s'), &state),
        ViewOutcome::Notice(_)
    ));
}

/// After the finish, the view opens on the last phase whose agents ran.
#[test]
fn a_finished_workflow_opens_on_its_last_phase_with_agents() {
    let mut data = workflow(1, "council", Vec::new());
    let done = |session: &str| WorkflowRow::Agent(agent(session, "build:x", RowStatus::Done));
    data.phases = vec![
        phase("Understand", RowStatus::Done, vec![done("s-001")]),
        phase("Build", RowStatus::Done, vec![done("s-002"), done("s-003")]),
        phase("Merge", RowStatus::Done, Vec::new()),
    ];
    let view = WorkflowView::new(data.clone());
    assert_eq!(view.top().phase().map(|p| p.name.as_str()), Some("Build"));

    // While it runs, it opens where the work is, and a merge waiting on you says so.
    data.phases[2].status = RowStatus::NeedsYou;
    let mut view = WorkflowView::new(data.clone());
    assert_eq!(view.top().phase().map(|p| p.name.as_str()), Some("Merge"));
    let text = screen(&mut view);
    assert!(
        text.contains("Waiting for your answer in the conversation"),
        "{text}"
    );

    // With the run's question known, it names the question.
    data.phases[2].status = RowStatus::Running;
    let mut state = AppState::default();
    let mut run_state = RunState::new(run());
    run_state.approvals.push(crate::frame::state::OpenApproval {
        opened: Box::new(asking("Merge this run into main?")),
        set_aside: false,
        replayed: false,
    });
    state.runs.insert(run(), run_state);
    let mut view = WorkflowView::new(data);
    let text = draw_with(&mut view, 100, &theme(), &state);
    assert!(
        text.contains("Waiting for your answer: Merge this run into main?"),
        "{text}"
    );
}

fn asking(title: &str) -> argonauts_core::runs::events::approval::ApprovalOpened {
    use argonauts_core::runs::events::approval::{
        ApprovalKind, ApprovalOpened, ApprovalPacket, FreeText, QuestionPacket,
    };
    use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
    use argonauts_core::settings::asking::WhenDeclined;

    ApprovalOpened {
        id: argonauts_core::base::names::ApprovalId::numbered(1),
        kind: ApprovalKind::Merge,
        title: title.into(),
        task: None,
        packet: ApprovalPacket::Question(QuestionPacket {
            from: "codex".parse().unwrap(),
            task: argonauts_core::TaskId::new("task-001").unwrap(),
            text: title.into(),
            options: Vec::new(),
        }),
        evidence_sha256: "0".repeat(64),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        when_declined: WhenDeclined::Decide,
        options: Vec::new(),
        free_text: FreeText::None,
        needs_you: true,
    }
}

/// During Understand the view shows it, and the run's own workflow takes its place once the
/// main agent picks it.
#[test]
fn the_runs_own_workflow_replaces_understand() {
    let understand = workflow(2, "understanding", Vec::new());
    let mut view = WorkflowView::new(understand);
    let own = workflow(1, "council", Vec::new());
    assert!(view.accept_workflow(&own));
    assert_eq!(view.current().name, "council");
    // An agent's own workflow never takes the run's place.
    let mut agents = workflow(3, "probe", Vec::new());
    agents.id = argonauts_core::app::views::cli_workflow_run_id("w4d273qmn");
    assert!(!view.accept_workflow(&agents));
    assert_eq!(view.current().name, "council");
}

/// The totals count the agents this workflow started and the run's time, as the panel
/// row times it. Spend stays on the panel row.
#[test]
fn the_totals_count_the_workflows_agents_and_the_runs_time() {
    let theme = theme();
    let wall = OffsetDateTime::now_utc();
    let mut state = AppState::default();
    let mut run = RunState::new(run());
    run.started = Some(wall - time::Duration::seconds(125));
    run.ended = Some(wall);
    run.finished = Some(argonauts_core::runs::events::run::RunOutcome::Merged);
    state.runs.insert(run.id.clone(), run);
    let mut view = WorkflowView::new(workflow(1, "council", Vec::new()));
    let text = draw_with(&mut view, 120, &theme, &state);
    assert!(text.contains("1/3 agents · 2m05s · done"), "{text}");
    assert!(!text.contains('$'), "{text}");
}
