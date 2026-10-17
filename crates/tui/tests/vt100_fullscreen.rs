//! The fullscreen screen through a vt100 parser: every frame is one synchronized update on the
//! alternate screen, the main screen is never written, and the frame is sound at every width,
//! scrolled up or not.

mod common;

use std::io::{self, Write};
use std::time::{Duration, Instant};

use argonauts_core::app::UiCommand;
use argonauts_tui::keymap::Keymap;
use argonauts_tui::model::{
    AgentReady, AssistantMessage, QuestionOption, QuestionPrompt, SessionHeader, ToolCall,
    ToolItem, UserMessage, ViewModel, WorkingLine,
};
use argonauts_tui::theme::Role;
use argonauts_tui::views::help::HelpView;
use argonauts_tui::{InlineTerminal, Ui, UiAction, stability};
use common::{t0, theme};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Size};
use ratatui::style::Color;

const BEGIN: &str = "\x1b[?2026h";
const END: &str = "\x1b[?2026l";
const ROWS: u16 = 24;
const SHELL_LINE: &str = "$ argonauts";

/// Keeps each flush's bytes apart, so a test can tell frames from stray writes.
struct Recorder {
    parser: vt100::Parser,
    pending: Vec<u8>,
    flushes: Vec<String>,
}

impl Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.parser.process(buf);
        self.pending.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.pending.is_empty() {
            let bytes = std::mem::take(&mut self.pending);
            self.flushes
                .push(String::from_utf8_lossy(&bytes).into_owned());
        }
        Ok(())
    }
}

struct Fullscreen {
    ui: Ui<Recorder>,
    now: Instant,
    cols: u16,
}

impl Fullscreen {
    /// The shell printed one line before the start, and the cursor waits under it.
    fn new(cols: u16) -> Self {
        crossterm::style::force_color_output(true);
        let mut parser = vt100::Parser::new(ROWS, cols, 0);
        parser.process(format!("{SHELL_LINE}\r\n").as_bytes());
        let recorder = Recorder {
            parser,
            pending: Vec::new(),
            flushes: Vec::new(),
        };
        let terminal = InlineTerminal::new(recorder, Size::new(cols, ROWS), Position::new(0, 1));
        let mut ui = Ui::new(terminal, theme(), ViewModel::new("fullscreen"), t0());
        ui.use_fullscreen();
        Self {
            ui,
            now: t0(),
            cols,
        }
    }

    /// Runs a frame and checks it wrote nothing, or exactly one synchronized update.
    fn frame(&mut self) {
        self.now += Duration::from_millis(20);
        let before = self.ui.terminal().writer().flushes.len();
        self.ui.frame(self.now).unwrap();
        let writer = self.ui.terminal().writer();
        assert!(writer.pending.is_empty(), "{:?}", writer.pending);
        let new = &writer.flushes[before..];
        assert!(new.len() <= 1, "one write per frame: {new:#?}");
        if let Some(frame) = new.first() {
            assert!(frame.starts_with(BEGIN), "{frame:?}");
            assert!(frame.ends_with(END), "{frame:?}");
        }
    }

    fn key(&mut self, code: KeyCode) {
        let key = KeyEvent::new(code, KeyModifiers::NONE);
        self.ui.handle_event(Event::Key(key), self.now);
    }

    fn wheel(&mut self, kind: MouseEventKind) {
        let mouse = MouseEvent {
            kind,
            column: 4,
            row: 4,
            modifiers: KeyModifiers::NONE,
        };
        self.ui.handle_event(Event::Mouse(mouse), self.now);
    }

    fn mouse(&mut self, kind: MouseEventKind, (column, row): (u16, u16)) -> Vec<UiAction> {
        let mouse = MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        self.ui.handle_event(Event::Mouse(mouse), self.now)
    }

