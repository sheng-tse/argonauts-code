//! A seat's own session, opened from its row or one of its messages: exactly what the seat read,
//! each turn as its CLI shows a prompt, and what it answered. The seat reads the others by its
//! own letters, so the head gives you the key. What you write here goes to this seat alone: the
//! input's rules take the seat's CLI colour and the label says "private", so it never reads as
//! the talk.

use argonauts_core::agents::cli_sessions::CliSessionRef;
use argonauts_core::base::names::{MessageId, SeatId, SeatLetter};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::talk::faces::Faces;
use super::super::talk::row::Row;
use super::super::talk::words::{self, Mark, Piece, Tone};
use crate::base::kit::logo::{ModelLabel, NoPicture};
use crate::base::text::str_width;
use crate::base::theme::{Role, Theme};

/// One row group of the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Turn {
    /// A prompt the seat read, on the prompt band: who it is from, then the words, and a time
    /// at the right end for one of yours. `yours` names a private message of yours that you
    /// can still share with the council.
    Read {
        from: Vec<Piece>,
        words: String,
        time: Option<String>,
        yours: Option<MessageId>,
    },
    /// What the seat answered.
    Said(String),
    /// A stance it took, and why.
    Stance(Mark, String),
    /// Folded rows of the answer above.
    Fold(u32),
    /// A tool call and what it shows so far.
    Tool {
        name: String,
        argument: String,
        result: String,
    },
    /// A row laid out already.
    Row(Vec<Piece>),
    Gap,
}

impl Turn {
    /// A turn from the council, "From the council · " and who it is from.
    pub fn council(from: &str, words: &str) -> Self {
        Turn::Read {
            from: vec![Piece::dim(format!("From the council · {from}"))],
            words: words.to_owned(),
            time: None,
            yours: None,
        }
    }

    /// A private turn of yours, with its time when known; `message` while it is yours to share.
    pub fn private(words: &str, time: Option<&str>, message: Option<MessageId>) -> Self {
        Turn::Read {
            from: vec![Piece::tone(PRIVATELY, Tone::Label)],
            words: words.to_owned(),
            time: time.map(str::to_owned),
            yours: message,
        }
    }

    /// The private message of yours this turn is, while you can share it.
    fn shareable(&self) -> Option<&MessageId> {
        match self {
            Turn::Read { yours, .. } => yours.as_ref(),
            _ => None,
        }
    }
}

/// How a seat's session names a prompt of yours that no other seat reads.
pub const PRIVATELY: &str = "From the user, privately: ";

/// The bottom line of a seat's session: Esc back to the talk, `o` while the session can carry
/// on in its CLI, and ↑ while it holds a private message of yours to share.
pub fn session_keys(carry_on_in: Option<&str>, share: bool) -> String {
    let mut keys = vec!["esc to go back to the talk".to_owned()];
    if let Some(cli) = carry_on_in {
        keys.push(format!("o to carry it on in {cli}"));
    }
    if share {
        keys.push("↑  to pick a message, s to share it with the council".to_owned());
    }
    keys.join(" · ")
}

/// The key at the head of a seat's own session, which only you see: the letter the seat calls
/// itself by, then the letter it reads each other seat by, in seat order, with that seat's
/// badge and name. `letters` holds every seat's letter, the reader's own among them.
pub fn key_line(letters: &[(SeatId, SeatLetter)], reader: &SeatId) -> Vec<Piece> {
    let mut pieces = vec![Piece::tone("Only you see this key: ", Tone::Label)];
    if let Some((_, own)) = letters.iter().find(|(seat, _)| seat == reader) {
        pieces.push(Piece::dim(format!("Seat {own} is this seat")));
    }
    for (seat, letter) in letters.iter().filter(|(seat, _)| seat != reader) {
        pieces.push(Piece::dim(format!(" · {letter} is ")));
        pieces.push(Piece::Called(seat.clone(), Tone::Dim));
    }
    pieces
}

/// The bottom line while a private message of yours is picked.
pub const SHARE_KEYS: &str =
    "s to share it with the council · ↑ ↓  to pick another · esc to put it down";

