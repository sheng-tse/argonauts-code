//! The talk as a transcript, for terminals that draw no pictures under their text and for
//! `ui.council_view = "transcript"`: Claude Code's own items, one under another with a blank row
//! between, and everything the talk says kept. A seat's message is its logo and its name bold
//! in its CLI's colour, with its words under it; a reply names who it answers and quotes them
//! under a bar; tapbacks sit at the name row's right end; yours is Claude Code's prompt band
//! with "Read by" under it; Messages' centred lines become a dim line on a faint rule.

use ratatui::text::Span;

use super::bubbles::{body_rows, draw as draw_bubbles, line_rows, read_pieces};
use super::faces::Faces;
use super::item::{Drawn, Item, SeatSays, YouSay, inset_rows};
use super::row::Row;
use super::words::{self, Piece, Tone, tapbacks, toned, wrap_all};
use crate::text::str_width;
use crate::theme::{Role, Theme};

/// A seat's words start here, under its name.
pub const TEXT: usize = 3;

pub fn words_room(width: usize) -> usize {
    width.saturating_sub(TEXT + 2)
}

pub fn draw(item: &Item, width: usize, faces: &Faces, theme: &Theme) -> Drawn {
    let rows = match item {
        Item::Header(_) => return draw_bubbles(item, width, faces, theme),
        Item::Line(pieces) => {
            // Wrapped where it runs past the rule's own room, the rows after the first under
            // the first one's words.
            let rows = line_rows(pieces, width.saturating_sub(1), faces, theme);
            rows.iter()
                .enumerate()
                .map(|(at, row)| match at {
                    0 => ruled(row, width, faces, theme),
                    _ => {
                        let mut under = Row::new();
                        under.pieces(3, row, faces, theme);
                        under.line(width)
                    }
                })
                .collect()
        }
        Item::Seat(run) => run
            .iter()
            .flat_map(|says| seat(says, width, faces, theme))
            .collect(),
        Item::You(you) => yours(you, width, faces, theme),
    };
    Drawn {
        rows,
        ..Drawn::default()
    }
}

/// A centred line's words on a faint rule.
pub fn ruled(
    pieces: &[Piece],
    width: usize,
    faces: &Faces,
    theme: &Theme,
) -> ratatui::text::Line<'static> {
    let faint = theme.style(Role::Faint);
    let mut row = Row::new().with(0, Span::styled("── ", faint));
    let used = row.pieces(3, pieces, faces, theme);
    let rest = width.saturating_sub(used + 2);
    if rest > 0 {
        row.put(used + 1, Span::styled("─".repeat(rest), faint));
    }
    row.line(width)
}

fn seat(
    says: &SeatSays,
    width: usize,
    faces: &Faces,
    theme: &Theme,
) -> Vec<ratatui::text::Line<'static>> {
    let face = faces.face(&says.seat);
    let dim = Tone::Dim.style(theme);
    let mut head = Row::new();
    head.put_all(0, face.badge(theme));
    let mut at = head.put(
        TEXT,
        Span::styled(
            face.beside(theme),
            face.name_style(theme)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
    );
    if let Some(quote) = &says.reply {
        match &quote.seat {
            Some(seat) => {
                let to = faces.face(seat);
                head.put(at, Span::styled("  to  ", dim));
                head.put_all(at + 6, to.badge(theme));
                at = head.put(at + 9, Span::styled(to.beside(theme), to.name_style(theme)));
            }
            None => at = head.put(at, Span::styled("  to  you", dim)),
        }
    }
    if let Some(note) = &says.note {
        head.put(at, Span::styled(note.clone(), dim));
    }
    let mut end = width.saturating_sub(2);
    if let Some(right) = &says.right {
        head.put(end + 1 - str_width(right), Span::styled(right.clone(), dim));
        end = end.saturating_sub(str_width(right) + 3);
    }
    if !says.taps.is_empty() {
        let pieces = tapbacks(&says.taps);
        head.pieces(
            end + 1 - words::width(&pieces, faces, theme),
            &pieces,
            faces,
            theme,
        );
    }
    let mut rows = vec![head];
    if let Some(quote) = &says.reply {
        let bar = match &quote.seat {
            Some(seat) => faces.face(seat).name_style(theme),
            None => theme.style(Role::Text),
        };
        rows.push(Row::new().with(TEXT, Span::styled("│", bar)).with(
            TEXT + 2,
            Span::styled(quote.words.clone(), Tone::Quote.style(theme)),
        ));
    }
    let room = words_room(width);
    let lines = body_rows(&says.body, room, width, faces, theme);
    let inset = inset_rows(says, room, faces, theme);
    let hint = |row: &mut Row| {
        if let Some(hint) = &says.hint {
            row.put(
                width + 1 - 2 - str_width(hint),
                Span::styled(hint.clone(), dim),
            );
        }
    };
    for (i, line) in lines.iter().enumerate() {
        let mut row = Row::new();
        row.pieces(TEXT, line, faces, theme);
        if i == 0 && inset.is_empty() {
            hint(&mut row);
        }
        rows.push(row);
    }
    if !inset.is_empty() {
        if !lines.is_empty() {
            rows.push(Row::new());
        }
        for (i, line) in inset.iter().enumerate() {
            let mut row = Row::new();
            row.pieces(TEXT, line, faces, theme);
            if i == 0 {
                hint(&mut row);
            }
            rows.push(row);
        }
    }
    if says.picked {
        let band = theme.style(Role::Selection);
        rows = rows.into_iter().map(|row| row.filled(band)).collect();
    }
    if let Some(below) = &says.below {
        let mut row = Row::new();
        row.pieces(TEXT, below, faces, theme);
        rows.push(row);
    }
    rows.into_iter().map(|row| row.line(width)).collect()
}