    /// Presses at `from`, drags to `to`, and lets go there, drawing as a terminal would.
    fn drag(&mut self, from: (u16, u16), to: (u16, u16)) -> Vec<UiAction> {
        self.mouse(MouseEventKind::Down(MouseButton::Left), from);
        self.frame();
        self.mouse(MouseEventKind::Drag(MouseButton::Left), to);
        self.frame();
        let actions = self.mouse(MouseEventKind::Up(MouseButton::Left), to);
        self.frame();
        actions
    }

    /// `times` quick presses on one cell, and what the last one copied.
    fn click(&mut self, at: (u16, u16), times: usize) -> Vec<UiAction> {
        let mut actions = Vec::new();
        for _ in 0..times {
            self.mouse(MouseEventKind::Down(MouseButton::Left), at);
            actions = self.mouse(MouseEventKind::Up(MouseButton::Left), at);
        }
        self.frame();
        actions
    }

    fn press(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Vec<UiAction> {
        self.ui
            .handle_event(Event::Key(KeyEvent::new(code, modifiers)), self.now)
    }

    /// The first row holding `needle`.
    fn row_of(&self, needle: &str) -> u16 {
        let row = self
            .rows()
            .iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} on screen:\n{}", self.shown()));
        u16::try_from(row).unwrap()
    }

    /// The screen with each run of selected cells in brackets.
    fn marked(&self) -> String {
        let Some(Color::Rgb(r, g, b)) = theme().style(Role::Selection).bg else {
            panic!("the selection has a background");
        };
        let selected = vt100::Color::Rgb(r, g, b);
        let screen = self.parser().screen();
        let mut out = Vec::new();
        for row in 0..ROWS {
            let mut line = String::new();
            let mut inside = false;
            for column in 0..self.cols {
                let Some(cell) = screen.cell(row, column) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let on = cell.bgcolor() == selected;
                if on != inside {
                    line.push(if on { '[' } else { ']' });
                    inside = on;
                }
                let contents = cell.contents();
                line.push_str(if contents.is_empty() { " " } else { contents });
            }
            if inside {
                line.push(']');
            }
            out.push(line.trim_end().to_owned());
        }
        out.join("\n")
    }

    fn parser(&self) -> &vt100::Parser {
        &self.ui.terminal().writer().parser
    }

    fn alternate(&self) -> bool {
        self.parser().screen().alternate_screen()
    }

