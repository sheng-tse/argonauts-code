//! Bubbles drawn with block cells, for `ui.bubbles = "blocks"` in any terminal: a bubble's
//! corners are block quadrants, which terminals draw themselves so they meet the cell edges, a
//! one-row bubble's ends are the Powerline half circles, and the tail is one lower quadrant
//! under the bubble's bottom corner, beside the logo. Items keep Claude Code's blank row
//! between them.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::super::talk::faces::Faces;
use super::super::talk::item::{Drawn, Item, Quote, SeatSays, YouSay, inset_rows};
use super::super::talk::row::Row;
use super::super::talk::words::{self, Piece, Tone, tapbacks, toned, wrap_all};
use super::bubbles::{
    TEXT, body_rows, draw as draw_bubbles, quote_pieces, read_pieces, replies, words_room,
};
use crate::base::draw::shapes::{Fill, colour, picked_fill};
use crate::base::text::str_width;
use crate::base::theme::Theme;

/// A seat's bubble's left edge; the logo is at 0 and 1, the tail at 2.
const LEFT: usize = 3;

/// A one-row pill's or bubble's round ends, Powerline's half circles.
const CAP_LEFT: &str = "\u{e0b6}";
const CAP_RIGHT: &str = "\u{e0b4}";

pub fn draw(item: &Item, width: usize, faces: &Faces, theme: &Theme) -> Drawn {
    let rows = match item {
        Item::Header(_) | Item::Line(_) => return draw_bubbles(item, width, faces, theme),
        Item::Seat(run) => {
            let mut rows = Vec::new();
            for (i, says) in run.iter().enumerate() {
                rows.extend(seat(says, i == 0, i + 1 == run.len(), width, faces, theme));
            }
            rows
        }
        Item::You(you) => yours(you, width, faces, theme),
    };
    Drawn {
        rows,
        ..Drawn::default()
    }
}

fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

struct Paint {
    /// The quadrants' colour, and the band's behind the words.
    fill: Color,
    inset: Color,
    chip: Color,
    text: Style,
}

impl Paint {
    fn new(fill: Fill, picked: bool, theme: &Theme) -> Self {
        let fill = if picked {
            rgb(picked_fill(theme.brightness))
        } else {
            rgb(colour(fill, theme.brightness))
        };
        Self {
            fill,
            inset: rgb(colour(Fill::Inset, theme.brightness)),
            chip: rgb(colour(Fill::Chip, theme.brightness)),
            text: Style::new(),
        }
    }

    fn edge(&self, glyph: &str) -> Span<'static> {
        Span::styled(glyph.to_owned(), Style::new().fg(self.fill))
    }

    fn inset_edge(&self, glyph: &str) -> Span<'static> {
        Span::styled(glyph.to_owned(), Style::new().fg(self.inset))
    }
}

/// A row of block cells: pieces over a band, its corners as quadrants.
fn band(row: &mut Row, from: usize, to: usize, colour: Color) {
    if to >= from {
        row.put(
            from,
            Span::styled(" ".repeat(to - from + 1), Style::new().bg(colour)),
        );
    }
}

/// `pieces` over a band, so the words keep the band's colour behind them.
fn over(pieces: &[Piece], faces: &Faces, theme: &Theme, colour: Color) -> Vec<Span<'static>> {
    words::spans(pieces, faces, theme)
        .into_iter()
        .map(|span| {
            let style = span.style.bg(colour);
            Span::styled(span.content, style)
        })
        .collect()
}

/// A one-row pill from `at`: a cap, a blank, the pieces, a blank, a cap. Returns the column of
/// its last cap.
fn pill(
    row: &mut Row,
    at: usize,
    pieces: &[Piece],
    fill: Color,
    faces: &Faces,
    theme: &Theme,
) -> usize {
    let size = words::width(pieces, faces, theme);
    row.put(at, Span::styled(CAP_LEFT, Style::new().fg(fill)));
    band(row, at + 1, at + size + 2, fill);
    replace_from(row, at + 2, over(pieces, faces, theme, fill));
    row.put(
        at + size + 3,
        Span::styled(CAP_RIGHT, Style::new().fg(fill)),
    );
    at + size + 3
}

