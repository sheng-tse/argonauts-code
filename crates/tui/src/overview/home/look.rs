//! How the Overview writes its numbers and picks its colours: big token counts, amounts that
//! share one unit, dollars with thousands, dates, each CLI's word and colour, and each model's
//! shade of its CLI's colour.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::overview::OverviewData;
use ratatui::style::Style;
use time::{Date, Month};

use crate::format::thousands;
use crate::kit::Kit;
use crate::overview::cards::cli_role;
use crate::overview::shades::{ModelColors, shade};
use crate::theme::{Role, Theme};

/// `812`, `12.3k`, `4.5M`, `118.4b`, `3b`: one decimal, dropped when it's nought.
pub fn big(count: u64) -> String {
    let amount = amount(count);
    match amount.strip_suffix(".0b") {
        Some(whole) => format!("{whole}b"),
        None => amount.replace(".0M", "M").replace(".0k", "k"),
    }
}

/// As [`big`], but a nought decimal stays, so a column of amounts lines up: `26.0b`.
pub fn amount(count: u64) -> String {
    let (value, unit) = match count {
        0..1_000 => return count.to_string(),
        1_000..999_950 => (count as f64 / 1e3, "k"),
        999_950..999_950_000 => (count as f64 / 1e6, "M"),
        _ => (count as f64 / 1e9, "b"),
    };
    format!("{value:.1}{unit}")
}

/// The unit a group of amounts is written in: the one its largest reads best in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Unit {
    size: f64,
    letter: &'static str,
}

impl Unit {
    /// The unit for amounts up to `largest`.
    pub fn for_largest(largest: u64) -> Unit {
        let (size, letter) = match largest {
            0..1_000 => (1.0, ""),
            1_000..999_950 => (1e3, "k"),
            999_950..999_950_000 => (1e6, "M"),
            _ => (1e9, "b"),
        };
        Unit { size, letter }
    }

    /// `110.0b`, `0.4b`, and a sliver to two places, `0.05b`, so it never reads as nought.
    pub fn write(self, count: u64) -> String {
        if self.letter.is_empty() {
            return count.to_string();
        }
        if count == 0 {
            return format!("0{}", self.letter);
        }
        let value = count as f64 / self.size;
        let written = if value >= 0.095 {
            format!("{value:.1}")
        } else if value >= 0.005 {
            format!("{value:.2}")
        } else {
            "<0.01".to_owned()
        };
        format!("{written}{}", self.letter)
    }
}

/// `~$9,640`, `~$60`, `~$4.20`: whole dollars from ten, with thousands marked. The value is
/// always a reckoning at list prices, so it carries the `~`.
pub fn dollars(usd: f64) -> String {
    if usd < 10.0 {
        return format!("~${usd:.2}");
    }
    format!("~${}", thousands(usd.round() as u64))
}

/// The share of `part` in `whole` as whole percents, `<1%` for a sliver.
pub fn share(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "0%".to_owned();
    }
    let percent = part as f64 / whole as f64 * 100.0;
    if part > 0 && percent < 1.0 {
        return "<1%".to_owned();
    }
    format!("{percent:.0}%")
}

/// The share of `part` in `whole` to a tenth of a percent, as Claude Code's Models tab writes
/// it: `46.0%`.
pub fn share_tenths(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "0.0%".to_owned();
    }
    format!("{:.1}%", part as f64 / whole as f64 * 100.0)
}

/// `57d 3h`, `2h 5m`, `14m`, `40s`: the two largest parts of a span.
pub fn duration(secs: u64) -> String {
    let (days, hours, minutes) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{secs}s")
    }
}

/// `18 days`, `1 day`.
pub fn days(n: u32) -> String {
    match n {
        1 => "1 day".to_owned(),
        n => format!("{n} days"),
    }
}

pub fn month_word(month: Month) -> &'static str {
    match month {
        Month::January => "Jan",
        Month::February => "Feb",
        Month::March => "Mar",
        Month::April => "Apr",
        Month::May => "May",
        Month::June => "Jun",
        Month::July => "Jul",
        Month::August => "Aug",
        Month::September => "Sep",
        Month::October => "Oct",
        Month::November => "Nov",
        Month::December => "Dec",
    }
}

/// `Aug 28`.
pub fn short_date(day: Date) -> String {
    format!("{} {}", month_word(day.month()), day.day())
}

/// `Oct 2025` for another year than `today`'s, else `Feb`.
pub fn since_word(day: Date, today: Date) -> String {
    if day.year() == today.year() {
        month_word(day.month()).to_owned()
    } else {
        format!("{} {}", month_word(day.month()), day.year())
    }
}

