//! The page's side of `mods/bottom-line.toml`: what `/mods bottom-line` offers for each key,
//! in its own words, and what its keys do to the file. The keys themselves are
//! [`BottomLineSettings`]'s, which the bottom line reads, so the page can't write a file the
//! line doesn't take.

use serde::Serialize;
use toml::{Table, Value};

use crate::settings::bottom_line::{
    ARTIFACT, AWAKE, BRANCH, BottomLineSettings, CHECKS, CONTEXT, LineForm, MODE, PLAN_USAGE,
    PlanUsageAs, PlanUsageResets, PlanUsageShow, PlanUsageWindows, SPEND, STATS, TOKENS,
};

/// The parts the page offers on the left, in its order, before your own mods.
pub const LEFT_OFFERS: [&str; 6] = [MODE, ARTIFACT, BRANCH, CHECKS, AWAKE, STATS];
/// And on the right.
pub const RIGHT_OFFERS: [&str; 5] = [PLAN_USAGE, SPEND, CONTEXT, TOKENS, STATS];

/// A part's name as the page says it.
pub fn part_label(part: &str) -> String {
    part.replace('-', " ")
}

/// A key that takes one of a few words, as the page lists them.
pub trait Choice: Copy + PartialEq + Serialize + 'static {
    /// In the order the page lists them. The first is the default.
    const ALL: &'static [Self];

    /// As the page says it.
    fn label(self) -> &'static str;
}

impl Choice for LineForm {
    const ALL: &'static [Self] = &LineForm::ALL;

    fn label(self) -> &'static str {
        match self {
            LineForm::Fit => "fit the terminal",
            LineForm::Bars => "bars",
            LineForm::Compact => "compact",
            LineForm::Gauges => "gauges",
            LineForm::Text => "text",
        }
    }
}

impl Choice for PlanUsageShow {
    const ALL: &'static [Self] = &[Self::All, Self::Main, Self::NearLimit];

    fn label(self) -> &'static str {
        match self {
            Self::All => "all in the crew",
            Self::Main => "the one in use",
            Self::NearLimit => "those near a limit",
        }
    }
}

impl Choice for PlanUsageWindows {
    const ALL: &'static [Self] = &[Self::Both, Self::Fullest, Self::FiveHours, Self::SevenDays];

    fn label(self) -> &'static str {
        match self {
            Self::Both => "both for the one in use",
            Self::Fullest => "fullest",
            Self::FiveHours => "5h",
            Self::SevenDays => "7d",
        }
    }
}

impl Choice for PlanUsageAs {
    const ALL: &'static [Self] = &[Self::Used, Self::Left];

    fn label(self) -> &'static str {
        match self {
            Self::Used => "used",
            Self::Left => "left",
        }
    }
}

impl Choice for PlanUsageResets {
    const ALL: &'static [Self] = &[Self::NearLimit, Self::Always, Self::Never];

    fn label(self) -> &'static str {
        match self {
            Self::NearLimit => "near a limit",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
}

/// The file's settings, or why it doesn't read, in one line. A file that leaves `mode` out or
/// names a part twice still reads: the line puts the mode back and keeps a part's first place.
pub fn read(text: &str) -> Result<BottomLineSettings, String> {
    BottomLineSettings::from_toml(text).map_err(|error| {
        // The reader says what, then where: `unknown variant ..` and `in `form``.
        let mut lines = error.lines().map(str::trim).filter(|line| !line.is_empty());
        let what = lines.next().unwrap_or("it doesn't read");
        match lines.next().and_then(|line| line.strip_prefix("in ")) {
            Some(key) => format!("{}: {what}", key.replace('`', "")),
            None => what.to_owned(),
        }
    })
}

/// Every key of the file with its value, a table's keys dotted: `plan-usage.as`.
fn leaves(settings: &BottomLineSettings) -> Vec<(String, Value)> {
    fn walk(prefix: &str, table: Table, found: &mut Vec<(String, Value)>) {
        for (key, value) in table {
            let key = if prefix.is_empty() {
                key
            } else {
                format!("{prefix}.{key}")
            };
            match value {
                Value::Table(inner) => walk(&key, inner, found),
                value => found.push((key, value)),
            }
        }
    }
    let mut found = Vec::new();
    if let Ok(table) = Table::try_from(settings) {
        walk("", table, &mut found);
    }
    found
}

/// What the page's keys do to the file's settings.
pub trait PageEdits {
    /// The keys whose values are different from `from`, each with its value as TOML text, in the
    /// file's order.
    fn changes_from(&self, from: &Self) -> Vec<(String, String)>;