/// Puts `spans` from `at` over whatever band is there, by cutting the band around them.
fn replace_from(row: &mut Row, at: usize, spans: Vec<Span<'static>>) {
    row.overlay(at, spans);
}

fn seat(
    says: &SeatSays,
    name: bool,
    tail: bool,
    width: usize,
    faces: &Faces,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let face = faces.face(&says.seat);
    let dim = Tone::Dim.style(theme);
    let paint = Paint::new(Fill::Seat, says.picked, theme);
    let room = words_room(width);
    let lines = body_rows(&says.body, room, width, faces, theme);
    let inset = inset_rows(says, room, faces, theme);
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
    // The picked task's hint sits at the first row's right end, clear of its words.
    let hint_width = says.hint.as_deref().map_or(0, str_width);
    if says.hint.is_some() {
        let first = lines
            .first()
            .map_or(0, |line| words::width(line, faces, theme));
        text_width = text_width.max(first + 3 + hint_width);
    }
    let right = TEXT + text_width + 1;
    #[derive(Clone, Copy, PartialEq)]
    enum Inner {
        Words,
        InsetTop,
        Inset,
    }
    let mut inner: Vec<(Inner, Option<&Vec<Piece>>)> = lines
        .iter()
        .map(|line| (Inner::Words, Some(line)))
        .collect();
    if !lines.is_empty() && !inset.is_empty() {
        inner.push((Inner::InsetTop, None));
    }
    inner.extend(inset.iter().map(|line| (Inner::Inset, Some(line))));
    let count = inner.len();
    let padded = count > 1;
    let taps = tapbacks(&says.taps);
    let taps_width = words::width(&taps, faces, theme) + 4;
    let tap_start = (right + 3)
        .saturating_sub(taps_width)
        .min(width.saturating_sub(taps_width));
    let mut out = Vec::new();
    if name {
        let mut row = Row::new();
        let mut used = row.put(TEXT, Span::styled(face.beside(theme), dim));
        if let Some(note) = &says.note {
            used = row.put(used, Span::styled(note.clone(), dim));
        }
        if let Some(words) = &says.right {
            row.put(
                right + 1 - str_width(words),
                Span::styled(words.clone(), dim),
            );
        }
        if !says.taps.is_empty() && !padded && says.reply.is_none() {
            pill(
                &mut row,
                tap_start.max(used + 2),
                &taps,
                paint.chip,
                faces,
                theme,
            );
        }
        out.push(row);
    }
    if let Some(quote) = &says.reply {
        let mut row = quote_row(quote, true, width, faces, theme);
        if !says.taps.is_empty() && !padded {
            let used = row.end();
            pill(
                &mut row,
                tap_start.max(used + 2),
                &taps,
                paint.chip,
                faces,
                theme,
            );
        }
        out.push(row);
    }
    if padded {
        let mut top = Row::new();
        top.put(LEFT, paint.edge("▗"));
        top.put(LEFT + 1, paint.edge(&"▄".repeat(right - LEFT - 1)));
        top.put(right, paint.edge("▖"));
        if !says.taps.is_empty() {
            // The pill's ends keep the screen's colour round them, as Messages rings a tapback.
            top.overlay(tap_start, vec![Span::raw(" ".repeat(taps_width))]);
            pill(&mut top, tap_start, &taps, paint.chip, faces, theme);
        }
        out.push(top);
    }
    let inset_at: Vec<usize> = inner
        .iter()
        .enumerate()
        .filter(|(_, (kind, _))| *kind == Inner::Inset)
        .map(|(i, _)| i)
        .collect();
    let inset_has_top = inner.iter().any(|(kind, _)| *kind == Inner::InsetTop);
    for (i, (kind, line)) in inner.iter().enumerate() {
        let (first, last) = (i == 0, i + 1 == count);
        let mut row = Row::new();
        band(&mut row, LEFT + 1, right - 1, paint.fill);
        let (left_glyph, right_glyph) = if padded {
            ("█", "█")
        } else if count == 1 {
            (if tail { "▟" } else { CAP_LEFT }, CAP_RIGHT)
        } else if first {
            ("▟", "▙")
        } else if last {
            (if tail { "█" } else { "▜" }, "▛")
        } else {
            ("█", "█")
        };
        row.put(LEFT, paint.edge(left_glyph));
        row.put(right, paint.edge(right_glyph));
        match kind {
            Inner::InsetTop => {
                row.overlay(
                    LEFT + 1,
                    vec![
                        paint.inset_edge("▗"),
                        paint.inset_edge(&"▄".repeat(right - LEFT - 3)),
                        paint.inset_edge("▖"),
                    ],
                );
            }
            Inner::Inset => {
                let inset_first = inset_at.first() == Some(&i) && !inset_has_top;
                let inset_last = inset_at.last() == Some(&i);
                let (l, r) = match (inset_first, inset_last) {
                    (true, false) => ("▟", "▙"),
                    (false, true) => ("▜", "▛"),
                    (true, true) => ("▐", "▌"),
                    (false, false) => ("█", "█"),
                };
                let mut spans = vec![paint.inset_edge(l)];
                spans.push(Span::styled(
                    " ".repeat(right - LEFT - 3),
                    Style::new().bg(paint.inset),
                ));
                spans.push(paint.inset_edge(r));
                row.overlay(LEFT + 1, spans);
                if let Some(line) = line {
                    row.overlay(TEXT + 1, over(line, faces, theme, paint.inset));
                }
            }
            Inner::Words => {
                if let Some(line) = line {
                    row.overlay(TEXT, over(line, faces, theme, paint.fill));
                }
                if first && let Some(hint) = &says.hint {
                    let style = dim.bg(paint.fill);
                    row.overlay(
                        right - 1 - hint_width,
                        vec![Span::styled(hint.clone(), style)],
                    );
                }
            }
        }
        if last {
            row.put_all(0, face.badge(theme));
            if tail && !padded {
                row.put(2, paint.edge("▗"));
            }
        }
        out.push(row);
    }
    if padded {
        let mut row = Row::new();
        row.put(LEFT, paint.edge(if tail { "▀" } else { "▝" }));
        row.put(LEFT + 1, paint.edge(&"▀".repeat(right - LEFT - 1)));
        row.put(right, paint.edge("▘"));
        if tail {
            row.put(2, paint.edge("▝"));
        }
        out.push(row);
    }
    if let Some(below) = &says.below {
        let mut row = Row::new();
        row.pieces(TEXT, below, faces, theme);
        out.push(row);
    }
    let _ = paint.text;
    out.into_iter().map(|row| row.line(width)).collect()
}