/// The session view's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatSession {
    /// The badge before the name: the seat's, or a logo for a session that is no seat.
    pub badge: Piece,
    /// Its whole name, which reads after its badge as the badge draws.
    pub name: String,
    /// The CLI that runs it, whose colour its name and the input's rules take.
    pub cli: String,
    /// Dim words after the name, then the council's name: " · seat 2 of 4 · in the council ".
    pub about: String,
    pub council: String,
    pub stats: String,
    /// The session's id and how to carry it on in its CLI, then the command itself.
    pub resume: String,
    pub command: String,
    /// What only you see: the key to the seat's letters, or who made the change.
    pub only_you: Vec<Piece>,
    pub turns: Vec<Turn>,
    pub placeholder: String,
    pub keys: String,
    /// The private message of yours picked to share, by its place in `turns`.
    pub picked: Option<usize>,
    /// The seat's session in its own CLI, which `o` twice opens there.
    pub carried: Option<CliSessionRef>,
}

impl SeatSession {
    /// The name after its badge: the seat's as its badge has it, or the model's as its logo
    /// draws.
    fn badged_name(&self, faces: &Faces, theme: &Theme) -> String {
        match &self.badge {
            Piece::Badge(seat) => faces.face(seat).beside(theme),
            Piece::Logo(logo) => ModelLabel::named(logo, &self.name, theme, NoPicture::Dot).name,
            _ => self.name.clone(),
        }
    }

    /// Where each private message of yours you can share sits in `turns`, newest first.
    pub fn shareable_newest_first(&self) -> Vec<usize> {
        let mut at: Vec<usize> = self
            .turns
            .iter()
            .enumerate()
            .filter(|(_, turn)| turn.shareable().is_some())
            .map(|(index, _)| index)
            .collect();
        at.reverse();
        at
    }

    /// The private message of yours picked to share.
    pub fn picked_message(&self) -> Option<&MessageId> {
        self.turns.get(self.picked?)?.shareable()
    }

    /// The whole screen, `height` rows of `width`.
    pub fn lines(
        &self,
        width: usize,
        height: usize,
        faces: &Faces,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let colour = theme.style(Role::for_cli(&self.cli));
        let name = self.badged_name(faces, theme);
        let mut head = Row::new();
        let after = head.pieces(
            0,
            &[
                self.badge.clone(),
                Piece::InColourOf(self.cli.clone(), format!(" {name}"), true),
                Piece::dim(self.about.clone()),
                Piece::tone(self.council.clone(), Tone::Label),
            ],
            faces,
            theme,
        );
        let _ = after;
        head.put(
            width.saturating_sub(1 + str_width(&self.stats)),
            Span::styled(self.stats.clone(), Tone::Dim.style(theme)),
        );
        let mut resume = Row::new();
        let at = resume.put(3, Span::styled(self.resume.clone(), Tone::Dim.style(theme)));
        let command = Span::styled(self.command.clone(), theme.style(Role::Code));
        // A command too long to follow its words goes whole on the row under them.
        let mut under = None;
        if at + str_width(&self.command) <= width {
            resume.put(at, command);
        } else {
            under = Some(Row::new().with(3, command).line(width));
        }
        let mut only = Row::new();
        only.pieces(3, &self.only_you, faces, theme);
        let mut top = vec![head.line(width), resume.line(width)];
        top.extend(under);
        top.extend([
            only.line(width),
            rule(width, theme.style(Role::Faint)),
            Line::default(),
        ]);
        for (index, turn) in self.turns.iter().enumerate() {
            let picked = self.picked == Some(index);
            top.extend(turn_rows(turn, picked, width, faces, theme));
        }
        let mut label = Row::new();
        let label_pieces = [
            self.badge.clone(),
            Piece::tone(format!(" {name} · private"), Tone::Label),
        ];
        let label_width = words::width(&label_pieces, faces, theme);
        let left = width.saturating_sub(label_width + 3);
        label.put(0, Span::styled("─".repeat(left), colour));
        label.pieces(left + 1, &label_pieces, faces, theme);
        label.put(left + label_width + 2, Span::styled("─", colour));
        let bottom = vec![
            label.line(width),
            Row::new()
                .with(0, Span::styled("❯", colour))
                .with(
                    2,
                    Span::styled(self.placeholder.clone(), Tone::Dim.style(theme)),
                )
                .line(width),
            rule(width, colour),
            Row::new()
                .with(2, Span::styled(self.bottom_keys(), Tone::Dim.style(theme)))
                .line(width),
        ];
        let room = height.saturating_sub(bottom.len());
        // The newest turns stay in view: the oldest go off the top under the head.
        if top.len() > room {
            let cut = top.len() - room;
            top.drain(5..(5 + cut).min(top.len()));
        }
        top.resize(room, Line::default());
        top.extend(bottom);
        top
    }
}