    fn rows(&self) -> Vec<String> {
        self.parser()
            .screen()
            .rows(0, self.cols)
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    fn shown(&self) -> String {
        self.rows().join("\n")
    }

    /// The frame on screen passes the frame checker.
    fn assert_sound(&self) {
        let buffer = self.ui.terminal().screen_buffer().expect("a drawn frame");
        stability::assert_frames(buffer);
    }
}

fn header() -> SessionHeader {
    let ready = |name: &str, note: &str| AgentReady {
        name: name.into(),
        ready: true,
        note: Some(note.into()),
    };
    SessionHeader {
        app: "Argonauts Code".into(),
        version: "0.0.1".into(),
        main_agent: Some("Claude Code".into()),
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        workflow: "council".into(),
        folder: "~/github/argonauts".into(),
        agents: vec![ready("Claude Code", "2.1.284"), ready("Codex", "0.158.0")],
        runs_going: 0,
        runs_need_you: 0,
    }
}

/// A header, a few turns, and an agent at work: taller than the screen.
fn conversation(f: &mut Fullscreen) {
    let history = &mut f.ui.model_mut().history;
    history.push(header());
    for n in 1..=4 {
        history.push(UserMessage::new(format!(
            "Question {n}: what does the export do?"
        )));
        history.push(AssistantMessage::new(format!(
            "Answer {n}. The export writes **JSON** to stdout, one object per line, and \
             `--pretty` indents it.\n\n- It reads the store\n- It writes each row\n- It flushes"
        )));
    }
    let mut block = ToolCall::agent("GPT on Codex", None, "worker", "Wire --json");
    block.push(ToolItem::text("step one"));
    history.push_active(block);
    f.ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", f.now));
}

#[test]
fn frames_stay_on_the_alternate_screen_and_the_main_one_is_never_written() {
    let mut f = Fullscreen::new(80);
    f.frame();
    assert!(f.alternate());
    assert!(f.ui.terminal().writer().flushes[0].contains("\x1b[?1049h"));
    assert!(f.ui.terminal().writer().flushes[0].contains("\x1b[?1000h\x1b[?1002h\x1b[?1006h"));
    conversation(&mut f);
    for _ in 0..6 {
        f.frame();
    }
    assert!(f.shown().contains("Wire --json"), "{}", f.shown());

    // A question, the transcript, and a view all draw on the same alternate screen.
    f.ui.model_mut().bottom.question = Some(QuestionPrompt::new(
        1,
        "Merge task-002 into main?",
        "Merge task-002?",
        vec![QuestionOption::new("Merge")],
    ));
    f.frame();
    assert!(f.shown().contains("Merge task-002"), "{}", f.shown());
    f.ui.model_mut().bottom.question = None;
    f.ui.handle_event(
        Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        f.now,
    );
    f.frame();
    assert!(
        f.shown().contains("Showing detailed transcript"),
        "{}",
        f.shown()
    );
    f.key(KeyCode::Esc);
    f.frame();
    assert!(f.alternate());
    assert!(!f.shown().contains("Showing detailed transcript"));
    assert!(f.shown().contains("Wire --json"), "{}", f.shown());

    f.ui.finish().unwrap();
    assert!(!f.alternate());
    let rows = f.rows();
    assert_eq!(rows[0], SHELL_LINE);
    assert!(rows[1..].iter().all(String::is_empty), "{rows:#?}");
}

#[test]
fn the_wheel_and_page_keys_scroll_and_new_output_waits_below() {
    let mut f = Fullscreen::new(80);
    conversation(&mut f);
    f.frame();
    assert_eq!(f.ui.scroll_offset(), Some(0));
    let bottom = f.shown();
    assert!(bottom.contains("Wire --json"), "{bottom}");

    f.wheel(MouseEventKind::ScrollUp);
    f.frame();
    assert_eq!(f.ui.scroll_offset(), Some(3));
    // The question being answered is kept at the top, and the last row says how to reach it.
    assert!(
        f.shown().contains("or press ctrl+↑, to jump"),
        "{}",
        f.shown()
    );

    f.key(KeyCode::PageUp);
    f.frame();
    let offset = f.ui.scroll_offset().unwrap();
    assert!(offset > 3, "{offset}");

    // Rows that arrive below keep the view where it is.
    let before = f.rows();
    f.ui.model_mut()
        .history
        .push(UserMessage::new("A late question"));
    f.frame();
    let after = f.rows();
    assert_eq!(after[..10], before[..10]);
    assert!(f.shown().contains("New output below"), "{}", f.shown());

    f.key(KeyCode::PageDown);
    f.key(KeyCode::PageDown);
    f.key(KeyCode::PageDown);
    f.frame();
    assert_eq!(f.ui.scroll_offset(), Some(0));
    assert!(f.shown().contains("A late question"), "{}", f.shown());

    // The inline screen leaves the wheel and the page keys alone.
    let terminal = InlineTerminal::new(Vec::new(), Size::new(80, ROWS), Position::new(0, 0));
    let mut inline = Ui::new(terminal, theme(), ViewModel::new("inline"), t0());
    inline.handle_event(
        Event::Key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)),
        t0(),
    );
    assert_eq!(inline.scroll_offset(), None);
}

#[test]
fn the_frame_is_sound_at_every_width() {
    for cols in [80, 100, 120, 200] {
        let mut f = Fullscreen::new(cols);
        conversation(&mut f);
        f.frame();
        f.assert_sound();
        insta::assert_snapshot!(format!("fullscreen_bottom_{cols}"), f.shown());

        f.key(KeyCode::PageUp);
        f.frame();
        f.assert_sound();
        insta::assert_snapshot!(format!("fullscreen_scrolled_{cols}"), f.shown());
    }
}

