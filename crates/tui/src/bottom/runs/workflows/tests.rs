use argonauts_core::base::names::AgentId;
use argonauts_core::look::logos::{Logo, LogoMark};
use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::style::Color;

use super::*;
use crate::base::stability::assert_lines;
use crate::base::text::plain_text;
use crate::base::theme::{ColorLevel, Glyphs};
use crate::bottom::{AgentRow, AgentState};
use crate::testing::pictures::workflow_pictures::{Picture, buffer_of, dark, picture};

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn runs(count: usize) -> WorkflowPanel {
    WorkflowPanel {
        rows: (0..count)
            .map(|i| {
                let mut row = WorkflowRow::new(
                    format!("run-{i}"),
                    format!("workflow-{i}"),
                    WorkflowRowState::Running,
                );
                row.count = Some((1, 3));
                row
            })
            .collect(),
        max_rows: 2,
        ..WorkflowPanel::default()
    }
}

/// Who serves a row's model, as the pictures name them.
#[derive(Clone, Copy)]
enum Cli {
    Claude,
    Codex,
    Agy,
    /// A local model run through OpenCode, which shows Ollama's llama.
    Local,
}

fn logo(cli: Cli) -> Logo {
    let (agent, mark) = match cli {
        Cli::Claude => ("claude-code", LogoMark::ClaudeCode),
        Cli::Codex => ("codex", LogoMark::Codex),
        Cli::Agy => ("antigravity-cli", LogoMark::Antigravity),
        Cli::Local => ("opencode", LogoMark::Ollama),
    };
    Logo {
        agent: AgentId(agent.to_owned()),
        mark: Some(mark),
    }
}

/// A row of the pictures: a request with its count, or with `under` a task or what does its
/// work.
fn pictured(
    name: &str,
    state: WorkflowRowState,
    seconds: u64,
    tokens: u64,
    count: Option<(u32, u32)>,
) -> WorkflowRow {
    let mut row = WorkflowRow::new(format!("run-{name}"), name, state);
    row.count = count;
    row.elapsed_ms = seconds * 1000;
    row.tokens = tokens;
    row
}

fn task(mut row: WorkflowRow, cli: Cli, way: &str) -> WorkflowRow {
    row.kind = RunRowKind::Task;
    row.depth = 1;
    row.logo = Some(logo(cli));
    row.way = Some(way.to_owned());
    row
}

fn work(mut row: WorkflowRow, cli: Cli, doing: &str) -> WorkflowRow {
    row.kind = RunRowKind::Work;
    row.depth = 2;
    row.logo = Some(logo(cli));
    row.doing = Some(doing.to_owned());
    row
}

/// The made-up runs of the pictures. Two requests are open: Add due dates with a task of each
/// way, its workflow's steps and its agents showing; Audit the auth routes with its three tasks.
/// Seven more are shut, one for each other mark. Date cases is picked.
fn the_pictures_panel() -> WorkflowPanel {
    use WorkflowRowState::*;
    let p = pictured;
    let mut sample = task(
        p("Sample data", Running, 182, 88_400, None),
        Cli::Agy,
        "Own session",
    );
    sample.doing = Some("Bash(npm run seed -- --rows 500)".to_owned());
    let mut money = p(
        "Fix the money rounding",
        Running,
        1864,
        462_000,
        Some((5, 6)),
    );
    money.near_limit = true;
    let rows = vec![
        p("Add due dates", Running, 760, 1_100_400, Some((1, 4))),
        task(
            p("Due date field", Done, 252, 194_000, Some((5, 5))),
            Cli::Claude,
            "Own session",
        ),
        task(
            p("Date cases", Running, 400, 612_000, Some((1, 2))),
            Cli::Codex,
            "Maker-checker",
        ),
        work(
            p("Build", Done, 200, 380_000, None),
            Cli::Codex,
            "Done, 6 files changed",
        ),
        work(
            p("Review", Running, 200, 232_000, None),
            Cli::Claude,
            "Reading the diff",
        ),
        task(
            p("Reminder emails", Running, 248, 164_000, Some((1, 3))),
            Cli::Local,
            "3 agents",
        ),
        work(
            p("Templates", Done, 112, 58_000, None),
            Cli::Local,
            "14 tests pass",
        ),
        work(
            p("Scheduler", Running, 248, 61_000, None),
            Cli::Local,
            "Write(src/remind/schedule.ts)",
        ),
        work(
            p("Settings", Running, 248, 45_000, None),
            Cli::Local,
            "Bash(npm test -- settings)",
        ),
        sample,
        p(
            "Audit the auth routes",
            NeedsYou,
            494,
            240_600,
            Some((1, 3)),
        ),
        task(
            p("Find the routes", Done, 150, 71_200, Some((3, 3))),
            Cli::Claude,
            "Own session",
        ),
        // One of your own workflows, whose name is cut at 13 cells.
        task(
            p("Route checks", NeedsYou, 408, 131_000, Some((1, 3))),
            Cli::Codex,
            "Review with tests",
        ),
        task(
            p("Login tests", Waiting, 65, 18_400, Some((0, 4))),
            Cli::Claude,
            "Own session",
        ),
        money,
        p("Speed up the importer", Fallback, 146, 83_600, Some((1, 3))),
        p(
            "Rename the config keys",
            Pausing,
            591,
            341_000,
            Some((3, 4)),
        ),
        p("Per key rate limit", Paused, 62, 38_800, Some((1, 3))),
        p("Retry the webhook", PullRequest, 490, 70_200, Some((4, 4))),
        p("Tidy the log lines", Done, 380, 17_700, Some((2, 2))),
        p("Port the CSV export", Stopped, 302, 220_000, Some((3, 6))),
    ];
    WorkflowPanel {
        max_rows: rows.len(),
        rows,
        selected: Some(2),
        ..WorkflowPanel::default()
    }
}

