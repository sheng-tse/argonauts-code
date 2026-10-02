//! The fullscreen screen through a vt100 parser: every frame is one synchronized update on the
//! alternate screen, the main screen is never written, and the frame is sound at every width,
//! scrolled up or not.

mod common;

use std::io::{self, Write};
use std::time::{Duration, Instant};

use argonauts_tui::model::{
    AgentReady, AssistantMessage, CardOption, CardPrompt, SessionHeader, ToolCall, ToolItem,
    UserMessage, ViewModel, WorkingLine,
};
use argonauts_tui::{InlineTerminal, Ui, stability};
use common::{t0, theme};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Size};

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
        ui.use_fullscreen("Shift");
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
        app: "Argonauts".into(),
        version: "0.0.1".into(),
        default_agent: Some("Claude Code".into()),
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        preset: "peer-crew".into(),
        folder: "~/github/argonauts".into(),
        agents: vec![ready("Claude Code", "2.1.284"), ready("Codex", "0.158.0")],
        runs_going: 0,
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
    let mut block = ToolCall::agent("GPT on Codex", None, "owner", "Wire --json");
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
    assert!(f.ui.terminal().writer().flushes[0].contains("\x1b[?1000h\x1b[?1006h"));
    conversation(&mut f);
    for _ in 0..6 {
        f.frame();
    }
    assert!(f.shown().contains("Wire --json"), "{}", f.shown());

    // A card, the transcript, and a view all draw on the same alternate screen.
    f.ui.model_mut().bottom.card = Some(CardPrompt::new(
        1,
        "Merge card-002 into main?",
        "Merge card-002?",
        vec![CardOption::new("Merge")],
    ));
    f.frame();
    assert!(f.shown().contains("Merge card-002"), "{}", f.shown());
    f.ui.model_mut().bottom.card = None;
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
    assert!(
        f.shown().contains("3 rows below · pgdn to scroll"),
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
