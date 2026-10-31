//! Every approved Context drawing, drawn from its own numbers at its size: the main session
//! before its count, while its CLI loads and once it has said, on each CLI, and with a crew.
//! The card above the input says what each of these tabs ends with.

use std::time::Instant;

use argonauts_core::app::views::{ContextRow, ContextView, EndedHow, SessionLeft, WindowFigures};
use argonauts_core::app::{AppEvent, AppMode, ConversationEvent};
use argonauts_core::base::names::{ModelRef, SessionKey, TurnId};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::models::model_catalog::CatalogEntry;
use argonauts_core::runs::events::context::{
    FirstCount, NamedTokens, StartHolds, StartPart, StartedFrom, StartingCount, StartingTools,
};
use argonauts_core::settings::ui::{ModName, UiSettings};
use argonauts_core::{AgentEvent, Spend, Verbosity};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use time::macros::datetime;

use super::super::example;
use crate::ViewModel;
use crate::app::options::UiOptions;
use crate::base::draw::render::{Clock, RenderCtx};
use crate::bridge::Bridge;
use crate::frame::state::{AppState, ContextCount, RunState, SessionState};
use crate::frame::view::{View, ViewCtx};
use crate::panels::{OverviewView, Tab, UsageWatch};
use crate::testing::pictures::approved_screen;

fn names() -> DisplayNames {
    let mut names = DisplayNames::default();
    for (id, name) in [
        ("claude-code", "Claude Code"),
        ("codex", "Codex"),
        ("opencode", "OpenCode"),
        ("antigravity-cli", "agy"),
    ] {
        names.clis.insert(id.to_owned(), name.to_owned());
    }
    let mut entry = CatalogEntry::new(ModelRef::new(
        "opencode",
        Some("ollama/qwen3-coder:30b".to_owned()),
    ));
    entry.name = Some("Qwen3 Coder 30B".to_owned());
    names.catalog.models.push(entry);
    let mut entry = CatalogEntry::new(ModelRef::new(
        "antigravity-cli",
        Some("gemini-3.8-flash-low".to_owned()),
    ));
    entry.name = Some("Gemini 3.8 Flash (Low)".to_owned());
    names.catalog.models.push(entry);
    let mut entry = CatalogEntry::new(ModelRef::new("antigravity-cli", Some(NO_WINDOW.to_owned())));
    entry.name = Some("Gemini 3.6 Pro".to_owned());
    names.catalog.models.push(entry);
    names
}

fn part(name: &str, tokens: u64, color: &str) -> StartPart {
    StartPart::named(name, tokens, color)
}

const CX_WINDOW: u64 = 872_000;
/// Argonauts Code's own tools and the lines about them on Claude Code's very first run, to the
/// figures the app's screen rounds them to.
const FIRST_APP_TOOLS: u64 = 31_800;
const FIRST_APP_NOTE: u64 = 889;
/// agy's model whose window no list gives.
const NO_WINDOW: &str = "gemini-3.6-pro";

struct Cli {
    slug: &'static str,
    agent: &'static str,
    model: &'static str,
    window: u64,
    buffer: u64,
    used: u64,
    estimated: bool,
    cli_window: bool,
    version: &'static str,
    first_count: FirstCount,
    own: u64,
    tools: Option<StartingTools>,
}

fn clis() -> Vec<Cli> {
    vec![
        Cli {
            slug: "codex",
            agent: "codex",
            model: "gpt-6-luna",
            window: CX_WINDOW,
            buffer: 0,
            used: 61_400,
            estimated: true,
            cli_window: false,
            version: "0.163.0",
            first_count: FirstCount::Turn,
            own: 15_236,
            tools: None,
        },
        Cli {
            slug: "opencode",
            agent: "opencode",
            model: "ollama/qwen3-coder:30b",
            window: 262_144,
            buffer: 0,
            used: 23_700,
            estimated: false,
            cli_window: true,
            version: "2.0.26",
            first_count: FirstCount::Step,
            own: 7_743,
            tools: None,
        },
        Cli {
            slug: "agy",
            agent: "antigravity-cli",
            model: "gemini-3.8-flash-low",
            window: 1_048_576,
            buffer: 0,
            used: 38_900,
            estimated: false,
            cli_window: false,
            version: "1.4.4",
            first_count: FirstCount::Step,
            own: 4_226,
            tools: Some(StartingTools {
                named: 60,
                read_only: Some(5),
            }),
        },
    ]
}

