//! vt100 0.16 drops rows scrolled out of a region, as some terminals do, and keeps only rows
//! that scroll off the whole screen. So its scrollback shows what any terminal would keep.

use ratatui::layout::{Position, Size};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::*;

const WIDTH: u16 = 20;
const SCROLLBACK: usize = 100;

fn terminal(rows: u16, cursor_row: u16) -> InlineTerminal<vt100::Parser> {
    crossterm::style::force_color_output(true);
    InlineTerminal::new(
        vt100::Parser::new(rows, WIDTH, SCROLLBACK),
        Size::new(WIDTH, rows),
        Position::new(0, cursor_row),
    )
}

fn screen(term: &InlineTerminal<vt100::Parser>) -> Vec<String> {
    term.writer()
        .screen()
        .rows(0, WIDTH)
        .map(|row| row.trim_end().to_owned())
        .collect()
}

/// Oldest row first.
fn scrollback(term: &mut InlineTerminal<vt100::Parser>) -> Vec<String> {
    let screen = term.writer_mut().screen_mut();
    screen.set_scrollback(usize::MAX);
    let mut rows = Vec::new();
    for offset in (1..=screen.scrollback()).rev() {
        screen.set_scrollback(offset);
        rows.extend(
            screen
                .rows(0, WIDTH)
                .next()
                .map(|row| row.trim_end().to_owned()),
        );
    }
    screen.set_scrollback(0);
    rows
}

fn draw(term: &mut InlineTerminal<vt100::Parser>, lines: &[Line<'static>]) {
    term.draw(lines.len() as u16, |frame| frame.paint_lines(lines))
        .unwrap();
}

fn live(rows: &[&str]) -> Vec<Line<'static>> {
    rows.iter().map(|row| Line::from(row.to_string())).collect()
}

fn history(rows: &[&str]) -> Vec<Line<'static>> {
    live(rows)
}

fn with_shell_line(term: &mut InlineTerminal<vt100::Parser>) {
    term.writer_mut().process(b"$ ui-demo\r\n");
}

#[test]
fn the_start_scrolls_shell_output_into_scrollback_without_erasing_it() {
    crossterm::style::force_color_output(true);
    let mut shell = vt100::Parser::new(6, WIDTH, SCROLLBACK);
    shell.process(b"one\r\ntwo\r\n$ ui-demo\r\n");
    let mut term = InlineTerminal::new(Vec::new(), Size::new(WIDTH, 6), Position::new(0, 3));
    term.insert_history(history(&["header"]));
    term.draw(2, |frame| frame.paint_lines(&live(&["-- live --", "> "])))
        .unwrap();
    let bytes = term.writer().clone();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("\x1b[3J"), "{text:?}");
    assert!(!text.contains("\x1b[2J"), "{text:?}");

    shell.process(&bytes);
    let mut term = InlineTerminal::new(shell, Size::new(WIDTH, 6), Position::new(0, 0));
    assert_eq!(screen(&term), ["header", "", "", "", "-- live --", ">"]);
    assert_eq!(scrollback(&mut term), ["one", "two", "$ ui-demo"]);
}

#[test]
fn history_fills_down_from_the_top_and_the_live_region_keeps_the_bottom() {
    let mut term = terminal(8, 1);
    with_shell_line(&mut term);
    draw(&mut term, &live(&["-- live --", "> "]));
    assert_eq!(screen(&term)[6..], ["-- live --", ">"]);
    assert!(screen(&term)[..6].iter().all(String::is_empty));

    term.insert_history(history(&["first", "second"]));
    draw(&mut term, &live(&["-- live --", "> "]));
    assert_eq!(
        screen(&term),
        ["first", "second", "", "", "", "", "-- live --", ">"]
    );
    // The viewport reaches up to history, for rows that trail right under it.
    assert_eq!(term.viewport(), Rect::new(0, 2, WIDTH, 6));
    assert_eq!(term.history_end(), 2);
    assert_eq!(scrollback(&mut term), ["$ ui-demo"]);
}

