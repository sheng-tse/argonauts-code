//! The progress bar the panel's rows share: a colored fill on a grey track, or block glyphs
//! without color.

use ratatui::style::Style;
use ratatui::text::Span;

use crate::theme::{ColorLevel, Role, Theme};

/// Claude Code's workflow bar is about this wide.
pub(super) const BAR_CELLS: usize = 22;

/// Below this the bar says too little to be worth its columns.
pub(super) const MIN_BAR_CELLS: usize = 4;

/// `steps` holds done and total. `fill` carries the fill color as its background.
pub(super) fn bar(
    steps: Option<(u32, u32)>,
    cells: usize,
    fill: Style,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let filled = match steps {
        Some((done, total)) if total > 0 => {
            let done = u64::from(done.min(total));
            (done * cells as u64 / u64::from(total)) as usize
        }
        _ => 0,
    };
    let track = theme.style(Role::BarTrack);
    let colored = theme.level != ColorLevel::None && track.bg.is_some() && fill.bg.is_some();
    let mut spans = Vec::new();
    if colored {
        if filled > 0 {
            spans.push(Span::styled(" ".repeat(filled), fill));
        }
        if cells > filled {
            spans.push(Span::styled(" ".repeat(cells - filled), track));
        }
    } else {
        if filled > 0 {
            spans.push(Span::raw(theme.glyphs.bar_fill.repeat(filled)));
        }
        if cells > filled {
            spans.push(Span::styled(
                theme.glyphs.bar_track.repeat(cells - filled),
                theme.style(Role::Dim),
            ));
        }
    }
    spans
}

/// The run row's lavender, the card color, as a fill.
pub(super) fn run_fill(theme: &Theme) -> Style {
    theme
        .style(Role::Card)
        .fg
        .map_or_else(Style::new, |color| Style::new().bg(color))
}