fn copied(actions: &[UiAction]) -> Option<&str> {
    match actions {
        [UiAction::Copy(text)] => Some(text),
        _ => None,
    }
}

#[test]
fn a_drag_selects_and_copies_the_text_without_the_screens_glyphs() {
    let mut f = Fullscreen::new(80);
    conversation(&mut f);
    f.frame();
    let answer = f.row_of("● Answer 4.");
    let actions = f.drag((2, answer), (13, answer + 5));
    assert_eq!(
        copied(&actions),
        Some(
            "Answer 4. The export writes JSON to stdout, one object per line, and --pretty \
             indents it.\n\n- It reads the store\n- It writes each row\n- It flushes"
        )
    );
    f.assert_sound();
    insta::assert_snapshot!("selection_after_a_drag", f.marked());

    // A drag back up from the press selects the other way.
    let tool = f.row_of("⎿  step one");
    let actions = f.drag((79, tool), (0, tool - 1));
    assert_eq!(
        copied(&actions),
        Some("GPT on Codex · worker (Wire --json)\n   step one")
    );
}

/// A copy pastes what was written: a wrapped link or command comes back whole, and box lines
/// in an answer stay, since only what the screen draws around the text is left out.
#[test]
fn a_copy_keeps_wrapped_lines_whole_and_box_lines_in_the_text() {
    let mut f = Fullscreen::new(60);
    let url = "https://example.com/a/long/path/that/keeps/going/and/going/until/it/wraps";
    let command = "pytest tests/ -q -k 'test_*.py' --verbose --maxfail=1 --durations=10";
    f.ui.model_mut().history.push(AssistantMessage::new(format!(
        "See {url} for the rest.\n\nRun `{command}` now.\n\n\
         ```\nsrc/\n│   ├── app.rs\n│   └── lib.rs\n```\n\n```\n╭─────╮\n│ app │\n╰─────╯\n```"
    )));
    f.frame();
    let first = f.row_of("● See");
    let last = f.row_of("╰─────╯");
    assert_eq!(f.row_of("now."), f.row_of("Run") + 1, "the command wraps");
    let actions = f.drag((0, first), (59, last));
    let text = copied(&actions).expect("a copy");
    let rows: Vec<&str> = text.lines().map(str::trim).collect();
    assert_eq!(rows[0], format!("See {url} for the rest."), "{text}");
    assert_eq!(rows[2], format!("Run {command} now."), "{text}");
    let code: Vec<&str> = rows[4..]
        .iter()
        .copied()
        .filter(|row| !row.is_empty())
        .collect();
    assert_eq!(
        code,
        [
            "src/",
            "│   ├── app.rs",
            "│   └── lib.rs",
            "╭─────╮",
            "│ app │",
            "╰─────╯"
        ],
        "{text}"
    );
}

/// Output that streams in while you drag would move the text out from under the pointer, so
/// the view holds still until the button comes up, then follows again.
#[test]
fn the_text_under_a_drag_holds_still_while_output_streams_in() {
    let mut f = Fullscreen::new(80);
    conversation(&mut f);
    f.frame();
    let answer = f.row_of("● Answer 4.");
    let before = f.rows();
    f.mouse(MouseEventKind::Down(MouseButton::Left), (2, answer));
    for n in 1..=3 {
        f.ui.model_mut()
            .history
            .push(AssistantMessage::new(format!("Streamed answer {n}.")));
        f.frame();
    }
    assert_eq!(f.rows()[..10], before[..10], "{}", f.shown());
    f.mouse(MouseEventKind::Drag(MouseButton::Left), (11, answer));
    f.frame();
    let actions = f.mouse(MouseEventKind::Up(MouseButton::Left), (11, answer));
    assert_eq!(copied(&actions), Some("Answer 4."));
    f.frame();
    assert!(f.shown().contains("Streamed answer 3."), "{}", f.shown());
    assert_eq!(f.ui.scroll_offset(), Some(0), "following again");

    // A release the terminal never reports can't hold the view for good: a key lets go.
    let before = f.rows();
    f.mouse(MouseEventKind::Down(MouseButton::Left), (2, 2));
    for n in 4..=6 {
        f.ui.model_mut()
            .history
            .push(AssistantMessage::new(format!("Streamed answer {n}.")));
        f.frame();
    }
    assert_eq!(f.rows()[..5], before[..5], "held");
    f.press(KeyCode::Char('x'), KeyModifiers::NONE);
    f.frame();
    assert_ne!(f.rows()[..5], before[..5], "following again");
    assert!(f.shown().contains("Streamed answer 6."), "{}", f.shown());
}

