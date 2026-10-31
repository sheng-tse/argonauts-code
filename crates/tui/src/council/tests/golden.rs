//! Council's goldens: how its conversation prints, and the files it is checked against.

use std::time::Instant;

use argonauts_core::base::names::SeatId;
use ratatui::style::Style;
use ratatui::text::Line;

use super::story::{self};
use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::kit::logo::golden::{council_text, pictured};
use crate::base::theme::Theme;
use crate::cells::cell::Cell;
use crate::cells::cell::history::History;
use crate::council::{Faces, Item, Look, Piece, Spacing, TalkCell};

/// The style of the first cell of `needle` in the printed rows.
pub(crate) fn style_of(lines: &[Line<'_>], needle: &str) -> Style {
    for line in lines {
        let mut row = String::new();
        let mut cells = Vec::new();
        for span in &line.spans {
            for ch in span.content.chars() {
                row.push(ch);
                cells.push(line.style.patch(span.style));
            }
        }
        if let Some(at) = row.find(needle) {
            return cells[row[..at].chars().count()];
        }
    }
    panic!("no {needle:?} in the rows");
}

pub(crate) fn text(line: &Line<'_>) -> String {
    council_text(line).trim_end().to_owned()
}

/// The theme a screen in `look` is drawn with: logos as pictures, but for the transcript's,
/// whose screens show a terminal that draws none.
pub(crate) fn theme_for(look: Look) -> Theme {
    match look {
        Look::Transcript => Theme::dark(),
        Look::Pictures | Look::Plain | Look::Blocks => pictured(&Theme::dark()),
    }
}

/// The conversation's rows as the history prints `items`.
pub(crate) fn printed(items: &[Item], look: Look, width: u16) -> Vec<String> {
    printed_with(items, look, width, &story::faces())
}

pub(crate) fn printed_with(items: &[Item], look: Look, width: u16, faces: &Faces) -> Vec<String> {
    let theme = theme_for(look);
    let mut history = History::default();
    let mut spacing = Spacing::default();
    for item in items {
        history.push(Cell::Custom(Box::new(TalkCell::new(
            item.clone(),
            look,
            faces.clone(),
            &mut spacing,
        ))));
    }
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    history.drain_printable(&ctx).iter().map(text).collect()
}

/// The logos each of the conversation's rows draws as pictures, as [`printed_with`] prints
/// them, each by the mark its picture shows.
pub(crate) fn printed_logos(
    items: &[Item],
    look: Look,
    width: u16,
    faces: &Faces,
) -> Vec<Vec<String>> {
    let theme = theme_for(look);
    let mut history = History::default();
    let mut spacing = Spacing::default();
    for item in items {
        history.push(Cell::Custom(Box::new(TalkCell::new(
            item.clone(),
            look,
            faces.clone(),
            &mut spacing,
        ))));
    }
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    history
        .drain_printable(&ctx)
        .iter()
        .map(crate::base::kit::logo::golden::logos_drawn)
        .collect()
}

/// The typing row for `writers`, the middle dot lit.
pub(crate) fn typing(writers: &[SeatId], reason: &[Piece], look: Look, width: u16) -> String {
    typing_with(writers, reason, look, width, &story::faces())
}

pub(crate) fn typing_with(
    writers: &[SeatId],
    reason: &[Piece],
    look: Look,
    width: u16,
    faces: &Faces,
) -> String {
    let theme = theme_for(look);
    let (line, _) = look.typing(writers, 1, reason, usize::from(width), faces, &theme);
    text(&line)
}

pub(crate) fn golden_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/council")
        .join(format!("{name}.txt"))
}

/// A golden's rows.
pub(crate) fn golden(name: &str) -> Vec<String> {
    let path = golden_path(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{}: {err}", path.display()))
        .lines()
        .map(|row| row.trim_end().to_owned())
        .collect()
}

/// With `ARGONAUTS_PICTURE_RECORD` set, a copy of the golden in that folder with `rows` in
/// place from row `first`, each padded to its row's width, so a changed screen can be put
/// beside its golden. A screen several tests draw parts of fills in a part at a time, so the
/// copies want one test thread.
pub(crate) fn record(name: &str, rows: &[String], first: usize) -> bool {
    let Some(dir) = std::env::var_os("ARGONAUTS_PICTURE_RECORD").map(std::path::PathBuf::from)
    else {
        return false;
    };
    let copy = dir.join(format!("{name}.txt"));
    let from = if copy.exists() {
        copy.clone()
    } else {
        golden_path(name)
    };
    let mut lines: Vec<String> = std::fs::read_to_string(&from)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    for (at, row) in (first..).zip(rows) {
        if at >= lines.len() {
            lines.resize(at + 1, String::new());
        }
        let width = crate::base::text::str_width(&lines[at]);
        let pad = width.saturating_sub(crate::base::text::str_width(row));
        lines[at] = format!("{row}{}", " ".repeat(pad));
    }
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&copy, lines.join("\n") + "\n");
    true
}

/// Compares `rows` with the golden's rows that end at `last`, bottom-aligned, so a screen
/// whose top shows rows of another part compares only the rows drawn here.
#[track_caller]
pub(crate) fn assert_screen(name: &str, rows: &[String], last: usize) {
    let golden = golden(name);
    let count = rows.len().min(last + 1);
    assert!(count >= 10, "{name}: only {count} rows drawn");
    let got = &rows[rows.len() - count..];
    if record(name, got, last + 1 - count) {
        return;
    }
    let want = &golden[last + 1 - count..=last];
    let unlike: Vec<String> = want
        .iter()
        .zip(got)
        .enumerate()
        .filter(|(_, (want, got))| want != got)
        .map(|(i, (want, got))| {
            format!(
                "row {}:\n  want |{want}|\n  got  |{got}|",
                last + 1 - count + i
            )
        })
        .collect();
    assert!(
        unlike.is_empty(),
        "{name}: {} rows are different\n{}",
        unlike.len(),
        unlike.join("\n")
    );
}

/// Compares `rows` with the golden's rows from `first` on.
#[track_caller]
pub(crate) fn assert_rows_from(name: &str, rows: &[String], first: usize) {
    if record(name, rows, first) {
        return;
    }
    let golden = golden(name);
    let want = &golden[first..first + rows.len()];
    let unlike: Vec<String> = want
        .iter()
        .zip(rows)
        .enumerate()
        .filter(|(_, (want, got))| want != got)
        .map(|(i, (want, got))| format!("row {}:\n  want |{want}|\n  got  |{got}|", first + i))
        .collect();
    assert!(
        unlike.is_empty(),
        "{name}: {} rows are different\n{}",
        unlike.len(),
        unlike.join("\n")
    );
}

/// The conversation, a blank row, then the typing row.
pub(crate) fn with_typing(mut rows: Vec<String>, typing: String) -> Vec<String> {
    rows.push(String::new());
    rows.push(typing);
    rows
}

/// Why a seat writes, dim, each `{seat}` named as its badge names it.
pub(crate) fn reason(text: &str) -> Vec<Piece> {
    story::said(text)
        .into_iter()
        .map(|piece| match piece {
            Piece::Seat(seat) => Piece::Name(seat, crate::council::Tone::Dim),
            Piece::Words(words, _) => Piece::dim(words),
            other => other,
        })
        .collect()
}

pub(crate) const QWEN_WRITES: &str = "{qwen} is writing · {flash} replied to its message";
