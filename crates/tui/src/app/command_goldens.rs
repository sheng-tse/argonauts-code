//! Screens drawn by the printers that draw them live, from their pictures' example numbers, and
//! compared cell for cell with `tests/golden/<name>.txt` and its colours at the pictures' width of
//! 118 columns: the screen right after `/clear`, `/update` while it works and once it's done,
//! and the line a workflow starts with, over its row. The inline screen's start over is checked
//! on a terminal of its own.

use std::time::{Duration, Instant};

use argonauts_core::AgentEvent;
use argonauts_core::app::{AppEvent, CommandWork};
use argonauts_core::base::names::{ConversationId, ModelRef};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use ratatui::text::Line;

use super::{compose_live, sticky};
use crate::app::model::{BottomLine, CliUse, LinePart, ViewModel, WindowUse};
use crate::app::options::UiOptions;
use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::theme::{Glyphs, Role, Theme};
use crate::bottom::{ApprovalMode, VimMode};
use crate::bridge::Bridge;
use crate::cells::cell::{AssistantMessage, HistoryCell};
use crate::testing::pictures::picture_check::assert_rows_match;

const WIDTH: u16 = 118;

fn picture(name: &str) -> (String, String) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/");
    let read = |ext: &str| std::fs::read_to_string(format!("{dir}{name}.{ext}")).unwrap();
    let txt = read("txt")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    (txt, read("html"))
}

/// The dark theme with the Unicode glyphs the pictures draw: `●` for a report.
fn unicode() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

/// As on macOS, where an item starts with Claude Code's `⏺`, as the Starting line's picture
/// draws it.
fn on_macos() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::MACOS;
    theme
}

/// The screen as the bridge leaves it, drawn in full as the full screen draws a frame.
struct Screen {
    bridge: Bridge,
    model: ViewModel,
    started: Instant,
    now: Instant,
    theme: Theme,
}

impl Screen {
    fn new(theme: Theme) -> Self {
        let started = Instant::now();
        Self {
            bridge: Bridge::new(&UiOptions::default()),
            model: ViewModel::default(),
            started,
            now: started,
            theme,
        }
    }

    fn send(&mut self, event: AppEvent) {
        self.bridge.on_event(&mut self.model, &event, self.now);
    }

    /// What prints now, from the last time the screen started over.
    fn printed(&mut self) -> Vec<Line<'static>> {
        let ctx = RenderCtx::new(WIDTH, &self.theme, Clock::still(self.now));
        let mut rows = self.model.history.drain_printable(&ctx);
        if let Some(at) = self.model.history.take_started_over() {
            rows.drain(..at);
        }
        rows
    }

    /// `rows` of screen as the full screen draws them: the conversation, what printed and the
    /// live blocks under it, from the top, or its last rows once it fills the room, then the
    /// live region pinned to the bottom. With them, where the terminal's cursor goes, as
    /// (column, row).
    fn frame(&mut self, printed: Vec<Line<'static>>, rows: usize) -> Frame {
        as_pictured(&mut self.model);
        let ctx = RenderCtx::new(WIDTH, &self.theme, Clock::new(self.started, self.now))
            .with_rows(rows as u16);
        let blocks = self.model.history.live_blocks(&ctx);
        let view = compose_live(
            &blocks,
            &self.model.history,
            &self.model.bottom,
            &ctx,
            rows as u16 - 2,
        );
        let (trailing, pinned) = view.lines.split_at(view.trailing);
        let room = rows.saturating_sub(pinned.len());
        let mut screen = printed;
        screen.extend(trailing.iter().cloned());
        let screen_len = screen.len();
        let mut screen = screen.split_off(screen_len.saturating_sub(room));
        screen.resize(room, Line::default());
        screen.extend(pinned.iter().cloned());
        let (column, row) = view.cursor.expect("the input's cursor");
        let row = usize::from(row)
            .checked_sub(view.trailing)
            .expect("the cursor in the pinned input");
        Frame {
            rows: screen,
            cursor: (usize::from(column), room + row),
        }
    }
}

struct Frame {
    rows: Vec<Line<'static>>,
    cursor: (usize, usize),
}