/// Checks each logo's two cells hold what the app draws for that logo, then blanks them: the
/// pictures leave a logo's cells blank, since a terminal that draws pictures puts the logo over
/// them. The rows start at `top`.
fn logos_drawn(panel: &WorkflowPanel, buf: &mut Buffer, top: usize, theme: &Theme) {
    for (at, row) in panel.rows.iter().enumerate() {
        let y = top + at;
        let Some(logo) = &row.logo else {
            continue;
        };
        let x = INSET + STEP_IN * row.depth + MARK_CELLS;
        let wanted = buffer_of(&[Line::from(logo_spans(logo, theme))], LOGO_WIDTH);
        for at in 0..LOGO_WIDTH {
            let cell = &mut buf[(x as u16 + at, y as u16)];
            let want = &wanted[(at, 0)];
            assert_eq!(
                (cell.symbol(), cell.fg),
                (want.symbol(), want.fg),
                "{}: its logo's cell {at}",
                row.name
            );
            cell.set_symbol(" ");
            cell.fg = Color::Reset;
        }
    }
}

/// The screen's bottom with the panel's third row picked: the key line, a blank, and the rows.
fn drawn_as(picture: Picture, width: u16) {
    let theme = dark();
    let panel = the_pictures_panel();
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    let mut bottom = crate::bottom::BottomState::new();
    bottom.workflows = panel.clone();
    let view = bottom.render(&ctx);
    let shown = panel.rows.len() + 2;
    let lines = &view.lines[view.lines.len() - shown..];
    assert_lines(lines, width);
    assert_eq!(
        lines[2..],
        panel_lines(&panel, &AgentPanel::default(), &ctx)[..],
        "the panel's rows draw as the panel draws them alone"
    );
    let mut buf = buffer_of(lines, width);
    logos_drawn(&panel, &mut buf, 2, &theme);
    picture.assert_drawn(0..shown, &buf, 0);
}

/// Every run mark, every way and every kind of row, at 120 columns, cell for cell and colour for
/// colour: the way column from column 29, the bar from 43, the row ending at 95.
#[test]
fn the_rows_read_as_their_picture() {
    drawn_as(picture!("run_rows"), 120);
}

/// Below 100 columns the way column drops and the bar gives way next, 17 cells at 80 columns.
#[test]
fn at_80_columns_the_way_goes_and_the_bar_gives_way() {
    drawn_as(picture!("run_rows_80"), 80);
}

/// At 60 columns the bar is down to 4 cells and the word `tokens` goes; names stay whole, what
/// a row is doing is cut to the cells it has, and the count, time and tokens stay.
#[test]
fn at_60_columns_the_word_tokens_goes_too() {
    drawn_as(picture!("run_rows_60"), 60);
}

