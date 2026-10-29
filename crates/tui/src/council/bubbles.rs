//! The talk as Messages draws it: the seats' bubbles on the left, each with its name dim above
//! and its logo at the tail, yours on the right in blue, and Messages' own lines centred. The
//! words keep Claude Code's grid, a seat's from column 5 and yours ending four cells from the
//! right edge, and the bubbles are pictures under them ([`super::shapes`]), so the cells hold
//! only the words. Without the pictures the words stay where they are.

use ratatui::text::Span;

use super::faces::Faces;
use super::item::{Body, Drawn, Edge, Header, Item, Quote, SeatSays, YouSay, inset_rows};
use super::row::{Row, centred};
use super::shapes::{CELL, Fill, Kind, PAD, Ring, Shape, Side, x_of, y_of};
use super::words::{self, Piece, Tone, tapbacks, toned, wrap_all};
use crate::text::str_width;
use crate::theme::Theme;

/// A seat's words start here; its logo sits at column 0 and the tail between.
pub const TEXT: usize = 5;

/// Where a reply's quote starts, after its bracket.
const QUOTE: usize = 6;

/// The cells a seat's words may take: a bubble reaches at most about three quarters across, as
/// Messages leaves room on the other side.
pub fn words_room(width: usize) -> usize {
    ((width * 76 + 50) / 100).saturating_sub(1 + TEXT)
}

pub fn draw(item: &Item, width: usize, faces: &Faces, theme: &Theme) -> Drawn {
    match item {
        Item::Header(header) => draw_header(header, width, faces, theme),
        Item::Line(pieces) => Drawn {
            rows: line_rows(pieces, width, faces, theme)
                .iter()
                .map(|row| centred(row, width, faces, theme).line(width))
                .collect(),
            shapes: Vec::new(),
            first: Some(Edge::Centred),
            last: Some(Edge::Centred),
        },
        Item::Seat(run) => seat_run(run, width, faces, theme),
        Item::You(you) => yours(you, width, faces, theme),
    }
}

/// A centred line's words in rows that keep two cells clear at each side, wrapped where they
/// would run past them. A mark the line opens with, such as "✔  ", keeps its blanks on the
/// first row.
pub fn line_rows(pieces: &[Piece], width: usize, faces: &Faces, theme: &Theme) -> Vec<Vec<Piece>> {
    let room = width.saturating_sub(4).max(1);
    if words::width(pieces, faces, theme) <= room {
        return vec![pieces.to_vec()];
    }
    let (mark, rest) = match pieces.split_first() {
        Some((first @ Piece::Words(words, _), rest)) if words.ends_with(' ') => {
            (Some(first.clone()), rest)
        }
        _ => (None, pieces),
    };
    let marked = mark.as_ref().map_or(0, |mark| mark.width(faces, theme));
    let mut rows = words::wrap(rest, room.saturating_sub(marked).max(1), faces, theme);
    if let (Some(mark), Some(first)) = (mark, rows.first_mut()) {
        first.insert(0, mark);
    }
    rows
}

fn draw_header(header: &Header, width: usize, faces: &Faces, theme: &Theme) -> Drawn {
    let mut badges = Vec::new();
    for (i, seat) in header.seats.iter().enumerate() {
        if i > 0 {
            badges.push(Piece::dim(" "));
        }
        badges.push(Piece::Badge(seat.clone()));
    }
    let rows = [
        badges,
        vec![Piece::tone(header.title.clone(), Tone::Bold)],
        vec![Piece::dim(header.subtitle.clone())],
    ];
    Drawn {
        rows: rows
            .iter()
            .map(|pieces| centred(pieces, width, faces, theme).line(width))
            .collect(),
        shapes: Vec::new(),
        first: Some(Edge::Text),
        last: Some(Edge::Text),
    }
}

/// Messages from one seat in a row, as Messages groups them: the name over the first, the tail
/// and the logo on the last, and each next bubble starting in the row under the one before.
fn seat_run(run: &[SeatSays], width: usize, faces: &Faces, theme: &Theme) -> Drawn {
    let mut out = Drawn::default();
    for (i, says) in run.iter().enumerate() {
        let last = i + 1 == run.len();
        let mut rows = seat_rows(says, i == 0, last, width, faces, theme);
        if !last {
            // Its bottom pad row is the next one's top pad row.
            rows.rows.pop();
        }
        let at = out.rows.len();
        out.shapes
            .extend(rows.shapes.into_iter().map(|shape| shape.moved(at)));
        out.rows.extend(rows.rows);
    }
    let (first, last) = Item::Seat(run.to_vec()).edges();
    out.first = Some(first);
    out.last = Some(last);
    out
}