fn start_of(cli: &Cli, from: StartedFrom) -> StartingCount {
    StartingCount {
        agent: cli.agent.to_owned(),
        model: Some(cli.model.to_owned()),
        window: Some(cli.window),
        buffer: cli.buffer,
        parts: vec![
            StartPart::holding(StartHolds::OwnPromptAndTools, cli.own, "promptBorder"),
            StartPart::holding(StartHolds::InstructionFiles, 1_200, "claude"),
        ],
        files: vec![NamedTokens {
            name: "AGENTS.md".to_owned(),
            tokens: 1_200,
        }],
        tools: cli.tools,
        from,
        first_count: Some(cli.first_count),
    }
}

fn claude_start(from: StartedFrom, first: bool) -> StartingCount {
    // A very first run counts what the made-up home holds on Claude Code 2.1.289's recorded
    // start; a kept start is the made-up setup's last session.
    let (prompt, tools, mcp_tools, mcp_note, skills) = if first {
        (6_888, 15_135, FIRST_APP_TOOLS, FIRST_APP_NOTE, 1_656)
    } else {
        (4_380, 19_380, 2_115, 318, 2_104)
    };
    StartingCount {
        agent: "claude-code".to_owned(),
        model: Some("claude-opus-5-5".to_owned()),
        window: Some(1_000_000),
        buffer: 33_000,
        parts: vec![
            part("System prompt", prompt, "promptBorder"),
            part("System tools", tools, "inactive"),
            part("MCP tools", mcp_tools, "cyan_FOR_SUBAGENTS_ONLY"),
            part(
                "MCP server instructions",
                mcp_note,
                "green_FOR_SUBAGENTS_ONLY",
            ),
            part("Memory files", 503, "claude"),
            part("Skills", skills, "warning"),
        ],
        files: Vec::new(),
        tools: None,
        from,
        first_count: Some(FirstCount::Answer),
    }
}

fn main_row(
    agent: &str,
    model: &str,
    used: u64,
    window: Option<u64>,
    counts: bool,
    estimated: bool,
) -> ContextRow {
    ContextRow {
        run: None,
        session: SessionKey::numbered(100),
        label: "main".to_owned(),
        worker: ModelRef::new(agent, Some(model.to_owned())),
        used,
        window,
        cap: None,
        tool_calls: 0,
        spent: Spend::default(),
        rotations: 0,
        estimated,
        counts,
        categories: Vec::new(),
        recorded: None,
        ended: None,
        answered_for: None,
    }
}

struct Case {
    file: String,
    count: Option<ContextCount>,
    view: ContextView,
    worker: Option<ModelRef>,
    running: bool,
    crew: bool,
    every_list: bool,
}

