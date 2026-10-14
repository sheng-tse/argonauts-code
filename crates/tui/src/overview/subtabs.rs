//! The row of choices under the panel's tabs, such as Stats' By CLI, By model and By role or
//! the Overview's periods, and which row the keys are on. Focus shows by fill alone: the
//! chosen item filled while its row has the keys, the open tab in the accent and a choice
//! under it in orange, on the grey band while it doesn't, and never a drawn box. Tab moves
//! the sub-tabs from the cards too, so there they stay orange, as Claude Code's /stats keeps
//! its tab filled while Tab switches it.

use ratatui::text::Span;

use crate::kit::Kit;
use crate::theme::Role;

/// The blank cells between two tabs or two choices on a row.
pub const TAB_GAP: &str = "  ";

/// Which row of a tab the keys are on, top to bottom.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Level {
    /// The panel's tabs, where ←→ change the tab.
    #[default]
    Tabs,
    /// The choices under them, where ←→ change the choice and the cards follow at once.
    SubTabs,
    /// The cards and their rows.
    Cards,
}

impl Level {
    /// ↓: one row further in, staying on the cards.
    pub fn down(self) -> Level {
        match self {
            Level::Tabs => Level::SubTabs,
            Level::SubTabs | Level::Cards => Level::Cards,
        }
    }

    /// ↑: one row back out, staying on the tabs.
    pub fn up(self) -> Level {
        match self {
            Level::Cards => Level::SubTabs,
            Level::SubTabs | Level::Tabs => Level::Tabs,
        }
    }

    /// Tab moves the sub-tabs from here: from their own row and from the cards under it.
    pub fn moves_sub_tabs(self) -> bool {
        self != Level::Tabs
    }
}

/// `items` side by side, the one `at` filled as [`chosen_sub_tab`] fills it.
pub fn row(kit: &Kit<'_>, items: &[&str], at: usize, focused: bool) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(TAB_GAP));
        }
        spans.push(if index == at {
            chosen_sub_tab(kit, item, focused)
        } else {
            kit.dim(format!(" {item} "))
        });
    }
    spans
}

/// The open tab: filled in the accent while the tabs have the keys, and on the grey band while
/// they are elsewhere. Without colour, brackets say which row has the keys.
pub fn chosen(kit: &Kit<'_>, name: &str, focused: bool) -> Span<'static> {
    filled(kit, name, focused, Role::Question)
}

/// The chosen item of a row under the tabs: filled in orange while its row has the keys, so it
/// reads apart from the open tab above it, and on the grey band while they are elsewhere.
pub fn chosen_sub_tab(kit: &Kit<'_>, name: &str, focused: bool) -> Span<'static> {
    filled(kit, name, focused, Role::AgentOrange)
}

fn filled(kit: &Kit<'_>, name: &str, focused: bool, fill: Role) -> Span<'static> {
    let bold = ratatui::style::Modifier::BOLD;
    match (kit.has_color(), focused) {
        (true, true) => Span::styled(
            format!(" {name} "),
            kit.cell(Role::BadgeText, fill).add_modifier(bold),
        ),
        (true, false) => Span::styled(
            format!(" {name} "),
            kit.fill(Role::UserBand).add_modifier(bold),
        ),
        (false, true) => kit.strong(format!("[{name}]")),
        (false, false) => kit.strong(format!(" {name} ")),
    }
}

/// The choice `by` places along from `at` among `count`, wrapping at both ends.
pub fn step(at: usize, by: isize, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let count = count as isize;
    (at as isize + by).rem_euclid(count) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::spans_width;
    use crate::theme::{ColorLevel, Theme};

    #[test]
    fn the_chosen_item_is_filled_never_boxed() {
        let theme = Theme::dark().with_level(ColorLevel::TrueColor);
        let kit = Kit::new(&theme);
        let focused = row(&kit, &["By CLI", "By model", "By role"], 1, true);
        let text: String = focused.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(text, " By CLI    By model    By role ");
        assert_eq!(focused[2].style.bg, theme.color(Role::AgentOrange));
        let resting = row(&kit, &["By CLI", "By model", "By role"], 1, false);
        assert_eq!(resting[2].style.bg, theme.color(Role::UserBand));
        assert_eq!(spans_width(&focused), spans_width(&resting));
        assert!(focused[0].style.bg.is_none());
        // The open tab above the row fills in the accent instead.
        assert_eq!(
            chosen(&kit, "Stats", true).style.bg,
            theme.color(Role::Question)
        );
        assert_eq!(
            chosen(&kit, "Stats", false).style.bg,
            theme.color(Role::UserBand)
        );
    }

    #[test]
    fn without_color_brackets_mark_the_row_with_the_keys() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let text = |focused| -> String {
            row(&kit, &["All time", "Last 30 days"], 0, focused)
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };
        assert_eq!(text(true), "[All time]   Last 30 days ");
        assert_eq!(text(false), " All time    Last 30 days ");
    }

    #[test]
    fn steps_wrap_at_both_ends_and_levels_stop() {
        assert_eq!(step(0, -1, 3), 2);
        assert_eq!(step(2, 1, 3), 0);
        assert_eq!(Level::Tabs.up(), Level::Tabs);
        assert_eq!(Level::Tabs.down(), Level::SubTabs);
        assert_eq!(Level::Tabs.down().down().down(), Level::Cards);
        assert!(!Level::Tabs.moves_sub_tabs());
        assert!(Level::SubTabs.moves_sub_tabs() && Level::Cards.moves_sub_tabs());
    }
}