/// A seat's words as rows of pieces: the paragraphs shown, then the fold.
pub fn body_rows(
    body: &Body,
    room: usize,
    width: usize,
    faces: &Faces,
    theme: &Theme,
) -> Vec<Vec<Piece>> {
    match body {
        Body::Rows(rows) => rows.clone(),
        Body::Task(work) => work.rows(width),
        Body::Words { paragraphs, shown } => {
            let shown = shown.unwrap_or(paragraphs.len()).min(paragraphs.len());
            let mut rows = wrap_all(&paragraphs[..shown], room, faces, theme);
            let folded = wrap_all(&paragraphs[shown..], room, faces, theme).len();
            if folded > 0 {
                rows.push(vec![Piece::dim(format!(
                    "… +{folded} lines (ctrl+o to expand)"
                ))]);
            }
            rows
        }
    }
}

struct Rows {
    rows: Vec<ratatui::text::Line<'static>>,
    shapes: Vec<Shape>,
}

/// Name row, a reply's quote, the top pad row (a tapback sits on it), the words, an inset task,
/// the bottom pad row, and a dim line under it all.
fn seat_rows(
    says: &SeatSays,
    name: bool,
    tail: bool,
    width: usize,
    faces: &Faces,
    theme: &Theme,
) -> Rows {
    let face = faces.face(&says.seat);
    let room = words_room(width);
    let lines = body_rows(&says.body, room, width, faces, theme);
    let inset = inset_rows(says, room, faces, theme);
    let hint_width = says.hint.as_deref().map_or(0, str_width);
    let mut text_width = lines
        .iter()
        .map(|line| words::width(line, faces, theme))
        .chain(
            inset
                .iter()
                .map(|line| words::width(line, faces, theme) + 2),
        )
        .chain([says.min_width.min(room)])
        .max()
        .unwrap_or(0);
    if says.hint.is_some() {
        let first = lines
            .first()
            .map_or(0, |line| words::width(line, faces, theme));
        text_width = text_width.max(first + 3 + hint_width);
    }
    let x0 = x_of(TEXT) - PAD.0;
    let x1 = x_of(TEXT + text_width) + PAD.0;
    let dim = Tone::Dim.style(theme);
    let mut out = Rows {
        rows: Vec::new(),
        shapes: Vec::new(),
    };
    if name {
        let mut row = Row::new();
        let after = row.put(TEXT, Span::styled(face.beside(theme), dim));
        if let Some(note) = &says.note {
            let tone = if says.held { Tone::Warn } else { Tone::Dim };
            row.put(after, Span::styled(note.clone(), tone.style(theme)));
        }
        if let Some(right) = &says.right {
            row.put(
                (TEXT + text_width).saturating_sub(str_width(right)),
                Span::styled(right.clone(), dim),
            );
        }
        out.rows.push(row.line(width));
    }
    if let Some(quote) = &says.reply {
        let at = out.rows.len();
        let (row, shapes) = quote_left(quote, at, faces, theme);
        out.rows.push(row.line(width));
        out.shapes.extend(shapes);
    }
    // The inner rows: the words, a blank row between them and a task, then the task.
    let gap = !lines.is_empty() && !inset.is_empty();
    let inner = lines.len() + usize::from(gap) + inset.len();
    let top = out.rows.len();
    let mut pad = Row::new();
    out.shapes.push(
        Shape::new(
            top,
            Kind::Bubble {
                tail: tail.then_some(Side::Left),
            },
            (x0, x1),
            (CELL.1 - PAD.1, y_of(inner + 1) + PAD.1),
        )
        .filled(if says.held { Fill::Held } else { Fill::Seat })
        .ringed(says.picked.then_some(Ring::Picked)),
    );
    if !inset.is_empty() {
        let first = lines.len() + usize::from(gap);
        let y0 = if first > 0 {
            y_of(first) + 26
        } else {
            CELL.1 - PAD.1 + 12
        };
        out.shapes.push(
            Shape::new(
                top,
                Kind::Inset { radius: 24 },
                (x0 + 12, x1 - 12),
                (y0, y_of(inner + 1) + 6),
            )
            .filled(Fill::Inset),
        );
    }
    if !says.taps.is_empty() {
        let pieces = tapbacks(&says.taps);
        let size = words::width(&pieces, faces, theme);
        let start = (TEXT + text_width + 2).saturating_sub(size);
        out.shapes.push(
            Shape::new(
                top,
                Kind::Pill { dot: true },
                (x_of(start) - 14, x_of(start + size) + 6),
                (3, 39),
            )
            .filled(Fill::Chip)
            .ringed(Some(Ring::Screen)),
        );
        pad.pieces(start, &pieces, faces, theme);
    }
    out.rows.push(pad.line(width));
    let words_and_inset = lines
        .iter()
        .map(|line| (TEXT, Some(line)))
        .chain(gap.then_some((TEXT, None)))
        .chain(inset.iter().map(|line| (TEXT + 1, Some(line))));
    for (i, (at, line)) in words_and_inset.enumerate() {
        let mut row = Row::new();
        if let Some(line) = line {
            row.pieces(at, line, faces, theme);
        }
        if i == 0
            && let Some(hint) = &says.hint
        {
            row.put(
                TEXT + text_width - hint_width,
                Span::styled(hint.clone(), dim),
            );
        }
        if i + 1 == inner && tail {
            row.put_all(0, face.badge(theme));
        }
        out.rows.push(row.line(width));
    }
    out.rows.push(Row::new().line(width));
    if let Some(below) = &says.below {
        let mut row = Row::new();
        row.pieces(TEXT, below, faces, theme);
        out.rows.push(row.line(width));
    }
    out
}

