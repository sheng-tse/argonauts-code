//! The spend card: what the period spent, one row a model. A model recorded under several
//! names is one row, and spend recorded with no model goes to the model its CLI runs when
//! none is named, so no model shows twice and no CLI stands in a row by itself. A thin bar of
//! the models' shares, a blank row, then a table of each model, the CLI it ran on, its spend,
//! share and tokens, and what it spent on each day of this week; under them, how much ran
//! inside a plan and how much was charged to an API key.
//!
//! Each column is as wide as its longest entry or its head, with the room between columns
//! the shared legend keeps, and every figure is written as Stats writes it.

use argonauts_core::Spend;
use argonauts_core::api::{UsagePeriod, UsageSummary};
use argonauts_core::display_names::DisplayNames;
use ratatui::style::Style;
use ratatui::text::Span;
use time::{Date, Duration};

use super::cards::period_words;
use super::shades::{ModelColors, shown_as};
use super::stats::draw::{self, Fill};
use super::stats::letter;
use crate::format::{self, Figures};
use crate::kit::legend::{self, BEFORE_NUMBERS, BEFORE_SHARE, BETWEEN_WORDS, SHARE_CELLS};
use crate::kit::logo::{ModelLabel, NoPicture};
use crate::kit::{Badge, Drawn, Kit, Level, fit, spread};
use crate::text::{At, cut_spans, spans_width, str_width, truncate_spans};
use crate::theme::Role;

/// From this share of the day's limit on, the spend shows amber.
const NEAR_LIMIT: f64 = 0.9;
/// How a row with no model reads where nothing says which one its CLI runs.
const DEFAULT_MODEL: &str = "default model";
/// The fewest cells the model's column keeps: a few letters of its name.
const LEAST_MODEL: usize = 6;
/// The swatch's cells, in the model's shape.
const SWATCH: usize = 2;
/// Blanks after the swatch.
const AFTER_SWATCH: usize = 2;
/// A day's cells in the week's column, where its bar runs across in half cells.
const DAY_CELLS: usize = 3;
/// A day's cells where the card is too narrow for [`DAY_CELLS`].
const NARROW_DAY_CELLS: usize = 1;
/// Said once, in the week's column, while no model spent anything this week.
const NOTHING_THIS_WEEK: &str = "nothing spent this week";

/// The cells of the week's column with days `day` cells wide, a blank apart.
fn week_cells(day: usize) -> usize {
    7 * day + 6
}

/// The blanks before the week's column with days `day` cells wide: a column of bars' room,
/// and more where a day's letter, which sits in the middle of its day, would stand nearer the
/// tokens than a column of words may.
fn before_week(day: usize) -> usize {
    BEFORE_NUMBERS.max(BETWEEN_WORDS.saturating_sub(day / 2))
}

/// Where the table's columns fall, each as wide as its longest entry or its head. A column
/// that has given way to a narrow card is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    name: usize,
    /// The logo's cells and the blank after them, or none where logos draw no picture.
    logo: usize,
    on: Option<usize>,
    spend: usize,
    share: bool,
    tokens: Option<usize>,
    /// Each day's cells.
    day: Option<usize>,
}

impl Layout {
    fn width(&self) -> usize {
        let mut cells = SWATCH + AFTER_SWATCH + self.name;
        if let Some(on) = self.on {
            cells += BETWEEN_WORDS + self.logo + on;
        }
        cells += BEFORE_NUMBERS + self.spend;
        if self.share {
            cells += BEFORE_SHARE + SHARE_CELLS;
        }
        if let Some(tokens) = self.tokens {
            cells += BEFORE_NUMBERS + tokens;
        }
        if let Some(day) = self.day {
            cells += before_week(day) + week_cells(day);
        }
        cells
    }

    /// The layout that fits in `width` cells: the days narrow to one cell first, then the
    /// columns give way, the week first, then the CLI, the tokens and the share. Where even
    /// the model and its spend don't fit, the model's name is cut.
    fn fitting(self, width: usize) -> Self {
        let gives_way: [fn(&mut Self); 5] = [
            |layout| layout.day = Some(NARROW_DAY_CELLS),
            |layout| layout.day = None,
            |layout| layout.on = None,
            |layout| layout.tokens = None,
            |layout| layout.share = false,
        ];
        let mut layout = self;
        for give_way in gives_way {
            if layout.width() <= width {
                return layout;
            }
            give_way(&mut layout);
        }
        let over = layout.width().saturating_sub(width);
        layout.name = layout.name.saturating_sub(over).max(LEAST_MODEL);
        layout
    }
}

