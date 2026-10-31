//! The progress bar of a row in the run list, as Claude Code draws it: a cell for each share of
//! what sits under the row done, filled in the text colour and hollow in dim. Near its limit the
//! filled part turns amber and the rest becomes a dashed amber track, which reads with colour
//! off and with nothing done.

use ratatui::text::Span;

use crate::base::theme::{Role, Theme};

/// Claude Code's workflow bar is this wide.
pub(crate) const BAR_CELLS: usize = 20;

/// Below this the bar says too little to be worth its columns.
pub(crate) const MIN_BAR_CELLS: usize = 4;

/// `count` holds done and total. A cell fills once its whole share is done.
pub(crate) fn bar(
    count: Option<(u32, u32)>,
    cells: usize,
    near_limit: bool,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let filled = match count {
        Some((done, total)) if total > 0 => {
            let done = u64::from(done.min(total));
            (done * cells as u64 / u64::from(total)) as usize
        }
        _ => 0,
    };
    let (full, hollow) = if theme.glyphs.is_ascii() {
        (theme.glyphs.bar_fill, theme.glyphs.bar_track)
    } else {
        ("▰", "▱")
    };
    let (fill, track, rest) = if near_limit {
        (Role::Warn, near_track(theme), Role::Warn)
    } else {
        (Role::Text, hollow, Role::Dim)
    };
    let mut spans = Vec::new();
    if filled > 0 {
        spans.push(Span::styled(full.repeat(filled), theme.style(fill)));
    }
    if cells > filled {
        spans.push(Span::styled(
            track.repeat(cells - filled),
            theme.style(rest),
        ));
    }
    spans
}

/// The dashed track a bar near its limit leaves for what is still to do: a shape of its own,
/// so a near-limit row is told apart without colour.
pub(crate) fn near_track(theme: &Theme) -> &'static str {
    if theme.glyphs.is_ascii() { "-" } else { "╌" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::theme::Glyphs;

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn drawn(spans: &[Span<'_>]) -> String {
        spans.iter().map(|span| span.content.as_ref()).collect()
    }

    #[test]
    fn a_cell_fills_once_its_whole_share_is_done() {
        let theme = theme();
        let third = bar(Some((1, 3)), 20, false, &theme);
        assert_eq!(drawn(&third), "▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱");
        assert_eq!(drawn(&bar(Some((4, 4)), 4, false, &theme)), "▰▰▰▰");
        assert_eq!(drawn(&bar(Some((0, 0)), 4, false, &theme)), "▱▱▱▱");
        assert_eq!(drawn(&bar(None, 4, false, &theme)), "▱▱▱▱");
    }

    #[test]
    fn near_its_limit_the_filled_part_turns_amber() {
        let theme = theme();
        let near = bar(Some((5, 6)), 20, true, &theme);
        assert_eq!(near[0].style, theme.style(Role::Warn));
        // The rest is a dashed amber track, a shape that reads with colour off.
        assert_eq!(near[1].style, theme.style(Role::Warn));
        assert_eq!(drawn(&near), format!("{}{}", "▰".repeat(16), "╌".repeat(4)));
        let nothing_done = bar(Some((0, 6)), 4, true, &theme);
        assert_eq!(drawn(&nothing_done), "╌╌╌╌");
        let mut ascii = theme.clone();
        ascii.glyphs = Glyphs::ASCII;
        assert_eq!(drawn(&bar(Some((0, 6)), 4, true, &ascii)), "----");
        let calm = bar(Some((5, 6)), 20, false, &theme);
        assert_eq!(calm[0].style, theme.style(Role::Text));
    }

    /// With no colour at all the near-limit bar still reads apart, by its track's shape.
    #[test]
    fn without_colour_a_bar_near_its_limit_still_reads_apart() {
        let plain = theme().with_level(crate::base::theme::ColorLevel::None);
        let near = drawn(&bar(Some((5, 6)), 20, true, &plain));
        let calm = drawn(&bar(Some((5, 6)), 20, false, &plain));
        assert_ne!(near, calm);
        assert!(near.ends_with("╌╌╌╌"), "{near}");
    }
}
