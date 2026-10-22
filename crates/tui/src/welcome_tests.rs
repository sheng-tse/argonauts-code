//! The greeting drawn from its pictures' examples at their width of 118 columns: the first
//! start's, with the usage line under it, and the end of the curl install's.

use std::time::Instant;

use super::*;
use crate::bridge::transcript::picture_check::assert_rows_match;
use crate::cell::HistoryCell;
use crate::render::{Clock, RenderCtx};
use crate::theme::Glyphs;

const FIRST_START: (&str, &str) = (
    include_str!("../tests/golden/welcome/first_start.txt"),
    include_str!("../tests/golden/welcome/first_start.html"),
);
const CURL_END: (&str, &str) = (
    include_str!("../tests/golden/welcome/curl_end.txt"),
    include_str!("../tests/golden/welcome/curl_end.html"),
);

/// The usage line as the app shows it.
const USAGE: &str = "Sends one anonymous usage report a day, never your code or prompts (PRIVACY.md) \
                     · argonauts telemetry off stops it";
const STOP: &str = "argonauts telemetry off";

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

#[test]
fn the_first_start_greets_as_the_picture_draws_it() {
    let theme = theme();
    let cell = Greeting {
        version: "0.1.0".to_owned(),
        usage: Some((USAGE.to_owned(), STOP.to_owned())),
        picture: false,
    };
    let lines = cell.lines(&RenderCtx::new(118, &theme, Clock::still(Instant::now())));
    assert_rows_match("first_start", &lines, FIRST_START, 2..10, &theme, &[]);
}

/// The usage line shows only once, so at any width its way out is on screen: it wraps under
/// its own start, and a screen narrower than the block letters names Argonauts Code in words.
#[test]
fn the_usage_line_wraps_and_a_narrow_screen_names_it_in_words() {
    let theme = theme();
    let cell = Greeting {
        version: "0.1.0".to_owned(),
        usage: Some((USAGE.to_owned(), STOP.to_owned())),
        picture: false,
    };
    for width in [50u16, 60, 80, 100] {
        let rows: Vec<String> = cell
            .lines(&RenderCtx::new(width, &theme, Clock::still(Instant::now())))
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        let text = rows.join("\n");
        let read: Vec<&str> = rows.iter().map(|row| row.trim()).collect();
        assert!(
            read.join(" ").contains("argonauts telemetry off stops it"),
            "{width} columns:\n{text}"
        );
        assert!(!text.contains('…'), "{width} columns:\n{text}");
        assert!(
            rows.iter()
                .all(|row| crate::text::str_width(row) <= usize::from(width)),
            "{width} columns:\n{text}"
        );
        if width < 79 {
            assert!(
                text.contains("Argonauts Code   v0.1.0"),
                "{width} columns:\n{text}"
            );
        }
    }
}

fn greeting_rows(picture: bool, theme: &Theme) -> Vec<String> {
    let cell = Greeting {
        version: "0.1.0".to_owned(),
        usage: None,
        picture,
    };
    cell.lines(&RenderCtx::new(118, theme, Clock::still(Instant::now())))
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect()
}

/// Where a picture covers the argonaut, its nine by three cells print blank, under the blank
/// row on top and from the header's mascot column, and the rest of each row stays as it was.
#[test]
fn a_picture_takes_the_argonauts_cells() {
    let mut theme = theme();
    theme.mascot_pictures = true;
    let drawn = greeting_rows(false, &theme);
    let covered = greeting_rows(true, &theme);
    assert_eq!(drawn.len(), covered.len());
    let cells = MARK_COL..MARK_COL + mascot::WIDTH;
    for (row, (drawn, covered)) in drawn.iter().zip(&covered).enumerate() {
        let (drawn, covered): (Vec<char>, Vec<char>) =
            (drawn.chars().collect(), covered.chars().collect());
        if (1..1 + mascot::ROWS).contains(&row) {
            assert!(drawn[cells.clone()].iter().any(|c| *c != ' '), "row {row}");
            assert!(
                covered[cells.clone()].iter().all(|c| *c == ' '),
                "row {row}"
            );
            assert_eq!(drawn[cells.end..], covered[cells.end..], "row {row}");
        } else {
            assert_eq!(drawn, covered, "row {row}");
        }
    }
}

/// Without a picture placed, or with pictures off, the argonaut is drawn in its cells.
#[test]
fn the_argonaut_is_drawn_unless_a_picture_covers_it() {
    let mut theme = theme();
    let drawn = greeting_rows(false, &theme);
    assert_eq!(greeting_rows(true, &theme), drawn);
    theme.mascot_pictures = true;
    assert_eq!(greeting_rows(false, &theme), drawn);
}

#[test]
fn the_curl_install_ends_with_the_greeting_as_the_picture_draws_it() {
    let theme = theme();
    let lines = greeting("0.1.0", Some(NEXT_STEP), &theme);
    assert_rows_match("curl_end", &lines, CURL_END, 9..15, &theme, &[]);
}

/// On a plain terminal the greeting is the same characters in its colours; without colour, the
/// characters alone.
#[test]
fn printed_keeps_the_characters_and_adds_only_colour() {
    let theme = theme();
    let lines = greeting("0.1.0", Some(NEXT_STEP), &theme);
    let plain = printed(&lines, false);
    let want: Vec<&str> = CURL_END.0.split('\n').collect();
    let got: Vec<&str> = plain.lines().collect();
    assert_eq!(got, want[9..15].to_vec());
    let coloured = printed(&lines, true);
    assert!(
        coloured.contains("\x1b[38;2;232;163;61m"),
        "the amber: {coloured:?}"
    );
    let stripped = strip_escapes(&coloured);
    assert_eq!(stripped, plain);
}

/// The name stands in block letters three rows tall, ARGONAUTS three cells from CODE.
#[test]
fn the_name_is_two_words_in_block_letters() {
    let first = word(WORDS.0);
    assert_eq!(first[0], "▄▀▀▄ █▀▀▄ ▄▀▀▀ ▄▀▀▄ █▄ █ ▄▀▀▄ █  █ ▀█▀ ▄▀▀▀");
    assert_eq!(word(WORDS.1)[2], " ▀▀▀  ▀▀  ▀▀▀  ▀▀▀▀");
}

fn strip_escapes(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
