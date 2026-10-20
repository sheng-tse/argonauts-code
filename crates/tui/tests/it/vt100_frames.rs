//! Every frame reaches the terminal as one synchronized update in one write, whatever it holds:
//! history, the live region, the title, and switches to and from the alternate screen.

use crate::common;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use argonauts_tui::model::{
    AssistantMessage, QuestionOption, QuestionPrompt, ToolCall, ToolItem, UserMessage, ViewModel,
    WorkingLine,
};
use argonauts_tui::{InlineTerminal, Ui};
use common::{t0, theme};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::Style;

const BEGIN: &str = "\x1b[?2026h";
const END: &str = "\x1b[?2026l";

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

#[derive(Debug)]
struct Panel;

impl argonauts_tui::views::View for Panel {
    fn render(&mut self, area: Rect, buf: &mut Buffer, _: &argonauts_tui::views::ViewCtx<'_>) {
        buf.set_string(area.x, area.y, "a full-screen view", Style::default());
    }

    fn handle_key(
        &mut self,
        _: KeyEvent,
        _: &argonauts_tui::views::ViewCtx<'_>,
    ) -> argonauts_tui::views::ViewOutcome {
        argonauts_tui::views::ViewOutcome::Close
    }
}

struct Frames {
    ui: Ui<Recorder>,
    now: Instant,
    drawn: usize,
}

impl Frames {
    /// Runs a frame and checks what it wrote: nothing, or exactly one synchronized update,
    /// flushed once, with nothing left unflushed.
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
            assert_eq!(frame.matches(BEGIN).count(), 1, "{frame:?}");
            assert_eq!(frame.matches(END).count(), 1, "{frame:?}");
            self.drawn += 1;
        }
    }

    fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        self.ui
            .handle_event(Event::Key(KeyEvent::new(code, modifiers)), self.now);
    }

    fn last(&self) -> &str {
        self.ui
            .terminal()
            .writer()
            .flushes
            .last()
            .map_or("", String::as_str)
    }
}

#[test]
fn each_frame_one_synchronized_update() {
    crossterm::style::force_color_output(true);
    let recorder = Recorder {
        parser: vt100::Parser::new(24, 80, 0),
        pending: Vec::new(),
        flushes: Vec::new(),
    };
    let terminal = InlineTerminal::new(recorder, Size::new(80, 24), Position::new(0, 0));
    let mut f = Frames {
        ui: Ui::new(terminal, theme(), ViewModel::new("frames"), t0()),
        now: t0(),
        drawn: 0,
    };

    // The first frame sets the title and draws the prompt together.
    f.frame();
    assert!(f.last().contains("\x1b]0;✳ frames\x07"), "{:?}", f.last());

    for c in "typing".chars() {
        f.key(KeyCode::Char(c), KeyModifiers::NONE);
        f.frame();
    }
    let history = &mut f.ui.model_mut().history;
    history.push(UserMessage::new("hi"));
    history.push(AssistantMessage::new("Hello there."));
    let mut block = ToolCall::agent("GPT on Codex", None, "worker", "Wire --json");
    block.push(ToolItem::text("step one"));
    f.ui.model_mut().history.push_active(block);
    f.ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", f.now));
    for _ in 0..12 {
        f.frame();
    }

    // A question changes the title in the same update as the question itself.
    f.ui.model_mut().bottom.question = Some(QuestionPrompt::new(
        1,
        "Merge task-002 into main?",
        "Merge task-002?",
        vec![QuestionOption::new("Merge")],
    ));
    f.frame();
    assert!(f.last().contains("◆ needs you · frames"), "{:?}", f.last());
    assert!(f.last().contains("Merge task-002"), "{:?}", f.last());

    // Switching to the transcript prints waiting history, switches, and draws in one go.
    f.ui.model_mut()
        .history
        .push(UserMessage::new("while working"));
    f.key(KeyCode::Char('o'), KeyModifiers::CONTROL);
    f.frame();
    assert!(f.ui.terminal().writer().parser.screen().alternate_screen());
    assert!(f.last().contains("\x1b[?1049h"), "{:?}", f.last());
    f.frame();
    f.key(KeyCode::Esc, KeyModifiers::NONE);
    f.frame();
    assert!(!f.ui.terminal().writer().parser.screen().alternate_screen());

    f.ui.push_view(Box::new(Panel));
    f.frame();
    assert!(f.ui.terminal().writer().parser.screen().alternate_screen());
    f.ui.pop_view();
    f.frame();
    assert!(!f.ui.terminal().writer().parser.screen().alternate_screen());

    f.ui.resize(Size::new(60, 20), None);
    f.ui.terminal_mut()
        .writer_mut()
        .parser
        .screen_mut()
        .set_size(20, 60);
    f.frame();
    for _ in 0..5 {
        f.frame();
    }
    assert!(f.drawn >= 10, "{}", f.drawn);
}
