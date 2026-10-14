//! The Models sub-tab, as Claude Code's /stats draws it: "Tokens per Day" for the top three
//! models with its dates and legend, the favourite and the total, then each model in two
//! columns, four at a time, with the CLI that ran it, what it would cost at list prices and
//! its sessions.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::overview::{ModelDay, OverviewData, OverviewModel};
use ratatui::style::Style;
use ratatui::text::Span;

use super::chart::{self, Cell, LABEL_CELLS};
use super::look::{
    Unit, big, cli_word, dollars, model_style, model_word, share_tenths, short_date,
};
use super::numbers::heading;
use crate::text::truncate_spans;
use crate::theme::Role;
use crate::widgets::{Drawn, Kit, fit, merge};

/// Models shown at once, two to a row.
pub const PAGE: usize = 4;
/// Models a row holds.
const PER_ROW: usize = 2;
/// The chart's height in rows over its lowest, as Claude Code sets it.
const CHART_HEIGHT: usize = 8;
/// Days the chart shows on a wide screen.
const MOST_DAYS: usize = 52;
/// Columns the chart keeps however narrow the screen.
const FEWEST_DAYS: usize = 20;
/// The left column of models, in cells.
const LEFT_COLUMN: usize = 46;

/// What the sub-tab draws from.
pub struct Models<'a> {
    pub data: &'a OverviewData,
    pub names: &'a DisplayNames,
}

impl Models<'_> {
    /// `Tokens per Day`, the chart, its dates and its legend, once there are two days to
    /// draw a line between.
    pub fn chart(&self, kit: &Kit<'_>, width: usize) -> Option<Drawn> {
        let columns = width
            .saturating_sub(LABEL_CELLS + 1)
            .clamp(FEWEST_DAYS, MOST_DAYS);
        let days = spread_days(&self.data.model_days, columns);
        if days.len() < 2 {
            return None;
        }
        let top = self.data.models.len().min(days[0].tokens.len());
        let mut drawn_models: Vec<&OverviewModel> = Vec::new();
        let mut series: Vec<Vec<u64>> = Vec::new();
        for (at, model) in self.data.models.iter().take(top).enumerate() {
            let values: Vec<u64> = days
                .iter()
                .map(|day| day.tokens.get(at).copied().unwrap_or(0))
                .collect();
            if values.iter().any(|tokens| *tokens > 0) {
                drawn_models.push(model);
                series.push(values);
            }
        }
        if series.is_empty() {
            return None;
        }
        let styles: Vec<Style> = drawn_models
            .iter()
            .map(|model| model_style(kit, self.data, &model.agent, model.model.as_deref()))
            .collect();
        let mut drawn = Drawn::new();
        drawn.push(vec![heading("Tokens per Day")]);
        for (label, cells) in chart::plot(&series, CHART_HEIGHT).rows {
            let mut row = vec![kit.dim(format!("{label} "))];
            for cell in cells {
                row.push(match cell {
                    Cell::Blank => Span::raw(" "),
                    Cell::Axis(glyph) => kit.span(glyph.to_string(), Role::Faint),
                    Cell::Line(glyph, at) => Span::styled(glyph.to_string(), styles[at]),
                });
            }
            drawn.push(truncate_spans(&merge(row), width));
        }
        drawn.push(truncate_spans(&[kit.dim(dates(&days))], width));
        let mut legend = Vec::new();
        for (at, model) in drawn_models.iter().enumerate() {
            if at > 0 {
                legend.push(kit.dim(" · "));
            }
            legend.push(Span::styled("● ", styles[at]));
            legend.push(Span::raw(model_word(
                self.names,
                &model.agent,
                model.model.as_deref(),
            )));
        }
        drawn.push(truncate_spans(&legend, width));
        Some(drawn)
    }

    /// `★ Favorite: Opus 5.5 · ● Total: 118.4b tokens`.
    pub fn favourite(&self, kit: &Kit<'_>, width: usize) -> Option<Drawn> {
        let first = self.data.models.first()?;
        let name = model_word(self.names, &first.agent, first.model.as_deref());
        let row = vec![
            Span::raw("★ Favorite: "),
            Span::styled(name, kit.bold(Role::Card)),
            Span::raw(" · ● Total: "),
            kit.span(big(self.data.total.total()), Role::Card),
            Span::raw(" tokens"),
        ];
        let mut drawn = Drawn::new();
        drawn.push(truncate_spans(&row, width));
        Some(drawn)
    }

    /// Four models from `first`, two to a row, or one to a row where two don't fit.
    pub fn columns(&self, kit: &Kit<'_>, width: usize, first: usize) -> Drawn {
        let shown: Vec<Vec<Vec<Span<'static>>>> = self
            .data
            .models
            .iter()
            .skip(first)
            .take(PAGE)
            .map(|model| self.cell(kit, model))
            .collect();
        let mut drawn = Drawn::new();
        if width < LEFT_COLUMN * 2 {
            for line in shown.into_iter().flatten() {
                drawn.push(truncate_spans(&line, width));
            }
            return drawn;
        }
        for pair in shown.chunks(PER_ROW) {
            let rows = pair.iter().map(Vec::len).max().unwrap_or(0);
            for row in 0..rows {
                let left = pair[0].get(row).cloned().unwrap_or_default();
                let mut line = fit(left, LEFT_COLUMN);
                if let Some(right) = pair.get(1).and_then(|cell| cell.get(row)) {
                    line.extend(truncate_spans(right, width - LEFT_COLUMN));
                }
                drawn.push(line);
            }
        }
        drawn
    }

    /// A model's four lines: its name and share, its CLI with its value and sessions, its
    /// input and output, and its cache.
    fn cell(&self, kit: &Kit<'_>, model: &OverviewModel) -> Vec<Vec<Span<'static>>> {
        let (agent, id) = (&model.agent, model.model.as_deref());
        let tokens = model.tokens;
        let unit = Unit::for_largest(
            [
                tokens.input,
                tokens.output,
                tokens.cache_read,
                tokens.cache_write,
            ]
            .into_iter()
            .max()
            .unwrap_or(0),
        );
        let mut on = vec![format!("on {}", cli_word(self.names, agent))];
        if let Some(usd) = model.api_value {
            on.push(dollars(usd));
        }
        on.push(match model.sessions {
            1 => "1 session".to_owned(),
            n => format!("{n} sessions"),
        });
        vec![
            vec![
                Span::styled("● ", model_style(kit, self.data, agent, id)),
                heading(model_word(self.names, agent, id)),
                kit.dim(format!(
                    " ({})",
                    share_tenths(tokens.total(), self.data.total.total())
                )),
            ],
            vec![kit.dim(format!("  {}", on.join(" · ")))],
            vec![kit.dim(format!(
                "  In: {} · Out: {}",
                unit.write(tokens.input),
                unit.write(tokens.output)
            ))],
            vec![kit.dim(format!(
                "  Cache: {} read · {} write",
                unit.write(tokens.cache_read),
                unit.write(tokens.cache_write)
            ))],
        ]
    }
}