/// The picture paints the terminal's own cursor as a block, a blank on the text's colour: the
/// block's column and row within the picture's `rows`.
fn block(html: &str, rows: std::ops::Range<usize>) -> (usize, usize) {
    let grid = argonauts_testkit::pictures::picture::grid(html);
    let (row, column) = argonauts_testkit::pictures::picture::find(&grid[rows], |cell| {
        cell.ch == ' ' && cell.background.as_deref() == Some("var(--term-fg)")
    });
    (column, row)
}

/// The input and the line under it as the pictures draw them: Opus 5.5 on the input's rule,
/// Vim's INSERT, auto mode, and the three CLIs' plan usage.
fn as_pictured(model: &mut ViewModel) {
    let bottom = &mut model.bottom;
    bottom.composer.label = Some("Opus 5.5".to_owned());
    bottom.composer.examples_off = true;
    bottom.mode.vim = Some(VimMode::Insert);
    bottom.mode.approval = ApprovalMode::Auto;
    bottom.mode.footer_hints = false;
    let cli = |name: &str, windows: &[(&str, f64)]| {
        let mut cli = CliUse::new(name, Role::for_cli(name));
        cli.windows = windows
            .iter()
            .map(|(label, used)| WindowUse::new(*label, *used))
            .collect();
        cli
    };
    let mut claude = cli("Claude", &[("5h", 0.02), ("7d", 0.87)]);
    claude.main = true;
    let clis = vec![
        claude,
        cli("Codex", &[("7d", 0.03)]),
        cli("agy", &[("7d", 0.0)]),
    ];
    bottom.line = BottomLine {
        right: vec![LinePart::PlanUsage(clis)],
        ..BottomLine::default()
    };
}

/// The names the CLIs gave their models that day.
fn names() -> AppEvent {
    use argonauts_core::look::display_names::DisplayNames;
    use argonauts_core::models::model_catalog::CatalogEntry;
    let mut names = DisplayNames::default();
    let mut opus = CatalogEntry::new(ModelRef::new(
        "claude-code",
        Some("claude-opus-5-5".to_owned()),
    ));
    opus.name = Some("Opus 5.5".to_owned());
    names.catalog.models.push(opus);
    names
        .clis
        .insert("claude-code".to_owned(), "Claude Code".to_owned());
    AppEvent::Names {
        names: Box::new(names),
    }
}

fn welcome() -> AppEvent {
    AppEvent::Welcome {
        version: "0.0.1".to_owned(),
        repo: "~/code/tasks-cli".to_owned(),
        workflow: "captain".to_owned(),
        agents: Vec::new(),
        runs_going: 0,
        runs_need_you: 0,
        example_prompt: None,
        main_agent: Some(ModelRef::new(
            "claude-code",
            Some("claude-opus-5-5".to_owned()),
        )),
        effort: Some("xhigh".to_owned()),
    }
}

/// After `/clear` the screen starts over: the header as a fresh start prints it, the way back
/// to the old conversation under it, then the empty input and the line under it.
#[test]
fn the_screen_after_clear_is_its_picture() {
    let mut screen = Screen::new(unicode());
    screen.send(names());
    screen.send(welcome());
    let first = screen.printed();
    assert!(!first.is_empty(), "the start's header");
    screen.send(AppEvent::ConversationCleared {
        way_back: Some(ConversationId::numbered(7)),
    });
    let printed = screen.printed();
    let frame = screen.frame(printed, 15);
    let (txt, html) = picture("clear_after");
    assert_rows_match(
        "clear_after",
        &frame.rows,
        (&txt, &html),
        1..16,
        &screen.theme,
    );
    assert_eq!(frame.cursor, block(&html, 1..16));
}

