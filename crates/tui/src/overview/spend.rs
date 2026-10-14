//! The spend card: what the period spent, one row a model. A model recorded under several
//! names is one row, and spend recorded with no model goes to the model its CLI runs when
//! none is named, so no model shows twice and no CLI stands in a row by itself. A thin bar of
//! the models' shares, a blank row, then each model's spend, share, tokens, what it did and
//! what it spent on each day of this week; under them, how much ran inside a plan and how
//! much was charged to an API key.

use std::collections::BTreeMap;

use argonauts_core::Spend;
use argonauts_core::api::{UsagePeriod, UsageSummary};
use argonauts_core::crew_stats::StatsRole;
use argonauts_core::display_names::DisplayNames;
use ratatui::style::Style;
use ratatui::text::Span;
use time::{Date, Duration};

use super::cards::period_words;
use super::home::look::{Unit, cli_word};
use super::shades::{ModelColors, shown_as};
use super::stats::draw;
use crate::format;
use crate::kit::{Badge, Drawn, Kit, Level, fit, spread};
use crate::text::{spans_width, truncate_spans};
use crate::theme::Role;

/// From this share of the day's limit on, the spend shows amber.
const NEAR_LIMIT: f64 = 0.9;
/// How a row with no model reads where nothing says which one its CLI runs.
const DEFAULT_MODEL: &str = "default model";
/// The fewest cells the model's column keeps: its square and a few letters of its name.
const LEAST_MODEL: usize = 8;

/// The card's columns, left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Model,
    On,
    Spend,
    Share,
    Tokens,
    Did,
    Week,
}

impl Column {
    const ALL: [Column; 7] = [
        Column::Model,
        Column::On,
        Column::Spend,
        Column::Share,
        Column::Tokens,
        Column::Did,
        Column::Week,
    ];

    /// What a narrow card gives up, the first first. The model and its spend stay.
    const GIVES_WAY: [Column; 5] = [
        Column::Week,
        Column::Did,
        Column::On,
        Column::Tokens,
        Column::Share,
    ];

    /// The column's slot in cells, the blanks before the next column included. Slots are
    /// fixed, so a number changing never moves a column.
    fn slot(self) -> usize {
        match self {
            Column::Model => 22,
            Column::On => 14,
            Column::Spend => 9,
            Column::Share => 7,
            Column::Tokens => 11,
            Column::Did => 26,
            Column::Week => 16,
        }
    }

    fn head(self) -> &'static str {
        match self {
            Column::Model => "model",
            Column::On => "on",
            Column::Spend => "spend",
            Column::Share => "share",
            Column::Tokens => "tokens",
            Column::Did => "did",
            Column::Week => "a day, this week",
        }
    }

    /// The columns that fit in `width` cells, each with its cells. Where even the model and
    /// its spend don't fit, the model's name gives way.
    fn fitting(width: usize) -> Vec<(Column, usize)> {
        let mut columns = Column::ALL.to_vec();
        let cells = |columns: &[Column]| columns.iter().map(|column| column.slot()).sum::<usize>();
        for gone in Column::GIVES_WAY {
            if cells(&columns) <= width {
                break;
            }
            columns.retain(|column| *column != gone);
        }
        let over = cells(&columns).saturating_sub(width);
        columns
            .into_iter()
            .map(|column| match column {
                Column::Model => (column, column.slot().saturating_sub(over).max(LEAST_MODEL)),
                column => (column, column.slot()),
            })
            .collect()
    }
}

/// One model's row.
#[derive(Debug, Clone)]
struct Row {
    /// The CLI's id.
    agent: String,
    /// The model as one of its records names it, which finds its color.
    model: Option<String>,
    /// The name it shows under, empty while nothing names the model.
    name: String,
    spent: Spend,
    pays_per_token: bool,
    /// What it spent on each day of this week, in the units of [`units`].
    week: [u64; 7],
    /// The pieces of work it did, by role.
    did: BTreeMap<StatsRole, u32>,
}

impl Row {
    fn add(&mut self, more: &Row) {
        self.spent = self.spent.plus(&more.spent);
        self.pays_per_token |= more.pays_per_token;
        for (day, spent) in self.week.iter_mut().zip(more.week) {
            *day += spent;
        }
        for (role, done) in &more.did {
            *self.did.entry(*role).or_default() += done;
        }
    }

