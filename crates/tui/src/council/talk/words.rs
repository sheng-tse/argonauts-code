//! The words of the talk as pieces: runs of text in a tone, and seats, which every reader draws
//! its own way (your screen as a badge and the model's name). Paragraphs wrap at word breaks,
//! and a seat's mention never breaks across rows.

use argonauts_core::base::names::SeatId;
use argonauts_core::runs::events::council::{Stance, TextPart};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use super::faces::{BADGE, Face, Faces};
use crate::base::kit::logo::logo_spans;
use crate::base::text::str_width;
use crate::base::theme::{Role, Theme};
use argonauts_core::look::logos::Logo;

/// Text on your blue bubble, white as Messages writes it whatever the theme.
pub const ON_YOUR_BUBBLE: Color = Color::Rgb(0xff, 0xff, 0xff);

/// Messages' own blue, for what marks your words outside your bubble.
pub const YOUR_BLUE: Color = Color::Rgb(0x0a, 0x84, 0xff);

/// How a run of words is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    Text,
    Bold,
    Dim,
    /// Quoted words: dim and slanted.
    Quote,
    Faint,
    Bright,
    Ok,
    OkBold,
    Danger,
    Warn,
    WarnBold,
    /// Words of yours outside your bubble, in its blue.
    Yours,
    /// Words on your blue bubble.
    OnYours,
    /// A held message's words, faded until you choose what happens to it.
    Faded,
    /// Words of a held message that match what you wrote in private.
    Matching,
    /// What only you see, in the colour of the input's label.
    Label,
    /// The pointer and the value ← and → change, in the questions' colour.
    Pointer,
    PointerBold,
}

impl Tone {
    pub fn style(self, theme: &Theme) -> Style {
        match self {
            Tone::Text => theme.style(Role::Text),
            Tone::Bold => theme.style(Role::Text).add_modifier(Modifier::BOLD),
            Tone::Dim => theme.style(Role::Dim),
            Tone::Quote => theme.style(Role::Dim).add_modifier(Modifier::ITALIC),
            Tone::Faint => theme.style(Role::Faint),
            Tone::Faded => theme.style(Role::Dim),
            Tone::Bright => theme.style(Role::Bright),
            Tone::Ok => theme.style(Role::Success),
            Tone::OkBold => theme.style(Role::Success).add_modifier(Modifier::BOLD),
            Tone::Danger => theme.style(Role::Danger),
            Tone::Warn => theme.style(Role::Warn),
            Tone::WarnBold => theme.style(Role::Warn).add_modifier(Modifier::BOLD),
            Tone::Yours => Style::new().fg(YOUR_BLUE),
            Tone::OnYours => Style::new().fg(ON_YOUR_BUBBLE),
            Tone::Matching => theme.style(Role::Warn),
            Tone::Label => theme.style(Role::Label),
            Tone::Pointer => theme.style(Role::Question),
            Tone::PointerBold => theme.style(Role::Question).add_modifier(Modifier::BOLD),
        }
    }
}

/// A tapback's mark, or a stance's: ✔ agree, ✘ object, ? a question, and · for a tapback cast
/// but kept sealed until every seat has cast its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mark {
    Agree,
    Object,
    Question,
    Sealed,
}

impl Mark {
    /// The mark as cells hold it: a two-cell glyph and the blank it spills into.
    pub fn text(self) -> &'static str {
        match self {
            Mark::Agree => "✔ ",
            Mark::Object => "✘ ",
            Mark::Question => "? ",
            Mark::Sealed => "· ",
        }
    }

    pub fn tone(self) -> Tone {
        match self {
            Mark::Agree => Tone::Ok,
            Mark::Object => Tone::Danger,
            Mark::Question => Tone::WarnBold,
            Mark::Sealed => Tone::Dim,
        }
    }

    /// Whether the glyph takes two cells, so a blank always follows it.
    fn wide(self) -> bool {
        matches!(self, Mark::Agree | Mark::Object)
    }
}

impl From<Stance> for Mark {
    fn from(stance: Stance) -> Self {
        match stance {
            Stance::Agree => Mark::Agree,
            Stance::Object => Mark::Object,
            Stance::Question => Mark::Question,
        }
    }
}

