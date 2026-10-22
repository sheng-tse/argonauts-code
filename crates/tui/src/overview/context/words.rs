//! The Context tab's wording: Claude Code's own names for where things come from, its
//! shares of the window, and how each of its colours draws.

use ratatui::style::{Color, Style};
use time::OffsetDateTime;

use crate::format::ClockTime;
use crate::kit::Kit;
use crate::theme::Role;
use crate::theme::{Brightness, downgrade};

/// A share of the window in tenths of a percent, as Claude Code's legend writes it, and
/// `<0.1%` where the tenths would read `0.0%` for something that does take room.
pub(super) fn share(tokens: u64, of: u64) -> String {
    if of == 0 {
        return String::new();
    }
    let tenths = format!("{:.1}%", tokens as f64 / of as f64 * 100.0);
    if tenths == "0.0%" && tokens > 0 {
        "<0.1%".to_owned()
    } else {
        tenths
    }
}

/// `3 files`, `1 file`.
pub(super) fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// `3:02 AM` for a time today, else with its day as [`day_and_time`] writes it, in the zone
/// `now` is in.
pub(super) fn time_of(at: OffsetDateTime, now: OffsetDateTime) -> String {
    let local = at.to_offset(now.offset());
    if local.date() == now.date() {
        ClockTime::from(local).to_string()
    } else {
        day_and_time(at, now)
    }
}

/// `today at 1:52 AM`, `yesterday at 1:52 AM`, then the day, `Oct 8 at 1:52 AM`, with its year
/// when that isn't this one, in the zone `now` is in.
fn day_and_time(at: OffsetDateTime, now: OffsetDateTime) -> String {
    let at = at.to_offset(now.offset());
    let time = ClockTime::from(at);
    let (day, today) = (at.date(), now.date());
    if day == today {
        return format!("today at {time}");
    }
    if today.previous_day() == Some(day) {
        return format!("yesterday at {time}");
    }
    let month = &day.month().to_string()[..3];
    if day.year() == today.year() {
        format!("{month} {} at {time}", day.day())
    } else {
        format!("{month} {}, {} at {time}", day.day(), day.year())
    }
}

/// The order Claude Code lists its source groups in. A plugin's own group follows `Plugin`,
/// and a source without a place here comes after them all.
const GROUP_ORDER: [&str; 7] = [
    "Project",
    "User",
    "claude.ai sync",
    "Managed",
    "Plugin",
    "MCP",
    "Built-in",
];

/// Claude Code's label for where an agent or a skill comes from, with the plugin's name
/// where it has one. A source it has no label for keeps its own name.
pub(super) fn source_label(source: &str, plugin: Option<&str>) -> String {
    let label = match source {
        "userSettings" => "User",
        "projectSettings" => "Project",
        "localSettings" => "Local",
        "flagSettings" => "Flag",
        "policySettings" => "Managed",
        "plugin" => "Plugin",
        "built-in" => "Built-in",
        "mcp" => "MCP",
        "memoryStore" => "Memory store",
        "syncedSkills" => "claude.ai sync",
        other => other,
    };
    match plugin.map(str::trim).filter(|plugin| !plugin.is_empty()) {
        Some(plugin) => format!("{label} ({plugin})"),
        None => label.to_owned(),
    }
}

/// Items by source group, in Claude Code's group order, the largest first inside a group.
pub(super) fn grouped<T>(
    items: impl IntoIterator<Item = T>,
    label: impl Fn(&T) -> String,
    tokens: impl Fn(&T) -> u64,
) -> Vec<(String, Vec<T>)> {
    let mut groups: Vec<(String, Vec<T>)> = Vec::new();
    for item in items {
        let label = label(&item);
        match groups.iter_mut().find(|(known, _)| *known == label) {
            Some((_, group)) => group.push(item),
            None => groups.push((label, vec![item])),
        }
    }
    for (_, group) in &mut groups {
        group.sort_by_key(|item| std::cmp::Reverse(tokens(item)));
    }
    let mut ordered = Vec::with_capacity(groups.len());
    for base in GROUP_ORDER {
        if let Some(at) = groups.iter().position(|(label, _)| label == base) {
            ordered.push(groups.remove(at));
        }
        let own = format!("{base} (");
        let mut named: Vec<(String, Vec<T>)> = Vec::new();
        let mut at = 0;
        while at < groups.len() {
            if groups[at].0.starts_with(&own) {
                named.push(groups.remove(at));
            } else {
                at += 1;
            }
        }
        named.sort_by(|a, b| a.0.cmp(&b.0));
        ordered.extend(named);
    }
    ordered.extend(groups);
    ordered
}