/// Narrower still, the bar goes and the name column gives up cells, and no row is ever wider
/// than the screen.
#[test]
fn a_narrow_screen_never_overflows() {
    let theme = theme();
    let panel = the_pictures_panel();
    for width in [20, 30, 40, 50, 59] {
        let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
        let lines = panel_lines(&panel, &AgentPanel::default(), &ctx);
        assert_lines(&lines, width);
        let text = plain_text(&lines);
        assert!(text.contains("Add due"), "{width} columns\n{text}");
    }
    let ctx = RenderCtx::new(50, &theme, Clock::still(Instant::now()));
    let text = plain_text(&panel_lines(&panel, &AgentPanel::default(), &ctx));
    let first = text.lines().next().unwrap_or_default();
    assert!(
        first.contains("1/4 · 12m40s") && !first.contains('▰'),
        "{first}"
    );
}

/// A row waiting out an outage and a row that hasn't started are different in shape, not
/// colour alone, in plain glyphs too.
#[test]
fn waiting_and_queued_rows_have_marks_of_their_own() {
    let mark_of = |state: WorkflowRowState, glyphs: &Glyphs| {
        let mut theme = theme();
        theme.glyphs = glyphs.clone();
        let row = WorkflowRow::new("run-0", "workflow-0", state);
        let clock = Clock::new(Instant::now(), Instant::now());
        mark(&row, &RenderCtx::new(100, &theme, clock)).0
    };
    for glyphs in [Glyphs::UNICODE, Glyphs::ASCII] {
        let waiting = mark_of(WorkflowRowState::Waiting, &glyphs);
        let queued = mark_of(WorkflowRowState::Queued, &glyphs);
        assert_ne!(waiting, queued, "{glyphs:?}");
    }
    assert_eq!(mark_of(WorkflowRowState::Queued, &Glyphs::UNICODE), "·");
    assert_eq!(mark_of(WorkflowRowState::Queued, &Glyphs::ASCII), "_");
}

/// A row at work turns its mark a frame every quarter second, and the screen wakes for each
/// frame; reduced motion and plain glyphs hold it still.
#[test]
fn a_row_at_work_turns_its_mark() {
    let t0 = Instant::now();
    let mark_at = |theme: &Theme, at: Duration| {
        let panel = runs(1);
        let ctx = RenderCtx::new(100, theme, Clock::new(t0, t0 + at));
        let text = plain_text(&panel_lines(&panel, &AgentPanel::default(), &ctx));
        text.chars().nth(2).unwrap_or(' ')
    };
    let turning = theme();
    assert_eq!(mark_at(&turning, Duration::ZERO), '⣾');
    assert_eq!(mark_at(&turning, Duration::from_millis(250)), '⣽');
    assert_eq!(mark_at(&turning, Duration::from_secs(2)), '⣾');
    let panel = runs(1);
    let clock = Clock::new(t0, t0 + Duration::from_millis(100));
    assert_eq!(
        panel.next_change(&AgentPanel::default(), &clock, &turning),
        Some(t0 + Duration::from_millis(250))
    );

    let mut still = theme();
    still.reduced_motion = true;
    assert_eq!(mark_at(&still, Duration::from_millis(250)), '⣾');
    assert_eq!(
        panel.next_change(&AgentPanel::default(), &clock, &still),
        None
    );
    let mut plain = theme();
    plain.glyphs = Glyphs::ASCII;
    assert_eq!(mark_at(&plain, Duration::from_millis(250)), 'o');
}

/// The moment a run's steps are known its row shows them all to come: its name, an empty bar,
/// none of the four done and its time. No token is counted yet, so none are shown, as in Claude
/// Code's row. The time then counts by itself.
#[test]
fn a_run_whose_steps_were_just_written_has_an_empty_bar_and_its_time() {
    let theme = dark();
    let t0 = Instant::now();
    let mut row = WorkflowRow::new("run-0", "Add due dates", WorkflowRowState::Running);
    row.count = Some((0, 4));
    row.elapsed_ms = 2_000;
    row.measured_at = Some(t0);
    let panel = WorkflowPanel {
        rows: vec![row],
        max_rows: 5,
        ..WorkflowPanel::default()
    };
    let mut still = theme.clone();
    still.reduced_motion = true;
    let read = |at: Instant| {
        let ctx = RenderCtx::new(80, &still, Clock::still(at));
        plain_text(&panel_lines(&panel, &AgentPanel::default(), &ctx))
    };
    let empty = "▱".repeat(BAR_CELLS);
    let name = format!("{:<24}", "Add due dates");
    assert_eq!(read(t0), format!("  ⣾  {name}{empty}  0/4 · 2s"));
    assert_eq!(
        read(t0 + Duration::from_secs(1)),
        format!("  ⣾  {name}{empty}  0/4 · 3s")
    );
}