/// A run of a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Words(String, Tone),
    /// A seat named: its badge, a blank and its name.
    Seat(SeatId),
    /// A seat's badge alone.
    Badge(SeatId),
    /// A seat's name alone, as its badge names it, in a tone: "3.8 Flash replied to its
    /// message".
    Name(SeatId, Tone),
    /// A seat's badge, a blank and its name in its CLI's colour, not bold, as a task names who
    /// does a step.
    Named(SeatId),
    /// A seat's badge, a blank and its name in a tone, as a line that starts with the seat
    /// says what it did: "5.6 Luna moved to a token bucket".
    Called(SeatId, Tone),
    /// A logo of something that is no seat, such as a fresh session that checks a task.
    Logo(Logo),
    /// A model that is no seat, its logo, a blank and its name as it reads beside the logo, in
    /// a tone, as a line names the fresh session that checked a task.
    Model(Face, Tone),
    /// Words in a seat's CLI colour, bold or not.
    InColour(SeatId, String, bool),
    /// Words in a CLI's colour, bold or not, for a session that is no seat.
    InColourOf(String, String, bool),
}

impl Piece {
    pub fn text(words: impl Into<String>) -> Self {
        Piece::Words(words.into(), Tone::Text)
    }

    pub fn dim(words: impl Into<String>) -> Self {
        Piece::Words(words.into(), Tone::Dim)
    }

    pub fn tone(words: impl Into<String>, tone: Tone) -> Self {
        Piece::Words(words.into(), tone)
    }

    pub fn width(&self, faces: &Faces, theme: &Theme) -> usize {
        match self {
            Piece::Words(words, _) => str_width(words),
            Piece::Seat(seat) => faces.face(seat).mention_width(theme),
            Piece::Badge(_) | Piece::Logo(_) => BADGE,
            Piece::Name(seat, _) => faces.face(seat).beside_width(theme),
            Piece::Named(seat) | Piece::Called(seat, _) => {
                BADGE + 1 + faces.face(seat).beside_width(theme)
            }
            Piece::Model(face, _) => BADGE + 1 + face.beside_width(theme),
            Piece::InColour(_, words, _) | Piece::InColourOf(_, words, _) => str_width(words),
        }
    }

    pub fn spans(&self, faces: &Faces, theme: &Theme) -> Vec<Span<'static>> {
        match self {
            Piece::Words(words, tone) => vec![Span::styled(words.clone(), tone.style(theme))],
            Piece::Seat(seat) => faces.face(seat).mention(theme),
            Piece::Badge(seat) => faces.face(seat).badge(theme),
            Piece::Name(seat, tone) => {
                vec![Span::styled(
                    faces.face(seat).beside(theme),
                    tone.style(theme),
                )]
            }
            Piece::Named(seat) => {
                let face = faces.face(seat);
                let mut spans = face.badge(theme);
                spans.push(Span::styled(
                    format!(" {}", face.beside(theme)),
                    face.name_style(theme),
                ));
                spans
            }
            Piece::Called(seat, tone) => {
                let face = faces.face(seat);
                let mut spans = face.badge(theme);
                spans.push(Span::styled(
                    format!(" {}", face.beside(theme)),
                    tone.style(theme),
                ));
                spans
            }
            Piece::Model(face, tone) => {
                let mut spans = face.badge(theme);
                spans.push(Span::styled(
                    format!(" {}", face.beside(theme)),
                    tone.style(theme),
                ));
                spans
            }
            Piece::Logo(logo) => logo_spans(logo, theme),
            Piece::InColourOf(cli, words, bold) => {
                let mut style = theme.style(crate::base::theme::Role::for_cli(cli));
                if *bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                vec![Span::styled(words.clone(), style)]
            }
            Piece::InColour(seat, words, bold) => {
                let mut style = faces.face(seat).name_style(theme);
                if *bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                vec![Span::styled(words.clone(), style)]
            }
        }
    }

    fn is_words(&self) -> bool {
        matches!(self, Piece::Words(..))
    }
}

pub fn width(pieces: &[Piece], faces: &Faces, theme: &Theme) -> usize {
    pieces.iter().map(|piece| piece.width(faces, theme)).sum()
}