/// On the inline screen, `/clear` sends what the screen showed up into the terminal's
/// scrollback, as a start does, and the header prints again at the top.
#[test]
fn the_inline_screen_starts_over_at_the_top() {
    use ratatui::layout::{Position, Size};

    use crate::cells::cell::UserMessage;
    use crate::terminal::InlineTerminal;

    let (columns, rows) = (60, 16);
    let terminal = InlineTerminal::new(
        vt100::Parser::new(rows, columns, 100),
        Size::new(columns, rows),
        Position::new(0, 0),
    );
    let t0 = Instant::now();
    let mut ui = super::Ui::new(terminal, unicode(), ViewModel::new("conversation"), t0);
    let header = |way_back: Option<&str>| crate::cells::cell::SessionHeader {
        app: "Argonauts Code".to_owned(),
        version: "0.0.1".to_owned(),
        main_agent: Some("Claude Code".to_owned()),
        model: None,
        effort: None,
        workflow: "captain".to_owned(),
        ultra: None,
        folder: "~/repo".to_owned(),
        agents: Vec::new(),
        runs_going: 0,
        runs_need_you: 0,
        way_back: way_back.map(str::to_owned),
    };
    ui.model_mut().history.push(header(None));
    ui.model_mut()
        .history
        .push(UserMessage::new("the old question"));
    ui.frame(t0).unwrap();
    let shown = |ui: &super::Ui<vt100::Parser>| -> Vec<String> {
        let screen = ui.terminal().writer().screen();
        screen
            .rows(0, screen.size().1)
            .map(|row| row.trim_end().to_owned())
            .collect()
    };
    assert!(
        shown(&ui)
            .iter()
            .any(|row| row.contains("the old question"))
    );

    ui.model_mut().history.start_over();
    ui.model_mut()
        .history
        .push(header(Some("conversation-001")));
    ui.frame(t0 + Duration::from_millis(50)).unwrap();
    let rows = shown(&ui);
    assert!(
        !rows.iter().any(|row| row.contains("the old question")),
        "{rows:#?}"
    );
    assert_eq!(rows[0], "", "the top row is blank: {rows:#?}");
    assert!(rows[1].contains("Argonauts Code"), "{rows:#?}");
    assert!(
        rows.iter()
            .any(|row| row.contains("/resume conversation-001 picks the last one up again")),
        "{rows:#?}"
    );
    let screen = ui.terminal_mut().writer_mut().screen_mut();
    screen.set_scrollback(usize::MAX);
    let mut kept = Vec::new();
    for offset in (1..=screen.scrollback()).rev() {
        screen.set_scrollback(offset);
        kept.extend(
            screen
                .rows(0, columns)
                .next()
                .map(|row| row.trim_end().to_owned()),
        );
    }
    assert!(
        kept.iter().any(|row| row.contains("the old question")),
        "the terminal's scrollback keeps the old screen: {kept:#?}"
    );
}

const DOCTOR: [&str; 7] = [
    "Claude Code 2.1.303       newer than checked · works    its flags and start-up message \
     match 2.1.295 · logged in · 4 new commands read from it",
    "Codex 0.163.0             newer than checked            logged in",
    "agy 1.4.2                 newer than checked",
    "OpenCode 2.0.32           new major version             checked with 1.18.34 · compared \
     at start-up",
    "git 2.51.2",
    "Ready with 4 of 4 agent CLIs.",
    "Claude Code's own checkup: /checkup",
];

const UPDATES: [&str; 4] = [
    "Claude Code   2.1.303   up to date",
    "Codex         0.163.0   up to date",
    "agy           1.4.2     up to date",
    "OpenCode      2.0.32    up to date · also through ACP",
];