fn yours(
    you: &YouSay,
    width: usize,
    faces: &Faces,
    theme: &Theme,
) -> Vec<ratatui::text::Line<'static>> {
    let band = theme.style(Role::UserBand);
    let prompt = Span::styled("❯", theme.style(Role::Faint));
    let mut rows = Vec::new();
    if let Some(quote) = &you.reply {
        let bar = match &quote.seat {
            Some(seat) => faces.face(seat).name_style(theme),
            None => theme.style(Role::Text),
        };
        let mut row = Row::new()
            .with(0, prompt.clone())
            .with(2, Span::styled("│", bar));
        let mut pieces = Vec::new();
        if let Some(seat) = &quote.seat {
            pieces.push(Piece::Badge(seat.clone()));
        }
        pieces.push(Piece::tone(format!(" {}", quote.words), Tone::Quote));
        row.pieces(4, &pieces, faces, theme);
        rows.push(row.filled(band));
    }
    let paragraphs: Vec<Vec<Piece>> = you
        .paragraphs
        .iter()
        .map(|paragraph| toned(paragraph, Tone::Bright))
        .collect();
    for (i, line) in wrap_all(&paragraphs, width.saturating_sub(4), faces, theme)
        .iter()
        .enumerate()
    {
        let mut row = Row::new();
        if i == 0 && you.reply.is_none() {
            row.put(0, prompt.clone());
        }
        row.pieces(2, line, faces, theme);
        rows.push(row.filled(band));
    }
    if !you.read_by.is_empty() || !you.read_after.is_empty() {
        let mut row = Row::new().with(2, Span::styled("⎿  ", Tone::Dim.style(theme)));
        row.pieces(5, &read_pieces(you), faces, theme);
        rows.push(row);
    }
    rows.into_iter().map(|row| row.line(width)).collect()
}

/// Who writes now, without the bubble: each writer's logo and three dots, the lit one at
/// `frame`, then why it is that seat.
pub fn typing(
    writers: &[argonauts_core::names::SeatId],
    frame: usize,
    reason: &[Piece],
    width: usize,
    faces: &Faces,
    theme: &Theme,
) -> ratatui::text::Line<'static> {
    let mut row = Row::new();
    let mut at = 0;
    for seat in writers {
        row.put_all(at, faces.face(seat).badge(theme));
        for dot in 0..3 {
            let tone = if dot == frame % 3 {
                Tone::Bright
            } else {
                Tone::Dim
            };
            row.put(at + 3 + 2 * dot, Span::styled("•", tone.style(theme)));
        }
        at += 10;
    }
    row.pieces(at, reason, faces, theme);
    row.line(width)
}
