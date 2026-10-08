//! Checks on screen text and on the raw bytes the screen wrote.

use argonauts_testkit::Screen;
use regex::Regex;
use unicode_width::UnicodeWidthStr;

/// Patterns for the parts every screen has.
pub const BULLET: &str = "[●⏺]";
pub const TOP_RULE: &str = "^─+ council · Haiku ─$";
pub const INPUT_ROW: &str = "^❯";
/// The mode line, with each CLI's plan usage against its right edge. The mode's hints are the
/// first things the chips take the room of, so at 100 columns they may be gone.
pub const MODE_IDLE: &str =
    r"^  ǁ supervised mode on( \(shift\+tab to cycle\))?( · \? for shortcuts)?( {2,}\S.*)?$";
/// The mode line's right end at 100 columns: the CLI in use marked and bold, the other dim.
pub const STATUS_IDLE: &str = r" {2,}● Claude 5h .{6} 71%  ○ Codex 5h .{6} 38%$";
/// Work is going: the mode line says how to stop it, or, where the chips took that hint's
/// room, the working line is up.
pub const WORKING: &str = r"esc to interrupt|^[∙✢✲✶✻✼*] [\w'-]+…";
/// The turn end, such as `✻ Sailed for 3s · done 10:42 AM`.
pub const TURN_END: &str = r"^✻ \w+ for \d+s · done \d{1,2}:\d{2}";
pub const WORKING_LINE: &str = r"^[∙✢✲✶✻✼*] [\w'-]+…";

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// Glyphs that draw a frame's vertical edges and the joints on its horizontal edges.
const FRAME_GLYPHS: &str = "│┃║╭╮╰╯┌┐└┘├┤┬┴┼";

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|err| panic!("bad pattern {pattern:?}: {err}"))
}

/// The index of the first visible row matching `pattern`.
pub fn row_of(screen: &Screen, pattern: &str) -> Option<usize> {
    let regex = regex(pattern);
    screen.visible.iter().position(|row| regex.is_match(row))
}

/// Like `row_of`, but from the bottom.
pub fn last_row_of(screen: &Screen, pattern: &str) -> Option<usize> {
    let regex = regex(pattern);
    screen.visible.iter().rposition(|row| regex.is_match(row))
}

pub fn expect_row(screen: &Screen, pattern: &str) -> usize {
    row_of(screen, pattern).unwrap_or_else(|| {
        panic!(
            "no visible row matches {pattern:?}; the screen was:\n{}",
            screen.visible_text()
        )
    })
}

/// Asserts each pattern matches a visible row below the one the pattern before matched.
pub fn assert_order(screen: &Screen, patterns: &[&str]) {
    let mut above: Option<(usize, &str)> = None;
    for pattern in patterns {
        let Some((prev, prev_pattern)) = above else {
            above = Some((expect_row(screen, pattern), pattern));
            continue;
        };
        // The first match below the row before, since a pattern such as `^❯` can also match
        // higher up.
        let regex = regex(pattern);
        let Some(row) =
            (prev + 1..screen.visible.len()).find(|&row| regex.is_match(&screen.visible[row]))
        else {
            panic!(
                "{pattern:?} should be below {prev_pattern:?} (row {prev}); the screen was:\n{}",
                screen.visible_text()
            );
        };
        above = Some((row, pattern));
    }
}

/// How many rows in the scrollback and on screen match.
pub fn count_rows(screen: &Screen, pattern: &str) -> usize {
    let regex = regex(pattern);
    screen
        .scrollback
        .iter()
        .chain(&screen.visible)
        .filter(|row| regex.is_match(row))
        .count()
}

pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// The rows after the last non-blank one, which an inline screen leaves free.
pub fn blank_rows_at_bottom(screen: &Screen) -> usize {
    screen
        .visible
        .iter()
        .rev()
        .take_while(|row| row.trim().is_empty())
        .count()
}

/// A full-width dim rule, as the input's top and bottom edges are drawn.
pub fn is_rule(row: &str) -> bool {
    row.starts_with("──") && row.trim_end().ends_with('─')
}