pub fn spans(pieces: &[Piece], faces: &Faces, theme: &Theme) -> Vec<Span<'static>> {
    pieces
        .iter()
        .flat_map(|piece| piece.spans(faces, theme))
        .collect()
}

/// Every piece in one tone, as a reply's quote or a faded message draws its words.
pub fn toned(pieces: &[Piece], tone: Tone) -> Vec<Piece> {
    pieces
        .iter()
        .map(|piece| match piece {
            Piece::Words(words, _) => Piece::Words(words.clone(), tone),
            Piece::Called(seat, _) => Piece::Called(seat.clone(), tone),
            Piece::Name(seat, _) => Piece::Name(seat.clone(), tone),
            Piece::Model(face, _) => Piece::Model(face.clone(), tone),
            other => other.clone(),
        })
        .collect()
}

/// The seats' tapbacks on one message, side by side: each seat's badge and its mark.
pub fn tapbacks(taps: &[(SeatId, Mark)]) -> Vec<Piece> {
    let mut pieces = Vec::new();
    for (i, (seat, mark)) in taps.iter().enumerate() {
        if i > 0 {
            pieces.push(Piece::text(" "));
        }
        pieces.push(Piece::Badge(seat.clone()));
        let last = i + 1 == taps.len();
        // The last mark keeps its blank only when its glyph spills into it.
        let text = if last && !mark.wide() {
            mark.text().trim_end()
        } else {
            mark.text()
        };
        pieces.push(Piece::tone(text, mark.tone()));
    }
    pieces
}

/// A message's words split into paragraphs at its line breaks, with each seat it names kept as
/// a seat.
pub fn paragraphs(text: &[TextPart], tone: Tone) -> Vec<Vec<Piece>> {
    let mut paragraphs = vec![Vec::new()];
    for part in text {
        match part {
            TextPart::Words(words) => {
                for (i, line) in words.split('\n').enumerate() {
                    if i > 0 {
                        paragraphs.push(Vec::new());
                    }
                    if !line.is_empty() {
                        paragraphs
                            .last_mut()
                            .expect("one paragraph at least")
                            .push(Piece::Words(line.to_owned(), tone));
                    }
                }
            }
            TextPart::Seat(seat) => paragraphs
                .last_mut()
                .expect("one paragraph at least")
                .push(Piece::Seat(seat.clone())),
        }
    }
    paragraphs.retain(|paragraph| !paragraph.is_empty());
    paragraphs
}

/// A paragraph wrapped to rows of at most `room` cells; a paragraph that starts "- " is a point,
/// whose rows after the first hang under its words.
pub fn wrap(paragraph: &[Piece], room: usize, faces: &Faces, theme: &Theme) -> Vec<Vec<Piece>> {
    if let Some(Piece::Words(first, tone)) = paragraph.first()
        && let Some(rest) = first.strip_prefix("- ")
    {
        let mut words = paragraph.to_vec();
        words[0] = Piece::Words(rest.to_owned(), *tone);
        return wrap_words(&words, room.saturating_sub(2), faces, theme)
            .into_iter()
            .enumerate()
            .map(|(i, mut row)| {
                let opening = if i == 0 { "- " } else { "  " };
                row.insert(0, Piece::Words(opening.to_owned(), *tone));
                row
            })
            .collect();
    }
    wrap_words(paragraph, room, faces, theme)
}

/// Paragraphs wrapped one after another.
pub fn wrap_all(
    paragraphs: &[Vec<Piece>],
    room: usize,
    faces: &Faces,
    theme: &Theme,
) -> Vec<Vec<Piece>> {
    paragraphs
        .iter()
        .flat_map(|paragraph| wrap(paragraph, room, faces, theme))
        .collect()
}

/// Greedy wrapping at blanks. A word is everything between two blanks, so "Luna's" with a
/// seat in it stays whole.
fn wrap_words(pieces: &[Piece], room: usize, faces: &Faces, theme: &Theme) -> Vec<Vec<Piece>> {
    let words = split_words(pieces);
    let mut rows: Vec<Vec<Piece>> = Vec::new();
    let mut row: Vec<Piece> = Vec::new();
    let mut used = 0;
    let mut row_tone = Tone::Text;
    for word in words {
        let size = width(&word, faces, theme);
        if !row.is_empty() && used + 1 + size > room {
            rows.push(merge(std::mem::take(&mut row)));
            used = 0;
        }
        if !row.is_empty() {
            row.push(Piece::Words(" ".to_owned(), row_tone));
            used += 1;
        }
        if let Some(Piece::Words(_, tone)) = word.iter().rev().find(|piece| piece.is_words()) {
            row_tone = *tone;
        }
        used += size;
        row.extend(word);
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(merge(row));
    }
    rows
}