/// Messages' inline reply on a seat's side: the answered words faded in a small bubble, and a
/// line from it down into the reply's bubble.
fn quote_left(quote: &Quote, at: usize, faces: &Faces, theme: &Theme) -> (Row, Vec<Shape>) {
    let pieces = quote_pieces(quote);
    let size = words::width(&pieces, faces, theme);
    let dim = Tone::Dim.style(theme);
    let mut row = Row::new()
        .with(3, Span::styled("╭", dim))
        .with(4, Span::styled("─", dim));
    let end = row.pieces(QUOTE, &pieces, faces, theme);
    if quote.replies > 0 {
        row.put(end + 2, Span::styled(replies(quote.replies), dim));
    }
    let shapes = vec![
        Shape::new(
            at,
            Kind::Pill { dot: false },
            (x_of(QUOTE - 1), x_of(QUOTE + size) + 14),
            (4, 38),
        )
        .filled(quote_fill(quote)),
        Shape::new(
            at,
            Kind::Rule,
            (x_of(TEXT) - PAD.0, x_of(TEXT) - PAD.0 + 2),
            (CELL.1, y_of(3) - PAD.1 + 40),
        )
        .filled(Fill::Thread),
    ];
    (row, shapes)
}

/// The same on your side, the line coming down at the right.
fn quote_right(
    quote: &Quote,
    at: usize,
    width: usize,
    faces: &Faces,
    theme: &Theme,
) -> (Row, Vec<Shape>) {
    let pieces = quote_pieces(quote);
    let size = words::width(&pieces, faces, theme);
    let dim = Tone::Dim.style(theme);
    let end = width.saturating_sub(5);
    let start = (end + 1).saturating_sub(size);
    let mut row = Row::new()
        .with(width.saturating_sub(3), Span::styled("─", dim))
        .with(width.saturating_sub(2), Span::styled("╮", dim));
    row.pieces(start, &pieces, faces, theme);
    if quote.replies > 0 {
        let count = replies(quote.replies);
        row.put(
            start.saturating_sub(2 + str_width(&count)),
            Span::styled(count, dim),
        );
    }
    let line_x = x_of(width.saturating_sub(2)) + 8;
    let shapes = vec![
        Shape::new(
            at,
            Kind::Pill { dot: false },
            (x_of(start) - 14, x_of(width.saturating_sub(3))),
            (4, 38),
        )
        .filled(quote_fill(quote)),
        Shape::new(
            at,
            Kind::Rule,
            (line_x, line_x + 2),
            (CELL.1, y_of(3) - PAD.1 + 40),
        )
        .filled(Fill::Thread),
    ];
    (row, shapes)
}