#[test]
fn double_and_triple_clicks_take_a_word_and_a_row() {
    let mut f = Fullscreen::new(80);
    conversation(&mut f);
    f.frame();
    let answer = f.row_of("● Answer 4.");
    let stdout = u16::try_from(f.rows()[usize::from(answer)].find("stdout").unwrap()).unwrap();
    assert_eq!(copied(&f.click((stdout + 1, answer), 1)), None);
    assert!(!f.marked().contains('['), "a click selects nothing");

    f.now += std::time::Duration::from_secs(2);
    assert_eq!(copied(&f.click((stdout + 1, answer), 2)), Some("stdout"));
    let tool = f.row_of("⎿  step one");
    f.now += std::time::Duration::from_secs(2);
    assert_eq!(copied(&f.click((30, tool), 3)), Some("step one"));
    insta::assert_snapshot!("selection_after_a_triple_click", f.marked());
}

#[test]
fn esc_lets_go_of_a_selection_and_does_nothing_else() {
    let mut f = Fullscreen::new(80);
    conversation(&mut f);
    f.frame();
    let answer = f.row_of("● Answer 4.");
    f.drag((2, answer), (20, answer));
    assert!(f.marked().contains('['));

    // Work is going, where Esc would interrupt it.
    let actions = f.press(KeyCode::Esc, KeyModifiers::NONE);
    assert!(actions.is_empty(), "{actions:?}");
    f.frame();
    assert!(!f.marked().contains('['), "{}", f.marked());
    let actions = f.press(KeyCode::Esc, KeyModifiers::NONE);
    assert!(actions.contains(&UiAction::Interrupt), "{actions:?}");

    // The release copied it, so Ctrl+C lets go of it and does what it always does.
    f.drag((2, answer), (7, answer));
    let actions = f.press(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(copied(&actions), None, "{actions:?}");
    f.frame();
    assert!(!f.marked().contains('['));
    assert!(f.shown().contains("again to exit"), "{}", f.shown());
    f.now += Duration::from_secs(2);
    f.frame();

    // Typing lets go of it and still types.
    f.drag((2, answer), (7, answer));
    f.press(KeyCode::Char('x'), KeyModifiers::NONE);
    f.frame();
    assert!(!f.marked().contains('['));
    assert!(f.shown().contains("❯ x"), "{}", f.shown());
}

/// You select an error, then stop the command that's hung: one Ctrl+C stops it.
#[test]
fn ctrl_c_stops_a_running_command_while_text_is_selected() {
    let mut f = Fullscreen::new(80);
    conversation(&mut f);
    f.ui.model_mut().bottom.shell_running = true;
    f.frame();
    let answer = f.row_of("● Answer 4.");
    let actions = f.drag((2, answer), (20, answer));
    assert!(copied(&actions).is_some());
    let actions = f.press(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(actions, [UiAction::Command(UiCommand::StopShell)]);
    f.frame();
    assert!(!f.marked().contains('['), "{}", f.marked());
}

#[test]
fn the_selection_goes_when_the_view_scrolls_away_from_it() {
    let mut f = Fullscreen::new(80);
    conversation(&mut f);
    f.frame();
    let answer = f.row_of("● Answer 4.");
    f.drag((2, answer), (20, answer));
    assert!(f.marked().contains('['));
    f.wheel(MouseEventKind::ScrollUp);
    f.frame();
    assert!(!f.marked().contains('['), "{}", f.marked());

    // A resize lets go of it too.
    f.wheel(MouseEventKind::ScrollDown);
    f.frame();
    let answer = f.row_of("● Answer 4.");
    f.drag((2, answer), (20, answer));
    f.ui.resize(Size::new(80, ROWS), None);
    f.frame();
    assert!(!f.marked().contains('['), "{}", f.marked());
}

#[test]
fn the_transcript_and_full_screen_views_select_too() {
    let mut f = Fullscreen::new(80);
    conversation(&mut f);
    f.frame();
    f.ui.handle_event(
        Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        f.now,
    );
    f.frame();
    let question = f.row_of("Question 4: what");
    assert_eq!(
        copied(&f.click((10, question), 3)),
        Some("Question 4: what does the export do?")
    );
    // Esc lets go of the selection first, and the transcript stays.
    f.press(KeyCode::Esc, KeyModifiers::NONE);
    f.frame();
    assert!(f.shown().contains("Showing detailed transcript"));
    f.press(KeyCode::Esc, KeyModifiers::NONE);
    f.frame();
    assert!(!f.shown().contains("Showing detailed transcript"));

    f.ui.push_view(Box::new(HelpView::new(Keymap::default())));
    f.frame();
    let row = f.row_of("│ ctrl+c ");
    f.now += std::time::Duration::from_secs(2);
    let text = f.click((4, row), 3);
    let text = copied(&text).expect("a row copied");
    assert!(text.starts_with("ctrl+c "), "{text:?}");
    assert!(
        text.ends_with("clear the prompt, twice to exit"),
        "{text:?}"
    );
    insta::assert_snapshot!("selection_in_a_view", f.marked());
    assert_eq!(f.ui.views_open(), 1);
    f.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(f.ui.views_open(), 1, "Esc let go of the selection only");
}

#[test]
fn the_inline_screen_leaves_the_mouse_to_the_terminal() {
    let terminal = InlineTerminal::new(Vec::new(), Size::new(80, ROWS), Position::new(0, 0));
    let mut inline = Ui::new(terminal, theme(), ViewModel::new("inline"), t0());
    inline.frame(t0()).unwrap();
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        let mouse = MouseEvent {
            kind,
            column: 2,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert!(inline.handle_event(Event::Mouse(mouse), t0()).is_empty());
    }
    let written = String::from_utf8_lossy(inline.terminal().writer()).into_owned();
    assert!(!written.contains("\x1b[?1002h"), "{written:?}");
}

/// A run's workflow with one phase going and its two agents, for the docked view.
fn docked_workflow() -> Box<argonauts_tui::views::workflow::WorkflowView> {
    let data: argonauts_core::views::WorkflowView = serde_json::from_value(serde_json::json!({
        "run": "run-01", "id": "wf-001", "name": "fix-slugify-median",
        "description": "Fix slugify's dashes and median's even and empty lists, with tests",
        "agents_done": 1, "agents_total": 2, "elapsed_ms": 271000,
        "spent": {"input_tokens": 0, "output_tokens": 0, "cached_tokens": 0, "cost_usd": 0.0,
                  "cost_partial": false, "cost_estimated": false},
        "phases": [{"name": "Fix", "standard": "do", "status": "running",
                    "count": {"done": 1, "total": 2},
                    "rows": [
            {"row": "agent", "session": "s-001", "label": "fix:slugify",
             "worker": "claude-code/claude-haiku-4-5", "status": "done", "tokens": 18100,
             "spent": {"input_tokens": 0, "output_tokens": 0, "cached_tokens": 0,
                       "cost_usd": 0.0, "cost_partial": false, "cost_estimated": false},
             "elapsed_ms": 130000, "subagents": 0, "context": null, "fallback_for": null},
            {"row": "agent", "session": "s-002", "label": "fix:median",
             "worker": "codex/gpt-5.6-luna", "status": "running", "tokens": 31200,
             "spent": {"input_tokens": 0, "output_tokens": 0, "cached_tokens": 0,
                       "cost_usd": 0.0, "cost_partial": false, "cost_estimated": false},
             "elapsed_ms": 151000, "subagents": 0, "context": null, "fallback_for": null}
        ]},
        {"name": "Review", "standard": "check", "status": "waiting", "rows": []}]
    }))
    .expect("a workflow view");
    Box::new(argonauts_tui::views::workflow::WorkflowView::new(data))
}

/// The workflow view docks under the conversation in place of the input, as Claude Code's
/// open view does, and Esc gives the prompt back.
#[test]
fn the_workflow_view_docks_under_the_conversation() {
    let mut f = Fullscreen::new(100);
    conversation(&mut f);
    f.frame();
    let mode = f.row_of("supervised mode on");
    f.ui.push_view(docked_workflow());
    f.frame();
    let shown = f.shown();
    assert!(shown.contains("Answer 4."), "{shown}");
    assert!(shown.contains("Rowing"), "the working line stays:\n{shown}");
    assert!(
        !shown.contains("supervised mode on"),
        "the input gives way:\n{shown}"
    );
    let name = f.row_of(" fix-slugify-median");
    assert!(f.row_of("Answer 4.") < name, "{shown}");
    assert!(f.row_of("╭ Phases") > name, "{shown}");
    assert!(f.row_of("esc back") > f.row_of("╰"), "{shown}");
    f.assert_sound();
    f.press(KeyCode::Esc, KeyModifiers::NONE);
    f.frame();
    assert_eq!(f.ui.views_open(), 0);
    assert_eq!(f.row_of("supervised mode on"), mode);
}

/// Scrolled up past a prompt, the prompt being answered stays at the top; Ctrl+↑ or a click
/// on it goes to where it starts, and back at the bottom it goes.
#[test]
fn the_prompt_being_answered_stays_at_the_top_and_takes_you_back_to_it() {
    let mut f = Fullscreen::new(80);
    conversation(&mut f);
    f.frame();
    assert!(!f.rows()[0].starts_with("❯ Question"), "{}", f.shown());

    f.key(KeyCode::PageUp);
    f.frame();
    let top = f.rows()[0].clone();
    assert!(top.starts_with("❯ Question "), "{}", f.shown());
    assert!(f.rows()[1].starts_with('─'), "{}", f.shown());
    assert!(
        f.shown().contains("or press ctrl+↑, to jump"),
        "{}",
        f.shown()
    );

    f.press(KeyCode::Up, KeyModifiers::CONTROL);
    f.frame();
    assert_eq!(f.rows()[0], top.trim_end(), "{}", f.shown());
    assert!(
        !f.rows()[1].starts_with('─'),
        "the prompt starts the view:\n{}",
        f.shown()
    );

    // A click on the row at the top does the same. That prompt then starts the view, under
    // the one before it, which is now the prompt being answered above.
    f.key(KeyCode::PageDown);
    f.frame();
    f.wheel(MouseEventKind::ScrollUp);
    f.frame();
    let held = f.rows()[0].clone();
    assert!(held.starts_with("❯ Question 2"), "{}", f.shown());
    let actions = f.click((10, 0), 1);
    assert!(actions.is_empty(), "nothing is copied: {actions:?}");
    assert!(f.rows()[0].starts_with("❯ Question 1"), "{}", f.shown());
    assert_eq!(f.rows()[2], held.trim_end(), "{}", f.shown());

    f.key(KeyCode::End);
    for _ in 0..3 {
        f.key(KeyCode::PageDown);
    }
    f.frame();
    assert_eq!(f.ui.scroll_offset(), Some(0));
    assert!(!f.rows()[1].starts_with('─'), "{}", f.shown());
}
