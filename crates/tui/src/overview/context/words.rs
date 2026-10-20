//! The Context tab's wording: Claude Code's own names for where things come from, its
//! rounding of small counts, and which of the theme's colours draws each of its colours.

use argonauts_core::Spend;
use argonauts_core::views::RecordedContext;
use time::OffsetDateTime;

use crate::format::{self, ClockTime};
use crate::theme::Role;

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

/// A whole percent, rounded half up as Claude Code's suggestions round theirs.
pub(super) fn whole_percent(tokens: u64, of: u64) -> u64 {
    if of == 0 {
        return 0;
    }
    (tokens as f64 / of as f64 * 100.0 + 0.5).floor() as u64
}

/// A skill's tokens as Claude Code lists them: an estimate to the nearest ten, or `< 20`.
pub(super) fn skill_tokens(tokens: u64) -> String {
    if tokens < 20 {
        return "< 20".to_owned();
    }
    format!("~{}", crate::format::tokens((tokens + 5) / 10 * 10))
}

/// `3 files`, `1 file`.
pub(super) fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// `as its last answer left it, yesterday at 1:52 AM · 1 turn · 73.7k in, 916 out · $0.0040`,
/// for a session read from its conversation's record.
pub(super) fn as_left(recorded: &RecordedContext, spent: &Spend, now: OffsetDateTime) -> String {
    let mut parts = vec![
        format!(
            "as its last answer left it, {}",
            day_and_time(recorded.at, now)
        ),
        count(recorded.turns as usize, "turn"),
    ];
    if spent.total_tokens() > 0 {
        parts.push(format!(
            "{} in, {} out",
            format::tokens(spent.input_tokens),
            format::tokens(spent.output_tokens)
        ));
    }
    if spent.cost_usd > 0.0 || spent.cost_partial {
        parts.push(cost(spent));
    }
    parts.join(" \u{b7} ")
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

/// A cost as Claude Code writes one: to four places under half a dollar, `$0.0040`, and to the
/// cent from there, marked `~` where it is worked out and `+` where some of it is missing.
fn cost(spent: &Spend) -> String {
    let usd = spent.cost_usd;
    let amount = if usd > 0.5 {
        format!("${:.2}", (usd * 100.0).round() / 100.0)
    } else {
        format!("${usd:.4}")
    };
    let estimated = if spent.cost_estimated { "~" } else { "" };
    let partial = if spent.cost_partial { "+" } else { "" };
    format!("{estimated}{amount}{partial}")
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

/// A tool as the lists name it: an MCP tool without the `mcp__<server>__` its server's name
/// already says.
pub(super) fn tool_name(name: &str) -> &str {
    name.strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .map(|(_, tool)| tool)
        .filter(|tool| !tool.is_empty())
        .unwrap_or(name)
}

/// A row out of the window by what it is: `System tools (deferred)` reads `system tools`.
pub(super) fn deferred_name(name: &str) -> String {
    let name = name.strip_suffix(" (deferred)").unwrap_or(name).trim();
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        // An acronym keeps its capitals.
        (Some(first), Some(second)) if second.is_lowercase() => {
            format!("{}{}{}", first.to_lowercase(), second, chars.as_str())
        }
        _ => name.to_owned(),
    }
}

/// The theme colour that draws one of Claude Code's, by the name its reply gives a row. Its
/// two greys read as one here, and a name this version doesn't know draws dim.
pub(super) fn color_role(color: &str) -> Role {
    match color {
        "cyan_FOR_SUBAGENTS_ONLY" => Role::AgentCyan,
        "green_FOR_SUBAGENTS_ONLY" | "success" => Role::Success,
        "permission" | "suggestion" => Role::Question,
        "claude" => Role::CliClaude,
        "warning" => Role::Warn,
        "purple_FOR_SUBAGENTS_ONLY" => MESSAGES,
        "blue_FOR_SUBAGENTS_ONLY" => Role::AgentBlue,
        "red_FOR_SUBAGENTS_ONLY" | "error" => Role::Danger,
        "yellow_FOR_SUBAGENTS_ONLY" => Role::AgentYellow,
        "orange_FOR_SUBAGENTS_ONLY" => Role::AgentOrange,
        "pink_FOR_SUBAGENTS_ONLY" => Role::AgentPink,
        _ => Role::Dim,
    }
}

/// The colour of the messages, on the grid and on the bars that split them up.
pub(super) const MESSAGES: Role = Role::Label;

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
    fn a_whole_percent_rounds_half_up() {
        assert_eq!(whole_percent(180_200, 1_000_000), 18);
        assert_eq!(whole_percent(199_000, 1_000_000), 20);
        assert_eq!(whole_percent(25, 1_000), 3);
    }

    #[test]
    fn a_skill_reads_as_an_estimate() {
        assert_eq!(skill_tokens(181), "~180");
        assert_eq!(skill_tokens(128), "~130");
        assert_eq!(skill_tokens(125), "~130");
        assert_eq!(skill_tokens(19), "< 20");
        assert_eq!(skill_tokens(20), "~20");
        assert_eq!(skill_tokens(2_204), "~2.2k");
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

    #[test]
    fn an_mcp_tool_drops_what_its_server_says() {
        assert_eq!(tool_name("mcp__linear__create_issue"), "create_issue");
        assert_eq!(
            tool_name("mcp__argonauts__argonauts_lookup"),
            "argonauts_lookup"
        );
        assert_eq!(tool_name("Bash"), "Bash");
        assert_eq!(tool_name("mcp__odd"), "mcp__odd");
    }

    #[test]
    fn a_row_out_of_the_window_reads_as_what_it_is() {
        assert_eq!(deferred_name("System tools (deferred)"), "system tools");
        assert_eq!(deferred_name("MCP tools (deferred)"), "MCP tools");
    }

    #[test]
    fn claude_codes_colours_draw_in_the_themes() {
        assert_eq!(color_role("promptBorder"), Role::Dim);
        assert_eq!(color_role("inactive"), Role::Dim);
        assert_eq!(color_role("permission"), Role::Question);
        assert_eq!(color_role("purple_FOR_SUBAGENTS_ONLY"), MESSAGES);
        assert_eq!(color_role("a-colour-from-later"), Role::Dim);
    }

    /// The day a record's answer ended, said from now, and its cost as Claude Code writes one.
    #[test]
    fn a_recorded_session_says_when_and_what_it_cost() {
        use time::macros::datetime;
        let now = datetime!(2026-10-15 00:35 UTC);
        assert_eq!(
            day_and_time(datetime!(2026-10-15 00:10 UTC), now),
            "today at 12:10 AM"
        );
        assert_eq!(
            day_and_time(datetime!(2026-10-14 13:52 UTC), now),
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
        let spent = |cost_usd: f64, cost_estimated: bool| Spend {
            cost_usd,
            cost_estimated,
            ..Spend::default()
        };
        assert_eq!(cost(&spent(0.004_032_7, false)), "$0.0040");
        assert_eq!(cost(&spent(0.5, false)), "$0.5000");
        assert_eq!(cost(&spent(1.256, true)), "~$1.26");
        let recorded = RecordedContext {
            at: datetime!(2026-10-14 01:52 UTC),
            turns: 3,
        };
        assert_eq!(
            as_left(&recorded, &Spend::default(), now),
            "as its last answer left it, yesterday at 1:52 AM \u{b7} 3 turns"
        );
    }
}