/// `/update` echoes at once and its working line says it updates, with a word that fits the
/// work, while the Update report gains a row as each CLI is done. Once Doctor has checked, the
/// working line is gone and both reports stand.
#[test]
fn update_as_it_works_and_once_its_done_is_its_picture() {
    let mut screen = Screen::new(unicode());
    screen.send(names());
    screen.send(welcome());
    let mut printed = screen.printed();
    let (txt, html) = picture("update_after");

    // The echo prints as the input sends the command, as every command's does.
    screen
        .model
        .history
        .push(crate::cells::cell::UserMessage::command("/update"));
    screen.send(AppEvent::CommandWorking {
        work: Some(CommandWork::Updating),
    });
    screen.now += Duration::from_secs(2);
    screen.send(AppEvent::ReportSoFar {
        title: "Update".to_owned(),
        lines: vec![UPDATES[0].to_owned()],
    });
    // The picture is the screen's last eleven rows, the conversation above them, at a moment
    // two seconds in when the spinner shows the picture's frame.
    printed.extend(screen.printed());
    let started = screen.started;
    let working = (2_000..3_000)
        .step_by(10)
        .find_map(|ms| {
            screen.now = started + Duration::from_millis(ms);
            let frame = screen.frame(printed.clone(), 14);
            frame.rows[8].to_string().starts_with('✻').then_some(frame)
        })
        .expect("a moment the spinner shows ✻");
    assert_rows_match(
        "update_after",
        &working.rows[3..],
        (&txt, &html),
        2..13,
        &screen.theme,
    );
    let (column, row) = working.cursor;
    assert_eq!((column, row - 3), block(&html, 2..13));

    screen.send(AppEvent::Report {
        title: "Update".to_owned(),
        lines: UPDATES.iter().map(|row| (*row).to_owned()).collect(),
    });
    screen.send(AppEvent::CommandWorking {
        work: Some(CommandWork::Checking),
    });
    screen.send(AppEvent::Report {
        title: "Doctor".to_owned(),
        lines: DOCTOR.iter().map(|row| (*row).to_owned()).collect(),
    });
    screen.send(AppEvent::CommandWorking { work: None });
    printed.extend(screen.printed());
    let done = screen.frame(printed, 22);
    assert_rows_match(
        "update_after",
        &done.rows,
        (&txt, &html),
        17..39,
        &screen.theme,
    );
    assert_eq!(done.cursor, block(&html, 17..39));
}

/// The recorded Captain run whose words the Starting line's picture uses, its placeholders
/// filled in as a run on this machine would have them.
fn captain_run() -> Vec<RunEvent> {
    const RECORDED: &str =
        include_str!("../../../crew/tests/fixtures/runs/captain-review-loop.jsonl");
    let text = [
        ("{run}", "run-20261014-054425-0001"),
        ("{repo}", "/repo"),
        ("{run_dir}", "/state/run"),
        ("{worktrees}", "/worktrees"),
        ("{base_commit}", "eecdf983f2dfa422118f1a999438c399f3d446f8"),
        ("{branch}", "argonauts/run"),
        ("{item}", "money"),
    ]
    .iter()
    .fold(RECORDED.to_owned(), |text, (from, to)| {
        text.replace(from, to)
    });
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a run's journal line"))
        .collect()
}

const PROMPT: &str = "Use a workflow for this, with Codex as the reviewer, and choose everything \
                      else yourself. Fix the money rounding so the three failing tests in \
                      tests/test_money.py pass without changing those tests.";

/// The line the app builds for that run, which its own test checks against the same picture.
const STARTING: &str = "Starting a workflow: Captain, as Claude Code's own, with **GPT 6 Luna on \
                        Codex** reviewing as you asked; **Haiku 5.5 on Claude Code** plans and \
                        builds.";

/// What the main agent said once the run in the Captain run's picture had merged, its
/// underscores escaped as Markdown keeps them.
const MERGED: &str = "The money rounding fix is merged into main as 0a7b4656. parse() now applies \
                      a leading minus to the whole amount and pads a single cents digit, and \
                      format_cents() formats negatives with a leading \"-\". The three failing \
                      tests in tests/test_money.py now pass, all 8 tests pass, and the test file \
                      is unchanged. GPT 6 Luna's review and Gemini's sign-off both approved with \
                      no blocking findings, and the run needed no input from you. The reviewer's \
                      written comments were not captured, so the approval is recorded but its \
                      reasoning isn't visible. The invoice/\\_\\_pycache\\_\\_/ and \
                      tests/\\_\\_pycache\\_\\_/ directories left by the test runs are \
                      untracked, so don't commit them.";