/// How one of Claude Code's colours draws, by the name its reply gives a category: as its
/// `/context` draws it in a dark terminal, the system's two greys as one, and the theme's own
/// colours in a light one. A name this version doesn't know draws dim.
pub(super) fn cc_style(kit: &Kit<'_>, color: &str) -> Style {
    let (dark, light, role) = match color {
        "promptBorder" | "inactive" => (0x8e8e8e, 0x707070, Role::Dim),
        "green_FOR_SUBAGENTS_ONLY" | "success" => (0x5fd7af, 0x2f9e7a, Role::Success),
        "warning" => (0xd7af5f, 0x9a6f12, Role::Warn),
        "purple_FOR_SUBAGENTS_ONLY" | "permission" | "suggestion" => {
            (0xb1b9f9, 0x5769f7, Role::Question)
        }
        "cyan_FOR_SUBAGENTS_ONLY" => return kit.fg(Role::AgentCyan),
        "claude" => return kit.fg(Role::CliClaude),
        "blue_FOR_SUBAGENTS_ONLY" => return kit.fg(Role::AgentBlue),
        "red_FOR_SUBAGENTS_ONLY" | "error" => return kit.fg(Role::Danger),
        "yellow_FOR_SUBAGENTS_ONLY" => return kit.fg(Role::AgentYellow),
        "orange_FOR_SUBAGENTS_ONLY" => return kit.fg(Role::AgentOrange),
        "pink_FOR_SUBAGENTS_ONLY" => return kit.fg(Role::AgentPink),
        _ => return kit.fg(Role::Dim),
    };
    let hex: u32 = match kit.theme.brightness {
        Brightness::Dark => dark,
        Brightness::Light => light,
    };
    let rgb = Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
    downgrade(role, Style::new().fg(rgb), kit.theme.level)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_share_reads_in_tenths_and_never_as_nothing() {
        assert_eq!(share(6_888, 1_000_000), "0.7%");
        assert_eq!(share(719_981, 1_000_000), "72.0%");
        assert_eq!(share(412, 1_000_000), "<0.1%");
        assert_eq!(share(0, 1_000_000), "0.0%");
        assert_eq!(share(5, 0), "");
    }

    #[test]
    fn sources_read_as_claude_code_labels_them() {
        assert_eq!(source_label("projectSettings", None), "Project");
        assert_eq!(source_label("syncedSkills", None), "claude.ai sync");
        assert_eq!(source_label("plugin", Some("docs")), "Plugin (docs)");
        assert_eq!(source_label("policySettings", Some(" ")), "Managed");
        assert_eq!(source_label("somewhere-new", None), "somewhere-new");
    }

    #[test]
    fn groups_keep_claude_codes_order_with_the_largest_first() {
        let items = [
            ("simplify", "built-in", None, 50),
            ("lint", "plugin", Some("zeta"), 90),
            ("notes", "projectSettings", None, 128),
            ("review", "built-in", None, 249),
            ("fmt", "plugin", Some("alpha"), 40),
            ("deploy", "projectSettings", None, 181),
            ("odd", "somewhere-new", None, 7),
            ("style", "userSettings", None, 102),
        ];
        let groups = grouped(items, |item| source_label(item.1, item.2), |item| item.3);
        let read: Vec<(&str, Vec<&str>)> = groups
            .iter()
            .map(|(label, group)| {
                (
                    label.as_str(),
                    group.iter().map(|item| item.0).collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(
            read,
            [
                ("Project", vec!["deploy", "notes"]),
                ("User", vec!["style"]),
                ("Plugin (alpha)", vec!["fmt"]),
                ("Plugin (zeta)", vec!["lint"]),
                ("Built-in", vec!["review", "simplify"]),
                ("somewhere-new", vec!["odd"]),
            ]
        );
    }

    /// A time today reads as the clock, and another day says which.
    #[test]
    fn a_time_says_its_day_when_it_isnt_today() {
        use time::macros::datetime;
        let now = datetime!(2026-10-15 00:35 UTC);
        assert_eq!(time_of(datetime!(2026-10-15 00:10 UTC), now), "12:10 AM");
        assert_eq!(
            time_of(datetime!(2026-10-14 13:52 UTC), now),
            "yesterday at 1:52 PM"
        );
        assert_eq!(
            day_and_time(datetime!(2026-10-08 09:05 UTC), now),
            "Oct 8 at 9:05 AM"
        );
        assert_eq!(
            day_and_time(datetime!(2025-12-31 23:00 UTC), now),
            "Dec 31, 2025 at 11:00 PM"
        );
    }

    /// Claude Code's category colours draw as its `/context` draws them, and fit a terminal
    /// with fewer colours.
    #[test]
    fn claude_codes_colours_draw_as_its_context_draws_them() {
        use crate::theme::{ColorLevel, Theme};
        let theme = Theme::dark().with_level(ColorLevel::TrueColor);
        let kit = Kit::new(&theme);
        let fg = |name| cc_style(&kit, name).fg;
        assert_eq!(fg("inactive"), Some(Color::Rgb(0x8e, 0x8e, 0x8e)));
        assert_eq!(fg("promptBorder"), fg("inactive"));
        assert_eq!(
            fg("green_FOR_SUBAGENTS_ONLY"),
            Some(Color::Rgb(0x5f, 0xd7, 0xaf))
        );
        assert_eq!(fg("warning"), Some(Color::Rgb(0xd7, 0xaf, 0x5f)));
        assert_eq!(
            fg("purple_FOR_SUBAGENTS_ONLY"),
            Some(Color::Rgb(0xb1, 0xb9, 0xf9))
        );
        assert_eq!(cc_style(&kit, "a-colour-from-later"), kit.fg(Role::Dim));
        let fewer = Theme::dark().with_level(ColorLevel::Ansi256);
        let kit = Kit::new(&fewer);
        assert!(matches!(
            cc_style(&kit, "warning").fg,
            Some(Color::Indexed(_))
        ));
    }
}