#[test]
fn history_that_reaches_the_live_region_scrolls_into_scrollback() {
    let mut term = terminal(6, 2);
    term.writer_mut().process(b"one\r\ntwo\r\n");
    draw(&mut term, &live(&["-- live --", "> "]));

    term.insert_history(history(&["h1", "h2", "h3"]));
    draw(&mut term, &live(&["-- live --", "> "]));
    assert_eq!(screen(&term), ["h1", "h2", "h3", "", "-- live --", ">"]);

    term.insert_history(history(&["h4", "h5"]));
    draw(&mut term, &live(&["-- live --", "> "]));
    assert_eq!(screen(&term), ["h2", "h3", "h4", "h5", "-- live --", ">"]);
    assert_eq!(term.viewport(), Rect::new(0, 4, WIDTH, 2));
    assert_eq!(scrollback(&mut term), ["one", "two", "h1"]);
}

#[test]
fn a_taller_live_region_pushes_history_up_and_a_shorter_one_leaves_blank_rows() {
    let mut term = terminal(6, 0);
    term.insert_history(history(&["h1", "h2", "h3"]));
    draw(&mut term, &live(&["x", "y"]));
    assert_eq!(screen(&term), ["h1", "h2", "h3", "", "x", "y"]);

    draw(&mut term, &live(&["x", "y", "z", "w"]));
    assert_eq!(screen(&term), ["h2", "h3", "x", "y", "z", "w"]);
    assert_eq!(term.viewport(), Rect::new(0, 2, WIDTH, 4));
    assert_eq!(scrollback(&mut term), ["h1"]);

    draw(&mut term, &live(&["only"]));
    assert_eq!(screen(&term), ["h2", "h3", "", "", "", "only"]);
    assert_eq!(term.history_end(), 2);
}

#[test]
fn history_that_overflows_the_screen_lands_in_scrollback() {
    let mut term = terminal(6, 1);
    with_shell_line(&mut term);
    let viewport = live(&["-- live --", "> "]);
    draw(&mut term, &viewport);

    let rows: Vec<String> = (1..=10).map(|n| format!("h{n}")).collect();
    term.insert_history(rows.iter().map(|row| Line::from(row.clone())).collect());
    draw(&mut term, &viewport);
    // Then one row at a time, as an answer streams.
    for row in ["h11", "h12", "h13"] {
        term.insert_history(history(&[row]));
        draw(&mut term, &viewport);
    }

    assert_eq!(
        screen(&term),
        ["h10", "h11", "h12", "h13", "-- live --", ">"]
    );
    let mut expected = vec!["$ ui-demo".to_owned()];
    expected.extend((1..=9).map(|n| format!("h{n}")));
    assert_eq!(scrollback(&mut term), expected);
    assert_eq!(term.viewport(), Rect::new(0, 4, WIDTH, 2));
}

#[test]
fn a_row_wider_than_the_screen_scrolls_by_every_row_it_wraps_to() {
    let mut term = terminal(4, 0);
    draw(&mut term, &live(&["live"]));
    let wide = "w".repeat(usize::from(WIDTH) + 5);
    term.insert_history(history(&["h1", "h2", &wide]));
    draw(&mut term, &live(&["live"]));
    assert_eq!(screen(&term), ["h2", &wide[..20], "wwwww", "live"]);
    assert_eq!(scrollback(&mut term), ["h1"]);
    assert_eq!(term.viewport(), Rect::new(0, 3, WIDTH, 1));
}

#[test]
fn history_under_an_empty_live_region_reaches_the_last_row() {
    let mut term = terminal(3, 0);
    term.insert_history(history(&["h1", "h2", "h3"]));
    draw(&mut term, &[]);
    assert_eq!(screen(&term), ["h1", "h2", "h3"]);
    assert_eq!(term.history_end(), 3);

    term.insert_history(history(&["h4"]));
    draw(&mut term, &[]);
    assert_eq!(screen(&term), ["h2", "h3", "h4"]);
    assert_eq!(scrollback(&mut term), ["h1"]);

    // The shell prompt gets a fresh row under the last one.
    term.clear_live().unwrap();
    assert_eq!(screen(&term), ["h3", "h4", ""]);
    assert_eq!(term.writer().screen().cursor_position(), (2, 0));
}

#[test]
fn a_line_fills_to_the_edge_except_unfilled_spans() {
    let mut term = terminal(4, 2);
    let band = Style::new().bg(Color::Blue);
    let row = Line::from(vec![
        Span::styled("  ", Style::new().bg(Color::Reset)),
        Span::raw("added"),
    ])
    .style(band);
    term.insert_history(vec![row.clone()]);
    draw(&mut term, &[row]);

    let screen = term.writer().screen();
    let blue = vt100::Color::Idx(4);
    for row in [0, 3] {
        assert_eq!(
            screen.cell(row, 0).unwrap().bgcolor(),
            vt100::Color::Default
        );
        assert_eq!(
            screen.cell(row, 1).unwrap().bgcolor(),
            vt100::Color::Default
        );
        for col in 2..WIDTH {
            assert_eq!(
                screen.cell(row, col).unwrap().bgcolor(),
                blue,
                "{row},{col}"
            );
        }
    }
}

