//! A thread, opened with `t` on a message with replies: that message and its replies alone, as
//! Messages' thread view hides the rest of the talk. A reply to the thread's first message
//! needs no quote here; a reply to a reply keeps its quote. What you write here goes to every
//! seat, as a reply in the thread.

use argonauts_core::base::names::SeatId;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::Look;
use super::super::talk::cell::Spacing;
use super::super::talk::faces::Faces;
use super::super::talk::item::Item;
use super::super::talk::row::Row;
use super::super::talk::words::{Piece, Tone};
use crate::base::text::str_width;
use crate::base::theme::{Role, Theme};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// The seat whose message starts it, or `None` for yours, and how many replies it has.
    pub starter: Option<SeatId>,
    pub replies: usize,
    pub items: Vec<Item>,
}

/// The keys while a thread is open.
pub const THREAD_KEYS: &str = "esc to go back · ↑ ↓  to pick a message · r to reply to it";

impl Thread {
    /// The whole screen, `height` rows of `width`.
    pub fn lines(
        &self,
        width: usize,
        height: usize,
        faces: &Faces,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let dim = Tone::Dim.style(theme);
        let mut head = Row::new();
        let replies = match self.replies {
            1 => "1 reply".to_owned(),
            n => format!("{n} replies"),
        };
        let whose = match &self.starter {
            Some(seat) => [
                Piece::Called(seat.clone(), Tone::Dim),
                Piece::dim(format!("'s message and its {replies}")),
            ],
            None => [
                Piece::dim("❯"),
                Piece::dim(format!(" Your message and its {replies}")),
            ],
        };
        let mut title = vec![Piece::tone("Thread", Tone::Label), Piece::dim("  ·  ")];
        title.extend(whose);
        head.pieces(0, &title, faces, theme);
        let hint = "esc to go back to the whole talk";
        head.put(
            width.saturating_sub(1 + str_width(hint)),
            Span::styled(hint, dim),
        );
        let mut rows = vec![head.line(width), rule(width, theme.style(Role::Faint))];
        let mut spacing = Spacing::default();
        for item in &self.items {
            let (follows, blank) = spacing.place(item, Look::Pictures);
            if blank || !follows && rows.len() > 2 {
                rows.push(Line::default());
            }
            rows.extend(item.draw(Look::Pictures, width, faces, theme).rows);
        }
        let bottom = vec![
            labelled(width, "thread", theme),
            Row::new()
                .with(0, Span::styled("❯", theme.style(Role::Text)))
                .with(
                    2,
                    Span::styled("Reply in this thread; every seat sees it", dim),
                )
                .line(width),
            rule(width, theme.style(Role::Rule)),
            Row::new()
                .with(2, Span::styled(THREAD_KEYS, dim))
                .line(width),
        ];
        let room = height.saturating_sub(bottom.len());
        if rows.len() > room {
            rows.drain(2..2 + (rows.len() - room));
        }
        rows.resize(room, Line::default());
        rows.extend(bottom);
        rows
    }
}

fn rule(width: usize, style: Style) -> Line<'static> {
    Line::from(Span::styled("─".repeat(width), style))
}

/// The input's top rule with its label at the right, as Claude Code writes `─ council ─`.
pub fn labelled(width: usize, label: &str, theme: &Theme) -> Line<'static> {
    let label = format!(" {label} ");
    let left = width.saturating_sub(str_width(&label) + 1);
    let rule_style = theme.style(Role::Rule);
    Line::from(vec![
        Span::styled("─".repeat(left), rule_style),
        Span::styled(label, theme.style(Role::Label)),
        Span::styled("─", rule_style),
    ])
}
