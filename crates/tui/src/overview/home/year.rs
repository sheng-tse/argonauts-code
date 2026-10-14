//! The year of days, each in the ramp of the CLI that ran most that day (or of its model, or
//! one colour), and beside it the hours of the day and each CLI's share.

use std::collections::BTreeMap;

use argonauts_core::display_names::DisplayNames;
use argonauts_core::overview::{OverviewData, OverviewDay};
use argonauts_core::settings::overview::YearColors;
use ratatui::style::Style;
use ratatui::text::Span;
use time::{Date, Duration, Month};

use super::look::{self, cli_style, cli_word, empty_day, model_style, ramp};
use crate::overview::stats::draw;
use crate::text::spans_width;
use crate::theme::Role;
use crate::widgets::{Drawn, Kit, merge};

/// Weeks the year shows, so a full year of days fits whatever weekday today is.
pub const WEEKS: usize = 53;
/// Cells left of the days for their names.
const DAY_NAMES: usize = 4;
/// The year's width: the day names and a cell a week.
pub const YEAR_WIDTH: usize = DAY_NAMES + WEEKS;

/// What the year draws with.
pub struct Year<'a> {
    pub data: &'a OverviewData,
    pub names: &'a DisplayNames,
    pub colors: YearColors,
}

impl Year<'_> {
    /// The months over the weeks, seven rows of days Sunday first, and the legend under them
    /// in `width` cells.
    pub fn draw(&self, kit: &Kit<'_>, width: usize) -> Drawn {
        let today = self.data.today;
        let first = first_sunday(today);
        let by_day: BTreeMap<Date, &OverviewDay> = self
            .data
            .days
            .iter()
            .filter(|day| day.day >= first && day.day <= today)
            .map(|day| (day.day, day))
            .collect();
        let cuts = quartiles(by_day.values().map(|day| day.tokens).collect());
        let mut drawn = Drawn::new();
        drawn.push(self.months(kit, first));
        for row in 0..7 {
            let name = match row {
                1 => "Mon",
                3 => "Wed",
                5 => "Fri",
                _ => "",
            };
            let mut line = vec![kit.dim(format!("{name:<width$}", width = DAY_NAMES))];
            for week in 0..WEEKS {
                let day = first + Duration::days((week * 7 + row) as i64);
                if day > today {
                    line.push(Span::raw(" "));
                    continue;
                }
                line.push(match by_day.get(&day) {
                    Some(day) => self.cell(kit, day, level(day.tokens, &cuts)),
                    None => self.empty(kit),
                });
            }
            drawn.push(merge(line));
        }
        for row in self.legend(kit, &by_day, width) {
            drawn.push(row);
        }
        drawn
    }

    /// `Oct`, `Dec`, `Feb`: every other month's name over the week it starts in.
    fn months(&self, kit: &Kit<'_>, first: Date) -> Vec<Span<'static>> {
        let mut row = " ".repeat(DAY_NAMES);
        let mut last_month = None;
        let mut shown = 0;
        for week in 0..WEEKS {
            let sunday = first + Duration::days((week * 7) as i64);
            let month = (sunday + Duration::days(6)).month();
            if last_month == Some(month) {
                continue;
            }
            let fresh = last_month.is_some();
            last_month = Some(month);
            if !fresh {
                continue;
            }
            shown += 1;
            let at = DAY_NAMES + week;
            if shown % 2 == 1 && row.chars().count() <= at {
                row.push_str(&" ".repeat(at - row.chars().count()));
                row.push_str(month_word(month));
            }
        }
        vec![kit.dim(row)]
    }

    fn cell(&self, kit: &Kit<'_>, day: &OverviewDay, level: usize) -> Span<'static> {
        if !kit.has_color() || kit.glyphs.halves.is_none() {
            return Span::raw(["░", "▒", "▓", "█"][level - 1]);
        }
        let full = match self.colors {
            YearColors::Cli => cli_style(kit, &day.agent),
            YearColors::Model => model_style(kit, self.data, &day.agent, day.model.as_deref()),
            YearColors::One => kit.fg(Role::Accent),
        };
        let shaded = ramp(kit, full, level);
        // A palette without steps tells the amounts apart by the glyph.
        let glyph = if shaded == full && level < 4 {
            ["░", "▒", "▓", "█"][level - 1]
        } else {
            "■"
        };
        Span::styled(glyph, shaded)
    }

    fn empty(&self, kit: &Kit<'_>) -> Span<'static> {
        if !kit.has_color() || kit.glyphs.halves.is_none() {
            return Span::raw("·");
        }
        Span::styled("■", empty_day(kit))
    }

    /// Each CLI's ramp and name, then what the colours mean and the busiest day.
    fn legend(
        &self,
        kit: &Kit<'_>,
        by_day: &BTreeMap<Date, &OverviewDay>,
        width: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let mut entries: Vec<Vec<Span<'static>>> = Vec::new();
        if self.colors != YearColors::One {
            for (agent, _) in self.data.by_cli() {
                let full = cli_style(kit, &agent);
                let mut entry: Vec<Span<'static>> = (1..=4)
                    .map(|level| self.swatch(kit, ramp(kit, full, level), level))
                    .collect();
                entry.push(Span::raw(" "));
                entry.push(kit.dim(cli_word(self.names, &agent)));
                entries.push(merge(entry));
            }
        }
        let what = match self.colors {
            YearColors::Cli => "each day in its main CLI's colour, darker for less",
            YearColors::Model => "each day in its main model's shade, darker for less",
            YearColors::One => "darker for less",
        };
        let busiest = by_day
            .values()
            .max_by_key(|day| (day.tokens, std::cmp::Reverse(day.day)))
            .map(|day| format!(" · busiest {}", short_date(day.day)))
            .unwrap_or_default();
        entries.push(vec![kit.dim(format!("{what}{busiest}"))]);
        flow(entries, width, 3)
    }

    fn swatch(&self, kit: &Kit<'_>, style: Style, level: usize) -> Span<'static> {
        if !kit.has_color() || kit.glyphs.halves.is_none() {
            return Span::raw(["░", "▒", "▓", "█"][level - 1]);
        }
        Span::styled("■", style)
    }
}

/// Entries one after another, `gap` cells apart, onto as many rows of `width` as they need.
pub fn flow(entries: Vec<Vec<Span<'static>>>, width: usize, gap: usize) -> Vec<Vec<Span<'static>>> {
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    for entry in entries {
        let used = spans_width(&row);
        let wide = spans_width(&entry);
        if !row.is_empty() && used + gap + wide > width {
            rows.push(std::mem::take(&mut row));
        }
        if !row.is_empty() {
            row.push(Span::raw(" ".repeat(gap)));
        }
        row.extend(entry);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

/// The Sunday that starts the year's first week, so today sits in the last.
fn first_sunday(today: Date) -> Date {
    let back = i64::from(today.weekday().number_days_from_sunday());
    today - Duration::days(back + 7 * (WEEKS as i64 - 1))
}

/// The amounts that split the days with any into four even groups.
fn quartiles(mut amounts: Vec<u64>) -> [u64; 3] {
    amounts.sort_unstable();
    if amounts.is_empty() {
        return [0; 3];
    }
    let at = |q: usize| amounts[(amounts.len() * q / 4).min(amounts.len() - 1)];
    [at(1), at(2), at(3)]
}

/// 1 for the quietest quarter of the days, 4 for the busiest.
fn level(tokens: u64, cuts: &[u64; 3]) -> usize {
    1 + cuts.iter().filter(|cut| tokens > **cut).count()
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

/// `9pm`, `12am`.
pub fn hour_word(hour: usize) -> String {
    match hour {
        0 => "12am".to_owned(),
        1..=11 => format!("{hour}am"),
        12 => "12pm".to_owned(),
        _ => format!("{}pm", hour - 12),
    }
}

/// The hours of the day as columns `width` cells wide, with their times under them, then each
/// CLI's share of the tokens as one bar and its legend.
pub fn hours_and_share(
    kit: &Kit<'_>,
    data: &OverviewData,
    names: &DisplayNames,
    width: usize,
) -> Drawn {
    let mut drawn = Drawn::new();
    let hours = &data.hours;
    let busiest = (0..hours.len()).max_by_key(|&hour| (hours[hour], std::cmp::Reverse(hour)));
    let mut title = "by hour of day".to_owned();
    if let Some(hour) = busiest.filter(|&hour| hours[hour] > 0) {
        title.push_str(&format!(" · busiest {}", hour_word(hour)));
    }
    drawn.push(vec![kit.dim(title)]);
    let per = if width >= 48 { 2 } else { 1 };
    let top = hours.iter().copied().max().unwrap_or(0).max(1);
    let rows = 3;
    let style = kit.fg(Role::Accent);
    for row in (0..rows).rev() {
        let mut line = Vec::new();
        for &amount in hours.iter().take(24) {
            let eighths = (amount as f64 / top as f64 * (rows * 8) as f64).round() as usize;
            let here = eighths.saturating_sub(row * 8).min(8);
            let glyph = match here {
                0 if row == 0 && amount > 0 => kit.glyphs.columns[0],
                0 => " ",
                n => kit.glyphs.columns[n - 1],
            };
            line.push(Span::styled(glyph.repeat(per), style));
        }
        drawn.push(merge(line));
    }
    let mut times = String::new();
    for (hour, word) in [(0, "12am"), (6, "6am"), (12, "12pm"), (18, "6pm")] {
        let at = hour * per;
        if times.chars().count() <= at {
            times.push_str(&" ".repeat(at - times.chars().count()));
            times.push_str(word);
        }
    }
    drawn.push(vec![kit.dim(times)]);
    drawn.push(vec![kit.dim("by CLI")]);
    let clis = data.by_cli();
    let whole: u64 = clis.iter().map(|(_, tokens)| tokens).sum();
    let parts: Vec<(u64, Style)> = clis
        .iter()
        .map(|(agent, tokens)| (*tokens, cli_style(kit, agent)))
        .collect();
    drawn.push(draw::bar(kit, &parts, whole, width));
    let entries = clis
        .iter()
        .map(|(agent, tokens)| {
            vec![
                swatch(kit, cli_style(kit, agent)),
                Span::raw(format!(
                    "{} {}",
                    cli_word(names, agent),
                    look::share(*tokens, whole)
                )),
            ]
        })
        .collect();
    for row in flow(entries, width, 3) {
        drawn.push(row);
    }
    drawn
}

/// A legend's colour mark.
pub fn swatch(kit: &Kit<'_>, style: Style) -> Span<'static> {
    if kit.has_color() {
        Span::styled("▌ ", style)
    } else {
        Span::raw("▌ ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Weekday;
    use time::macros::date;

    #[test]
    fn the_year_ends_in_the_week_of_today_and_starts_on_a_sunday() {
        let today = date!(2026 - 10 - 08);
        let first = first_sunday(today);
        assert_eq!(first.weekday(), Weekday::Sunday);
        assert!(today - first < Duration::days(7 * WEEKS as i64));
        assert!(today - first >= Duration::days(7 * (WEEKS as i64 - 1)));
    }

    #[test]
    fn days_fall_into_four_levels() {
        let cuts = quartiles(vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(level(1, &cuts), 1);
        assert_eq!(level(8, &cuts), 4);
        assert_eq!(hour_word(21), "9pm");
        assert_eq!(hour_word(0), "12am");
    }
}
