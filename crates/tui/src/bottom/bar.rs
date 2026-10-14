//! The progress bar of a run's row, as Claude Code draws it: a cell for each share of the
//! agents done, filled in the text color and hollow in dim.

use ratatui::text::Span;

use crate::theme::{Role, Theme};

/// Claude Code's workflow bar is this wide.
pub(super) const BAR_CELLS: usize = 20;

/// Below this the bar says too little to be worth its columns.
pub(super) const MIN_BAR_CELLS: usize = 4;

/// `agents` holds done and total. A cell fills once its whole share is done.
pub(super) fn bar(agents: Option<(u32, u32)>, cells: usize, theme: &Theme) -> Vec<Span<'static>> {
    let filled = match agents {
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
    let mut spans = Vec::new();
    if filled > 0 {
        spans.push(Span::styled(full.repeat(filled), theme.style(Role::Text)));
    }
    if cells > filled {
        spans.push(Span::styled(
            hollow.repeat(cells - filled),
            theme.style(Role::Dim),
        ));
    }
    spans
}