    /// `3 tasks · 13 reviews`.
    fn did_words(&self) -> String {
        StatsRole::BY_WORK
            .into_iter()
            .filter_map(|role| {
                let done = self.did.get(&role).copied().filter(|done| *done > 0)?;
                Some(match done {
                    1 => format!("1 {}", role.unit()),
                    done => format!("{done} {}s", role.unit()),
                })
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// Dollars as a whole number a bar can share out: ten-thousandths.
fn units(usd: f64) -> u64 {
    (usd.max(0.0) * 10_000.0).round() as u64
}

/// `$0.26`, and `$0` for nothing.
fn dollars(usd: f64) -> String {
    if usd == 0.0 {
        "$0".to_owned()
    } else {
        format::usd(usd)
    }
}

/// What the card draws from.
#[derive(Debug, Clone, Copy)]
pub(super) struct SpendCard<'a> {
    pub usage: &'a UsageSummary,
    pub names: &'a DisplayNames,
    /// Today on the clock times are shown in, for when the numbers carry no day of their own.
    pub today: Date,
}

impl SpendCard<'_> {
    /// Today as the numbers have it: the last of their days of history.
    fn today(&self) -> Date {
        self.usage.history.last().map_or(self.today, |day| day.day)
    }

    fn week_start(&self) -> Date {
        let today = self.today();
        UsagePeriod::Week
            .first_day(today, self.usage.week_starts.weekday())
            .unwrap_or(today)
    }

    fn spent_today(&self) -> Spend {
        let today = self.today();
        let day = self.usage.history.iter().rev().find(|day| day.day == today);
        day.map(|day| day.spent).unwrap_or_default()
    }

    /// The period's models, the most spent first.
    fn rows(&self) -> Vec<Row> {
        let week_start = self.week_start();
        let mut rows: Vec<Row> = Vec::new();
        let row_of = |rows: &mut Vec<Row>, agent: &str, model: Option<&str>| -> usize {
            let (agent, name) = shown_as(self.names, agent, model);
            let known = rows
                .iter()
                .position(|row| row.agent == agent && row.name == name);
            known.unwrap_or_else(|| {
                rows.push(Row {
                    agent,
                    model: model.map(str::to_owned),
                    name,
                    spent: Spend::default(),
                    pays_per_token: false,
                    week: [0; 7],
                    did: BTreeMap::new(),
                });
                rows.len() - 1
            })
        };
        for share in &self.usage.by_agent {
            let at = row_of(&mut rows, &share.agent, share.model.as_deref());
            let row = &mut rows[at];
            row.spent = row.spent.plus(&share.spent);
            row.pays_per_token |= share.pays_per_token;
            for day in &share.this_week {
                let at = (day.day - week_start).whole_days();
                if let Some(slot) = usize::try_from(at).ok().and_then(|at| row.week.get_mut(at)) {
                    *slot += units(day.spent.cost_usd);
                }
            }
        }
        // Work counts for the models that spent in the period, which are the card's rows.
        let spenders = rows.len();
        let work = self.usage.stats.iter().flat_map(|stats| &stats.work);
        for work in work.filter(|work| work.done > 0) {
            let at = row_of(&mut rows, &work.agent, work.model.as_deref());
            *rows[at].did.entry(work.role).or_default() += work.done;
        }
        rows.truncate(spenders);
        fold_unnamed(&mut rows);
        rows.sort_by(|a, b| b.spent.cost_usd.total_cmp(&a.spent.cost_usd));
        rows
    }

    /// The card's first row: `title`, the period, and the numbers. On a narrow card the
    /// numbers say less, then the period's words go, and what the period spent stays.
    pub fn header(
        &self,
        kit: &Kit<'_>,
        title: Vec<Span<'static>>,
        width: usize,
    ) -> Vec<Span<'static>> {
        let mut with_period = title.clone();
        with_period.push(kit.dim(format!("  {}", period_words(self.usage.period))));
        let room = |left: &[Span<'static>]| width.saturating_sub(spans_width(left) + 2);
        let (left, numbers) = match self.numbers(kit, room(&with_period)) {
            Some(numbers) => (with_period, numbers),
            None => {
                let room = room(&title);
                let total = [kit.strong(format::spend(&self.usage.total))];
                let numbers = self.numbers(kit, room);
                (
                    title,
                    numbers.unwrap_or_else(|| truncate_spans(&total, room)),
                )
            }
        };
        spread(left, numbers, width)
    }

    /// The header's numbers, as many of them as fit in `room` cells: the period's spend, the
    /// day's limit or that none is set, and what today spent. `None` where not even the
    /// period's spend fits.
    fn numbers(&self, kit: &Kit<'_>, room: usize) -> Option<Vec<Span<'static>>> {
        let usage = self.usage;
        let total = kit.strong(format::spend(&usage.total));
        let today = self.spent_today();
        let all_today = usage.period == UsagePeriod::Today;
        // What the period spent, then what today adds beside it where the two aren't one.
        let mut period = vec![total.clone()];
        let mut beside = Vec::new();
        match usage.spend_limit.filter(|limit| *limit > 0.0) {
            Some(limit) => {
                let (counted, words) = if all_today {
                    (usage.total, &mut period)
                } else {
                    beside.push(kit.dim(" · "));
                    beside.push(kit.strong(format::spend(&today)));
                    (today, &mut beside)
                };
                let share = counted.cost_usd / limit;
                let level = if share >= 1.0 {
                    Level::Danger
                } else if share >= NEAR_LIMIT {
                    Level::Warn
                } else {
                    Level::Ok
                };
                words.push(kit.dim(format!(" of {} today", format::usd(limit))));
                words.push(Span::raw(" "));
                words.extend(Badge::percent(share, level).spans(kit));
            }
            None => {
                period.push(kit.dim(" · no limit set"));
                if !all_today {
                    beside.push(kit.dim(" · "));
                    beside.push(kit.strong(format::spend(&today)));
                    beside.push(kit.dim(" today"));
                }
            }
        }
        if spans_width(&period) + spans_width(&beside) <= room {
            period.extend(beside);
            Some(period)
        } else if spans_width(&period) <= room {
            Some(period)
        } else {
            let total = vec![total];
            (spans_width(&total) <= room).then_some(total)
        }
    }

    /// Each model's spend in words, one a line: `Haiku 5.5 on Claude Code: $0.24`.
    pub fn spoken(&self) -> Vec<String> {
        let rows = self.rows();
        let line = |row: &Row| {
            let cli = self.names.cli(&row.agent);
            format!("{} on {cli}: {}", row.name, format::spend(&row.spent))
        };
        rows.iter().map(line).collect()
    }

    /// The bar, then in full the rows under it.
    pub fn body(&self, kit: &Kit<'_>, width: usize, full: bool) -> Drawn {
        let rows = self.rows();
        let shown = rows.iter();
        let shades = ModelColors::new(&self.usage.all_time_models, self.names)
            .with(shown.map(|row| (row.agent.as_str(), row.model.as_deref())));
        let styles: Vec<Style> = rows
            .iter()
            .map(|row| shades.style(kit, &row.agent, row.model.as_deref()))
            .collect();
        let bar = self.bar(kit, &rows, &styles, width);
        let mut body = Drawn::new();
        if !full {
            body.push(bar);
            return body;
        }
        body.push(Vec::new());
        body.push(bar);
        if rows.is_empty() {
            return body;
        }
        body.push(Vec::new());
        for line in self.table(kit, &rows, &styles, width) {
            body.push(line);
        }
        body.push(Vec::new());
        body.push(truncate_spans(&self.billing(kit, &rows), width));
        body
    }

    /// Each model's share of the spend. Against a limit for the day, today's bar is the
    /// limit, and what's unspent stays track.
    fn bar(
        &self,
        kit: &Kit<'_>,
        rows: &[Row],
        styles: &[Style],
        width: usize,
    ) -> Vec<Span<'static>> {
        let mut parts: Vec<(u64, Style)> = rows
            .iter()
            .zip(styles)
            .map(|(row, style)| (units(row.spent.cost_usd), *style))
            .collect();
        let spent: u64 = parts.iter().map(|(spent, _)| spent).sum();
        let limit = self.usage.spend_limit.map(units).unwrap_or(0);
        if self.usage.period == UsagePeriod::Today && limit > spent {
            parts.push((limit - spent, kit.fg(Role::Faint)));
        }
        draw::stacked(kit, &parts, width)
    }

    /// The column heads, then a row a model.
    fn table(
        &self,
        kit: &Kit<'_>,
        rows: &[Row],
        styles: &[Style],
        width: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let columns = Column::fitting(width);
        let total = self.usage.total.cost_usd;
        let tokens = |row: &Row| row.spent.total_tokens();
        let unit = Unit::for_largest(rows.iter().map(tokens).max().unwrap_or(0));
        // One scale for every model's days, so the rows compare.
        let fullest_day = rows.iter().flat_map(|row| row.week).max().unwrap_or(0);
        let mut lines = vec![line(&columns, |column| vec![kit.dim(column.head())])];
        for (row, style) in rows.iter().zip(styles) {
            lines.push(line(&columns, |column| match column {
                Column::Model => {
                    let mut cell = draw::swatch(kit, *style);
                    cell.push(Span::raw(row.name.clone()));
                    cell
                }
                Column::On => vec![kit.dim(cli_word(self.names, &row.agent))],
                Column::Spend => vec![kit.strong(format::spend(&row.spent))],
                Column::Share if total > 0.0 => {
                    vec![kit.dim(format::percent(row.spent.cost_usd / total))]
                }
                Column::Share => Vec::new(),
                Column::Tokens => vec![Span::raw(unit.write(tokens(row)))],
                Column::Did => vec![kit.dim(row.did_words())],
                Column::Week => self.week(kit, row, fullest_day, *style),
            }));
        }
        lines
    }

    /// A small bar for each day of this week, a blank cell apart: the lowest on the grey
    /// track for a day with no spend, and nothing for a day still ahead.
    fn week(&self, kit: &Kit<'_>, row: &Row, fullest_day: u64, style: Style) -> Vec<Span<'static>> {
        let (first, today) = (self.week_start(), self.today());
        let mut cells = Vec::new();
        for (at, spent) in row.week.iter().enumerate() {
            if at > 0 {
                cells.push(Span::raw(" "));
            }
            let ahead = first + Duration::days(at as i64) > today;
            let day = (!ahead).then_some(*spent);
            cells.extend(draw::spark(kit, &[day], fullest_day, style));
        }
        cells
    }

    /// `on plans $0.26 (inside Max and Plus, not charged) · on API keys $0`, and what `~`
    /// means where a number carries it.
    fn billing(&self, kit: &Kit<'_>, rows: &[Row]) -> Vec<Span<'static>> {
        let sum = |charged: bool| -> f64 {
            let rows = rows.iter().filter(|row| row.pays_per_token == charged);
            rows.map(|row| row.spent.cost_usd).sum()
        };
        let (on_plans, on_keys) = (sum(false), sum(true));
        let inside = if on_plans > 0.0 {
            format!(" (inside {}, not charged)", self.plans_of(rows))
        } else {
            String::new()
        };
        let mut line = vec![
            kit.dim("on plans "),
            kit.strong(dollars(on_plans)),
            kit.dim(format!("{inside} · on API keys ")),
            kit.strong(dollars(on_keys)),
        ];
        let estimated = |row: &Row| row.spent.cost_estimated && row.spent.cost_usd > 0.0;
        if rows.iter().any(estimated) {
            line.push(kit.dim(" · ~ is an estimate at list prices"));
        }
        line
    }

    /// `Max and Plus`: the plans the rows' CLIs are on, or `your plans` where one has no name.
    fn plans_of(&self, rows: &[Row]) -> String {
        let mut plans: Vec<&str> = Vec::new();
        let mut agents: Vec<&str> = Vec::new();
        for row in rows.iter().filter(|row| !row.pays_per_token) {
            if agents.contains(&row.agent.as_str()) {
                continue;
            }
            agents.push(&row.agent);
            let plan = self.usage.plans.iter().find(|plan| plan.agent == row.agent);
            match plan.and_then(|plan| plan.plan.as_deref()) {
                Some(name) if !plans.contains(&name) => plans.push(name),
                Some(_) => {}
                None => return "your plans".to_owned(),
            }
        }
        match plans.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
            Some((only, _)) => (*only).to_owned(),
            None => "your plans".to_owned(),
        }
    }
}

/// Spend recorded with no model, where nothing says which one its CLI runs, goes to that
/// CLI's model that spent the most. A CLI with no such model keeps the row, as its default.
fn fold_unnamed(rows: &mut Vec<Row>) {
    while let Some(at) = rows.iter().position(|row| row.name.is_empty()) {
        let unnamed = rows.remove(at);
        let named = rows.iter_mut().filter(|row| row.agent == unnamed.agent);
        match named.max_by(|a, b| a.spent.cost_usd.total_cmp(&b.spent.cost_usd)) {
            Some(row) => row.add(&unnamed),
            None => rows.push(Row {
                name: DEFAULT_MODEL.to_owned(),
                ..unnamed
            }),
        }
    }
}

/// Each column's cell in its cells, with a blank kept before the next column.
fn line(
    columns: &[(Column, usize)],
    cell: impl Fn(Column) -> Vec<Span<'static>>,
) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for (at, (column, cells)) in columns.iter().enumerate() {
        if at + 1 == columns.len() {
            out.extend(truncate_spans(&cell(*column), *cells));
        } else {
            out.extend(fit(cell(*column), cells.saturating_sub(1)));
            out.push(Span::raw(" "));
        }
    }
    out
}

#[cfg(test)]
#[path = "spend_tests.rs"]
mod tests;