/// The faded copy of what a reply answers, in a pill joined to the reply by a bracket.
fn quote_row(quote: &Quote, left: bool, width: usize, faces: &Faces, theme: &Theme) -> Row {
    let dim = Tone::Dim.style(theme);
    let fill = rgb(colour(
        match quote.seat {
            Some(_) => Fill::Quote,
            None => Fill::YourQuote,
        },
        theme.brightness,
    ));
    let pieces = quote_pieces(quote);
    let mut row = Row::new();
    if left {
        row.put(LEFT, Span::styled("╭", dim));
        row.put(LEFT + 1, Span::styled("─", dim));
        let end = pill(&mut row, LEFT + 2, &pieces, fill, faces, theme);
        if quote.replies > 0 {
            row.put(end + 2, Span::styled(replies(quote.replies), dim));
        }
        return row;
    }
    let size = words::width(&pieces, faces, theme) + 4;
    let right = width.saturating_sub(3);
    let start = right.saturating_sub(1 + size);
    if quote.replies > 0 {
        let count = replies(quote.replies);
        row.put(
            start.saturating_sub(2 + str_width(&count)),
            Span::styled(count, dim),
        );
    }
    pill(&mut row, start, &pieces, fill, faces, theme);
    row.put(right - 1, Span::styled("─", dim));
    row.put(right, Span::styled("╮", dim));
    row
}