/// `↑ ↓ 1-4 of 7 models`, when there are more models than show at once.
pub fn page_word(count: usize, first: usize) -> Option<String> {
    (count > PAGE).then(|| {
        format!(
            "↑ ↓ {}-{} of {count} models",
            first + 1,
            (first + PAGE).min(count)
        )
    })
}

/// The first model a page can start at: the page that ends on the last model, a row at a time.
pub fn last_page(count: usize) -> usize {
    count.saturating_sub(PAGE).next_multiple_of(PER_ROW)
}

/// The days the chart draws in `columns` columns: the last ones when there are more, and each
/// one repeated when there are fewer, as Claude Code stretches a short history.
fn spread_days(days: &[ModelDay], columns: usize) -> Vec<ModelDay> {
    if days.len() >= columns {
        return days[days.len() - columns..].to_vec();
    }
    let each = (columns / days.len().max(1)).max(1);
    days.iter()
        .flat_map(|day| std::iter::repeat_n(day.clone(), each))
        .collect()
}

/// Up to four dates under the chart, each under the column of its day: spread evenly from the
/// first day to the one before the last, so the last date stays near the chart's edge.
fn dates(days: &[ModelDay]) -> String {
    let count = (days.len() / 8).clamp(2, 4);
    let span = days.len().saturating_sub(2).max(1);
    let mut line = " ".repeat(LABEL_CELLS + 1);
    let mut end = 0;
    for nth in 0..count {
        let at = (nth * span / (count - 1)).min(days.len() - 1);
        let word = short_date(days[at].day);
        let column = (LABEL_CELLS + 1 + at).max(if end == 0 { 0 } else { end + 1 });
        line.push_str(&" ".repeat(column.saturating_sub(line.chars().count())));
        line.push_str(&word);
        end = line.chars().count();
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;
    use time::macros::date;

    fn days(n: usize) -> Vec<ModelDay> {
        (0..n)
            .map(|at| ModelDay {
                day: date!(2026 - 08 - 18) + Duration::days(at as i64),
                tokens: vec![1],
            })
            .collect()
    }

    #[test]
    fn four_dates_spread_under_the_columns_of_their_days() {
        assert_eq!(
            dates(&days(52)),
            "       Aug 18          Sep 3            Sep 20           Oct 7"
        );
    }

    #[test]
    fn a_short_history_is_stretched_and_a_long_one_cut_to_the_last_days() {
        assert_eq!(spread_days(&days(7), 52).len(), 49);
        let last = spread_days(&days(90), 52);
        assert_eq!(last.len(), 52);
        assert_eq!(last[51].day, days(90)[89].day);
    }

    #[test]
    fn the_page_moves_a_row_at_a_time_and_stops_on_the_last() {
        assert_eq!(page_word(7, 0).as_deref(), Some("↑ ↓ 1-4 of 7 models"));
        assert_eq!(page_word(4, 0), None);
        assert_eq!(last_page(7), 4);
        assert_eq!(last_page(8), 4);
        assert_eq!(last_page(3), 0);
    }
}