#[test]
fn a_full_row_keeps_its_last_cell() {
    let full = "x".repeat(usize::from(WIDTH));
    let line = Line::from(full.clone()).style(Style::new().bg(Color::Blue));
    let mut out = Vec::new();
    write_line(&mut out, &line, WIDTH).unwrap();
    let out = String::from_utf8(out).unwrap();
    // One erase clears the row before the text, and none may follow it: at the pending wrap
    // position an erase would eat the last cell.
    let after_text = &out[out.find(&full).unwrap() + full.len()..];
    assert!(!after_text.contains("\x1b[K"), "{out:?}");

    let short = Line::from("x").style(Style::new().bg(Color::Blue));
    let mut out = Vec::new();
    write_line(&mut out, &short, WIDTH).unwrap();
    assert!(String::from_utf8(out).unwrap().ends_with("\x1b[K\x1b[0m"));
}

/// An artifact's whole address in a history row is a link the terminal can open (OSC 8),
/// with the address as its text, so the row reads the same where links aren't known. One a
/// narrow row cut short stays plain.
#[test]
fn an_artifacts_address_is_written_as_a_link() {
    let url = "https://claude.ai/code/artifact/0b1e7c9a-4f1d-4c57-9a55-2f6f0d3a8e11";
    let written = |text: &str| {
        let mut out = Vec::new();
        write_line(&mut out, &Line::from(text.to_owned()), 120).unwrap();
        String::from_utf8(out).unwrap()
    };
    let out = written(&format!("  ⎿  Published {url}"));
    assert!(
        out.contains(&format!(
            "  ⎿  Published \x1b]8;;{url}\x1b\\{url}\x1b]8;;\x1b\\"
        )),
        "{out:?}"
    );
    let cut = written(&format!("  ⎿  Published {}", &url[..url.len() - 9]));
    assert!(!cut.contains("\x1b]8"), "{cut:?}");
    assert!(!written("see https://claude.ai/settings").contains("\x1b]8"));

    // The screen shows the row as it would without the link.
    let mut shown = vt100::Parser::new(2, 120, 0);
    shown.process(out.as_bytes());
    assert_eq!(
        shown.screen().contents().trim_end(),
        format!("  ⎿  Published {url}")
    );
}

#[test]
fn a_full_row_keeps_its_last_cell_on_screen() {
    let mut term = terminal(4, 2);
    let full = format!("{}!", "x".repeat(usize::from(WIDTH) - 1));
    let band = Style::new().bg(Color::Blue);
    term.insert_history(vec![
        Line::from(full.clone()).style(band),
        Line::from("next"),
    ]);
    draw(&mut term, &live(&["live"]));
    let rows = screen(&term);
    assert_eq!(rows[0], full);
    assert_eq!(rows[1], "next");
    let last = term.writer().screen().cell(0, WIDTH - 1).unwrap();
    assert_eq!(last.contents(), "!");
    assert_eq!(last.bgcolor(), vt100::Color::Idx(4));
}

#[test]
fn the_overlay_screen_rewrites_only_changed_rows() {
    crossterm::style::force_color_output(true);
    let mut term = InlineTerminal::new(Vec::new(), Size::new(WIDTH, 4), Position::new(0, 0));
    term.enter_alternate_screen().unwrap();
    let rows = |texts: &[&str]| -> Vec<Line<'static>> {
        texts
            .iter()
            .map(|text| Line::from(text.to_string()))
            .collect()
    };
    term.draw_screen(&rows(&["one", "two", "three"])).unwrap();
    let before = term.writer().len();
    term.draw_screen(&rows(&["one", "two", "three"])).unwrap();
    assert_eq!(term.writer().len(), before);

    term.draw_screen(&rows(&["one", "2"])).unwrap();
    let step = String::from_utf8(term.writer()[before..].to_vec()).unwrap();
    assert!(
        step.contains("\x1b[2;1H") && step.contains("\x1b[3;1H"),
        "{step:?}"
    );
    assert!(
        !step.contains("one") && !step.contains("\x1b[4;1H"),
        "{step:?}"
    );

    term.leave_alternate_screen().unwrap();
    term.enter_alternate_screen().unwrap();
    let before = term.writer().len();
    term.draw_screen(&rows(&["one", "2"])).unwrap();
    let again = String::from_utf8(term.writer()[before..].to_vec()).unwrap();
    assert!(again.contains("one"), "{again:?}");
}