fn cases(size: &str) -> Vec<Case> {
    let mut out = Vec::new();
    let cc = example::claude_code_alone();
    let recording = |v: &str| StartedFrom::Recording {
        version: v.to_owned(),
    };
    let cc_worker = Some(ModelRef::new(
        "claude-code",
        Some("claude-opus-5-5".to_owned()),
    ));
    out.push(Case {
        file: format!("context/claude-code@{size}.txt"),
        count: cc.count.clone(),
        view: cc.view.clone(),
        worker: cc_worker.clone(),
        running: false,
        crew: false,
        every_list: false,
    });
    for (state, start, row, running) in [
        (
            "before",
            claude_start(StartedFrom::LastSession, false),
            false,
            false,
        ),
        (
            "first",
            claude_start(recording("2.1.289"), true),
            false,
            false,
        ),
        (
            "loading",
            claude_start(StartedFrom::LastSession, false),
            true,
            true,
        ),
    ] {
        let mut view = ContextView::default();
        if row {
            view.rows.push(main_row(
                "claude-code",
                "claude-opus-5-5",
                0,
                Some(1_000_000),
                true,
                false,
            ));
        }
        view.main_start = Some(start);
        out.push(Case {
            file: format!("context/claude-code-{state}@{size}.txt"),
            count: None,
            view,
            worker: cc_worker.clone(),
            running,
            crew: false,
            every_list: false,
        });
    }
    for cli in clis() {
        let worker = Some(ModelRef::new(cli.agent, Some(cli.model.to_owned())));
        for (state, from, row_used, running) in [
            ("", StartedFrom::LastSession, Some(cli.used), false),
            ("-before", StartedFrom::LastSession, None, false),
            ("-first", recording(cli.version), None, false),
            ("-loading", StartedFrom::LastSession, Some(0), true),
        ] {
            let mut view = ContextView::default();
            if let Some(used) = row_used {
                view.rows.push(main_row(
                    cli.agent,
                    cli.model,
                    used,
                    // The app's row has a window only where the CLI says it.
                    cli.cli_window.then_some(cli.window),
                    false,
                    cli.estimated,
                ));
            }
            if cli.cli_window {
                view.main_window = Some(WindowFigures {
                    cli: Some(cli.window),
                    ..Default::default()
                });
            }
            view.main_start = Some(start_of(&cli, from));
            out.push(Case {
                file: format!("context/{}{state}@{size}.txt", cli.slug),
                count: None,
                view,
                worker: worker.clone(),
                running,
                crew: false,
                every_list: false,
            });
        }
    }
    // agy's very first run on a model whose window no list gives.
    let agy = clis()
        .into_iter()
        .find(|cli| cli.slug == "agy")
        .expect("agy");
    let mut start = start_of(&agy, recording(agy.version));
    start.model = Some(NO_WINDOW.to_owned());
    start.window = None;
    out.push(Case {
        file: format!("context/agy-no-window@{size}.txt"),
        count: None,
        view: ContextView {
            main_start: Some(start),
            ..ContextView::default()
        },
        worker: Some(ModelRef::new(agy.agent, Some(NO_WINDOW.to_owned()))),
        running: false,
        crew: false,
        every_list: false,
    });
    // Claude Code with a run at work.
    let mut view = cc.view.clone();
    let run = example::run();
    let crew = [
        (
            "plan",
            "claude-code/claude-haiku-5-5",
            13_300,
            1_000_000,
            true,
        ),
        (
            "build:per-api-key",
            "codex/gpt-6-luna",
            30_500,
            CX_WINDOW,
            false,
        ),
        (
            "build:cap-bucket-refills",
            "claude-code/claude-haiku-5-5",
            21_000,
            1_000_000,
            false,
        ),
    ];
    for (n, (label, worker, used, window, ended)) in crew.into_iter().enumerate() {
        let worker: ModelRef = worker.parse().expect("a model");
        view.rows.push(ContextRow {
            run: Some(run.clone()),
            session: SessionKey::numbered(n as u32 + 1),
            label: label.to_owned(),
            counts: worker.agent == "claude-code",
            worker,
            used,
            window: Some(window),
            cap: None,
            tool_calls: 6,
            spent: Spend::default(),
            rotations: 0,
            estimated: false,
            categories: Vec::new(),
            recorded: None,
            ended: ended.then_some(SessionLeft {
                at: datetime!(2026-10-20 12:30 UTC),
                how: EndedHow::Done,
                fallback_for: None,
            }),
            answered_for: None,
        });
    }
    for (file, every_list) in [("with-crew", false), ("crew-list", true)] {
        out.push(Case {
            file: format!("context/{file}@{size}.txt"),
            count: cc.count.clone(),
            view: view.clone(),
            worker: cc_worker.clone(),
            running: true,
            crew: true,
            every_list,
        });
    }
    out
}