/// A name longer than its column is cut with an ellipsis and a blank for it to spill into, so
/// every bar starts in one column whatever the names.
#[test]
fn a_long_name_is_cut_and_every_bar_starts_in_one_column() {
    let theme = theme();
    let mut panel = runs(2);
    panel.rows[0].name = "council · Add a --json flag to export, with tests".into();
    panel.rows[1].name = "goal".into();
    for width in [80, 100, 120, 200] {
        let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
        let lines = panel_lines(&panel, &AgentPanel::default(), &ctx);
        assert_lines(&lines, width);
        let text = plain_text(&lines);
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[0].contains("council · Add a --js… "), "{text}");
        // From 100 columns the way column sits before the bar, blank on a request's row.
        let at = if width >= 100 { 43 } else { 29 };
        let bar = |row: &str| row.chars().position(|cell| cell == '▰' || cell == '▱');
        assert_eq!(bar(rows[0]), Some(at), "{width} columns\n{text}");
        assert_eq!(bar(rows[1]), Some(at), "{width} columns\n{text}");
    }
}

/// A row deep in the tree keeps room for its name: it runs on into the way column it leaves
/// blank, and where there's no way column, its logo gives way first.
#[test]
fn a_row_deep_in_the_tree_keeps_room_for_its_name() {
    let theme = theme();
    let mut panel = runs(5);
    panel.max_rows = 10;
    for (depth, row) in panel.rows.iter_mut().enumerate() {
        row.depth = depth;
        row.name = "Sample data for the parser".into();
        row.logo = Some(Logo {
            agent: AgentId("codex".into()),
            mark: Some(LogoMark::Codex),
        });
    }
    let name_cells = |row: &str| {
        let at = row
            .find("Sa")
            .unwrap_or_else(|| panic!("the name in {row}"));
        row[at..]
            .split("  ")
            .next()
            .unwrap_or_default()
            .chars()
            .count()
    };
    let ctx = RenderCtx::new(120, &theme, Clock::still(Instant::now()));
    let text = plain_text(&panel_lines(&panel, &AgentPanel::default(), &ctx));
    for row in text.lines() {
        assert!(name_cells(row) >= 10, "{row}\n{text}");
    }
    let narrow = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
    let text = plain_text(&panel_lines(&panel, &AgentPanel::default(), &narrow));
    let deepest = text.lines().last().expect("the deepest row");
    assert!(!deepest.contains('●'), "the logo gave way: {deepest}");
}

/// The stats of every row are in one column each, the number of tokens right-aligned after
/// its arrow, whatever the other rows hold, and a row's tokens wait until it has some.
#[test]
fn every_field_lines_up_down_the_list() {
    let theme = theme();
    let mut panel = runs(3);
    panel.rows[0].count = Some((12, 14));
    panel.rows[0].elapsed_ms = 7_384_000;
    panel.rows[0].tokens = 1_840_000;
    panel.rows[1].elapsed_ms = 9_000;
    panel.rows[1].tokens = 900;
    panel.rows[2].elapsed_ms = 61_000;
    panel.max_rows = 5;
    let ctx = RenderCtx::new(120, &theme, Clock::still(Instant::now()));
    let text = plain_text(&panel_lines(&panel, &AgentPanel::default(), &ctx));
    let rows: Vec<&str> = text.lines().collect();
    // What follows the bar, which ends at column 63.
    let tail = |row: &str| row.chars().skip(63).collect::<String>();
    assert_eq!(tail(rows[0]), "  12/14 · 2h03m · ↓  1.8M tokens", "{text}");
    assert_eq!(tail(rows[1]), "    1/3 ·    9s · ↓   900 tokens", "{text}");
    assert_eq!(tail(rows[2]), "    1/3 · 1m01s", "{text}");
}

