//! Replying to a message: the row right above the input while you write the reply, naming who
//! you answer and quoting what, as Messages shows the message over the keyboard. Esc cancels the
//! reply and keeps what you wrote.

use argonauts_core::names::MessageId;
use ratatui::text::{Line, Span};

use super::faces::Face;
use super::row::Row;
use super::shapes::{Fill, Kind, Shape, x_of};
use super::words::Tone;
use crate::text::str_width;
use crate::theme::Theme;

const CANCEL: &str = "esc to cancel the reply";

/// What you are replying to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyBar {
    pub message: MessageId,
    /// The seat whose message it is, or `None` for one of yours.
    pub face: Option<Face>,
    pub quote: String,
}

impl ReplyBar {
    /// The row, and the pill under the quote where pictures go.
    pub fn line(&self, width: usize, theme: &Theme) -> (Line<'static>, Vec<Shape>) {
        let dim = Tone::Dim.style(theme);
        let mut row = Row::new();
        let mut at = row.put(2, Span::styled("Replying to ", dim));
        match &self.face {
            Some(face) => {
                at = row.put_all(at, face.badge(theme));
                at = row.put(
                    at,
                    Span::styled(format!(" {}  ", face.beside(theme)), face.name_style(theme)),
                );
            }
            None => at = row.put(at, Span::styled("you  ", dim)),
        }
        let room = width.saturating_sub(2 + str_width(CANCEL) + 3 + at + 4);
        let quote = cut(&self.quote, room);
        let size = str_width(&quote);
        row.put(at + 2, Span::styled(quote, Tone::Quote.style(theme)));
        row.put(
            width.saturating_sub(1 + str_width(CANCEL)),
            Span::styled(CANCEL, dim),
        );
        let pill = Shape::new(
            0,
            Kind::Pill { dot: false },
            (x_of(at) + 4, x_of(at + size + 4) - 4),
            (4, 38),
        )
        .filled(Fill::Quote);
        (row.line(width), vec![pill])
    }
}

/// `quote` in `room` cells, cut at its end with an ellipsis and the blank it spills into.
fn cut(quote: &str, room: usize) -> String {
    if str_width(quote) <= room {
        return quote.to_owned();
    }
    let mut kept = String::new();
    for ch in quote.chars() {
        if str_width(&kept) + 1 > room.saturating_sub(2) {
            break;
        }
        kept.push(ch);
    }
    format!("{}… ", kept.trim_end())
}