fn state_of(case: &Case, names: &DisplayNames) -> AppState {
    let mut state = AppState::default();
    state.names = names.clone();
    state.conversation.context = case.count.clone();
    state.conversation.worker = case.worker.clone();
    if case.running {
        state.mode = Some(AppMode::Answering {
            turn: argonauts_core::base::names::TurnId::numbered(1),
        });
    }
    if case.crew {
        let mut run = RunState::new(example::run());
        run.title = example::RUN_TITLE.to_owned();
        for row in case.view.rows.iter().filter(|row| row.run.is_some()) {
            let session = SessionState::new(
                row.session.clone(),
                row.worker.clone(),
                row.worker.agent.clone(),
                argonauts_core::base::names::RoleName::Worker,
                example::now(),
            );
            run.sessions.insert(row.session.clone(), session);
        }
        state.runs.insert(example::run(), run);
    }
    state
}

/// The panel docked at the bottom of a `cols` × `rows` screen, as the app draws it there. With
/// every list open, the tab is paged down from its tabs, which shows no pointer, until the
/// crew's last row is in view.
fn panel(case: &Case, cols: u16, rows: u16) -> Buffer {
    let theme = approved_screen::theme();
    let names = names();
    let state = state_of(case, &names);
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let mut view = OverviewView::new(
        Tab::Context,
        UsageWatch::default(),
        crate::panels::view::NoConfig::boxed(),
    )
    .at(example::now())
    .showing_paths_from(example::claude_code_alone().places.clone())
    .with_context(case.view.clone());
    if case.every_list {
        view = view.with_every_list();
    }
    let height = view
        .dock_height(Size::new(cols, rows), &ctx)
        .expect("the panel docks");
    let draw = |view: &mut OverviewView| {
        let mut buf = Buffer::empty(Rect::new(0, 0, cols, height));
        view.render(buf.area, &mut buf, &ctx);
        buf
    };
    let mut buf = draw(&mut view);
    if case.every_list {
        for _ in 0..40 {
            if shows(&buf, "└ build:cap-bucket-refills") {
                break;
            }
            view.handle_key(KeyEvent::from(KeyCode::PageDown), &ctx);
            buf = draw(&mut view);
        }
    }
    buf
}

fn shows(buf: &Buffer, text: &str) -> bool {
    (0..buf.area.height).any(|y| {
        let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
        row.contains(text)
    })
}

/// Each state at both sizes, docked where the app docks the panel, held to its drawing.
#[test]
fn every_context_state_matches_its_drawing() {
    for (cols, rows) in [(120u16, 38u16), (177, 40)] {
        for case in cases(&format!("{cols}x{rows}")) {
            let buf = panel(&case, cols, rows);
            let top = rows - buf.area.height;
            approved_screen::assert_matches(&case.file, &buf, (cols, rows), top);
        }
    }
}

/// The card the drawing `file`'s tab ends with, from the row that opens it down to the one
/// that closes it, or to the screen's last row of it where the card runs past the panel.
fn last_card(file: &str) -> Vec<String> {
    let path = argonauts_testkit::approved::dir().join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{file}: {err}"));
    let rows: Vec<&str> = text
        .lines()
        .skip_while(|line| *line != "text")
        .skip(1)
        .take_while(|line| *line != "paint")
        .collect();
    let opens = |row: &&str| row.trim_start().starts_with('╭');
    let top = rows
        .iter()
        .rposition(opens)
        .unwrap_or_else(|| panic!("{file} ends with no card"));
    let inside = rows[top + 1..]
        .iter()
        .take_while(|row| row.trim_start().starts_with('│'))
        .count();
    let closes = rows
        .get(top + 1 + inside)
        .is_some_and(|row| row.trim_start().starts_with('╰'));
    let bottom = top + inside + usize::from(closes);
    rows[top..=bottom]
        .iter()
        .map(|row| row.to_string())
        .collect()
}