#[test]
fn panel_down_scrolls_then_past_end() {
    let theme = theme();
    let t0 = Instant::now();
    let ctx = RenderCtx::new(100, &theme, Clock::still(t0));
    let mut panel = runs(5);
    let mut agents = AgentPanel::default();
    panel.focus();
    let text = plain_text(&panel_lines(&panel, &agents, &ctx));
    assert!(text.ends_with("  ↓  3 more"), "{text}");
    assert!(!text.contains("workflow-2"), "{text}");

    for expected in 1..5 {
        assert_eq!(
            handle_panel_key(&mut panel, &mut agents, key(KeyCode::Down)),
            PanelEvent::Moved
        );
        assert_eq!(panel.selected, Some(expected));
        let text = plain_text(&panel_lines(&panel, &agents, &ctx));
        assert!(
            text.contains(&format!("❯ ⣾  workflow-{expected}")),
            "{text}"
        );
    }
    let text = plain_text(&panel_lines(&panel, &agents, &ctx));
    assert!(text.starts_with("  ↑  3 more"), "{text}");
    assert_eq!(
        handle_panel_key(&mut panel, &mut agents, key(KeyCode::Down)),
        PanelEvent::PastEnd
    );
    assert_eq!(panel.selected, Some(4));
}

/// Enter opens what the row is: a request's or a task's view in the run's workflow view, and the
/// session of a row that is one. `x` stops a request's run or a row's session, and a task done
/// by several stops from its view.
#[test]
fn enter_opens_what_the_row_is_and_x_stops_it() {
    let session = |key: &str| AgentRef {
        run: Some("run-0".into()),
        session: key.into(),
    };
    let mut panel = the_pictures_panel();
    panel.rows[0].key = "run-0".into();
    panel.rows[1].key = "run-0#task-001".into();
    panel.rows[1].session = Some(session("s-001"));
    panel.rows[2].key = "run-0#task-002".into();
    panel.rows[3].key = "run-0#s-002".into();
    panel.rows[3].session = Some(session("s-002"));
    let mut agents = AgentPanel::default();
    let mut at = |row: usize, code: KeyCode| {
        panel.selected = Some(row);
        handle_panel_key(&mut panel, &mut agents, key(code))
    };
    assert_eq!(
        at(0, KeyCode::Enter),
        PanelEvent::OpenWorkflow("run-0".into())
    );
    assert_eq!(at(1, KeyCode::Enter), PanelEvent::Open(session("s-001")));
    assert_eq!(
        at(2, KeyCode::Right),
        PanelEvent::OpenWorkflow("run-0#task-002".into())
    );
    assert_eq!(at(3, KeyCode::Enter), PanelEvent::Open(session("s-002")));
    assert_eq!(
        at(0, KeyCode::Char('x')),
        PanelEvent::StopRun("run-0".into())
    );
    assert_eq!(
        at(3, KeyCode::Char('x')),
        PanelEvent::Stop {
            agent: session("s-002"),
            name: "Build".into(),
        }
    );
    assert_eq!(at(2, KeyCode::Char('x')), PanelEvent::Unhandled);
}

#[test]
fn enter_on_run_row_opens_workflow() {
    let mut panel = runs(2);
    panel.show_agents = true;
    let mut agents = AgentPanel::default();
    let mut codex = AgentRow::new("codex", "worker · task-002", AgentState::Working);
    codex.key = Some("s-002".into());
    codex.run = Some("run-0".into());
    agents.rows.push(codex);

    // Focus that came through the agents' rows moves to the panel as a whole.
    agents.focus();
    assert_eq!(
        handle_panel_key(&mut panel, &mut agents, key(KeyCode::Enter)),
        PanelEvent::OpenWorkflow("run-0".into())
    );
    assert!(!agents.has_focus() && panel.has_focus());
    assert_eq!(
        handle_panel_key(&mut panel, &mut agents, key(KeyCode::Down)),
        PanelEvent::Moved
    );
    let codex = AgentRef {
        run: Some("run-0".into()),
        session: "s-002".into(),
    };
    assert_eq!(
        handle_panel_key(&mut panel, &mut agents, key(KeyCode::Right)),
        PanelEvent::Open(codex.clone())
    );
    assert_eq!(
        handle_panel_key(&mut panel, &mut agents, key(KeyCode::Char('x'))),
        PanelEvent::Stop {
            agent: codex,
            name: "codex".into(),
        }
    );
    assert_eq!(
        handle_panel_key(&mut panel, &mut agents, key(KeyCode::Down)),
        PanelEvent::Moved
    );
    assert_eq!(
        handle_panel_key(&mut panel, &mut agents, key(KeyCode::Enter)),
        PanelEvent::OpenWorkflow("run-1".into())
    );

    // A run's row stops the run, never an agent that happens to share its name.
    let theme = theme();
    let t0 = Instant::now();
    let ctx = RenderCtx::new(100, &theme, Clock::still(t0));
    let before = plain_text(&panel_lines(&panel, &agents, &ctx));
    assert_eq!(
        handle_panel_key(&mut panel, &mut agents, key(KeyCode::Char('x'))),
        PanelEvent::StopRun("run-1".into())
    );
    assert_eq!(plain_text(&panel_lines(&panel, &agents, &ctx)), before);
    assert_eq!(
        handle_panel_key(&mut panel, &mut agents, key(KeyCode::Esc)),
        PanelEvent::Leave
    );
    assert!(!panel.has_focus());
    assert_eq!(
        handle_panel_key(&mut panel, &mut agents, key(KeyCode::Enter)),
        PanelEvent::Unhandled
    );
}

