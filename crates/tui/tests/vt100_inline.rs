//! vt100 0.16 doesn't keep rows that scroll off while a scroll region is set, which is how
//! history prints, so these tests look at the visible screen only.

mod common;

use std::io::{self, Write};
use std::time::{Duration, Instant};

use argonauts_tui::model::{
    ApprovalMode, AssistantMessage, CardOption, CardPrompt, ToolCall, ToolItem, ToolKind,
    ToolState, UserMessage, ViewModel, WorkingLine,
};
use argonauts_tui::theme::Role;
use argonauts_tui::{InlineTerminal, Ui, UiAction};
use common::{bg, fg, t0, theme};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Size};
use ratatui::style::Color;

struct Screen {
    parser: vt100::Parser,
    bytes: usize,
}

impl Write for Screen {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes += buf.len();
        self.parser.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.parser.flush()
    }
}

/// Starts with the cursor at the top, as an inline app starts wherever the shell left it.
struct Harness {
    ui: Ui<Screen>,
    now: Instant,
    cols: u16,
}

impl Harness {
    fn new(cols: u16, rows: u16) -> Self {
        // Tests may run with NO_COLOR set; the theme decides about color, not crossterm.
        crossterm::style::force_color_output(true);
        let screen = Screen {
            parser: vt100::Parser::new(rows, cols, 0),
            bytes: 0,
        };
        let terminal = InlineTerminal::new(screen, Size::new(cols, rows), Position::new(0, 0));
        let mut model = ViewModel::new("vt100 test");
        model.bottom.composer.label = Some("peer-crew".into());
        Self {
            ui: Ui::new(terminal, theme(), model, t0()),
            now: t0(),
            cols,
        }
    }

    fn settle(&mut self) {
        for _ in 0..20 {
            self.now += Duration::from_millis(20);
            match self.ui.next_deadline(self.now) {
                Some(due) if due <= self.now => {
                    self.ui.frame(self.now).unwrap();
                }
                Some(due) if due <= self.now + Duration::from_millis(100) => {
                    self.now = due;
                    self.ui.frame(self.now).unwrap();
                }
                _ => break,
            }
        }
    }

    fn key(&mut self, code: KeyCode) -> Vec<UiAction> {
        self.key_with(code, KeyModifiers::NONE)
    }