#[test]
fn wide_characters_print_whole() {
    let mut term = terminal(5, 2);
    term.insert_history(history(&["你好世界 ok", "😀 emoji"]));
    draw(&mut term, &live(&["東京 live"]));
    assert_eq!(
        screen(&term)[..5],
        ["你好世界 ok", "😀 emoji", "", "", "東京 live"]
    );
    let cell = term.writer().screen().cell(4, 0).unwrap();
    assert!(cell.is_wide());
}

#[test]
fn control_characters_never_reach_the_terminal() {
    let mut term = terminal(4, 2);
    term.writer_mut().process(b"keep me\r\n");
    let hostile = Line::from(vec![
        Span::raw("a\x1b[2Jb"),
        Span::raw("\x07c\x1b]0;pwned\x07"),
    ]);
    term.insert_history(vec![hostile]);
    draw(&mut term, &live(&["live"]));
    assert_eq!(screen(&term)[0], "a[2Jbc]0;pwned");
    assert_eq!(scrollback(&mut term), ["keep me", ""]);
}

#[test]
fn an_unchanged_frame_writes_nothing() {
    crossterm::style::force_color_output(true);
    let mut term = InlineTerminal::new(Vec::new(), Size::new(WIDTH, 10), Position::new(0, 4));
    let lines = live(&["✻ Rowing… (3s)", "", "─────", "❯ "]);
    let paint = |frame: &mut Frame<'_>| {
        frame.paint_lines(&lines);
        frame.set_cursor(2, 3);
    };
    term.draw(4, paint).unwrap();
    let written = term.writer().len();
    assert!(written > 0);
    term.draw(4, paint).unwrap();
    assert_eq!(term.writer().len(), written);
}

#[test]
fn a_spinner_step_writes_a_few_cells() {
    crossterm::style::force_color_output(true);
    let mut term = InlineTerminal::new(Vec::new(), Size::new(80, 20), Position::new(0, 4));
    let frame_with = |glyph: &str| {
        vec![
            Line::from(vec![
                Span::styled(glyph.to_owned(), Style::new().fg(Color::Yellow)),
                Span::raw(" Rowing… (3s · ↓ 1.2k tokens)"),
            ]),
            Line::default(),
            Line::from("─".repeat(80)),
            Line::from("❯ "),
            Line::from("─".repeat(80)),
        ]
    };
    let paint = |lines: &[Line<'static>]| {
        let lines = lines.to_vec();
        move |frame: &mut Frame<'_>| {
            frame.paint_lines(&lines);
            frame.set_cursor(2, 3);
        }
    };
    term.draw(5, paint(&frame_with("✢"))).unwrap();
    let before = term.writer().len();
    term.draw(5, paint(&frame_with("✳"))).unwrap();
    let step = String::from_utf8(term.writer()[before..].to_vec()).unwrap();
    // The synchronized update markers, a move, a color, the glyph, a reset, and the cursor
    // resting back in the composer.
    assert!(step.len() < 64, "{step:?}");
    assert_eq!(step.matches("\x1b[").count(), 6, "{step:?}");
    assert!(step.ends_with("\x1b[19;3H\x1b[?2026l"), "{step:?}");
    assert!(step.contains('✳'));
}

#[test]
fn resize_repaints_the_viewport_at_the_new_width() {
    let mut term = terminal(6, 2);
    draw(&mut term, &live(&["─".repeat(20).as_str(), "> typing"]));
    term.writer_mut().screen_mut().set_size(6, 12);
    term.resize(Size::new(12, 6), None).unwrap();
    draw(&mut term, &live(&["─".repeat(12).as_str(), "> typing"]));
    let rows: Vec<String> = term
        .writer()
        .screen()
        .rows(0, 12)
        .map(|row| row.trim_end().to_owned())
        .collect();
    assert!(rows[..4].iter().all(String::is_empty), "{rows:?}");
    assert_eq!(rows[4], "─".repeat(12));
    assert_eq!(rows[5], "> typing");
}

