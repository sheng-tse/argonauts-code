//! The Council screens' conversation held to their drawings in `approved/council/` at
//! 120 × 38, logos drawn as pictures: what the history prints under the session's header and
//! the live typing row, down to the row above the live region. The live region under it, the
//! working line, the input, the bottom line and the run's rows, is the bottom's to draw.

use std::time::Instant;

use argonauts_core::base::names::SeatId;
use argonauts_testkit::approved::{self, Drawing, Palette, differences_at, report};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::HistoryCell;
use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::theme::Theme;
use crate::cells::cell::Cell;
use crate::council::tests::golden::{QWEN_WRITES, reason};
use crate::council::tests::story::{self, *};
use crate::council::{Item, Look, Piece, Spacing, TalkCell};

const COLS: u16 = 120;
const ROWS: u16 = 38;

/// A council's conversation as the app prints it under the session's header.
struct Conversation {
    bridge: crate::bridge::Bridge,
    model: crate::app::model::ViewModel,
    typing: Vec<Line<'static>>,
    now: Instant,
}

impl Conversation {
    fn new() -> Self {
        let mut model = crate::app::model::ViewModel::default();
        model.history.push(Cell::Header(header()));
        Self {
            bridge: crate::bridge::Bridge::new(&crate::UiOptions::default()),
            model,
            typing: Vec::new(),
            now: Instant::now(),
        }
    }

    fn theme() -> Theme {
        crate::testing::pictures::approved_screen::theme()
    }

    /// The command that switched to the council, and its answer, as the conversation echoes
    /// them.
    fn switched(&mut self) {
        use argonauts_core::app::AppEvent;
        use argonauts_core::settings::SettingScope;

        use crate::app::UiAction;

        let typed = UiAction::Submit {
            text: "/council".into(),
            shown: "/council".into(),
        };
        self.bridge.on_action(&mut self.model, &typed, self.now);
        let switched = AppEvent::SettingChanged {
            key: "workflow".into(),
            value: "\"council\"".into(),
            scope: SettingScope::Global,
            previous: None,
            reset: false,
            why: None,
            kept: false,
        };
        self.bridge.on_event(&mut self.model, &switched, self.now);
    }

    /// The council's `items` as the history prints them.
    fn print(&mut self, items: &[Item]) {
        let mut spacing = Spacing::default();
        for item in items {
            self.model.history.push(Cell::Custom(Box::new(TalkCell::new(
                item.clone(),
                Look::Pictures,
                story::faces(),
                &mut spacing,
            ))));
        }
    }

    /// `blank` rows, then who writes now and why.
    fn typing(&mut self, blank: usize, writers: &[SeatId], why: &[Piece]) {
        let theme = Self::theme();
        let (line, _) =
            Look::Pictures.typing(writers, 1, why, usize::from(COLS), &story::faces(), &theme);
        self.typing.resize(blank, Line::default());
        self.typing.push(line);
    }

    /// The last `rows` rows of the conversation, under the session's header's own rows.
    fn rows(&mut self, rows: u16) -> Buffer {
        let theme = Self::theme();
        let ctx = RenderCtx::new(COLS, &theme, Clock::still(self.now)).with_rows(ROWS);
        let header = HistoryCell::lines(&header(), &ctx).len();
        let mut printed = self.model.history.drain_printable(&ctx);
        printed.drain(..header.min(printed.len()));
        printed.extend(self.typing.iter().cloned());
        let shown = &printed[printed.len().saturating_sub(rows.into())..];
        let mut buf = Buffer::empty(Rect::new(0, 0, COLS, rows));
        let first = usize::from(rows) - shown.len();
        for (y, line) in (first..).zip(shown) {
            buf.set_line(0, y as u16, line, COLS);
        }
        buf
    }
}

/// The session's header the council's screens sit under.
fn header() -> crate::cells::cell::SessionHeader {
    crate::cells::cell::SessionHeader {
        app: "Argonauts Code".into(),
        version: "0.2.0".into(),
        main_agent: Some("Claude Code".into()),
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        workflow: "council".into(),
        ultra: None,
        folder: "~/code/shop-api".into(),
        agents: Vec::new(),
        runs_going: 0,
        runs_need_you: 0,
        way_back: None,
    }
}