/// What each column of a line holds.
#[derive(Debug, Default)]
struct Cells {
    swatch: Vec<Span<'static>>,
    name: Vec<Span<'static>>,
    logo: Vec<Span<'static>>,
    on: Vec<Span<'static>>,
    spend: Vec<Span<'static>>,
    share: Vec<Span<'static>>,
    tokens: Vec<Span<'static>>,
    week: Vec<Span<'static>>,
}

/// `cells` laid out in `layout`'s columns: words to the left of theirs, numbers to the
/// right, and the room between them the shared rule's.
fn line(layout: &Layout, cells: Cells) -> Vec<Span<'static>> {
    let blank = |cells: usize| Span::raw(" ".repeat(cells));
    let mut out = fit(cells.swatch, SWATCH);
    out.push(blank(AFTER_SWATCH));
    out.extend(fit(
        cut_spans(&cells.name, layout.name, At::Word),
        layout.name,
    ));
    if let Some(on) = layout.on {
        out.push(blank(BETWEEN_WORDS));
        out.extend(fit(cells.logo, layout.logo));
        out.extend(fit(cut_spans(&cells.on, on, At::Word), on));
    }
    out.push(blank(BEFORE_NUMBERS));
    out.extend(right(cells.spend, layout.spend));
    if layout.share {
        out.push(blank(BEFORE_SHARE));
        out.extend(right(cells.share, SHARE_CELLS));
    }
    if let Some(tokens) = layout.tokens {
        out.push(blank(BEFORE_NUMBERS));
        out.extend(right(cells.tokens, tokens));
    }
    if let Some(day) = layout.day.filter(|_| !cells.week.is_empty()) {
        out.push(blank(before_week(day)));
        out.extend(cut_spans(&cells.week, week_cells(day), At::Word));
    }
    out
}

