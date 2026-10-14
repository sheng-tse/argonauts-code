//! A badge: ` 21% ` in bold dark text on its level's color, or `[21%]` without color.

use ratatui::style::Modifier;
use ratatui::text::Span;

use super::{Kit, Level};
use crate::text::str_width;
use crate::theme::Role;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badge {
    pub text: String,
    pub level: Level,
    /// Cells kept for the badge whatever its value, so the header doesn't shift between 9%
    /// and 10%.
    pub reserve: usize,
    /// No reading yet: a dim `–` with no fill, since no level is known.
    pub pending: bool,
}

impl Badge {
    /// The widest percent a badge shows, ` 100% `.
    pub const PERCENT_CELLS: usize = 6;

    /// A whole percent, as `21%` or `105%`, in its level's color.
    pub fn percent(ratio: f64, level: Level) -> Self {
        Self {
            text: format!("{:.0}%", ratio.max(0.0) * 100.0),
            level,
            reserve: Self::PERCENT_CELLS,
            pending: false,
        }
    }

    /// Before the first reading, holding the slot a percent will take.
    pub fn pending() -> Self {
        Self {
            text: String::from(crate::format::UNKNOWN),
            level: Level::Ok,
            reserve: Self::PERCENT_CELLS,
            pending: true,
        }
    }

    /// Words in place of a number, such as `compacting`.
    pub fn words(text: impl Into<String>, level: Level) -> Self {
        Self {
            text: text.into(),
            level,
            reserve: Self::PERCENT_CELLS,
            pending: false,
        }
    }

    /// The badge's own cells, one more than its text on each side.
    pub fn width(&self) -> usize {
        str_width(&self.text) + 2
    }

    /// The cells its slot takes: its reserve, or more for a longer badge.
    pub fn slot(&self) -> usize {
        self.width().max(self.reserve)
    }

    /// The badge right-aligned in its slot.
    pub fn spans(&self, kit: &Kit<'_>) -> Vec<Span<'static>> {
        let pad = self.slot() - self.width();
        let mut out = Vec::new();
        if pad > 0 {
            out.push(Span::raw(" ".repeat(pad)));
        }
        if kit.has_color() && self.pending {
            out.push(kit.dim(format!(" {} ", self.text)));
        } else if kit.has_color() {
            let style = kit
                .cell(Role::BadgeText, self.level.badge_role())
                .add_modifier(Modifier::BOLD);
            out.push(Span::styled(format!(" {} ", self.text), style));
        } else {
            out.push(Span::styled(
                format!("[{}]", self.text),
                kit.bold(Role::Bright),
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::spans_width;
    use crate::theme::{ColorLevel, Theme};

    #[test]
    fn a_badge_keeps_its_slot_and_brackets_without_color() {
        let dark = Theme::dark();
        let plain = Theme::dark().with_level(ColorLevel::None);
        for ratio in [0.0, 0.09, 0.1, 0.99, 1.0] {
            let badge = Badge::percent(ratio, Level::Ok);
            assert_eq!(spans_width(&badge.spans(&Kit::new(&dark))), 6);
            assert_eq!(spans_width(&badge.spans(&Kit::new(&plain))), 6);
        }
        let text: String = Badge::percent(0.21, Level::Ok)
            .spans(&Kit::new(&plain))
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(text, " [21%]");
        let colored = Badge::percent(0.92, Level::Warn).spans(&Kit::new(&dark));
        assert_eq!(colored[1].content, " 92% ");
        assert_eq!(colored[1].style.bg, dark.color(Role::Warn));
        assert_eq!(colored[1].style.fg, dark.color(Role::BadgeText));
    }
}