impl SeatSession {
    fn bottom_keys(&self) -> String {
        match self.picked {
            Some(_) => SHARE_KEYS.to_owned(),
            None => self.keys.clone(),
        }
    }
}

fn rule(width: usize, style: Style) -> Line<'static> {
    Line::from(Span::styled("─".repeat(width), style))
}

/// `text` in rows: the first `first` cells wide, the others `rest`.
fn wrap_words(text: &str, first: usize, rest: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut room = first;
    for word in text.split(' ') {
        if !line.is_empty() && str_width(&line) + 1 + str_width(word) > room {
            lines.push(std::mem::take(&mut line));
            room = rest;
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    lines.push(line);
    lines
}

fn turn_rows(
    turn: &Turn,
    picked: bool,
    width: usize,
    faces: &Faces,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let bright = theme.style(Role::Bright);
    let text = theme.style(Role::Text);
    let dim = Tone::Dim.style(theme);
    match turn {
        Turn::Read {
            from, words, time, ..
        } => {
            let from_width = words::width(from, faces, theme);
            let time_room = time.as_ref().map_or(0, |time| str_width(time) + 2);
            let first = width.saturating_sub(3 + from_width + time_room);
            // The picked one stands out as a picked row does in the talk's transcript.
            let (band, pointer) = if picked {
                (theme.style(Role::Selection), theme.style(Role::Accent))
            } else {
                (theme.style(Role::UserBand), theme.style(Role::Faint))
            };
            wrap_words(words, first, width.saturating_sub(3))
                .into_iter()
                .enumerate()
                .map(|(i, line)| {
                    let mut row = Row::new();
                    if i == 0 {
                        row.put(0, Span::styled("❯", pointer));
                        let at = row.pieces(2, from, faces, theme);
                        row.put(at, Span::styled(line, bright));
                        if let Some(time) = time {
                            row.put(
                                width.saturating_sub(1 + str_width(time)),
                                Span::styled(time.clone(), dim),
                            );
                        }
                    } else {
                        row.put(2, Span::styled(line, bright));
                    }
                    row.filled(band).line(width)
                })
                .collect()
        }
        Turn::Said(words) => wrap_words(words, width.saturating_sub(3), width.saturating_sub(3))
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                let mut row = Row::new();
                if i == 0 {
                    row.put(0, Span::styled("⏺", text));
                }
                row.put(2, Span::styled(line, text));
                row.line(width)
            })
            .collect(),
        Turn::Stance(mark, words) => vec![
            Row::new()
                .with(0, Span::styled("⏺", text))
                .with(
                    2,
                    Span::styled(mark.text().trim_end().to_owned(), mark.tone().style(theme)),
                )
                .with(5, Span::styled(words.clone(), text))
                .line(width),
        ],
        Turn::Fold(rows) => vec![
            Row::new()
                .with(
                    2,
                    Span::styled(format!("… +{rows} lines (ctrl+o to expand)"), dim),
                )
                .line(width),
        ],
        Turn::Tool {
            name,
            argument,
            result,
        } => vec![
            Row::new()
                .with(0, Span::styled("⏺", dim))
                .with(2, Span::styled(name.clone(), Tone::Bold.style(theme)))
                .with(
                    2 + str_width(name),
                    Span::styled(format!("({argument})"), text),
                )
                .line(width),
            Row::new()
                .with(2, Span::styled("⎿  ", dim))
                .with(5, Span::styled(result.clone(), dim))
                .line(width),
        ],
        Turn::Row(pieces) => {
            let mut row = Row::new();
            row.pieces(0, pieces, faces, theme);
            vec![row.line(width)]
        }
        Turn::Gap => vec![Line::default()],
    }
}

/// Draws `lines` into `area` of `buf`, a row each.
pub fn draw_lines(lines: &[Line<'_>], area: Rect, buf: &mut Buffer) {
    for (i, line) in lines.iter().enumerate().take(usize::from(area.height)) {
        let y = area.y + u16::try_from(i).unwrap_or(u16::MAX);
        buf.set_line(area.x, y, line, area.width);
    }
}
