//! The rows of choices in the panel's head, such as the tabs, Stats' By CLI, By model and By
//! role, and the periods, and which row the keys are on. One rule draws every pick: the row
//! the keys are on, or the nearest row whose content they're in, shows its pick in orange;
//! every other row shows its pick in lavender; the choices not picked are in the text colour.
//! A row of chips fills its pick, and a row of words writes its pick bold. Tab moves the
//! sub-tabs from the cards too, so there they stay orange, as Claude Code's /stats keeps its
//! tab filled while Tab switches it.

use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::base::kit::Kit;
use crate::base::theme::Role;

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
    /// ↓: one row further in, staying on the cards. From the sub-tabs the keys go into the
    /// cards only when they have rows to move through, so ←→ never land where they do nothing.
    pub fn down(self, rows_below: bool) -> Level {
        match self {
            Level::Tabs => Level::SubTabs,
            Level::SubTabs if !rows_below => Level::SubTabs,
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

/// `items` side by side, the one `at` filled as [`chosen`] fills it and the rest in the text
/// colour.
pub fn row(kit: &Kit<'_>, items: &[&str], at: usize, keys_here: bool) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(TAB_GAP));
        }
        spans.push(if index == at {
            chosen(kit, item, keys_here)
        } else {
            unchosen(item)
        });
    }
    spans
}

/// The pick of a row of chips: on the orange chip while the keys are on its row or in what it
/// shows, and on the lavender chip otherwise. Without colour, brackets say which row has the
/// keys.
pub fn chosen(kit: &Kit<'_>, name: &str, keys_here: bool) -> Span<'static> {
    let bold = Modifier::BOLD;
    match (kit.has_color(), keys_here) {
        (true, _) => Span::styled(
            format!(" {name} "),
            kit.cell(Role::BadgeText, pick_role(keys_here))
                .add_modifier(bold),
        ),
        (false, true) => kit.strong(format!("[{name}]")),
        (false, false) => kit.strong(format!(" {name} ")),
    }
}

/// A choice not picked on a row of chips, with the chip's blanks around it.
pub fn unchosen(name: &str) -> Span<'static> {
    Span::raw(format!(" {name} "))
}

/// The pick among words, such as a period or `1 Map`: bold orange while the keys are on its
/// row, and bold lavender otherwise.
pub fn chosen_word(kit: &Kit<'_>, word: impl Into<String>, keys_here: bool) -> Span<'static> {
    Span::styled(word.into(), kit.bold(pick_role(keys_here)))
}

fn pick_role(keys_here: bool) -> Role {
    if keys_here {
        Role::AgentOrange
    } else {
        Role::Question
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
    use crate::base::text::spans_width;
    use crate::base::theme::{ColorLevel, Theme};

    #[test]
    fn only_the_row_with_the_keys_shows_its_pick_in_orange() {
        let theme = Theme::dark().with_level(ColorLevel::TrueColor);
        let kit = Kit::new(&theme);
        let with_keys = row(&kit, &["By CLI", "By model", "By role"], 1, true);
        let text: String = with_keys.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(text, " By CLI    By model    By role ");
        assert_eq!(with_keys[2].style.bg, theme.color(Role::AgentOrange));
        let elsewhere = row(&kit, &["By CLI", "By model", "By role"], 1, false);
        assert_eq!(elsewhere[2].style.bg, theme.color(Role::Question));
        assert_eq!(spans_width(&with_keys), spans_width(&elsewhere));
        // The choices not picked are in the text colour, never dim.
        assert_eq!(with_keys[0].style, ratatui::style::Style::default());
        assert_eq!(
            chosen_word(&kit, "All time", true).style.fg,
            theme.color(Role::AgentOrange)
        );
        assert_eq!(
            chosen_word(&kit, "All time", false).style.fg,
            theme.color(Role::Question)
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
        assert_eq!(Level::Tabs.down(true), Level::SubTabs);
        assert_eq!(Level::Tabs.down(true).down(true).down(true), Level::Cards);
        assert!(!Level::Tabs.moves_sub_tabs());
        assert!(Level::SubTabs.moves_sub_tabs() && Level::Cards.moves_sub_tabs());
    }

    #[test]
    fn down_stays_on_the_sub_tabs_when_nothing_below_moves() {
        assert_eq!(Level::Tabs.down(false), Level::SubTabs);
        assert_eq!(Level::SubTabs.down(false), Level::SubTabs);
        assert_eq!(Level::SubTabs.down(true), Level::Cards);
        assert_eq!(Level::Cards.down(false), Level::Cards);
    }
}