/// `spans` right-aligned in `cells`.
fn right(spans: Vec<Span<'static>>, cells: usize) -> Vec<Span<'static>> {
    let used = spans_width(&spans);
    if used > cells {
        return truncate_spans(&spans, cells);
    }
    let mut out = vec![Span::raw(" ".repeat(cells - used))];
    out.extend(spans);
    out
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
    /// It ran on a server on this machine.
    local: bool,
    /// What it spent on each day of this week, in the units of [`units`].
    week: [u64; 7],
}

impl Row {
    fn add(&mut self, more: &Row) {
        self.spent = self.spent.plus(&more.spent);
        self.pays_per_token |= more.pays_per_token;
        self.local &= more.local;
        for (day, spent) in self.week.iter_mut().zip(more.week) {
            *day += spent;
        }
    }
}

/// Dollars as a whole number a bar can share out: ten-thousandths.
fn units(usd: f64) -> u64 {
    (usd.max(0.0) * 10_000.0).round() as u64
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
                    local: true,
                    week: [0; 7],
                });
                rows.len() - 1
            })
        };
        for share in &self.usage.by_agent {
            let at = row_of(&mut rows, &share.agent, share.model.as_deref());
            let row = &mut rows[at];
            row.spent = row.spent.plus(&share.spent);
            row.pays_per_token |= share.pays_per_token;
            row.local &= share.local;
            for day in &share.this_week {
                let at = (day.day - week_start).whole_days();
                if let Some(slot) = usize::try_from(at).ok().and_then(|at| row.week.get_mut(at)) {
                    *slot += units(day.spent.cost_usd);
                }
            }
        }
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
        let shades = ModelColors::for_screen(
            &self.usage.all_time_models,
            self.names,
            shown.map(|row| (row.agent.as_str(), row.model.as_deref())),
        );
        let fills: Vec<Fill> = rows
            .iter()
            .map(|row| {
                let (agent, model) = (row.agent.as_str(), row.model.as_deref());
                let style = shades.style(kit, agent, model);
                Fill::model(kit, style, shades.place(agent, model))
            })
            .collect();
        let halves: Vec<Option<&'static str>> = rows
            .iter()
            .map(|row| shades.half_texture(kit, &row.agent, row.model.as_deref()))
            .collect();
        let bar = self.bar(kit, &rows, &fills, width);
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
        for line in self.table(kit, &rows, &fills, &halves, width) {
            body.push(line);
        }
        body.push(Vec::new());
        body.push(self.billing(kit, &rows, width));
        body
    }

    /// Each model's share of the spend, in its texture. Against a limit for the day, today's
    /// bar is the limit, and what's unspent stays track.
    fn bar(&self, kit: &Kit<'_>, rows: &[Row], fills: &[Fill], width: usize) -> Vec<Span<'static>> {
        let mut parts: Vec<(u64, Fill)> = rows
            .iter()
            .zip(fills)
            .map(|(row, fill)| (units(row.spent.cost_usd), *fill))
            .collect();
        let spent: u64 = parts.iter().map(|(spent, _)| spent).sum();
        let limit = self.usage.spend_limit.map(units).unwrap_or(0);
        if self.usage.period == UsagePeriod::Today && limit > spent {
            parts.push((limit - spent, kit.fg(Role::Faint).into()));
        }
        draw::stacked(kit, &parts, width)
    }

    fn tokens(&self, count: u64) -> String {
        Figures::for_tokens_of(self.usage.total.total_tokens()).tokens(count)
    }

    /// The column heads, then a row a model.
    fn table(
        &self,
        kit: &Kit<'_>,
        rows: &[Row],
        fills: &[Fill],
        halves: &[Option<&'static str>],
        width: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let total = units(self.usage.total.cost_usd);
        let tokens = |row: &Row| self.tokens(row.spent.total_tokens());
        let local = |row: &Row| row.local && row.spent.cost_usd == 0.0;
        let spend = |row: &Row| {
            if local(row) {
                "local".to_owned()
            } else {
                format::spend(&row.spent)
            }
        };
        let on = |row: &Row| ModelLabel::cli(self.names, &row.agent, kit.theme, NoPicture::Nothing);
        let widest = |head: &str, of: &dyn Fn(&Row) -> usize| {
            rows.iter().map(of).max().unwrap_or(0).max(str_width(head))
        };
        let layout = Layout {
            name: widest("model", &|row| str_width(&row.name)),
            logo: rows
                .iter()
                .map(|row| spans_width(&on(row).logo))
                .max()
                .unwrap_or(0),
            on: Some(widest("on", &|row| str_width(&on(row).name))),
            spend: widest("spend", &|row| str_width(&spend(row))),
            share: true,
            tokens: Some(widest("tokens", &|row| str_width(&tokens(row)))),
            day: Some(DAY_CELLS),
        }
        .fitting(width);
        // One scale for every model's days, so the rows compare.
        let fullest_day = rows.iter().flat_map(|row| row.week).max().unwrap_or(0);
        let mut lines = vec![line(
            &layout,
            Cells {
                name: vec![kit.dim("model")],
                logo: Vec::new(),
                on: vec![kit.dim("on")],
                spend: vec![kit.dim("spend")],
                tokens: vec![kit.dim("tokens")],
                week: layout
                    .day
                    .map(|day| self.letters(kit, day))
                    .unwrap_or_default(),
                ..Cells::default()
            },
        )];
        for (at, (row, fill)) in rows.iter().zip(fills).enumerate() {
            let shape = fill.texture.unwrap_or(kit.glyphs.used);
            let week = match layout.day {
                Some(_) if fullest_day == 0 && at == 0 => vec![kit.dim(NOTHING_THIS_WEEK)],
                Some(_) if fullest_day == 0 => Vec::new(),
                Some(day) => {
                    let look = DayLook {
                        shape,
                        half: halves.get(at).copied().flatten(),
                        style: fill.style,
                        cells: day,
                    };
                    self.week(kit, row, fullest_day, look)
                }
                None => Vec::new(),
            };
            let label = on(row);
            lines.push(line(
                &layout,
                Cells {
                    swatch: vec![Span::styled(shape.repeat(SWATCH), fill.style)],
                    name: vec![Span::raw(row.name.clone())],
                    logo: label.logo,
                    on: vec![kit.dim(label.name)],
                    spend: vec![if local(row) {
                        kit.dim(spend(row))
                    } else {
                        kit.strong(spend(row))
                    }],
                    share: if total > 0 {
                        vec![kit.dim(legend::share(units(row.spent.cost_usd), total))]
                    } else {
                        Vec::new()
                    },
                    tokens: vec![Span::raw(tokens(row))],
                    week,
                },
            ));
        }
        lines
    }

    /// `S   M   T   W   T   F   S`: each day's letter over the middle of its cells, today's
    /// in the accent.
    fn letters(&self, kit: &Kit<'_>, day: usize) -> Vec<Span<'static>> {
        let (first, today) = (self.week_start(), self.today());
        let mut out = Vec::new();
        let mut used = 0;
        for at in 0..7 {
            let date = first + Duration::days(at as i64);
            let middle = at * (day + 1) + day / 2;
            out.push(Span::raw(" ".repeat(middle - used)));
            let letter = letter(date.weekday()).to_string();
            out.push(if date == today {
                Span::styled(letter, kit.bold(Role::Accent))
            } else {
                kit.dim(letter)
            });
            used = middle + 1;
        }
        out.push(Span::raw(" ".repeat(week_cells(day) - used)));
        out
    }

    /// A bar for each day of this week, a blank cell apart, running across the day's cells in
    /// the model's shape: a day with no spend on the faint line, and a day still ahead blank.
    fn week(
        &self,
        kit: &Kit<'_>,
        row: &Row,
        fullest_day: u64,
        look: DayLook,
    ) -> Vec<Span<'static>> {
        let (first, today) = (self.week_start(), self.today());
        let mut cells = Vec::new();
        for (at, spent) in row.week.iter().enumerate() {
            if at > 0 {
                cells.push(Span::raw(" "));
            }
            let ahead = first + Duration::days(at as i64) > today;
            cells.extend(match (ahead, *spent) {
                (true, _) => vec![Span::raw(" ".repeat(look.cells))],
                (false, 0) => {
                    let line = kit.glyphs.columns[0].repeat(look.cells);
                    vec![Span::styled(line, kit.fg(Role::Faint))]
                }
                (false, spent) => look.bar(spent, fullest_day),
            });
        }
        cells
    }

    /// `on plans ~$0.26 (inside your plans, not charged) · on API keys $0.00`, then
    /// `local $0.00` where a model ran on this machine, and what `~` means where a number
    /// carries it, cut to `width`.
    fn billing(&self, kit: &Kit<'_>, rows: &[Row], width: usize) -> Vec<Span<'static>> {
        let sum = |keep: &dyn Fn(&Row) -> bool| -> Spend {
            let kept = rows.iter().filter(|row| keep(row));
            kept.fold(Spend::default(), |sum, row| sum.plus(&row.spent))
        };
        let on_plans = sum(&|row| !row.local && !row.pays_per_token);
        let on_keys = sum(&|row| !row.local && row.pays_per_token);
        let inside = if on_plans.cost_usd > 0.0 {
            " (inside your plans, not charged)"
        } else {
            ""
        };
        let mut line = vec![
            kit.dim("on plans "),
            kit.strong(format::spend(&on_plans)),
            kit.dim(format!("{inside} · on API keys ")),
            kit.strong(format::spend(&on_keys)),
        ];
        if rows.iter().any(|row| row.local) {
            line.push(kit.dim(" · local "));
            line.push(kit.strong(format::spend(&sum(&|row| row.local))));
        }
        let estimated = |row: &Row| row.spent.cost_estimated && row.spent.cost_usd > 0.0;
        if rows.iter().any(estimated) {
            line.push(kit.dim(" · ~ estimated at list prices"));
        }
        truncate_spans(&line, width)
    }
}

/// How a row's days draw: its model's shape, the left half of it a bar can end in, its
/// colour, and each day's cells.
#[derive(Debug, Clone, Copy)]
struct DayLook {
    shape: &'static str,
    half: Option<&'static str>,
    style: Style,
    cells: usize,
}

impl DayLook {
    /// A day that spent `spent` of the week's fullest `top`, in half cells across the day's
    /// cells, at least one half for any spend.
    fn bar(&self, spent: u64, top: u64) -> Vec<Span<'static>> {
        let most = 2 * self.cells as u64;
        let halves = (spent.saturating_mul(most) + top / 2) / top.max(1);
        let mut halves = halves.clamp(1, most) as usize;
        // A shape with no half rounds up to a whole cell.
        if self.half.is_none() && halves % 2 == 1 {
            halves += 1;
        }
        let mut out = Vec::new();
        if halves >= 2 {
            out.push(Span::styled(self.shape.repeat(halves / 2), self.style));
        }
        if let Some(half) = self.half.filter(|_| halves % 2 == 1) {
            out.push(Span::styled(half, self.style));
        }
        let drawn = halves.div_ceil(2);
        if drawn < self.cells {
            out.push(Span::raw(" ".repeat(self.cells - drawn)));
        }
        out
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

#[cfg(test)]
#[path = "spend_tests.rs"]
mod tests;
