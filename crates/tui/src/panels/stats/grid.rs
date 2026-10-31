//! Cards side by side, as By CLI, By model and By role lay theirs out: as many to a row as
//! fit, two cells apart and all one height. Each card's bars come under its head, and the
//! rows after them start on the same row across the cards, so they read along as well as down.

use ratatui::text::Span;

use crate::base::kit::{Drawn, Frame, Kit, side_by_side};

/// The narrowest a card is drawn before the cards take a row each.
const LEAST_CARD: usize = 34;
/// Cells between two cards side by side.
const GAP: usize = 2;

type Row = Vec<Span<'static>>;

/// A card's rows: its title and what it is, a bar for each of its parts, and what follows.
#[derive(Default)]
pub(super) struct Body {
    pub head: Vec<Row>,
    pub bars: Vec<Row>,
    pub rest: Vec<Row>,
}

/// How `cards` cards share a screen `width` wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Layout {
    pub per_row: usize,
    /// The narrowest card from border to border.
    card: usize,
    /// The cells a row has over once its cards are all that wide. The first cards of a row
    /// take one each, so the last card ends where the box above the cards does.
    spare: usize,
}

impl Layout {
    pub fn of(width: usize, cards: usize) -> Self {
        let room = width.saturating_sub(2);
        let per_row = ((room + GAP) / (LEAST_CARD + GAP)).clamp(1, cards.max(1));
        let shared = room - GAP * (per_row - 1);
        Self {
            per_row,
            card: shared / per_row,
            spare: shared % per_row,
        }
    }

    /// The `at`-th card from border to border.
    pub fn card(&self, at: usize) -> usize {
        self.card + usize::from(at % self.per_row < self.spare)
    }

    /// The cells inside the `at`-th card's frame.
    pub fn inner(&self, at: usize) -> usize {
        self.card(at).saturating_sub(4)
    }

    /// The cells inside the narrowest card's frame, for what every card lays out alike.
    pub fn least_inner(&self) -> usize {
        self.card.saturating_sub(4)
    }

    /// The row of cards `card` is on.
    pub fn row_of(&self, card: usize) -> usize {
        card / self.per_row
    }
}

/// The cards in rows, `picked` the one the keys are on.
pub(super) fn draw(
    kit: &Kit<'_>,
    layout: Layout,
    bodies: Vec<Body>,
    picked: Option<usize>,
) -> Vec<Drawn> {
    let mut out = Vec::new();
    let mut bodies = bodies.into_iter().peekable();
    let mut first = 0;
    while bodies.peek().is_some() {
        let row: Vec<Body> = bodies.by_ref().take(layout.per_row).collect();
        let bars = row.iter().map(|body| body.bars.len()).max().unwrap_or(0);
        let rest = row.iter().map(|body| body.rest.len()).max().unwrap_or(0);
        let mut line = Drawn::new();
        let mut at = 1;
        for (k, body) in row.into_iter().enumerate() {
            let width = layout.card(first + k);
            let frame = Frame {
                left: 0,
                width,
                toggle: None,
                active: picked == Some(first + k),
            };
            let card = frame.draw(kit, rows_of(body, bars, rest));
            line = if k == 0 {
                let mut card = card;
                card.indent(1);
                card
            } else {
                side_by_side(line, card, at)
            };
            at += width + GAP;
        }
        first += layout.per_row;
        out.push(line);
    }
    out
}

/// The card's rows, its bars and what follows each padded to the tallest on its row.
fn rows_of(body: Body, bars: usize, rest: usize) -> Drawn {
    let mut drawn = Drawn::new();
    let padded = |rows: Vec<Row>, to: usize| {
        let blanks = to.saturating_sub(rows.len());
        rows.into_iter()
            .chain(std::iter::repeat_n(Vec::new(), blanks))
    };
    for row in body
        .head
        .into_iter()
        .chain(padded(body.bars, bars))
        .chain(padded(body.rest, rest))
    {
        drawn.push(row);
    }
    drawn
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_cards_fit_a_screen_of_120_and_one_a_screen_of_60() {
        let wide = Layout::of(120, 6);
        assert_eq!((wide.per_row, wide.card(0), wide.inner(0)), (3, 38, 34));
        assert_eq!(wide.row_of(2), 0);
        assert_eq!(wide.row_of(3), 1);
        let narrow = Layout::of(60, 6);
        assert_eq!((narrow.per_row, narrow.card(0)), (1, 58));
        // Fewer cards than fit share the row between them.
        assert_eq!(Layout::of(120, 2).per_row, 2);
    }

    /// The cards and the gaps between them fill the row at every width, the first cards a
    /// cell wider where the cells don't share out evenly, and each row's cards alike.
    #[test]
    fn a_row_of_cards_is_as_wide_as_the_screen_less_its_margins() {
        for width in 40..200 {
            for cards in 1..=7 {
                let layout = Layout::of(width, cards);
                let row: usize = (0..layout.per_row).map(|at| layout.card(at)).sum();
                let filled = row + GAP * (layout.per_row - 1);
                assert_eq!(filled, width - 2, "{width} cells, {cards} cards");
                for at in 0..cards {
                    assert_eq!(layout.card(at), layout.card(at % layout.per_row));
                    assert!(layout.card(at).abs_diff(layout.card(0)) <= 1);
                    assert!(layout.inner(at) >= layout.least_inner());
                }
            }
        }
        let uneven = Layout::of(140, 3);
        assert_eq!([0, 1, 2].map(|at| uneven.card(at)), [45, 45, 44]);
    }
}
