//! A card's first row: `◆ context` at the left, and the bold amount, its dim tail, and the
//! badge at the right. When the row is too narrow the tail gives way first, then the amount,
//! and never the badge.

use ratatui::text::Span;

use super::{Badge, Kit, spread};
use crate::text::{spans_width, truncate_spans};
use crate::theme::Role;

#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    /// The color of the mod's `◆`.
    pub glyph_role: Role,
    /// Bold and lowercase, such as `context`.
    pub title: String,
    /// What it's about when that isn't the default, as `main · Opus 5.5 on Claude Code`.
    pub subject: Option<String>,
    /// The headline number, in bold.
    pub amount: Option<String>,
    /// What the amount is measured against, as ` of 1M · compacts at 950k`.
    pub tail: Vec<Span<'static>>,
    pub badge: Option<Badge>,
}

/// The least room between the title and the numbers.
const GAP: usize = 2;
/// Cells a long subject keeps, ` · ` included, before the numbers give way to it.
const SUBJECT_KEEPS: usize = 16;

impl Header {
    pub fn new(title: impl Into<String>, glyph_role: Role) -> Self {
        Self {
            glyph_role,
            title: title.into(),
            subject: None,
            amount: None,
            tail: Vec::new(),
            badge: None,
        }
    }

    pub fn left(&self, kit: &Kit<'_>) -> Vec<Span<'static>> {
        let mut left = vec![
            kit.span(kit.glyphs.the_mod, self.glyph_role),
            Span::raw(" "),
            kit.strong(self.title.clone()),
        ];
        if let Some(subject) = &self.subject {
            left.push(kit.dim(format!(" · {subject}")));
        }
        left
    }

    /// The numbers, with as much as fits in `room` cells.
    fn right(&self, kit: &Kit<'_>, room: usize) -> Vec<Span<'static>> {
        let badge = self
            .badge
            .as_ref()
            .map(|badge| badge.spans(kit))
            .unwrap_or_default();
        let amount: Vec<Span<'static>> =
            self.amount.iter().map(|a| kit.strong(a.clone())).collect();
        let with_badge = |mut numbers: Vec<Span<'static>>| {
            if !numbers.is_empty() && !badge.is_empty() {
                numbers.push(Span::raw(" "));
            }
            numbers.extend(badge.iter().cloned());
            numbers
        };
        let full = with_badge([amount.clone(), self.tail.clone()].concat());
        if spans_width(&full) <= room {
            return full;
        }
        let short = with_badge(amount);
        if spans_width(&short) <= room {
            return short;
        }
        with_badge(Vec::new())
    }

    pub fn draw(&self, kit: &Kit<'_>, width: usize) -> Vec<Span<'static>> {
        let left = self.left(kit);
        // A long subject is cut before the numbers go, down to what it keeps.
        let title = spans_width(&left[..3]);
        let least_left = title + (spans_width(&left) - title).min(SUBJECT_KEEPS);
        let room = width.saturating_sub(least_left + GAP);
        let right = self.right(kit, room);
        // Even the badge alone can be too wide for a long title, which is cut instead.
        let left_room = width.saturating_sub(spans_width(&right) + GAP);
        let left = if spans_width(&left) > left_room {
            truncate_spans(&left, left_room)
        } else {
            left
        };
        spread(left, right, width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kit::Level;
    use crate::theme::{ColorLevel, Theme};

    fn header(kit: &Kit<'_>) -> Header {
        Header {
            amount: Some(String::from("212k")),
            tail: vec![kit.dim(" of 1M · compacts at 950k")],
            badge: Some(Badge::percent(0.21, Level::Ok)),
            ..Header::new("context", Role::Accent)
        }
    }

    fn text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn the_tail_goes_first_then_the_amount_never_the_badge() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let h = header(&kit);
        assert_eq!(
            text(&h.draw(&kit, 60)),
            format!(
                "◆ context{}212k of 1M · compacts at 950k  [21%]",
                " ".repeat(15)
            )
        );
        assert_eq!(
            text(&h.draw(&kit, 30)),
            format!("◆ context{}212k  [21%]", " ".repeat(10))
        );
        assert_eq!(text(&h.draw(&kit, 18)), "◆ context    [21%]");
        assert_eq!(text(&h.draw(&kit, 12)), "◆ c…   [21%]");
        for width in [12, 18, 30, 60, 90] {
            assert_eq!(spans_width(&h.draw(&kit, width)), width);
        }
    }
}