#[test]
fn clear_live_leaves_history_and_a_clean_row() {
    let mut term = terminal(6, 1);
    with_shell_line(&mut term);
    term.insert_history(history(&["answer"]));
    draw(&mut term, &live(&["", "─────", "> ", "─────"]));
    term.clear_live().unwrap();
    let rows = screen(&term);
    assert_eq!(rows[0], "answer");
    assert!(rows[1..].iter().all(String::is_empty), "{rows:?}");
    assert_eq!(term.writer().screen().cursor_position(), (1, 0));
    assert!(!term.writer().screen().hide_cursor());
}

#[test]
fn quitting_before_the_first_frame_leaves_the_shell_rows_alone() {
    let mut term = terminal(4, 1);
    with_shell_line(&mut term);
    term.clear_live().unwrap();
    assert_eq!(screen(&term), ["$ ui-demo", "", "", ""]);
    assert_eq!(term.writer().screen().cursor_position(), (1, 0));
    assert!(scrollback(&mut term).is_empty());
}

#[test]
fn a_second_start_shows_only_its_own_rows() {
    let mut term = terminal(8, 0);
    term.insert_history(history(&["first header", "first answer"]));
    draw(&mut term, &live(&["-- live --", "> "]));
    term.clear_live().unwrap();
    // The shell prints its prompt under the last history row, and the user starts again.
    term.writer_mut().process(b"$ ui-demo\r\n");
    let (row, column) = term.writer().screen().cursor_position();
    let parser = std::mem::replace(term.writer_mut(), vt100::Parser::new(8, WIDTH, 0));
    let mut term = InlineTerminal::new(parser, Size::new(WIDTH, 8), Position::new(column, row));

    term.insert_history(history(&["second header"]));
    draw(&mut term, &live(&["-- live --", "> "]));
    assert_eq!(
        screen(&term),
        ["second header", "", "", "", "", "", "-- live --", ">"]
    );
    assert_eq!(
        scrollback(&mut term),
        ["first header", "first answer", "$ ui-demo"]
    );
}

#[test]
fn a_view_before_the_first_frame_still_starts_on_a_clean_screen() {
    let mut term = terminal(4, 1);
    with_shell_line(&mut term);
    term.enter_alternate_screen().unwrap();
    term.leave_alternate_screen().unwrap();
    draw(&mut term, &live(&["live"]));
    assert_eq!(screen(&term), ["", "", "", "live"]);
    assert_eq!(scrollback(&mut term), ["$ ui-demo"]);
}

#[test]
fn the_title_is_written_once_per_change() {
    let mut term = InlineTerminal::new(Vec::new(), Size::new(WIDTH, 5), Position::new(0, 0));
    term.set_title("✳ conversation").unwrap();
    term.set_title("✳ conversation").unwrap();
    term.set_title("◐ conversation\x07").unwrap();
    let out = String::from_utf8(term.writer().clone()).unwrap();
    assert_eq!(out, "\x1b]0;✳ conversation\x07\x1b]0;◐ conversation\x07");
}

#[test]
fn the_progress_bar_is_written_once_per_change_and_never_before_work() {
    let mut term = InlineTerminal::new(Vec::new(), Size::new(WIDTH, 5), Position::new(0, 0));
    term.set_progress(false).unwrap();
    term.set_progress(true).unwrap();
    term.set_progress(true).unwrap();
    term.set_progress(false).unwrap();
    term.set_progress(false).unwrap();
    let out = String::from_utf8(term.writer().clone()).unwrap();
    assert_eq!(out, "\x1b]9;4;3\x07\x1b]9;4;0\x07");
}

#[test]
fn a_cursor_after_text_sends_its_row_to_scrollback_too() {
    let mut term = terminal(4, 3);
    term.writer_mut().process(b"\x1b[4;1Hpartial");
    let mut term = InlineTerminal::new(
        std::mem::replace(term.writer_mut(), vt100::Parser::new(4, WIDTH, 0)),
        Size::new(WIDTH, 4),
        Position::new(7, 3),
    );
    draw(&mut term, &live(&["live"]));
    assert_eq!(screen(&term), ["", "", "", "live"]);
    assert_eq!(
        scrollback(&mut term).last().map(String::as_str),
        Some("partial")
    );
}