    /// The parts on the line that a narrow terminal gives up whole, the first to go first:
    /// the ones `drop` names, in its order, then the rest, the last on the line first. The
    /// mode and plan usage aren't among them: they draw smaller in the line's own order.
    fn gives_up(&self) -> Vec<&str>;

    /// Moves the part at `at` in [`Self::gives_up`] one step earlier or later, and writes the
    /// order into `drop`. What `drop` names that isn't on the line now keeps its turn after
    /// them. `None` at the end of the list, else where the part now stands.
    fn shift_given_up(&mut self, at: usize, earlier: bool) -> Option<usize>;

    fn side(&self, left: bool) -> &[String];

    fn side_mut(&mut self, left: bool) -> &mut Vec<String>;

    /// Ticks `part` onto a side, or takes it off. `mode` only moves: unticked on one side it
    /// stays, and ticked on the other it crosses over. False when nothing changed.
    fn toggle(&mut self, left: bool, part: &str) -> bool;

    /// Moves `part` one step earlier or later on its side. False at the end of the list.
    fn shift(&mut self, left: bool, part: &str, earlier: bool) -> bool;
}

/// The order the file writes its keys in, which a table's own order (by name) isn't.
const KEY_ORDER: [&str; 8] = [
    "form",
    "left",
    "right",
    "plan-usage.show",
    "plan-usage.windows",
    "plan-usage.as",
    "plan-usage.resets",
    "narrow.drop",
];

impl PageEdits for BottomLineSettings {
    fn changes_from(&self, from: &Self) -> Vec<(String, String)> {
        let before = leaves(from);
        let mut pairs: Vec<(String, String)> = leaves(self)
            .into_iter()
            .filter(|leaf| !before.contains(leaf))
            .map(|(key, value)| (key, value.to_string()))
            .collect();
        let place = |key: &str| KEY_ORDER.iter().position(|have| *have == key);
        pairs.sort_by_key(|(key, _)| place(key).unwrap_or(KEY_ORDER.len()));
        pairs
    }

    fn gives_up(&self) -> Vec<&str> {
        let added: Vec<&str> = self
            .parts()
            .filter(|part| *part != MODE && *part != PLAN_USAGE)
            .collect();
        let mut order: Vec<&str> = self
            .narrow
            .drop
            .iter()
            .map(String::as_str)
            .filter(|part| added.contains(part))
            .collect();
        for part in added.iter().rev() {
            if !order.contains(part) {
                order.push(part);
            }
        }
        order
    }

    fn shift_given_up(&mut self, at: usize, earlier: bool) -> Option<usize> {
        let mut order: Vec<String> = self.gives_up().into_iter().map(str::to_owned).collect();
        let to = if earlier { at.checked_sub(1)? } else { at + 1 };
        if at >= order.len() || to >= order.len() {
            return None;
        }
        order.swap(at, to);
        let off_the_line: Vec<String> = self
            .narrow
            .drop
            .iter()
            .filter(|part| !order.contains(part))
            .cloned()
            .collect();
        order.extend(off_the_line);
        self.narrow.drop = order;
        Some(to)
    }

    fn side(&self, left: bool) -> &[String] {
        if left { &self.left } else { &self.right }
    }

    fn side_mut(&mut self, left: bool) -> &mut Vec<String> {
        if left {
            &mut self.left
        } else {
            &mut self.right
        }
    }

