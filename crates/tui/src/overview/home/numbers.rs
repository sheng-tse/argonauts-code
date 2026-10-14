//! The Overview sub-tab under the year, as Claude Code's /stats sets it out: label and value
//! pairs in two columns of 28 cells four apart, the values in the accent; the tokens by CLI
//! and by kind on dim lines; then one fun fact, what's worth knowing, and where the numbers
//! came from.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::overview::{OverviewData, OverviewPeriod};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::facts::{fun_fact, worth_knowing};
use super::look::{
    Unit, big, cli_word, days, dollars, duration, folder_name, model_word, short_date, thousands,
};
use crate::text::truncate_spans;
use crate::theme::Role;
use crate::widgets::{Drawn, Kit, fit};

/// A column of pairs, in cells.
const COLUMN: usize = 28;
/// Blank cells between the two columns.
const BETWEEN: usize = 4;

/// One label and its value, the value bold where Claude Code makes it so.
struct Pair {
    label: &'static str,
    value: Option<String>,
    bold: bool,
}

impl Pair {
    fn new(label: &'static str, value: Option<String>) -> Self {
        Pair {
            label,
            value,
            bold: false,
        }
    }

    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    fn spans(&self, kit: &Kit<'_>) -> Vec<Span<'static>> {
        let Some(value) = &self.value else {
            return Vec::new();
        };
        let style = if self.bold {
            kit.bold(Role::Card)
        } else {
            kit.style(Role::Card)
        };
        vec![
            Span::raw(format!("{} ", self.label)),
            Span::styled(value.clone(), style),
        ]
    }
}

/// What the numbers draw from.
pub struct Numbers<'a> {
    pub data: &'a OverviewData,
    pub names: &'a DisplayNames,
}

impl Numbers<'_> {
    /// The pairs two to a row, or one to a row where two columns don't fit, then the dim
    /// lines of tokens.
    pub fn pairs(&self, kit: &Kit<'_>, width: usize) -> Drawn {
        let mut drawn = Drawn::new();
        let two = width >= COLUMN * 2 + BETWEEN;
        for (left, right) in self.rows() {
            let (left, right) = (left.spans(kit), right.spans(kit));
            if two {
                let mut row = fit(left, COLUMN);
                row.push(Span::raw(" ".repeat(BETWEEN)));
                row.extend(fit(right, COLUMN));
                drawn.push(row);
            } else {
                for half in [left, right].into_iter().filter(|half| !half.is_empty()) {
                    drawn.push(truncate_spans(&half, width));
                }
            }
        }
        for line in self.token_lines() {
            drawn.push(truncate_spans(&[kit.dim(line)], width));
        }
        drawn
    }

    fn rows(&self) -> [(Pair, Pair); 6] {
        let data = self.data;
        let names = self.names;
        let favourite_model = data
            .models
            .first()
            .map(|model| model_word(names, &model.agent, model.model.as_deref()));
        let favourite_cli = data
            .by_cli()
            .first()
            .map(|(agent, _)| cli_word(names, agent));
        let longest = (data.longest_session_secs > 0).then(|| duration(data.longest_session_secs));
        let peak_hour = data
            .hours
            .iter()
            .enumerate()
            .filter(|(_, tokens)| **tokens > 0)
            .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(&a.0)))
            .map(|(hour, _)| format!("{hour}:00-{}:00", hour + 1));
        [
            (
                Pair::new("Favorite model:", favourite_model).bold(),
                Pair::new("Total tokens:", Some(big(data.total.total()))),
            ),
            (
                Pair::new("Favorite CLI:", favourite_cli).bold(),
                Pair::new("API value:", Some(dollars(data.api_value))),
            ),
            (
                Pair::new("Sessions:", Some(thousands(u64::from(data.sessions())))),
                Pair::new("Longest session:", longest),
            ),
            (
                Pair::new(
                    "Active days:",
                    Some(format!("{}/{}", data.active_days, data.span_days)),
                )
                .bold(),
                Pair::new("Longest streak:", Some(days(data.longest_streak))).bold(),
            ),
            (
                Pair::new("Most active day:", data.peak_day.map(short_date)),
                Pair::new("Current streak:", Some(days(data.streak))).bold(),
            ),
            (
                Pair::new(
                    "Top folder:",
                    data.folders.first().map(|(folder, _)| folder_name(folder)),
                ),
                Pair::new("Peak hour:", peak_hour),
            ),
        ]
    }

    /// `Claude Code 73.5b · Codex 33.1b · agy 11.8b` where more than one CLI worked, and
    /// `Input 1.2b · Output 0.4b · Cache read 110.0b · Cache write 6.8b`.
    fn token_lines(&self) -> Vec<String> {
        let data = self.data;
        let mut lines = Vec::new();
        let clis = data.by_cli();
        if clis.len() > 1 {
            let unit = Unit::for_largest(clis.iter().map(|(_, tokens)| *tokens).max().unwrap_or(0));
            let parts: Vec<String> = clis
                .iter()
                .map(|(agent, tokens)| {
                    format!("{} {}", cli_word(self.names, agent), unit.write(*tokens))
                })
                .collect();
            lines.push(parts.join(" · "));
        }
        let total = data.total;
        let unit = Unit::for_largest(
            [
                total.input,
                total.output,
                total.cache_read,
                total.cache_write,
            ]
            .into_iter()
            .max()
            .unwrap_or(0),
        );
        lines.push(format!(
            "Input {} · Output {} · Cache read {} · Cache write {}",
            unit.write(total.input),
            unit.write(total.output),
            unit.write(total.cache_read),
            unit.write(total.cache_write)
        ));
        lines
    }

    /// The fun fact in the accent, what's worth knowing, and `source` on its own dim line.
    pub fn words(&self, kit: &Kit<'_>, width: usize, source: String) -> Drawn {
        let mut drawn = Drawn::new();
        if let Some(fact) = fun_fact(self.data) {
            drawn.push(truncate_spans(&[kit.span(fact, Role::Card)], width));
        }
        if let Some(worth) = worth_knowing(self.data) {
            drawn.push(truncate_spans(&[kit.dim(worth)], width));
        }
        drawn.push(truncate_spans(&[kit.dim(source)], width));
        drawn
    }
}

/// `All time · Last 7 days · Last 30 days`, the one shown bold in the accent.
pub fn periods(kit: &Kit<'_>, shown: OverviewPeriod) -> Drawn {
    let mut row = Vec::new();
    for (at, period) in OverviewPeriod::ALL.into_iter().enumerate() {
        if at > 0 {
            row.push(kit.dim(" · "));
        }
        row.push(if period == shown {
            Span::styled(period.title(), kit.bold(Role::Card))
        } else {
            kit.dim(period.title())
        });
    }
    let mut drawn = Drawn::new();
    drawn.push(row);
    drawn
}

/// Bold in the screen's own text colour, as Claude Code writes a heading or a model's name.
pub fn heading(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::new().add_modifier(Modifier::BOLD))
}
