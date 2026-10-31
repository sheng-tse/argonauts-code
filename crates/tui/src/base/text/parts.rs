//! Rows built from parts that give way whole when the row is too narrow, so the bottom line never
//! ends in half a word.

use ratatui::text::{Line, Span};

use super::{spans_width, truncate_line};

/// The rank of a part that never gives way.
pub const KEEP: u16 = u16::MAX;

#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    /// Drawn before the part when an earlier part shows, such as a dim ` · `.
    pub joiner: Vec<Span<'static>>,
    pub spans: Vec<Span<'static>>,
    /// Lower ranks give way first, and among equal ranks the rightmost goes first. Parts ranked
    /// [`KEEP`] never go.
    pub rank: u16,
}

impl Part {
    pub fn new(spans: Vec<Span<'static>>, rank: u16) -> Self {
        Self {
            joiner: Vec::new(),
            spans,
            rank,
        }
    }

    pub fn joined(joiner: Span<'static>, spans: Vec<Span<'static>>, rank: u16) -> Self {
        Self {
            joiner: vec![joiner],
            spans,
            rank,
        }
    }

    fn width(&self, first: bool) -> usize {
        let joiner = if first { 0 } else { spans_width(&self.joiner) };
        joiner + spans_width(&self.spans)
    }
}

/// Which parts show in `room` columns. Parts ranked [`KEEP`] can still be too wide, which the
/// caller then cuts.
pub fn parts_that_fit(parts: &[Part], room: usize) -> Vec<bool> {
    let mut shown = vec![true; parts.len()];
    loop {
        if width_of(parts, &shown) <= room {
            return shown;
        }
        let next = parts
            .iter()
            .enumerate()
            .filter(|(i, part)| shown[*i] && part.rank != KEEP)
            .min_by_key(|(i, part)| (part.rank, std::cmp::Reverse(*i)))
            .map(|(i, _)| i);
        match next {
            Some(i) => shown[i] = false,
            None => return shown,
        }
    }
}

/// The columns the shown parts take, joiners included.
pub fn width_of(parts: &[Part], shown: &[bool]) -> usize {
    let mut first = true;
    let mut width = 0;
    for (part, show) in parts.iter().zip(shown) {
        if *show {
            width += part.width(first);
            first = false;
        }
    }
    width
}

pub fn full_width(parts: &[Part]) -> usize {
    parts
        .iter()
        .enumerate()
        .map(|(i, part)| part.width(i == 0))
        .sum()
}

pub fn join_parts(parts: &[Part], shown: &[bool]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut first = true;
    for (part, show) in parts.iter().zip(shown) {
        if !*show {
            continue;
        }
        if !first {
            spans.extend(part.joiner.iter().cloned());
        }
        spans.extend(part.spans.iter().cloned());
        first = false;
    }
    spans
}

/// `parts` in one row of `width` columns, after `opening`: whole parts give way by rank, and only
/// if the parts that never give way are still too wide is the row cut with `…`.
pub fn fit_parts(opening: Vec<Span<'static>>, parts: &[Part], width: usize) -> Line<'static> {
    let room = width.saturating_sub(spans_width(&opening));
    let shown = parts_that_fit(parts, room);
    let mut spans = opening;
    spans.extend(join_parts(parts, &shown));
    truncate_line(&Line::from(spans), width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::text::plain_text;

    fn part(text: &str, rank: u16) -> Part {
        Part::joined(Span::raw(" · "), vec![Span::raw(text.to_owned())], rank)
    }

    #[test]
    fn parts_give_way_whole_by_rank() {
        let parts = [
            Part::new(vec![Span::raw("mode")], KEEP),
            part("hint", 1),
            part("agents", KEEP),
            part("manage", 0),
        ];
        let row = |width| plain_text(&[fit_parts(vec![Span::raw("  ")], &parts, width)]);
        assert_eq!(row(80), "  mode · hint · agents · manage");
        assert_eq!(row(30), "  mode · hint · agents");
        assert_eq!(row(20), "  mode · agents");
        assert_eq!(row(10), "  mode · …");
    }

    #[test]
    fn equal_ranks_go_from_the_right() {
        let parts = [
            Part::new(vec![Span::raw("a")], KEEP),
            part("bb", 3),
            part("cc", 3),
        ];
        assert_eq!(parts_that_fit(&parts, 6), vec![true, true, false]);
    }

    #[test]
    fn the_first_shown_part_drops_its_joiner() {
        let parts = [part("one", 0), part("two", KEEP)];
        let shown = parts_that_fit(&parts, 3);
        assert_eq!(shown, vec![false, true]);
        assert_eq!(width_of(&parts, &shown), 3);
    }
}
