//! Text from agents and requests reaches the terminal only as text: it can't set the title,
//! clear the screen, write the clipboard, ring the bell, or resize the window.

mod common;

use std::io::{self, Write};
use std::time::{Duration, Instant};

use argonauts_tui::model::{
    AssistantMessage, CardOption, CardPrompt, ToolCall, ToolItem, UserMessage, ViewModel,
};
use argonauts_tui::{InlineTerminal, Ui};
use common::{t0, theme};
use crossterm::event::Event;
use ratatui::layout::{Position, Size};

const COLUMNS: u16 = 100;
const ROWS: u16 = 30;

/// Every way these sequences try to reach past the screen.
const HOSTILE: &str = "\x1b]0;pwned\x07\x1b]2;pwned\x1b\\\x1b]52;c;cHduZWQ=\x07\x1b[2J\x1b[3J\x1bc\x1b[8;5;5t\x07\x1b[?1049h\u{9b}2Jafter";

#[derive(Default)]
struct Seen {
    titles: Vec<String>,
    clipboard: usize,
    bells: usize,
    resizes: usize,
}

impl vt100::Callbacks for Seen {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bells += 1;
    }

    fn resize(&mut self, _: &mut vt100::Screen, _: (u16, u16)) {
        self.resizes += 1;
    }

    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.titles
            .push(String::from_utf8_lossy(title).into_owned());
    }

    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, _: &[u8], _: &[u8]) {
        self.clipboard += 1;
    }
}

struct Terminal(vt100::Parser<Seen>);

impl Write for Terminal {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.process(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn rows(ui: &Ui<Terminal>) -> Vec<String> {
    ui.terminal()
        .writer()
        .0
        .screen()
        .rows(0, COLUMNS)
        .map(|row| row.trim_end().to_owned())
        .collect()
}

fn settle(ui: &mut Ui<Terminal>, now: &mut Instant) {
    for _ in 0..10 {
        *now += Duration::from_millis(20);
        ui.frame(*now).unwrap();
    }
}

#[test]
fn agent_escape_cannot_set_title_or_clear() {
    crossterm::style::force_color_output(true);
    let mut parser = vt100::Parser::new_with_callbacks(ROWS, COLUMNS, 10, Seen::default());
    parser.process(b"$ argonauts\r\n");
    let terminal = InlineTerminal::new(
        Terminal(parser),
        Size::new(COLUMNS, ROWS),
        Position::new(0, 1),
    );
    let mut now = t0();
    let mut model = ViewModel::new(format!("Fix {HOSTILE} export"));
    model
        .history
        .push(UserMessage::new(format!("please {HOSTILE}")));
    model
        .history
        .push(AssistantMessage::new(format!("Answer {HOSTILE}")));
    let mut block = ToolCall::agent("GPT on Codex", None, "owner", HOSTILE);
    block.push(ToolItem::text(format!("output {HOSTILE}")));
    model.history.push_active(block);
    let mut ui = Ui::new(terminal, theme(), model, now);
    ui.handle_event(Event::Paste(format!("pasted {HOSTILE}")), now);
    settle(&mut ui, &mut now);

    // A card from a run says so in the title, which is still built from sanitized text.
    ui.model_mut().bottom.card = Some(CardPrompt::new(
        1,
        format!("Merge {HOSTILE}?"),
        format!("Merge {HOSTILE}?"),
        vec![CardOption::new(format!("Yes {HOSTILE}"))],
    ));
    settle(&mut ui, &mut now);
    let shown = rows(&ui);
    ui.finish_with(vec![format!("run-01 keeps going {HOSTILE}")])
        .unwrap();

    let screen = ui.terminal().writer().0.screen();
    assert!(!screen.alternate_screen());
    let seen = ui.terminal().writer().0.callbacks();
    assert_eq!(
        (seen.clipboard, seen.bells, seen.resizes),
        (0, 0, 0),
        "{:?}",
        seen.titles
    );
    assert!(!seen.titles.is_empty());
    for title in &seen.titles {
        assert!(!title.contains("pwned"), "{title:?}");
        assert!(!title.chars().any(char::is_control), "{title:?}");
        assert!(
            title.contains("Fix") && title.ends_with("after export"),
            "{title:?}"
        );
    }
    assert!(
        seen.titles
            .iter()
            .any(|title| title.starts_with("◆ needs you · "))
    );

    // History starts on the top row, and every item printed whole.
    assert!(shown[0].starts_with("❯ please"), "{shown:#?}");
    for needle in ["please", "Answer", "output", "pasted", "Merge"] {
        assert!(
            shown
                .iter()
                .any(|row| row.contains(needle) && row.contains("after")),
            "{needle}: {shown:#?}"
        );
    }
    assert!(!shown.iter().any(|row| row.contains("pwned")), "{shown:#?}");
    let after = rows(&ui);
    assert!(after[0].starts_with("❯ please"), "{after:#?}");
    assert!(after.iter().any(|row| row.contains("run-01 keeps going")));

    // The shell's line went up into scrollback, never erased.
    let screen = ui.terminal_mut().writer_mut().0.screen_mut();
    screen.set_scrollback(1);
    let oldest = screen.rows(0, COLUMNS).next().unwrap_or_default();
    assert_eq!(oldest.trim_end(), "$ argonauts");
}