fn split_words(pieces: &[Piece]) -> Vec<Vec<Piece>> {
    let mut words: Vec<Vec<Piece>> = vec![Vec::new()];
    for piece in pieces {
        match piece {
            Piece::Words(text, tone) => {
                for (i, part) in text.split(' ').enumerate() {
                    if i > 0 {
                        words.push(Vec::new());
                    }
                    if !part.is_empty() {
                        words
                            .last_mut()
                            .expect("one word at least")
                            .push(Piece::Words(part.to_owned(), *tone));
                    }
                }
            }
            other => words
                .last_mut()
                .expect("one word at least")
                .push(other.clone()),
        }
    }
    words.retain(|word| !word.is_empty());
    words
}

/// Neighbouring runs of the same tone joined into one, so a row is a few spans, not one a word.
fn merge(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::with_capacity(pieces.len());
    for piece in pieces {
        match (out.last_mut(), piece) {
            (Some(Piece::Words(last, last_tone)), Piece::Words(words, tone))
                if *last_tone == tone =>
            {
                last.push_str(&words);
            }
            (_, piece) => out.push(piece),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::council::talk::faces::Face;
    use argonauts_core::look::logos::Logo;

    fn faces() -> Faces {
        Faces::new([(
            SeatId::numbered(3),
            Face::new("3.8 Flash", "antigravity-cli", Logo::dot("antigravity-cli")),
        )])
    }

    fn plain(rows: &[Vec<Piece>], faces: &Faces) -> Vec<String> {
        rows.iter()
            .map(|row| {
                row.iter()
                    .map(|piece| match piece {
                        Piece::Words(words, _) => words.clone(),
                        Piece::Seat(seat) => format!("@{}", faces.face(seat).label()),
                        Piece::Name(seat, _) => faces.face(seat).label(),
                        Piece::Badge(_) | Piece::Logo(_) => "@".to_owned(),
                        Piece::Named(seat) | Piece::Called(seat, _) => {
                            format!("@{}", faces.face(seat).label())
                        }
                        Piece::Model(face, _) => format!("@{}", face.label()),
                        Piece::InColour(_, words, _) | Piece::InColourOf(_, words, _) => {
                            words.clone()
                        }
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_mention_wraps_whole_and_keeps_what_is_glued_to_it() {
        let faces = faces();
        let paragraph = vec![
            Piece::text("With "),
            Piece::Seat(SeatId::numbered(3)),
            Piece::text("'s Lua script a bucket is one call"),
        ];
        // "With " 4, the mention 2 + 1 + 9 and "'s" 2: 18 fits a row of 18 with its blank.
        let rows = wrap(&paragraph, 19, &faces, &Theme::dark());
        assert_eq!(
            plain(&rows, &faces),
            ["With @3.8 Flash's", "Lua script a bucket", "is one call"]
        );
    }

    #[test]
    fn a_point_hangs_under_its_first_word() {
        let faces = faces();
        let rows = wrap(
            &[Piece::text("- one two three four")],
            12,
            &faces,
            &Theme::dark(),
        );
        assert_eq!(plain(&rows, &faces), ["- one two", "  three four"]);
    }

    #[test]
    fn the_last_tapback_keeps_its_blank_only_after_a_wide_mark() {
        let seat = SeatId::numbered(1);
        let taps = tapbacks(&[(seat.clone(), Mark::Agree), (seat.clone(), Mark::Question)]);
        let faces = faces();
        assert_eq!(width(&taps, &faces, &Theme::dark()), 2 + 2 + 1 + 2 + 1);
        let taps = tapbacks(&[(seat, Mark::Object)]);
        assert_eq!(width(&taps, &faces, &Theme::dark()), 4);
    }
}