#[test]
fn resize_follows_the_cursor_of_a_rewrapping_terminal() {
    let mut term = terminal(8, 0);
    term.insert_history(history(&["h1", "h2"]));
    term.draw(2, |frame| {
        frame.paint_lines(&live(&["-- live --", "> "]));
        frame.set_cursor(2, 1);
    })
    .unwrap();
    // The terminal grew, pulled two rows back from scrollback, and says the cursor is two rows
    // lower than where it rested.
    term.resize(Size::new(WIDTH, 10), Some(Position::new(2, 9)))
        .unwrap();
    assert_eq!(term.history_end(), 4);
    term.resize(Size::new(WIDTH, 5), None).unwrap();
    assert_eq!(term.history_end(), 4);
    term.draw(2, |frame| frame.paint_lines(&live(&["-- live --", "> "])))
        .unwrap();
    assert_eq!(term.history_end(), 3);
    assert_eq!(term.viewport(), Rect::new(0, 3, WIDTH, 2));
}

#[test]
fn a_burst_of_resizes_adds_up_the_cursor_moves() {
    let mut term = terminal(10, 0);
    term.insert_history(history(&["h1", "h2", "h3", "h4"]));
    term.draw(2, |frame| {
        frame.paint_lines(&live(&["-- live --", "> "]));
        frame.set_cursor(2, 1);
    })
    .unwrap();
    // Resting on row 9. Each resize moves the cursor a row up before any frame.
    term.resize(Size::new(WIDTH, 10), Some(Position::new(2, 8)))
        .unwrap();
    term.resize(Size::new(WIDTH, 10), Some(Position::new(2, 7)))
        .unwrap();
    assert_eq!(term.history_end(), 2);
}

#[test]
fn growing_under_a_single_history_row_scrolls_the_whole_screen() {
    let mut term = terminal(5, 1);
    with_shell_line(&mut term);
    term.insert_history(history(&["h"]));
    draw(&mut term, &live(&["x"]));
    draw(&mut term, &live(&["1", "2", "3", "4", "5"]));
    assert_eq!(screen(&term), ["1", "2", "3", "4", "5"]);
    assert_eq!(term.viewport(), Rect::new(0, 0, WIDTH, 5));
    assert_eq!(scrollback(&mut term), ["$ ui-demo", "h"]);
}

fn draw_split(term: &mut InlineTerminal<Vec<u8>>, trailing: &[&str], pinned: &[&str]) {
    let (trailing, pinned) = (live(trailing), live(pinned));
    term.draw_live(trailing.len() as u16, pinned.len() as u16, |frame| {
        frame.paint_trailing(&trailing);
        frame.paint_lines(&pinned);
    })
    .unwrap();
}

fn replay(bytes: &[u8], rows: u16) -> Vec<String> {
    let mut parser = vt100::Parser::new(rows, WIDTH, SCROLLBACK);
    parser.process(bytes);
    parser
        .screen()
        .rows(0, WIDTH)
        .map(|row| row.trim_end().to_owned())
        .collect()
}

#[test]
fn trailing_rows_read_on_from_history_while_the_rest_stays_pinned() {
    crossterm::style::force_color_output(true);
    let mut term = InlineTerminal::new(Vec::new(), Size::new(WIDTH, 8), Position::new(0, 0));
    term.insert_history(history(&["header"]));
    draw_split(&mut term, &["Third li"], &["rule", "> "]);
    assert_eq!(
        replay(term.writer(), 8),
        ["header", "Third li", "", "", "", "", "rule", ">"]
    );

    // The finished line prints where the tail was, and the tail moves down a row. The pinned
    // rows stay as they are on screen, so nothing of them is written again.
    let before = term.writer().len();
    term.insert_history(history(&["Third line."]));
    draw_split(&mut term, &["Fourth"], &["rule", "> "]);
    let step = String::from_utf8_lossy(&term.writer()[before..]).into_owned();
    assert!(!step.contains("rule"), "{step:?}");
    assert_eq!(
        replay(term.writer(), 8),
        ["header", "Third line.", "Fourth", "", "", "", "rule", ">"]
    );
    assert_eq!(term.history_end(), 2);
}