    fn key_with(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Vec<UiAction> {
        self.now += Duration::from_millis(5);
        self.ui
            .handle_event(Event::Key(KeyEvent::new(code, modifiers)), self.now)
    }

    fn screen(&self) -> &vt100::Screen {
        self.ui.terminal().writer().parser.screen()
    }

    fn rows(&self) -> Vec<String> {
        self.screen()
            .rows(0, self.cols)
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    fn bytes(&self) -> usize {
        self.ui.terminal().writer().bytes
    }

    fn row_of(&self, needle: &str) -> Option<usize> {
        self.rows().iter().position(|row| row.contains(needle))
    }

    /// The live region's top row: its rule, or the working line or card above it.
    fn live_top(&self) -> usize {
        usize::from(self.ui.terminal().viewport().y)
    }
}

fn vt_color(color: Option<Color>) -> vt100::Color {
    match color {
        Some(Color::Rgb(r, g, b)) => vt100::Color::Rgb(r, g, b),
        Some(Color::Indexed(i)) => vt100::Color::Idx(i),
        _ => vt100::Color::Default,
    }
}

fn dump(rows: &[String]) -> String {
    rows.iter()
        .enumerate()
        .map(|(i, row)| format!("{i:2}|{row}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn history_fills_down_from_the_top_and_the_prompt_keeps_the_bottom() {
    let mut h = Harness::new(60, 20);
    let history = &mut h.ui.model_mut().history;
    history.push(UserMessage::new("hi"));
    history.push(AssistantMessage::new("Hello there."));
    h.settle();
    let rows = h.rows();

    let you = h.row_of("❯ hi").expect("your message printed");
    let answer = h.row_of("Hello there.").expect("the answer printed");
    let rule = h.row_of("peer-crew ─").expect("the composer's top rule");
    assert_eq!(you, 0, "history starts on the top row:\n{}", dump(&rows));
    assert_eq!(
        answer,
        you + 2,
        "one blank row between items:\n{}",
        dump(&rows)
    );
    assert!(
        rows[answer + 1..rule].iter().all(String::is_empty),
        "blank rows between history and the prompt:\n{}",
        dump(&rows)
    );
    assert_eq!(
        rule,
        16,
        "the prompt is pinned to the bottom:\n{}",
        dump(&rows)
    );
    assert!(rows[rule + 1].starts_with("❯"), "{}", dump(&rows));
    assert_eq!(rows[rule + 2], "─".repeat(60), "{}", dump(&rows));
    assert_eq!(
        rows[rule + 3],
        "  ǁ supervised mode on (shift+tab to cycle)",
        "{}",
        dump(&rows)
    );
    assert!(answer < h.live_top(), "{}", dump(&rows));
    assert_eq!(h.screen().cursor_position(), (rule as u16 + 1, 2));
}

#[test]
fn printed_rows_keep_their_colors_to_the_edge() {
    let mut h = Harness::new(40, 16);
    h.ui.model_mut().history.push(UserMessage::new("hi"));
    h.ui.model_mut().history.push(
        ToolCall::call(ToolKind::Read, "Read", "src/export.rs")
            .with_item(ToolItem::text("Read 212 lines"))
            .with_state(ToolState::Done),
    );
    h.settle();
    let band = vt_color(bg(Role::UserBand));
    let you = h.row_of("❯ hi").unwrap() as u16;
    for col in [0, 20, 39] {
        let cell = h.screen().cell(you, col).unwrap();
        assert_eq!(
            cell.bgcolor(),
            band,
            "column {col} of your message has the band"
        );
    }
    let call = h.row_of("Read(src/export.rs)").unwrap() as u16;
    let bullet = h.screen().cell(call, 0).unwrap();
    assert_eq!(
        bullet.fgcolor(),
        vt_color(fg(Role::Success)),
        "a done call's bullet is green"
    );
    assert!(
        h.screen().cell(call, 2).unwrap().bold(),
        "the tool name is bold"
    );
}

#[test]
fn wide_characters_keep_their_columns() {
    // CJK and emoji take two columns each, and wrapping must never split one.
    let mut h = Harness::new(40, 20);
    let answer = "日本語のテキストが折り返されても列がずれないことを確かめる長い文です。";
    h.ui.model_mut()
        .history
        .push(UserMessage::new("東京に行く 🚀 launch"));
    h.ui.model_mut().history.push(AssistantMessage::new(answer));
    h.settle();
    let rows = h.rows();

    let you = h
        .row_of("❯ 東京に行く 🚀 launch")
        .expect("your message printed") as u16;
    let band = vt_color(bg(Role::UserBand));
    assert_eq!(
        h.screen().cell(you, 39).unwrap().bgcolor(),
        band,
        "{}",
        dump(&rows)
    );

    let first = h.row_of("● 日本語").expect("the answer printed");
    let rule = h.row_of("peer-crew ─").expect("the composer's top rule");
    let body: Vec<&String> = rows[first..rule].iter().filter(|r| !r.is_empty()).collect();
    assert!(body.len() > 1, "the answer wraps:\n{}", dump(&rows));
    for row in &body[1..] {
        assert!(
            row.starts_with("  ") && !row[2..].starts_with(' '),
            "wrapped rows hang at column 2:\n{}",
            dump(&rows)
        );
    }
    let printed: String = body
        .iter()
        .map(|row| row.trim_start_matches("● ").trim())
        .collect();
    assert_eq!(printed, answer, "nothing lost or split:\n{}", dump(&rows));
    assert_eq!(rows[rule + 2], "─".repeat(40), "{}", dump(&rows));
}

#[test]
fn a_full_screen_keeps_order_and_the_live_region() {
    let mut h = Harness::new(50, 16);
    for batch in 0..4 {
        for i in 1..=10 {
            let n = batch * 10 + i;
            h.ui.model_mut()
                .history
                .push(UserMessage::new(format!("message {n:02}")));
        }
        h.settle();
    }
    let rows = h.rows();
    assert_eq!(
        rows[15],
        "  ǁ supervised mode on (shift+tab to cycle)",
        "{}",
        dump(&rows)
    );
    assert_eq!(rows[14], "─".repeat(50), "{}", dump(&rows));
    assert!(rows[13].starts_with('❯'), "{}", dump(&rows));
    assert!(rows[12].ends_with("peer-crew ─"), "{}", dump(&rows));
    let numbers: Vec<u32> = rows
        .iter()
        .filter_map(|row| row.strip_prefix("❯ message "))
        .map(|n| n.parse().unwrap())
        .collect();
    assert!(numbers.len() >= 5, "{}", dump(&rows));
    assert!(
        numbers.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "{}",
        dump(&rows)
    );
    assert_eq!(numbers.last(), Some(&40), "{}", dump(&rows));
    assert_eq!(rows[10], "❯ message 40", "{}", dump(&rows));
}

#[test]
fn a_streamed_answer_prints_each_finished_line() {
    // A finished line prints once the line after it is finished too, since that one could
    // still turn it into a heading or a table row. The unfinished line stays live.
    let mut h = Harness::new(60, 24);
    h.ui.model_mut().history.push(UserMessage::new("hi"));
    let id = h.ui.model_mut().history.start_stream();
    h.ui.model_mut()
        .history
        .push_stream(id, "First line.\nSecond line.\nThird li");
    h.settle();
    let rows = h.rows();
    let first = h.row_of("● First line.").expect("the first line shows");
    let second = h.row_of("  Second line.").expect("the second line shows");
    let tail = h.row_of("  Third li").expect("the tail shows");
    assert_eq!(second, first + 1, "{}", dump(&rows));
    assert_eq!(
        tail,
        second + 1,
        "the answer reads as one block:\n{}",
        dump(&rows)
    );
    assert!(
        first < h.live_top(),
        "a settled line is history:\n{}",
        dump(&rows)
    );
    assert!(
        tail >= h.live_top(),
        "the unfinished line stays live:\n{}",
        dump(&rows)
    );

    h.ui.model_mut()
        .history
        .push_stream(id, "ne.\nFourth line.\nFifth");
    h.settle();
    let rows = h.rows();
    let second = h.row_of("  Second line.").unwrap();
    let third = h.row_of("  Third line.").expect("the third line shows");
    assert!(
        third < h.live_top(),
        "it printed as the stream went on:\n{}",
        dump(&rows)
    );
    assert_eq!(
        third,
        second + 1,
        "no row printed twice or left out:\n{}",
        dump(&rows)
    );
    for line in ["First line.", "Second line.", "Third line."] {
        let count = rows.iter().filter(|r| r.contains(line)).count();
        assert_eq!(count, 1, "{line} shows once:\n{}", dump(&rows));
    }

    h.ui.model_mut().history.push_stream(id, " line.");
    h.ui.model_mut().history.finish_stream(id);
    h.settle();
    let rows = h.rows();
    let fourth = h.row_of("  Fourth line.").expect("the fourth line printed");
    let fifth = h.row_of("  Fifth line.").expect("the last line printed");
    assert_eq!(fourth, third + 1, "{}", dump(&rows));
    assert_eq!(fifth, fourth + 1, "{}", dump(&rows));
    assert!(
        fifth < h.live_top(),
        "all of it is history now:\n{}",
        dump(&rows)
    );
    assert!(!h.ui.model().history.is_streaming());
    h.ui.model_mut().history.push(UserMessage::new("thanks"));
    h.settle();
    let rows = h.rows();
    assert_eq!(h.row_of("❯ thanks"), Some(fifth + 2), "{}", dump(&rows));
}

#[test]
fn an_idle_frame_writes_nothing() {
    let mut h = Harness::new(60, 20);
    h.ui.model_mut().history.push(UserMessage::new("hi"));
    h.settle();
    let before = h.bytes();
    let screen = h.rows();
    assert_eq!(
        h.ui.next_deadline(h.now),
        None,
        "nothing is due on an idle screen"
    );
    h.now += Duration::from_secs(5);
    assert!(
        !h.ui.frame(h.now).unwrap(),
        "an idle frame reports no write"
    );
    assert_eq!(h.bytes(), before, "an idle frame writes zero bytes");
    assert_eq!(h.rows(), screen);
}

#[test]
fn typing_redraws_only_what_changed() {
    let mut h = Harness::new(80, 20);
    h.ui.model_mut()
        .history
        .push(AssistantMessage::new("Ask me anything."));
    h.settle();
    let before = h.bytes();
    h.key(KeyCode::Char('x'));
    h.settle();
    let typed = h.bytes() - before;
    assert!(
        h.rows().iter().any(|row| row == "❯ x"),
        "{}",
        dump(&h.rows())
    );
    assert!(
        typed > 0 && typed < 200,
        "one key wrote {typed} bytes, not a repaint"
    );

    let actions = {
        h.key(KeyCode::Char('!'));
        h.key(KeyCode::Enter)
    };
    assert_eq!(actions, vec![UiAction::submit("x!")]);
    assert!(h.ui.model().bottom.composer.is_empty());
}

/// A terminal made shorter pushes its top rows into scrollback before the app hears of it.
/// With the live blocks held to half the screen, a modest shrink pushes printed history only,
/// never a frozen copy of a live block, and after the repaint each item shows once.
#[test]
fn a_shorter_screen_pushes_only_history_and_shows_each_item_once() {
    let mut h = Harness::new(100, 36);
    for i in 0..12 {
        h.ui.model_mut()
            .history
            .push(UserMessage::new(format!("message {i}")));
    }
    for (harness, name, task) in [
        (
            "claude-code",
            "Opus 5.5 on Claude Code",
            "Add the JSON writer",
        ),
        ("codex", "GPT-5.6 on Codex", "Wire --json into the CLI"),
    ] {
        let mut block = ToolCall::agent(name, None, "owner", task);
        for line in 0..12 {
            block.push(ToolItem::text(format!("{harness} step {line}")));
        }
        h.ui.model_mut().history.push_active(block);
    }
    h.ui.model_mut().bottom.working = Some(WorkingLine::new("Building", h.now));
    h.settle();
    let before = h.rows();
    let live_top = h.live_top();

    // The terminal pushes the top rows into scrollback, keeping the cursor as far from the
    // bottom as it was, then tells the app.
    let shrink: u16 = 8;
    assert!(
        usize::from(shrink) <= live_top,
        "only history sits above the live region's top at row {live_top}:\n{}",
        dump(&before)
    );
    let (row, column) = h.screen().cursor_position();
    let screen = h.ui.terminal_mut().writer_mut();
    screen.parser.process(b"\x1b[r\x1b[36;1H");
    for _ in 0..shrink {
        screen.parser.process(b"\n");
    }
    screen.parser.screen_mut().set_size(36 - shrink, 100);
    h.now += Duration::from_millis(5);
    h.ui.resize(
        Size::new(100, 36 - shrink),
        Some(Position::new(column, row - shrink)),
    );
    h.settle();

    let rows = h.rows();
    for needle in [
        "Opus 5.5 on Claude Code · owner",
        "GPT-5.6 on Codex · owner",
        "peer-crew ─",
        "Building",
        "message 11",
    ] {
        assert_eq!(
            rows.iter().filter(|row| row.contains(needle)).count(),
            1,
            "{needle}:\n{}",
            dump(&rows)
        );
    }
    let eleven = h.row_of("message 11").unwrap();
    assert!(h.row_of("message 10").is_some_and(|row| row < eleven));
    assert!(eleven < h.live_top(), "{}", dump(&rows));
}

#[test]
fn a_resize_reflows_the_live_region() {
    let mut h = Harness::new(80, 20);
    h.ui.model_mut().history.push(UserMessage::new("hi"));
    h.ui.model_mut()
        .history
        .push(AssistantMessage::new("Hello there."));
    h.settle();

    h.ui.terminal_mut()
        .writer_mut()
        .parser
        .screen_mut()
        .set_size(20, 50);
    h.cols = 50;
    h.now += Duration::from_millis(5);
    h.ui.handle_event(Event::Resize(50, 20), h.now);
    h.settle();
    let rows = h.rows();
    let rule = h.row_of("peer-crew ─").expect("the top rule");
    assert_eq!(rows[rule].chars().count(), 50, "{}", dump(&rows));
    assert_eq!(rows[rule + 2], "─".repeat(50), "{}", dump(&rows));
    assert_eq!(
        rows[rule + 3],
        "  ǁ supervised mode on (shift+tab to cycle)"
    );
    assert!(
        h.row_of("Hello there.").is_some_and(|row| row < rule),
        "{}",
        dump(&rows)
    );
    assert_eq!(
        rows.iter().filter(|r| r.contains("peer-crew")).count(),
        1,
        "{}",
        dump(&rows)
    );

    h.ui.terminal_mut()
        .writer_mut()
        .parser
        .screen_mut()
        .set_size(20, 90);
    h.cols = 90;
    h.now += Duration::from_millis(5);
    h.ui.handle_event(Event::Resize(90, 20), h.now);
    h.settle();
    let rows = h.rows();
    let rule = h.row_of("peer-crew ─").expect("the top rule");
    assert_eq!(rows[rule].chars().count(), 90, "{}", dump(&rows));
    assert_eq!(
        rows.iter().filter(|r| r.contains("peer-crew")).count(),
        1,
        "{}",
        dump(&rows)
    );
}

/// Rows drawn as an input rule: the composer's labeled top rule and the plain one under it.
fn rule_rows(rows: &[String]) -> (usize, usize) {
    let rules: Vec<&String> = rows.iter().filter(|row| row.starts_with('─')).collect();
    let labeled = rules.iter().filter(|row| row.contains("peer-crew")).count();
    (labeled, rules.len())
}

#[test]
fn closing_the_menu_or_a_picker_leaves_one_input_rule() {
    use argonauts_tui::bottom::{MenuItem, Picker, PickerRow};

    let mut h = Harness::new(80, 30);
    h.ui.model_mut().bottom.menus.commands = (0..12)
        .map(|i| MenuItem::new(format!("command-{i}"), "Does one thing").in_group("Session"))
        .collect();
    h.ui.model_mut().history.push(UserMessage::new("hi"));
    h.settle();
    assert_eq!(rule_rows(&h.rows()), (1, 2), "{}", dump(&h.rows()));

    h.key(KeyCode::Char('/'));
    h.settle();
    let rows = h.rows();
    assert!(h.row_of("/command-0").is_some(), "{}", dump(&rows));
    assert_eq!(rule_rows(&rows), (1, 2), "{}", dump(&rows));

    h.key(KeyCode::Char('c'));
    h.key(KeyCode::Char('o'));
    h.settle();
    assert_eq!(rule_rows(&h.rows()), (1, 2), "{}", dump(&h.rows()));
    // A terminal can show cells the screen never wrote there, as after a reflow. Rows the
    // live region stops using are erased even so.
    let menu_top = h.row_of("/command-0").unwrap();
    let stale = format!("\x1b[{};1H{}", menu_top + 1, "─".repeat(70));
    h.ui.terminal_mut()
        .writer_mut()
        .parser
        .process(stale.as_bytes());
    h.key(KeyCode::Esc);
    h.settle();
    assert_eq!(rule_rows(&h.rows()), (1, 2), "{}", dump(&h.rows()));
    for _ in 0..3 {
        h.key(KeyCode::Backspace);
    }
    h.settle();
    assert_eq!(rule_rows(&h.rows()), (1, 2), "{}", dump(&h.rows()));

    let mut picker = Picker::new("Pick a theme", "How the screen looks");
    picker.set_rows(
        ["dark", "light", "dark-ansi", "light-ansi", "plain"]
            .into_iter()
            .map(|name| PickerRow::new(name, ""))
            .collect(),
    );
    h.ui.model_mut().bottom.picker = Some(picker);
    h.settle();
    let rows = h.rows();
    assert!(h.row_of("Pick a theme").is_some(), "{}", dump(&rows));
    assert!(h.row_of("light-ansi").is_some(), "{}", dump(&rows));
    let top = h.live_top();
    h.ui.model_mut().bottom.picker = None;
    h.ui.model_mut()
        .history
        .push(UserMessage::command("/theme"));
    h.settle();
    let rows = h.rows();
    assert_eq!(rule_rows(&rows), (1, 2), "{}", dump(&rows));
    assert!(h.row_of("light-ansi").is_none(), "{}", dump(&rows));
    assert!(h.live_top() > top, "{}", dump(&rows));
    let rule = h.row_of("peer-crew").unwrap();
    assert!(
        rows[h.row_of("/theme").unwrap() + 1..rule]
            .iter()
            .all(String::is_empty),
        "nothing left between history and the prompt:\n{}",
        dump(&rows)
    );
}

#[test]
fn ctrl_c_clears_and_warns_then_quits() {
    let mut h = Harness::new(80, 20);
    h.ui.model_mut().bottom.exit_note = Some("the run keeps going".into());
    h.settle();
    h.key(KeyCode::Char('x'));
    assert!(
        h.key_with(KeyCode::Char('c'), KeyModifiers::CONTROL)
            .is_empty()
    );
    assert!(
        h.ui.model().bottom.composer.is_empty(),
        "the first press clears the draft"
    );
    h.settle();
    let rows = h.rows();
    assert!(
        rows.iter()
            .any(|row| row.contains("Press Ctrl-C again to exit · the run keeps going")),
        "{}",
        dump(&rows)
    );
    assert_eq!(
        h.key_with(KeyCode::Char('c'), KeyModifiers::CONTROL),
        vec![UiAction::Quit]
    );
}

#[test]
fn keys_do_what_the_reference_says() {
    let mut h = Harness::new(80, 24);
    h.settle();
    assert_eq!(
        h.key_with(KeyCode::BackTab, KeyModifiers::SHIFT),
        vec![UiAction::ModeChanged(ApprovalMode::Shadow)]
    );
    h.settle();
    assert!(
        h.rows()
            .iter()
            .any(|row| row == "  ǁ shadow mode on (shift+tab to cycle) · ? for shortcuts"),
        "{}",
        dump(&h.rows())
    );

    assert!(h.key(KeyCode::Esc).is_empty());
    h.ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", h.now));
    assert_eq!(h.key(KeyCode::Esc), vec![UiAction::Interrupt]);
    h.ui.model_mut().bottom.working = None;

    let options = vec![
        CardOption::irreversible("Merge"),
        CardOption::irreversible("Discard"),
    ];
    h.ui.model_mut().bottom.card = Some(CardPrompt::new(
        7,
        "Merge card-002 into main?",
        "Merge card-002?",
        options,
    ));
    h.settle();
    assert!(
        h.rows().iter().any(|row| row == "❯ ↑ to answer the card"),
        "{}",
        dump(&h.rows())
    );
    assert!(h.key(KeyCode::Tab).is_empty());
    assert_eq!(h.key(KeyCode::Esc), vec![UiAction::SetAside { card: 7 }]);
    h.settle();
    assert!(
        h.rows()
            .iter()
            .any(|row| row == "◆ Merge card-002? · needs you · ↑ to answer"),
        "{}",
        dump(&h.rows())
    );
    // Discard can't be undone, so its number asks for Enter first, and the number again confirms.
    h.key(KeyCode::Up);
    h.settle();
    assert!(
        h.rows().iter().any(|row| row.contains("2. Discard")),
        "{}",
        dump(&h.rows())
    );
    assert!(h.key(KeyCode::Char('x')).is_empty());
    assert_eq!(h.ui.model().bottom.card.as_ref().unwrap().armed, None);
    // After a letter, a number waits for a key meant for the card.
    assert!(h.key(KeyCode::Char('2')).is_empty());
    assert_eq!(h.ui.model().bottom.card.as_ref().unwrap().armed, None);
    h.key(KeyCode::Up);
    assert!(h.key(KeyCode::Char('2')).is_empty());
    h.settle();
    assert!(
        h.rows()
            .iter()
            .any(|row| row == "  Enter to discard · Esc to cancel"),
        "{}",
        dump(&h.rows())
    );
    assert_eq!(
        h.key(KeyCode::Char('2')),
        vec![UiAction::Answer {
            card: 7,
            option: 1,
            note: None,
            checked: Vec::new(),
        }]
    );
    assert!(h.ui.model().bottom.card.is_none());

    assert_eq!(h.key(KeyCode::Left), vec![UiAction::OpenCrewView]);
}