#[test]
fn agent_rows_are_known_by_run_and_session() {
    let mut panel = runs(2);
    panel.show_agents = true;
    let mut agents = AgentPanel::default();
    let row = |run: &str| {
        let mut row = AgentRow::new("claude-code", "worker", AgentState::Working);
        row.key = Some("s-001".into());
        row.run = Some(run.into());
        row
    };
    agents.rows = vec![row("run-0"), row("run-1")];
    panel.focus();
    // run-0, its agent, run-1, then run-1's agent.
    panel.selected = Some(3);
    let id = panel.selected_id(&agents);
    assert_eq!(
        id,
        Some(PanelRowId::Agent(AgentRef {
            run: Some("run-1".into()),
            session: "s-001".into(),
        }))
    );
    // run-0's agent finished, so run-1's rows move up one.
    agents.rows.remove(0);
    panel.reselect(&agents, id.as_ref());
    assert_eq!(panel.selected, Some(2));
    assert_eq!(
        handle_panel_key(&mut panel, &mut agents, key(KeyCode::Enter)),
        PanelEvent::Open(AgentRef {
            run: Some("run-1".into()),
            session: "s-001".into(),
        })
    );
}

/// Under a run's row each agent is a row of Claude Code's list, a step further in: its
/// label, what it's doing, and how many tokens. Your conversation's row comes first.
#[test]
fn agents_under_a_run_read_by_label_and_what_they_do() {
    let mut theme = Theme::dark().with_level(ColorLevel::None);
    theme.glyphs = Glyphs::UNICODE;
    let t0 = Instant::now();
    let mut panel = runs(1);
    panel.show_agents = true;
    panel.max_rows = 6;
    let mut agents = AgentPanel::default();
    agents.rows.push(AgentRow::main());
    for (label, doing) in [
        ("build:flag", "Update(src/export.rs)"),
        ("review:flag", "Reading the diff"),
    ] {
        let mut row =
            AgentRow::new("claude-code", doing, AgentState::Working).with_label(label, label);
        row.run = Some("run-0".into());
        row.tokens_down = 18_100;
        agents.rows.push(row);
    }
    let at = |width| {
        let ctx = RenderCtx::new(width, &theme, Clock::still(t0));
        plain_text(&panel_lines(&panel, &agents, &ctx))
    };
    let wide = at(100);
    let rows: Vec<&str> = wide.lines().collect();
    assert_eq!(rows[0], "  ● main", "{wide}");
    assert!(rows[1].starts_with("  ⣾  workflow-0"), "{wide}");
    assert!(
        rows[2].starts_with("    ○ build:flag   Update(src/export.rs)")
            && rows[2].ends_with("↓  18.1k tokens"),
        "{wide}"
    );
    assert!(
        rows[3].starts_with("    ○ review:flag  Reading the diff"),
        "{wide}"
    );
    // What it's doing reads in the text color, and the stats dim.
    let colored = self::theme();
    let ctx = RenderCtx::new(100, &colored, Clock::still(t0));
    let lines = panel_lines(&panel, &agents, &ctx);
    let style_of = |text: &str| {
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.contains(text))
            .unwrap_or_else(|| panic!("{text} shows"))
            .style
    };
    assert_eq!(style_of("Reading the diff"), colored.style(Role::Text));
    assert_eq!(style_of("18.1k tokens"), colored.style(Role::Dim));
}