#[test]
fn trailing_rows_meet_the_pinned_ones_once_history_fills_the_screen() {
    crossterm::style::force_color_output(true);
    let mut term = InlineTerminal::new(Vec::new(), Size::new(WIDTH, 6), Position::new(0, 0));
    term.insert_history(history(&["h1", "h2", "h3", "h4"]));
    draw_split(&mut term, &["tail"], &["rule", "> "]);
    assert_eq!(
        replay(term.writer(), 6),
        ["h2", "h3", "h4", "tail", "rule", ">"]
    );
    assert_eq!(term.history_end(), 3);

    // A shorter tail leaves its row blank, and history keeps its place.
    draw_split(&mut term, &[], &["rule", "> "]);
    assert_eq!(
        replay(term.writer(), 6),
        ["h2", "h3", "h4", "", "rule", ">"]
    );
}

/// What printed before the page turn goes up into scrollback whole, rows queued with it
/// included, and what follows starts at the top of a blank screen above the live region.
#[test]
fn a_page_turn_scrolls_history_away_and_prints_the_rest_at_the_top() {
    let mut term = terminal(8, 0);
    term.insert_history(history(&["old header", "a note"]));
    draw(&mut term, &live(&["-- live --", "> "]));
    term.insert_history(history(&["an answer"]));
    term.turn_page();
    term.insert_history(history(&["new header", "saved"]));
    draw(&mut term, &live(&["-- live --", "> "]));

    assert_eq!(
        screen(&term),
        ["new header", "saved", "", "", "", "", "-- live --", ">"]
    );
    assert_eq!(scrollback(&mut term), ["old header", "a note", "an answer"]);

    // Later history goes on under the new page.
    term.insert_history(history(&["next"]));
    draw(&mut term, &live(&["-- live --", "> "]));
    assert_eq!(screen(&term)[..3], ["new header", "saved", "next"]);
}

/// A page turn when history already fills the screen keeps every row, in order.
#[test]
fn a_page_turn_after_a_full_screen_keeps_every_row() {
    let mut term = terminal(5, 0);
    let rows: Vec<String> = (1..=6).map(|n| format!("row {n}")).collect();
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    term.insert_history(history(&rows));
    draw(&mut term, &live(&["> "]));
    term.turn_page();
    term.insert_history(history(&["fresh"]));
    draw(&mut term, &live(&["> "]));

    assert_eq!(screen(&term), ["fresh", "", "", "", ">"]);
    assert_eq!(
        scrollback(&mut term),
        ["row 1", "row 2", "row 3", "row 4", "row 5", "row 6"]
    );
}

/// The header changes where it stands while it shows, and nothing under it moves.
#[test]
fn marked_rows_print_over_in_place_while_they_show() {
    let mut term = terminal(8, 0);
    term.insert_marked_history(history(&["", "old header", ""]), 1..2);
    draw(&mut term, &live(&["-- live --", "> "]));
    term.insert_history(history(&["a record", "another"]));
    draw(&mut term, &live(&["-- live --", "> "]));
    term.redraw_marked(history(&["new header"]));
    term.insert_history(history(&["saved"]));
    draw(&mut term, &live(&["-- live --", "> "]));

    assert_eq!(
        screen(&term),
        [
            "",
            "new header",
            "",
            "a record",
            "another",
            "saved",
            "-- live --",
            ">"
        ]
    );
    assert!(scrollback(&mut term).is_empty());
}

/// History that scrolled up still finds its marked rows.
#[test]
fn marked_rows_are_found_after_history_scrolls_up() {
    let mut term = terminal(6, 0);
    term.insert_marked_history(history(&["", "old header", "a"]), 1..2);
    draw(&mut term, &live(&["> "]));
    draw(&mut term, &live(&["1", "2", "3", "> "]));
    term.redraw_marked(history(&["new header"]));
    draw(&mut term, &live(&["1", "2", "3", "> "]));

    assert_eq!(screen(&term), ["new header", "a", "1", "2", "3", ">"]);
    assert_eq!(scrollback(&mut term), [""]);
}

/// Scrollback can't change, so a header that went up there stays as it was.
#[test]
fn marked_rows_in_scrollback_stay_as_they_were() {
    let mut term = terminal(5, 0);
    term.insert_marked_history(history(&["old header"]), 0..1);
    draw(&mut term, &live(&["> "]));
    term.insert_history(history(&["r1", "r2", "r3", "r4"]));
    draw(&mut term, &live(&["> "]));
    term.redraw_marked(history(&["new header"]));
    draw(&mut term, &live(&["> "]));

    assert_eq!(screen(&term), ["r1", "r2", "r3", "r4", ">"]);
    assert_eq!(scrollback(&mut term), ["old header"]);
}
