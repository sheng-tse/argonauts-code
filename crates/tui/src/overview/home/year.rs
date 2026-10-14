//! The year of days, as Claude Code's /stats draws it: a row of months, then Sunday to
//! Saturday with Mon, Wed and Fri named, one cell a day, a grey `·` for a day with nothing and
//! `░ ▒ ▓ █` for busier quarters of the days, each day in the colour of the CLI that ran most.
//! The shades are block elements, which fill exactly one cell in every font.

use std::collections::BTreeMap;

use argonauts_core::display_names::DisplayNames;
use argonauts_core::overview::{OverviewData, OverviewDay};
use argonauts_core::settings::overview::YearColors;
use ratatui::text::Span;
use time::{Date, Duration};

use super::look::{cli_style, cli_word, model_style, model_word, month_word};
use crate::text::truncate_spans;
use crate::theme::Role;
use crate::widgets::{Drawn, Kit, merge};

/// Weeks the year shows on a wide screen.
pub const MOST_WEEKS: usize = 52;
/// Weeks it shows however narrow the screen.
const FEWEST_WEEKS: usize = 10;
/// Cells left of the days for their names.
const DAY_NAMES: usize = 4;
/// The fewest blank cells between two month names, so a month whose name would crowd the one
/// before it is left out.
const MONTH_GAP: usize = 2;
/// A day's glyph by its quarter, the quietest first.
const SHADES: [&str; 4] = ["░", "▒", "▓", "█"];

/// What the year draws with.
pub struct Year<'a> {
    pub data: &'a OverviewData,
    pub names: &'a DisplayNames,
    pub colors: YearColors,
}

impl Year<'_> {
    /// The months over the weeks, then seven rows of days, Sunday first, in `width` cells.
    pub fn draw(&self, kit: &Kit<'_>, width: usize) -> Drawn {
        let weeks = width
            .saturating_sub(DAY_NAMES)
            .clamp(FEWEST_WEEKS, MOST_WEEKS);
        let today = self.data.today;
        let this_sunday =
            today - Duration::days(i64::from(today.weekday().number_days_from_sunday()));
        let first = this_sunday - Duration::weeks(weeks as i64 - 1);
        let by_day: BTreeMap<Date, &OverviewDay> = self
            .data
            .days
            .iter()
            .filter(|day| day.day >= first && day.day <= today && day.tokens > 0)
            .map(|day| (day.day, day))
            .collect();
        let cuts = quartiles(by_day.values().map(|day| day.tokens).collect());
        let mut drawn = Drawn::new();
        drawn.push(months(kit, first, today, weeks));
        for row in 0..7 {
            let name = match row {
                1 => "Mon",
                3 => "Wed",
                5 => "Fri",
                _ => "",
            };
            let mut line = vec![kit.dim(format!("{name:<width$}", width = DAY_NAMES))];
            for week in 0..weeks {
                let day = first + Duration::days((week * 7 + row) as i64);
                line.push(if day > today {
                    Span::raw(" ")
                } else {
                    match by_day.get(&day) {
                        Some(day) => self.cell(kit, day, level(day.tokens, &cuts)),
                        None => kit.span("·", Role::Faint),
                    }
                });
            }
            drawn.push(merge(line));
        }
        drawn
    }

    /// `Less ░ ▒ ▓ █ More`, then what each colour is and what the colours mean, cut from the
    /// right to `width`.
    pub fn legend(&self, kit: &Kit<'_>, width: usize) -> Drawn {
        let mut row = vec![
            Span::raw(" ".repeat(DAY_NAMES)),
            Span::raw("Less "),
            Span::raw(SHADES.join(" ")),
            Span::raw(" More"),
        ];
        let (swatches, meaning) = match self.colors {
            YearColors::Cli => (
                self.data
                    .by_cli()
                    .into_iter()
                    .map(|(agent, _)| (cli_style(kit, &agent), cli_word(self.names, &agent)))
                    .collect(),
                "each day in the colour of the CLI that ran most",
            ),
            YearColors::Model => (
                self.data
                    .models
                    .iter()
                    .map(|model| {
                        let (agent, model) = (&model.agent, model.model.as_deref());
                        (
                            model_style(kit, self.data, agent, model),
                            model_word(self.names, agent, model),
                        )
                    })
                    .collect(),
                "each day in the colour of the model that ran most",
            ),
            YearColors::One => (Vec::new(), ""),
        };
        for (at, (style, name)) in swatches.into_iter().enumerate() {
            row.push(Span::raw(if at == 0 { "    " } else { "  " }));
            row.push(Span::styled("● ", style));
            row.push(Span::raw(name));
        }
        if !meaning.is_empty() {
            row.push(kit.dim(format!("    {meaning}")));
        }
        let mut drawn = Drawn::new();
        drawn.push(truncate_spans(&row, width));
        drawn
    }

    fn cell(&self, kit: &Kit<'_>, day: &OverviewDay, level: usize) -> Span<'static> {
        let glyph = SHADES[level.clamp(1, 4) - 1];
        let style = match self.colors {
            YearColors::Cli => cli_style(kit, &day.agent),
            YearColors::Model => model_style(kit, self.data, &day.agent, day.model.as_deref()),
            YearColors::One => kit.fg(Role::Card),
        };
        Span::styled(glyph, style)
    }
}

/// Each month's name over the first week whose Sunday is in it, left out where it would come
/// within [`MONTH_GAP`] cells of the name before.
fn months(kit: &Kit<'_>, first: Date, today: Date, weeks: usize) -> Vec<Span<'static>> {
    let mut row = " ".repeat(DAY_NAMES);
    let mut last_month = None;
    for week in 0..weeks {
        let sunday = first + Duration::weeks(week as i64);
        if sunday > today || last_month == Some(sunday.month()) {
            continue;
        }
        last_month = Some(sunday.month());
        let at = DAY_NAMES + week;
        let end = row.chars().count();
        let room = if end == DAY_NAMES { 0 } else { MONTH_GAP };
        if at >= end + room {
            row.push_str(&" ".repeat(at - end));
            row.push_str(month_word(sunday.month()));
        }
    }
    vec![kit.dim(row)]
}

/// The amounts at a quarter, half and three quarters of the way through the busy days, as
/// Claude Code takes them.
fn quartiles(mut amounts: Vec<u64>) -> [u64; 3] {
    amounts.sort_unstable();
    let at = |fraction: f64| {
        amounts
            .get((amounts.len() as f64 * fraction) as usize)
            .copied()
            .unwrap_or(u64::MAX)
    };
    [at(0.25), at(0.5), at(0.75)]
}

/// 1 for the quietest quarter of the days, 4 for the busiest.
fn level(tokens: u64, cuts: &[u64; 3]) -> usize {
    1 + cuts.iter().filter(|cut| tokens >= **cut).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_fall_into_four_levels_at_the_quartiles() {
        let cuts = quartiles(vec![1, 2, 2, 3, 3, 3, 4, 4]);
        assert_eq!(cuts, [2, 3, 4]);
        assert_eq!(level(1, &cuts), 1);
        assert_eq!(level(2, &cuts), 2);
        assert_eq!(level(3, &cuts), 3);
        assert_eq!(level(4, &cuts), 4);
    }
}