/// The main agent's own subagents aren't a run's agents: they list under `main`, before
/// the runs, whether or not `ui.panel.agents` lists each run's.
#[test]
fn the_conversations_subagents_come_first_and_always_show() {
    let theme = theme();
    let t0 = Instant::now();
    let mut panel = runs(1);
    panel.max_rows = 6;
    let mut agents = AgentPanel::default();
    agents.rows.push(AgentRow::main());
    let mut sub = AgentRow::new("Count slowly", "Reading src", AgentState::Working)
        .with_label("Count slowly", "Count slowly");
    sub.kind = RowKind::Subagent;
    sub.run = Some("conversation".into());
    sub.key = Some("toolu_a".into());
    agents.rows.push(sub);
    let mut worker = AgentRow::new("codex", "Update(src/a.rs)", AgentState::Working)
        .with_label("build:flag", "build:flag");
    worker.run = Some("run-0".into());
    agents.rows.push(worker);
    let listed = |panel: &WorkflowPanel| {
        let ctx = RenderCtx::new(100, &theme, Clock::still(t0));
        let text = plain_text(&panel_lines(panel, &agents, &ctx));
        text.lines()
            .map(|row| row.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
    };
    assert_eq!(listed(&panel), ["● main", "○ Count", "⣾ workflow-0"]);
    panel.show_agents = true;
    assert_eq!(
        listed(&panel),
        ["● main", "○ Count", "⣾ workflow-0", "○ build:flag"]
    );
    // ↓ from the input lands on the first row past main, and typing there is a message.
    panel.focus_past_main(&agents);
    assert_eq!(
        panel.selected_id(&agents),
        Some(PanelRowId::Agent(AgentRef {
            run: Some("conversation".into()),
            session: "toolu_a".into()
        }))
    );
    assert_eq!(listed(&panel)[..2], ["○ main", "● Count"]);
}

/// Rows under a row hang on `⎿` from the first of them, a level further in for each, with
/// their bars in the request's column; the run's agents come after them.
#[test]
fn the_rows_under_a_request_follow_it() {
    let mut theme = Theme::dark().with_level(ColorLevel::None);
    theme.glyphs = Glyphs::UNICODE;
    let t0 = Instant::now();
    let mut panel = runs(1);
    panel.show_agents = true;
    panel.max_rows = 8;
    let mut inner = WorkflowRow::new(
        "run-0#add-due-dates",
        "add-due-dates",
        WorkflowRowState::Running,
    );
    inner.kind = RunRowKind::Task;
    inner.depth = 1;
    inner.count = Some((1, 3));
    let mut deeper = inner.clone();
    deeper.key = "run-0#date-cases".into();
    deeper.name = "date-cases".into();
    deeper.depth = 2;
    panel.rows.extend([inner, deeper]);
    let mut agents = AgentPanel::default();
    agents.rows.push(AgentRow::main());
    let mut agent = AgentRow::new("claude-code", "Working", AgentState::Working)
        .with_label("plan:due-dates", "plan:due-dates");
    agent.run = Some("run-0".into());
    agent.key = Some("s-001".into());
    agents.rows.push(agent);
    let ctx = RenderCtx::new(100, &theme, Clock::still(t0));
    let text = plain_text(&panel_lines(&panel, &agents, &ctx));
    let rows: Vec<&str> = text.lines().collect();
    assert!(rows[1].starts_with("  ⣾  workflow-0"), "{text}");
    assert!(rows[2].starts_with("    ⎿ ⣾  add-due-dates"), "{text}");
    assert!(rows[3].starts_with("        ⎿ ⣾  date-cases"), "{text}");
    let bar = |row: &str| row.chars().position(|cell| cell == '▰' || cell == '▱');
    assert_eq!(
        bar(rows[1]),
        bar(rows[2]),
        "one column for every bar: {text}"
    );
    assert_eq!(
        bar(rows[1]),
        bar(rows[3]),
        "one column for every bar: {text}"
    );
    assert!(rows[4].starts_with("    ○ plan:due-dates"), "{text}");
    assert_eq!(rows.len(), 5, "and listed once: {text}");
}

/// Two runs at once: a row under a request follows its own request's row.
#[test]
fn a_row_under_a_request_follows_its_own_request() {
    let mut theme = Theme::dark().with_level(ColorLevel::None);
    theme.glyphs = Glyphs::UNICODE;
    let t0 = Instant::now();
    let mut panel = runs(2);
    panel.max_rows = 12;
    let mut inner = WorkflowRow::new("run-1#fix-median", "fix-median", WorkflowRowState::Running);
    inner.kind = RunRowKind::Task;
    inner.depth = 1;
    panel.rows.push(inner);
    let ctx = RenderCtx::new(100, &theme, Clock::still(t0));
    let text = plain_text(&panel_lines(&panel, &AgentPanel::default(), &ctx));
    let rows: Vec<&str> = text.lines().collect();
    let at = |part: &str| rows.iter().position(|row| row.contains(part));
    assert_eq!(
        at("fix-median"),
        at("workflow-1").map(|run| run + 1),
        "{text}"
    );
}

/// A task's row that is one session asks before it stops that session, and the ask belongs to
/// the row it was pressed on, so the bottom line shows it there and a second `x` there stops.
#[test]
fn a_tasks_row_asks_before_it_stops_its_session() {
    let mut panel = the_pictures_panel();
    panel.rows[1].key = "run-0#task-001".into();
    panel.rows[1].session = Some(AgentRef {
        run: Some("run-0".into()),
        session: "s-001".into(),
    });
    panel.selected = Some(1);
    let mut agents = AgentPanel::default();
    let mut asked = None;
    let mut press = |asked: &mut Option<StopAsk>| {
        let event = handle_panel_key(&mut panel, &mut agents, key(KeyCode::Char('x')));
        let row = panel.selected_id(&agents);
        ask_before_stop(asked, event, row)
    };
    assert_eq!(press(&mut asked), PanelEvent::StopAsked);
    let ask = asked.clone().expect("an ask");
    assert_eq!(ask.row, PanelRowId::Run("run-0#task-001".into()));
    assert_eq!(ask.hint, "x again to stop Due date field · esc to keep");
    assert!(matches!(press(&mut asked), PanelEvent::Stop { .. }));
}

/// A row whose question waits on you, a task in its own session or a seat whose message is
/// held, brings that question up over the run's view in place of opening its session.
#[test]
fn enter_on_a_row_that_waits_on_you_brings_up_its_question() {
    let session = |key: &str| AgentRef {
        run: Some("run-0".into()),
        session: key.into(),
    };
    let mut panel = the_pictures_panel();
    panel.rows[1].key = "run-0#task-001".into();
    panel.rows[1].session = Some(session("s-001"));
    panel.rows[1].state = WorkflowRowState::NeedsYou;
    panel.rows[1].answers = Some(argonauts_core::base::names::ApprovalId::numbered(4));
    panel.rows[2].key = "run-0#task-002".into();
    panel.rows[2].opens = Some("run-0#wf-002".into());
    panel.rows[2].answers = Some(argonauts_core::base::names::ApprovalId::numbered(5));
    panel.rows[3].key = "run-0#s-002".into();
    panel.rows[3].session = Some(session("s-002"));
    panel.rows[3].state = WorkflowRowState::NeedsYou;
    let mut agents = AgentPanel::default();
    let mut at = |row: usize| {
        panel.selected = Some(row);
        handle_panel_key(&mut panel, &mut agents, key(KeyCode::Enter))
    };
    assert_eq!(at(1), PanelEvent::OpenWorkflow("run-0#task-001".into()));
    assert_eq!(at(2), PanelEvent::OpenWorkflow("run-0#task-002".into()));
    // A mark with no question of the row behind it, so Enter opens the session.
    assert_eq!(at(3), PanelEvent::Open(session("s-002")));
}

/// A task whose work runs in a workflow opens the run's view on that workflow.
#[test]
fn enter_on_a_task_opens_the_workflow_it_works_in() {
    let mut panel = the_pictures_panel();
    panel.rows[2].key = "run-0#task-002".into();
    panel.rows[2].opens = Some("run-0#wf-002".into());
    panel.selected = Some(2);
    let mut agents = AgentPanel::default();
    assert_eq!(
        handle_panel_key(&mut panel, &mut agents, key(KeyCode::Enter)),
        PanelEvent::OpenWorkflow("run-0#wf-002".into())
    );
}