/// The Captain run's picture draws the same Starting line, in red nowhere, and the main
/// agent's answer once the run merged. The picture wraps the answer two cells short of the
/// edge, where the screen wraps text at its full width, so the answer is drawn here as wide as
/// the picture's lines.
#[test]
fn the_captain_runs_start_and_answer_are_its_picture() {
    let theme = on_macos();
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    let (txt, html) = picture("captain_after");
    let mut starting = AssistantMessage::new(STARTING).lines(&ctx);
    starting.push(Line::default());
    assert_rows_match("captain_after", &starting, (&txt, &html), 89..92, &theme);
    let narrower = RenderCtx::new(WIDTH - 2, &theme, Clock::still(Instant::now()));
    let answer = AssistantMessage::new(MERGED).lines(&narrower);
    assert_rows_match("captain_after", &answer, (&txt, &html), 61..67, &theme);
}

/// The Captain run's picture keeps your prompt over the run's end and over its start, cut a
/// cell short of the edge.
#[test]
fn the_captain_runs_prompt_rows_are_its_picture() {
    let theme = on_macos();
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    let (txt, html) = picture("captain_after");
    let words = sticky::words(PROMPT).expect("the prompt's words");
    let prompt = [sticky::prompt_row(&words, &ctx), Line::default()];
    for top in [56, 87] {
        assert_rows_match(
            "captain_after",
            &prompt,
            (&txt, &html),
            top..top + 2,
            &theme,
        );
    }
}

/// Your words as the fork picture keeps them over either start, cut a cell short of the edge
/// as the Starting line's picture cuts them.
#[test]
fn the_fork_starts_prompt_row_is_its_picture() {
    const ASKED: &str = "Run what we just worked out as an orchestrator workflow, with Codex as \
                         the reviewer. Keep the due date optional, and list what is due first.";
    let theme = on_macos();
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    let (txt, html) = picture("fork_start");
    let words = sticky::words(ASKED).expect("the prompt's words");
    let prompt = [sticky::prompt_row(&words, &ctx), Line::default()];
    for top in [2, 31] {
        assert_rows_match("fork_start", &prompt, (&txt, &html), top..top + 2, &theme);
    }
}

/// The Starting line names Captain as Claude Code's own, the reviewer you asked for and the
/// builder, over the Workflow row that prints once Claude Code's script names its phases, with
/// your prompt kept at the top.
#[test]
fn the_starting_line_over_its_row_is_its_picture() {
    let theme = on_macos();
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    let (txt, html) = picture("starting_line_after");
    let words = sticky::words(PROMPT).expect("the prompt's words");
    let prompt = [sticky::prompt_row(&words, &ctx), Line::default()];
    assert_rows_match("starting_line_after", &prompt, (&txt, &html), 0..2, &theme);

    let mut starting = AssistantMessage::new(STARTING).lines(&ctx);
    starting.push(Line::default());
    assert_rows_match(
        "starting_line_after",
        &starting,
        (&txt, &html),
        2..5,
        &theme,
    );

    let mut screen = Screen::new(theme.clone());
    let events = captain_run();
    let shaped = events
        .iter()
        .find(|event| {
            matches!(&event.kind, RunEventKind::Agent(AgentEvent::Workflow { workflow })
                if !workflow.phases.is_empty())
        })
        .expect("a report with phases")
        .seq;
    let mut printed = Vec::new();
    for event in events.into_iter().filter(|event| event.seq <= shaped) {
        if event.seq == 0 {
            screen.send(AppEvent::Mode {
                mode: argonauts_core::app::AppMode::Working {
                    run: event.run_id.clone(),
                },
            });
        }
        screen.send(AppEvent::Run {
            event: Box::new(event),
        });
        printed.extend(screen.printed());
    }
    // The row prints, or stays live while the workflow runs: either way it is drawn here.
    let live_ctx = RenderCtx::new(WIDTH, &theme, Clock::still(screen.now));
    for block in screen.model.history.live_blocks(&live_ctx) {
        printed.extend(block.lines);
    }
    let row = printed
        .iter()
        .position(|line| line.to_string().starts_with("⏺ Workflow("))
        .unwrap_or_else(|| {
            let shown: Vec<String> = printed.iter().map(ToString::to_string).collect();
            panic!("no Workflow row:\n{}", shown.join("\n"))
        });
    assert_rows_match(
        "starting_line_after",
        &printed[row..row + 2],
        (&txt, &html),
        5..7,
        &theme,
    );
}