/// The frame checker: no row wider than the terminal, full-width rules exactly as wide, and in
/// every run of framed rows the edges and joints in the same columns.
pub fn assert_frames(screen: &Screen, cols: u16) {
    let cols = usize::from(cols);
    for (i, row) in screen.visible.iter().enumerate() {
        assert!(
            width(row) <= cols,
            "row {i} is {} cells, wider than {cols}: {row:?}\n{}",
            width(row),
            screen.visible_text()
        );
    }
    let mut run: Option<(usize, Vec<usize>)> = None;
    for (i, row) in screen.visible.iter().enumerate() {
        let edges = frame_columns(row);
        if edges.len() < 2 {
            run = None;
            continue;
        }
        match &run {
            Some((first, expected)) => assert_eq!(
                &edges,
                expected,
                "row {i}'s frame edges don't line up with row {first}'s:\n{}",
                screen.visible_text()
            ),
            None => run = Some((i, edges)),
        }
    }
}

/// The display columns of a row's frame glyphs.
fn frame_columns(row: &str) -> Vec<usize> {
    let mut column = 0;
    let mut edges = Vec::new();
    for c in row.chars() {
        if FRAME_GLYPHS.contains(c) {
            edges.push(column);
        }
        column += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
    }
    edges
}

/// Bells in the output. The bell that ends a title sequence (`ESC ] … BEL`) isn't one.
pub fn bells(raw: &[u8]) -> usize {
    let mut count = 0;
    let mut in_osc = false;
    let mut i = 0;
    while i < raw.len() {
        match raw[i] {
            ESC if raw.get(i + 1) == Some(&b']') => {
                in_osc = true;
                i += 1;
            }
            ESC if in_osc && raw.get(i + 1) == Some(&b'\\') => {
                in_osc = false;
                i += 1;
            }
            BEL if in_osc => in_osc = false,
            BEL => count += 1,
            _ => {}
        }
        i += 1;
    }
    count
}

/// Titles set with `ESC ] 0 ;` or `ESC ] 2 ;`, oldest first.
pub fn titles(raw: &[u8]) -> Vec<String> {
    let mut titles = Vec::new();
    let mut i = 0;
    while i + 3 < raw.len() {
        let starts = raw[i] == ESC
            && raw[i + 1] == b']'
            && matches!(raw[i + 2], b'0' | b'2')
            && raw[i + 3] == b';';
        if !starts {
            i += 1;
            continue;
        }
        let body = i + 4;
        let end = (body..raw.len())
            .find(|&j| raw[j] == BEL || (raw[j] == ESC && raw.get(j + 1) == Some(&b'\\')));
        let Some(end) = end else { break };
        titles.push(String::from_utf8_lossy(&raw[body..end]).into_owned());
        i = end + 1;
    }
    titles
}

/// The raw output with escape sequences removed, for checks on what was drawn at any time.
pub fn plain(raw: &[u8]) -> String {
    String::from_utf8_lossy(&strip_ansi_escapes::strip(raw)).into_owned()
}

/// Whether the output ever set a color: an SGR with 30-38, 40-48, 90-97, or 100-107.
pub fn sets_color(raw: &[u8]) -> bool {
    let text = String::from_utf8_lossy(raw);
    let sgr = Regex::new(r"\x1b\[([0-9;:]*)m").expect("a valid pattern");
    sgr.captures_iter(&text).any(|captures| {
        let params = captures[1].replace(':', ";");
        let mut parts = params.split(';').filter(|p| !p.is_empty());
        let mut colored = false;
        while let Some(part) = parts.next() {
            let Ok(n) = part.parse::<u16>() else { continue };
            match n {
                38 | 48 => {
                    colored = true;
                    // Skip the color's own numbers so they aren't read as attributes.
                    match parts.next() {
                        Some("5") => {
                            parts.next();
                        }
                        Some("2") => {
                            parts.next();
                            parts.next();
                            parts.next();
                        }
                        _ => {}
                    }
                }
                30..=37 | 40..=47 | 90..=97 | 100..=107 => colored = true,
                _ => {}
            }
        }
        colored
    })
}