/// How a CLI is named on the Overview: Antigravity CLI is `agy`, as its command is.
pub fn cli_word(names: &DisplayNames, agent: &str) -> String {
    match agent {
        "antigravity-cli" => "agy".to_owned(),
        _ => names.cli(agent),
    }
}

/// A CLI where room is short, as in a legend: `Claude`, `Codex`, `agy`.
pub fn cli_short(names: &DisplayNames, agent: &str) -> String {
    match agent {
        "claude-code" => "Claude".to_owned(),
        _ => cli_word(names, agent),
    }
}

/// An hour of the day on a 12-hour clock: `9pm`, `12am`.
pub fn hour_word(hour: usize) -> String {
    let half = if hour % 24 < 12 { "am" } else { "pm" };
    match hour % 12 {
        0 => format!("12{half}"),
        hour => format!("{hour}{half}"),
    }
}

/// `Opus 5.5`, or the CLI's own word when no model is named.
pub fn model_word(names: &DisplayNames, agent: &str, model: Option<&str>) -> String {
    names
        .model(agent, model)
        .unwrap_or_else(|| format!("{} default", cli_word(names, agent)))
}

/// The colour a CLI keeps everywhere: its own for the three it knows, else its agent colour.
fn cli_colour(agent: &str) -> Role {
    match Role::for_cli(agent) {
        Role::CatOther => cli_role(agent),
        role => role,
    }
}

/// Each CLI's colour as text, from the theme even where the palette has none.
pub fn cli_style(kit: &Kit<'_>, agent: &str) -> Style {
    let role = cli_colour(agent);
    let own = kit.fg(role);
    if own.fg.is_some() {
        return own;
    }
    match Theme::for_brightness(kit.theme.brightness).color(role) {
        Some(color) if kit.has_color() => Style::new().fg(color),
        _ => own,
    }
}

/// The colour a model draws in: its CLI's own for the model that has run the most on it,
/// then the CLI's other shades, whatever the period.
pub fn model_style(
    kit: &Kit<'_>,
    shades: &ModelColors<'_>,
    agent: &str,
    model: Option<&str>,
) -> Style {
    match shades.place(agent, model) {
        0 => cli_style(kit, agent),
        nth => shade(kit, cli_colour(agent), nth),
    }
}

/// Which shade each of the Overview's models takes.
pub fn model_colors<'a>(data: &OverviewData, names: &'a DisplayNames) -> ModelColors<'a> {
    let shown = data.models.iter();
    ModelColors::new(&data.all_time_models, names)
        .with(shown.map(|model| (model.agent.as_str(), model.model.as_deref())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_numbers_read_short() {
        assert_eq!(big(812), "812");
        assert_eq!(big(12_340), "12.3k");
        assert_eq!(big(4_500_000), "4.5M");
        assert_eq!(big(118_400_000_000), "118.4b");
        assert_eq!(big(3_000_000_000), "3b");
        assert_eq!(amount(26_000_000_000), "26.0b");
        assert_eq!(dollars(60.0), "~$60");
        assert_eq!(dollars(9_640.4), "~$9,640");
        assert_eq!(dollars(4.2), "~$4.20");
        assert_eq!(share(1, 1_000), "<1%");
        assert_eq!(share(62, 100), "62%");
        assert_eq!(share_tenths(46, 100), "46.0%");
    }

    #[test]
    fn a_group_of_amounts_shares_the_unit_of_its_largest() {
        let unit = Unit::for_largest(110_000_000_000);
        assert_eq!(unit.write(110_000_000_000), "110.0b");
        assert_eq!(unit.write(400_000_000), "0.4b");
        assert_eq!(unit.write(50_000_000), "0.05b");
        assert_eq!(unit.write(1_000_000), "<0.01b");
        assert_eq!(unit.write(0), "0b");
        let small = Unit::for_largest(900);
        assert_eq!(small.write(12), "12");
        assert_eq!(Unit::for_largest(340_000_000).write(1_200_000), "1.2M");
    }

    #[test]
    fn spans_read_as_their_two_largest_parts() {
        assert_eq!(duration(57 * 86_400 + 3 * 3600 + 59), "57d 3h");
        assert_eq!(duration(2 * 3600 + 5 * 60), "2h 5m");
        assert_eq!(duration(14 * 60), "14m");
        assert_eq!(duration(40), "40s");
    }
}