fn yours(you: &YouSay, width: usize, faces: &Faces, theme: &Theme) -> Vec<Line<'static>> {
    let paint = Paint::new(Fill::You, you.picked, theme);
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
    let right = width.saturating_sub(3);
    let left = right.saturating_sub(text_width + 3);
    let count = lines.len();
    let padded = count > 1;
    let mut out = Vec::new();
    if let Some(quote) = &you.reply {
        out.push(quote_row(quote, false, width, faces, theme));
    }
    if padded {
        let mut top = Row::new();
        top.put(left, paint.edge("▗"));
        top.put(left + 1, paint.edge(&"▄".repeat(right - left - 1)));
        top.put(right, paint.edge("▖"));
        out.push(top);
    }
    for (i, line) in lines.iter().enumerate() {
        let (first, last) = (i == 0, i + 1 == count);
        let mut row = Row::new();
        band(&mut row, left + 1, right - 1, paint.fill);
        let (left_glyph, right_glyph) = if padded {
            ("█", "█")
        } else if count == 1 {
            (CAP_LEFT, "▙")
        } else if first {
            ("▟", "▙")
        } else if last {
            ("▜", "█")
        } else {
            ("█", "█")
        };
        row.put(left, paint.edge(left_glyph));
        row.put(right, paint.edge(right_glyph));
        row.overlay(left + 2, over(line, faces, theme, paint.fill));
        if last && !padded {
            row.put(right + 1, paint.edge("▖"));
        }
        out.push(row);
    }
    if padded {
        let mut row = Row::new();
        row.put(left, paint.edge("▝"));
        row.put(left + 1, paint.edge(&"▀".repeat(right - left - 1)));
        row.put(right, paint.edge("▀"));
        row.put(right + 1, paint.edge("▘"));
        out.push(row);
    }
    if !you.read_by.is_empty() || !you.read_after.is_empty() {
        let pieces = read_pieces(you);
        let mut row = Row::new();
        row.pieces(
            right + 1 - words::width(&pieces, faces, theme),
            &pieces,
            faces,
            theme,
        );
        out.push(row);
    }
    out.into_iter().map(|row| row.line(width)).collect()
}

/// Who writes now: each writer's logo and a small block bubble of three dots, then why.
pub fn typing(
    writers: &[argonauts_core::base::names::SeatId],
    frame: usize,
    reason: &[Piece],
    width: usize,
    faces: &Faces,
    theme: &Theme,
) -> Line<'static> {
    let fill = rgb(colour(Fill::Seat, theme.brightness));
    let edge = |glyph: &str| Span::styled(glyph.to_owned(), Style::new().fg(fill));
    let mut row = Row::new();
    let mut at = 0;
    for seat in writers {
        row.put_all(at, faces.face(seat).badge(theme));
        row.put(at + 2, edge("▗"));
        row.put(at + 3, edge("▟"));
        band(&mut row, at + 4, at + 10, fill);
        let dots = (0..3)
            .flat_map(|dot| {
                let tone = if dot == frame % 3 {
                    Tone::Text
                } else {
                    Tone::Faint
                };
                [
                    Span::styled("•", tone.style(theme).bg(fill)),
                    Span::styled(" ", Style::new().bg(fill)),
                ]
            })
            .take(5)
            .collect();
        row.overlay(at + 5, dots);
        row.put(at + 11, edge(CAP_RIGHT));
        at += 15;
    }
    row.pieces(at.saturating_sub(1), reason, faces, theme);
    row.line(width)
}