/// Panics with the differences where `buf` is different from the drawing `file`'s rows from
/// `from` on, as many as `buf` has, its first row at row `top + from` of the screen.
#[track_caller]
fn assert_rows_match(file: &str, from: usize, buf: &Buffer, top: u16) {
    let text = std::fs::read_to_string(approved::dir().join(file))
        .unwrap_or_else(|err| panic!("{file}: {err}"));
    let rows = usize::from(buf.area.height);
    let drawing = Drawing::parse(file, &rows_of(&text, from, rows))
        .unwrap_or_else(|err| panic!("{file}: {err:#}"));
    let drawn = crate::base::stability::grid::on_screen(buf, (COLS, ROWS), top + from as u16);
    let placement = drawing.place(&drawn);
    let found = differences_at(
        &drawing,
        &placement,
        &drawn,
        &Palette::TOKYONIGHT_MOON,
        &|_, _| false,
    );
    if !found.is_empty() {
        panic!("{}", report(file, &drawing, &drawn, &placement, &found));
    }
}

/// A drawing's file with only `rows` of its rows of text and of paint from row `from`, placed
/// where those rows sit on its screen, and the keys they paint with.
fn rows_of(text: &str, from: usize, rows: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let at = |what: &str| lines.iter().position(|line| *line == what);
    let (Some(text_at), Some(paint_at), Some(key_at)) = (at("text"), at("paint"), at("key")) else {
        panic!("a drawing with text, paint and a key");
    };
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for line in &lines[..=text_at] {
        let line = if let Some(size) = line.strip_prefix("size ")
            && let Some((width, _)) = size.split_once('x')
        {
            format!("size {width}x{rows}")
        } else if let Some(place) = line.strip_prefix("at ")
            && let Some((row, col)) = place.split_once(',')
            && let Ok(row) = row.parse::<usize>()
        {
            format!("at {},{col}", row + from)
        } else {
            (*line).to_owned()
        };
        out.push(line);
    }
    let band = |first: usize| {
        lines[first + from..first + from + rows]
            .iter()
            .map(|l| l.to_string())
    };
    out.extend(band(text_at + 1));
    out.push("paint".to_owned());
    let paint: Vec<String> = band(paint_at + 1).collect();
    out.extend(paint.iter().cloned());
    out.push("key".to_owned());
    // A key no cell of these rows paints with leaves the drawing.
    out.extend(
        lines[key_at + 1..]
            .iter()
            .filter(|line| {
                line.chars()
                    .next()
                    .is_some_and(|key| paint.iter().any(|row| row.contains(key)))
            })
            .map(|line| line.to_string()),
    );
    out.join("\n")
}

#[test]
fn sent_draws_its_conversation_as_drawn() {
    let mut conversation = Conversation::new();
    conversation.switched();
    conversation.print(&[
        story::header(),
        line("Today 10:41 AM"),
        prompt(),
        alone_first(),
    ]);
    // A fresh conversation fills down from under the header, so a blank row more waits
    // between the centred line and the typing row.
    conversation.typing(2, &all(), &[]);
    // The echo of `/council` and its answer above it are the conversation's own rows.
    assert_rows_match("council/sent@120x38.txt", 3, &conversation.rows(17), 4);
}

#[test]
fn first_answers_draw_their_conversation_as_drawn() {
    let mut conversation = Conversation::new();
    let mut items = vec![prompt(), alone_first()];
    items.extend(first_answers());
    items.push(everyone_answered());
    conversation.print(&items);
    conversation.typing(1, &[opus()], &[]);
    assert_rows_match(
        "council/first-answers@120x38.txt",
        0,
        &conversation.rows(25),
        0,
    );
}

#[test]
fn the_talk_draws_its_conversation_as_drawn() {
    let mut conversation = Conversation::new();
    conversation.print(&talk(true, false));
    conversation.typing(1, &[qwen()], &reason(QWEN_WRITES));
    assert_rows_match("council/talk@120x38.txt", 0, &conversation.rows(24), 0);
}

#[test]
fn the_plan_draws_its_conversation_as_drawn() {
    let mut conversation = Conversation::new();
    conversation.print(&[
        flash_on_deprecation(3),
        everyone_agreed(),
        story::plan_and_who(),
        story::signed(),
    ]);
    assert_rows_match("council/plan@120x38.txt", 0, &conversation.rows(17), 0);
}

#[test]
fn the_work_starts_as_drawn() {
    let mut conversation = Conversation::new();
    conversation.print(&story::work_starts(None));
    assert_rows_match(
        "council/work-starts@120x38.txt",
        0,
        &conversation.rows(25),
        0,
    );
}