fn quote_fill(quote: &Quote) -> Fill {
    match quote.seat {
        Some(_) => Fill::Quote,
        None => Fill::YourQuote,
    }
}

/// The quoted seat's badge, then its words faded.
pub fn quote_pieces(quote: &Quote) -> Vec<Piece> {
    let mut pieces = Vec::new();
    if let Some(seat) = &quote.seat {
        pieces.push(Piece::Badge(seat.clone()));
        pieces.push(Piece::text(" "));
    }
    pieces.push(Piece::tone(quote.words.clone(), Tone::Quote));
    pieces
}

/// "1 Reply", "3 Replies", as Messages counts a thread.
pub fn replies(count: u32) -> String {
    if count == 1 {
        "1 Reply".to_owned()
    } else {
        format!("{count} Replies")
    }
}

/// "Read by" and the badges of the seats that read it, then anything after them.
pub fn read_pieces(you: &YouSay) -> Vec<Piece> {
    let mut pieces = vec![Piece::dim("Read by ")];
    for (i, seat) in you.read_by.iter().enumerate() {
        if i > 0 {
            pieces.push(Piece::dim(" "));
        }
        pieces.push(Piece::Badge(seat.clone()));
    }
    pieces.extend(you.read_after.iter().cloned());
    pieces
}

fn yours(you: &YouSay, width: usize, faces: &Faces, theme: &Theme) -> Drawn {
    let paragraphs: Vec<Vec<Piece>> = you
        .paragraphs
        .iter()
        .map(|paragraph| toned(paragraph, Tone::OnYours))
        .collect();
    let lines = wrap_all(&paragraphs, words_room(width) + 1, faces, theme);
    let text_width = lines
        .iter()
        .map(|line| words::width(line, faces, theme))
        .max()
        .unwrap_or(0);
    let end = width.saturating_sub(4);
    let start = (end + 1).saturating_sub(text_width);
    let mut out = Drawn::default();
    if let Some(quote) = &you.reply {
        let (row, shapes) = quote_right(quote, 0, width, faces, theme);
        out.rows.push(row.line(width));
        out.shapes.extend(shapes);
    }
    out.shapes.push(
        Shape::new(
            out.rows.len(),
            Kind::Bubble {
                tail: Some(Side::Right),
            },
            (x_of(start) - PAD.0, x_of(end + 1) + PAD.0),
            (CELL.1 - PAD.1, y_of(lines.len() + 1) + PAD.1),
        )
        .filled(Fill::You)
        .ringed(you.picked.then_some(Ring::Picked)),
    );
    out.rows.push(Row::new().line(width));
    for line in &lines {
        let mut row = Row::new();
        row.pieces(start, line, faces, theme);
        out.rows.push(row.line(width));
    }
    out.rows.push(Row::new().line(width));
    if !you.read_by.is_empty() || !you.read_after.is_empty() {
        let pieces = read_pieces(you);
        let size = words::width(&pieces, faces, theme);
        let mut row = Row::new();
        row.pieces(width.saturating_sub(2 + size), &pieces, faces, theme);
        out.rows.push(row.line(width));
    }
    let (first, last) = Item::You(you.clone()).edges();
    out.first = Some(first);
    out.last = Some(last);
    out
}

/// Who writes now: each writer's logo and Messages' typing bubble, its lit dot at `frame`, then
/// why it is that seat, dim.
pub fn typing(
    writers: &[argonauts_core::names::SeatId],
    frame: usize,
    reason: &[Piece],
    width: usize,
    faces: &Faces,
    theme: &Theme,
) -> (ratatui::text::Line<'static>, Vec<Shape>) {
    let mut row = Row::new();
    let mut shapes = Vec::new();
    let mut at = 0;
    for seat in writers {
        row.put_all(at, faces.face(seat).badge(theme));
        shapes.push(
            Shape::new(
                0,
                Kind::Typing,
                (x_of(at + 3) - 4, x_of(at + 10) + 4),
                (3, 39),
            )
            .filled(Fill::Seat),
        );
        for dot in 0..3 {
            let tone = if dot == frame % 3 {
                Tone::Bright
            } else {
                Tone::Dim
            };
            row.put(at + 4 + 2 * dot, Span::styled("•", tone.style(theme)));
        }
        at += 13;
    }
    if !reason.is_empty() {
        row.pieces(at.saturating_sub(1), reason, faces, theme);
    }
    (row.line(width), shapes)
}