/// What a card's row says: its words, without its frame, its fold toggle, its bar or its
/// legend's marks, which a card above the input draws narrower than a tab does.
fn says(row: &str) -> String {
    let drawn = |word: &&str| !word.chars().all(|c| "╭╮╰╯─│[-+]▏▎▍▌▋▊▉█".contains(c));
    row.split_whitespace()
        .filter(drawn)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `agent`'s CLI works out how full its session is from its totals.
fn estimates(agent: &str) -> bool {
    clis().iter().any(|cli| cli.agent == agent && cli.estimated)
}

/// What the screen is told of `case`'s main session before the Context tab opens: its start,
/// and, at work or once its CLI has said how full it is, its turn and that figure.
fn told(case: &Case) -> Vec<AppEvent> {
    let start = case.view.main_start.clone().expect("the main start");
    let cli_window = (case.view.main_window.as_ref()).is_some_and(|window| window.cli.is_some());
    let estimated = estimates(&start.agent);
    let mut events = vec![
        AppEvent::Names {
            names: Box::new(names()),
        },
        AppEvent::MainStart {
            start: Box::new(start),
            cli_window,
            estimated,
        },
    ];
    let main = case.view.rows.iter().find(|row| row.run.is_none());
    if !case.running && main.is_none() {
        return events;
    }
    let turn = TurnId::numbered(1);
    events.push(AppEvent::Conversation {
        event: ConversationEvent::TurnStarted {
            turn: turn.clone(),
            text: "hi".to_owned(),
            worker: case.worker.clone().expect("the main model"),
            read_only: true,
            commit: String::new(),
            uncommitted: Vec::new(),
            settings_sha256: None,
            continues: false,
            opens_a_talk: false,
        },
    });
    if let Some(used) = main.map(|row| row.used).filter(|used| *used > 0) {
        events.push(AppEvent::Conversation {
            event: ConversationEvent::Agent {
                turn,
                event: AgentEvent::ContextFill {
                    used,
                    size: None,
                    sent: None,
                },
            },
        });
    }
    events
}

/// The band above the input with the Context mod pinned, `cols` wide, after `events`.
fn band_after(events: &[AppEvent], cols: u16, rows: u16) -> Vec<String> {
    let theme = approved_screen::theme();
    let mut bridge = Bridge::new(&UiOptions::default());
    let mut model = ViewModel::default();
    let now = Instant::now();
    let mut ui = UiSettings::default();
    ui.above_input.mods = vec![ModName::Context];
    let pinned = AppEvent::UiSettings {
        vim: false,
        verbosity: Verbosity::Full,
        ui: Some(Box::new(ui)),
    };
    for event in std::iter::once(&pinned).chain(events) {
        bridge.on_event(&mut model, event, now);
    }
    let ctx = RenderCtx::new(cols, &theme, Clock::still(now));
    let lines = model
        .bottom
        .band
        .lines(&ctx, usize::from(rows), false, false);
    crate::base::text::plain_text(&lines)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Before the main session's first count, on a very first run, while its CLI loads, and for
/// a CLI that says only how full the session is, the card above the input says what the
/// Context tab ends with, row for row, on every CLI and before the tab has ever opened.
#[test]
fn the_card_above_the_input_says_what_the_tab_ends_with() {
    for (cols, rows) in [(120u16, 38u16), (177, 40)] {
        let started = cases(&format!("{cols}x{rows}"))
            .into_iter()
            .filter(|case| case.count.is_none() && !case.crew);
        for case in started {
            let tab: Vec<String> = last_card(&case.file).iter().map(|row| says(row)).collect();
            let band: Vec<String> = band_after(&told(&case), cols, rows)
                .iter()
                .take(tab.len())
                .map(|row| says(row))
                .collect();
            assert_eq!(band, tab, "{}", case.file);
        }
    }
}