    fn toggle(&mut self, left: bool, part: &str) -> bool {
        let on_side = self.side(left).iter().any(|have| have == part);
        if on_side && part == MODE {
            return false;
        }
        if on_side {
            self.side_mut(left).retain(|have| have != part);
        } else {
            // A part sits on one side only.
            self.side_mut(!left).retain(|have| have != part);
            self.side_mut(left).push(part.to_owned());
        }
        true
    }

    fn shift(&mut self, left: bool, part: &str, earlier: bool) -> bool {
        let side = self.side_mut(left);
        let Some(at) = side.iter().position(|have| have == part) else {
            return false;
        };
        let to = if earlier {
            at.checked_sub(1)
        } else {
            Some(at + 1)
        };
        match to {
            Some(to) if to < side.len() => {
                side.swap(at, to);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::bottom_line::PARTS;
    use crate::settings::mods::builtin;

    fn names(parts: &[String]) -> Vec<&str> {
        parts.iter().map(String::as_str).collect()
    }

    #[test]
    fn the_built_in_file_is_the_line_as_it_ships() {
        let text = builtin::builtin(builtin::BOTTOM_LINE).unwrap().text;
        assert_eq!(read(text).unwrap(), BottomLineSettings::default());
        assert_eq!(read("").unwrap(), BottomLineSettings::default());
    }

    #[test]
    fn the_page_offers_every_part_the_line_knows() {
        for part in PARTS {
            assert!(
                LEFT_OFFERS.contains(&part) || RIGHT_OFFERS.contains(&part),
                "{part}"
            );
        }
    }

    #[test]
    fn every_choice_is_listed_once_with_the_default_first() {
        fn check<T: Choice + Default + std::fmt::Debug>(count: usize) {
            assert_eq!(T::ALL.len(), count);
            assert_eq!(T::ALL[0], T::default());
            for (at, value) in T::ALL.iter().enumerate() {
                assert_eq!(T::ALL.iter().position(|have| have == value), Some(at));
            }
        }
        check::<LineForm>(5);
        check::<PlanUsageShow>(3);
        check::<PlanUsageWindows>(4);
        check::<PlanUsageAs>(2);
        check::<PlanUsageResets>(3);
    }

    #[test]
    fn a_file_that_doesnt_read_says_which_key_and_why() {
        let problem = |text: &str| read(text).unwrap_err();
        assert!(
            problem("form = \"wide\"").starts_with("form: unknown variant `wide`"),
            "{}",
            problem("form = \"wide\"")
        );
        assert!(
            problem("colour = 1").starts_with("unknown field `colour`"),
            "{}",
            problem("colour = 1")
        );
        assert!(
            problem("[plan-usage]\nas = \"percent\"").starts_with("plan-usage.as: "),
            "{}",
            problem("[plan-usage]\nas = \"percent\"")
        );
        assert!(!problem("form = ").is_empty());
        // The line puts the mode back and keeps a part's first place, so these read.
        let tidied = read("left = [\"branch\"]\nright = [\"branch\", \"spend\"]").unwrap();
        assert_eq!(names(&tidied.left), ["mode", "branch"]);
        assert_eq!(names(&tidied.right), ["spend"]);
    }

    #[test]
    fn mode_moves_and_never_goes() {
        let mut file = BottomLineSettings::default();
        assert!(!file.toggle(true, MODE));
        assert_eq!(names(&file.left), ["mode", "artifact"]);
        assert!(file.toggle(false, MODE));
        assert_eq!(names(&file.left), ["artifact"]);
        assert_eq!(names(&file.right), ["plan-usage", "mode"]);
    }

    #[test]
    fn a_part_ticks_on_and_off_and_changes_place() {
        let mut file = BottomLineSettings::default();
        assert!(file.toggle(true, "branch"));
        assert_eq!(names(&file.left), ["mode", "artifact", "branch"]);
        assert!(file.shift(true, "branch", true));
        assert_eq!(names(&file.left), ["mode", "branch", "artifact"]);
        assert!(!file.shift(true, "mode", true));
        assert!(!file.shift(true, "artifact", false));
        assert!(file.toggle(true, "branch"));
        assert_eq!(names(&file.left), ["mode", "artifact"]);
        // Ticked on the other side, it crosses over.
        assert!(file.toggle(false, "spend"));
        assert_eq!(names(&file.right), ["plan-usage", "spend"]);
        assert!(file.toggle(true, "spend"));
        assert_eq!(names(&file.right), ["plan-usage"]);
        assert_eq!(names(&file.left), ["mode", "artifact", "spend"]);
    }

    #[test]
    fn a_narrow_terminal_gives_up_what_drop_names_first_then_the_rest_from_the_end() {
        let mut file = BottomLineSettings::default();
        // As it ships only the artifact can go: the mode and plan usage draw smaller.
        assert_eq!(file.gives_up(), ["artifact"]);
        for part in ["branch", "context"] {
            file.toggle(true, part);
        }
        file.toggle(false, "spend");
        assert_eq!(file.gives_up(), ["spend", "branch", "context", "artifact"]);
        // Ticking parts on and off leaves the order you wrote alone.
        assert_eq!(names(&file.narrow.drop), ["spend", "branch"]);
    }

    #[test]
    fn the_order_parts_are_given_up_in_is_yours_to_change() {
        let mut file = BottomLineSettings::default();
        file.toggle(true, "branch");
        assert_eq!(file.gives_up(), ["branch", "artifact"]);
        assert_eq!(file.shift_given_up(0, true), None);
        assert_eq!(file.shift_given_up(1, false), None);
        assert_eq!(file.shift_given_up(0, false), Some(1));
        assert_eq!(file.gives_up(), ["artifact", "branch"]);
        // Spend isn't on the line, and keeps its turn for when it is.
        assert_eq!(names(&file.narrow.drop), ["artifact", "branch", "spend"]);
        file.toggle(false, "spend");
        assert_eq!(file.gives_up(), ["artifact", "branch", "spend"]);
    }

    #[test]
    fn only_what_changed_is_written_in_the_file_s_order() {
        let before = BottomLineSettings::default();
        let mut after = before.clone();
        assert!(after.changes_from(&before).is_empty());
        after.plan_usage.r#as = PlanUsageAs::Left;
        after.toggle(false, "context");
        after.form = LineForm::Compact;
        assert_eq!(
            after.changes_from(&before),
            [
                ("form".to_owned(), "\"compact\"".to_owned()),
                (
                    "right".to_owned(),
                    "[\"plan-usage\", \"context\"]".to_owned()
                ),
                ("plan-usage.as".to_owned(), "\"left\"".to_owned()),
            ]
        );
    }

    /// The page writes key by key, so every key the line reads needs its place in the order,
    /// and a file with every key changed must read back as the page had it.
    #[test]
    fn every_key_the_line_reads_can_be_written_and_reads_back() {
        let before = BottomLineSettings::default();
        let keys: Vec<String> = leaves(&before).into_iter().map(|(key, _)| key).collect();
        for key in &keys {
            assert!(KEY_ORDER.contains(&key.as_str()), "{key}");
        }
        assert_eq!(keys.len(), KEY_ORDER.len());

        let mut after = before.clone();
        after.form = LineForm::Gauges;
        after.toggle(true, "branch");
        after.toggle(false, "spend");
        after.plan_usage.show = PlanUsageShow::NearLimit;
        after.plan_usage.windows = PlanUsageWindows::SevenDays;
        after.plan_usage.r#as = PlanUsageAs::Left;
        after.plan_usage.resets = PlanUsageResets::Never;
        after.narrow.drop = vec!["branch".to_owned()];
        let pairs = after.changes_from(&before);
        assert_eq!(
            pairs
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            KEY_ORDER
        );
        let mut text = String::new();
        let mut table = "";
        for (key, value) in &pairs {
            let (section, leaf) = key.split_once('.').unwrap_or(("", key));
            if section != table {
                text.push_str(&format!("[{section}]\n"));
                table = section;
            }
            text.push_str(&format!("{leaf} = {value}\n"));
        }
        assert_eq!(read(&text).unwrap(), after, "{text}");
    }
}
