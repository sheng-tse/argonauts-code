//! The view Stats opens in, by CLI: every CLI together, one card for each CLI with its
//! models as bars, and who did what in each role.

use argonauts_core::api::{UsagePeriod, UsageSummary};
use argonauts_core::crew_stats::CrewRole;
use argonauts_core::limits::{QuotaReading, QuotaStatus};
use argonauts_core::settings::stats::StatsMeasure;
use ratatui::style::Style;
use ratatui::text::Span;

use super::data::{Cli, Crew, Model};
use super::draw::{self, Entry, Fared};
use crate::format;
use crate::overview::cards::{used_of, window_label};
use crate::text::{spans_width, str_width, truncate_spans};
use crate::theme::Role;
use crate::widgets::{Drawn, Frame, Header, Kit, Level, fit, spread};

/// The chart's width: ten columns two cells wide, a cell apart.
const CHART: usize = 30;
/// Cells between the chart and the bars beside it.
const CHART_GAP: usize = 2;
/// The chart's rows of columns, under them its labels.
const CHART_ROWS: usize = 8;
/// From this inner width the all-together card draws its days chart.
const CHART_FROM: usize = 64;
/// The narrowest a CLI's card is drawn before the cards take a row each.
const LEAST_CLI_CARD: usize = 34;
/// Cells between two cards side by side.
const CARD_GAP: usize = 2;
/// The fewest cells a card's column of model names takes, and the most: a longer name is cut
/// with `…`, still one blank before its bar.
const LEAST_NAME: usize = 16;
const MOST_NAME: usize = 19;
/// The narrowest a model's bar is drawn with the units of its count beside it, and without.
const LEAST_MODEL_BAR: usize = 8;
const LEAST_BARE_BAR: usize = 4;
/// Blank cells a card keeps after its model rows' words when they say what they count.
const SPARE: usize = 2;
/// Cells a card's row of how work fared gives its label.
const OUTCOME_LABEL: usize = 9;
/// The who-did-what card's column of role names.
const ROLE_COLUMN: usize = 16;
/// Squares before they stand for more than one task each.
pub(super) const SQUARES: usize = 10;

/// What the cards draw from.
pub(super) struct Stats<'a> {
    pub crew: &'a Crew,
    pub usage: &'a UsageSummary,
    pub measure: StatsMeasure,
}

/// Tokens in millions with two decimals, the second dropped when it's a zero: `5.0M`,
/// `1.55M`, `0.8M`. From a thousand million on they're billions as the Overview writes them,
/// `99.9b`. Every count beside them keeps the unit, so a small one reads `<0.01M`.
pub(super) fn millions(count: u64) -> String {
    in_millions(count, 2)
}

/// [`millions`] with one decimal, as the opened cards and the tables write tokens: `27.5M`,
/// `0.9M`.
pub(super) fn millions_short(count: u64) -> String {
    in_millions(count, 1)
}

fn in_millions(count: u64, decimals: usize) -> String {
    if count >= 1_000_000_000 {
        return format!("{:.1}b", count as f64 / 1_000_000_000.0);
    }
    let text = format!("{:.*}", decimals, count as f64 / 1_000_000.0);
    if count > 0 && text.bytes().all(|byte| byte == b'0' || byte == b'.') {
        return format!("<0.{}1M", "0".repeat(decimals - 1));
    }
    let text = match decimals {
        2 => text.strip_suffix('0').unwrap_or(&text),
        _ => &text,
    };
    format!("{text}M")
}

/// `1 task`, `24 tasks`, `14 sign-offs`.
pub(super) fn count(n: u32, unit: &str) -> String {
    match n {
        1 => format!("1 {unit}"),
        n => format!("{n} {unit}s"),
    }
}

/// A share, as `58%`.
pub(super) fn share(part: u64, whole: u64) -> String {
    format::percent(part as f64 / whole.max(1) as f64)
}

/// `4m 12s`, `3m 05s`, `41s`, `12h 40m`: the seconds take two digits beside minutes, so a
/// column of them lines up.
pub(super) fn took(secs: u64) -> String {
    match secs {
        3600.. => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
        60.. => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{secs}s"),
    }
}

/// Bold numbers and dim words, as a header's right side writes them, giving up pieces from
/// the end until they fit `room`.
pub(super) fn numbers(kit: &Kit<'_>, pieces: &[(String, &str)], room: usize) -> Vec<Span<'static>> {
    for keep in (1..=pieces.len()).rev() {
        let mut spans = Vec::new();
        for (k, (number, words)) in pieces[..keep].iter().enumerate() {
            if k > 0 {
                spans.push(kit.dim(" · "));
            }
            spans.push(kit.strong(number.clone()));
            if !words.is_empty() {
                spans.push(kit.dim(format!(" {words}")));
            }
        }
        if spans_width(&spans) <= room {
            return spans;
        }
    }
    Vec::new()
}

/// A card's first row: its mark and title at the left, the numbers at the right.
pub(super) fn header_row(
    kit: &Kit<'_>,
    left: Vec<Span<'static>>,
    pieces: &[(String, &str)],
    width: usize,
) -> Vec<Span<'static>> {
    let room = width.saturating_sub(spans_width(&left) + 2);
    spread(left, numbers(kit, pieces, room), width)
}

/// The model's color: its shade of its CLI's.
pub(super) fn model_style(kit: &Kit<'_>, cli: &Cli, model: &Model) -> Style {
    draw::shade(kit, cli.role(), model.shade)
}

impl Stats<'_> {
    /// Tokens as the tab writes them. From a crew total of a million on, every count is in
    /// millions, so the numbers side by side read on one scale: `5.0M`, `1.55M`, `0.55M`.
    pub fn tokens(&self, count: u64) -> String {
        if self.crew.total.total_tokens() < 1_000_000 {
            return format::tokens(count);
        }
        millions(count)
    }

    /// [`Stats::tokens`] with one decimal in millions, as the opened cards and the tables
    /// write them.
    pub fn tokens_short(&self, count: u64) -> String {
        if self.crew.total.total_tokens() < 1_000_000 {
            return format::tokens(count);
        }
        millions_short(count)
    }

    /// A measured amount as the bars' legends write it: `2.9M`, `24`, `$7.90`.
    pub fn amount(&self, value: u64) -> String {
        match self.measure {
            StatsMeasure::Tokens => self.tokens(value),
            StatsMeasure::Tasks => value.to_string(),
            StatsMeasure::Spend => format::usd(value as f64 / 100.0),
        }
    }

    /// Every CLI together: what the bars measure by CLI, what the tokens were, and how the
    /// tasks fared in review, a blank row between each, beside a chart of the period.
    pub fn all_together(&self, kit: &Kit<'_>, frame: Frame) -> Drawn {
        let inner = frame.inner();
        let crew = self.crew;
        let mut drawn = Drawn::new();
        let left = vec![
            kit.span(kit.glyphs.widget, Role::Card),
            Span::raw(" "),
            kit.strong("all together"),
            kit.dim(format!("  {}", self.period_words())),
        ];
        drawn.push(header_row(kit, left, &self.totals(), inner));
        drawn.push(Vec::new());

        let slots = crew.slots().len();
        let chart_width = CHART.max(slots * slot_width(slots));
        let chart = inner >= CHART_FROM;
        let right = if chart {
            inner - chart_width - CHART_GAP
        } else {
            inner
        };
        let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
        let measured = crew.measured(self.measure);
        rows.push(vec![kit.dim(format!(
            "{} by CLI · {}",
            self.measure.word(),
            self.amount(measured)
        ))]);
        let parts: Vec<(u64, Style)> = crew
            .clis
            .iter()
            .map(|cli| (cli.measured(self.measure), kit.fg(cli.role())))
            .collect();
        rows.push(draw::stacked(kit, &parts, right));
        let entries: Vec<Entry> = crew
            .clis
            .iter()
            .map(|cli| {
                let value = cli.measured(self.measure);
                Entry {
                    style: kit.fg(cli.role()),
                    name: cli.name.clone(),
                    amount: self.amount(value),
                    note: share(value, measured),
                }
            })
            .collect();
        rows.extend(draw::legend(kit, &entries, None, right));

        rows.push(Vec::new());
        let total = &crew.total;
        let tokens = total.total_tokens();
        let cached = total.cached_tokens;
        let new = total.input_tokens.saturating_sub(cached);
        let output = total.output_tokens;
        rows.push(vec![kit.dim("what those tokens were")]);
        let kinds = [
            (cached, Role::TokensCached, "from cache"),
            (new, Role::TokensInput, "new input"),
            (output, Role::TokensOutput, "output"),
        ];
        let parts: Vec<(u64, Style)> = kinds
            .iter()
            .map(|(n, role, _)| (*n, kit.fg(*role)))
            .collect();
        rows.push(draw::stacked(kit, &parts, right));
        let entries: Vec<Entry> = kinds
            .iter()
            .map(|(n, role, name)| Entry {
                style: kit.fg(*role),
                name: (*name).to_owned(),
                amount: self.tokens(*n),
                note: share(*n, tokens),
            })
            .collect();
        rows.extend(draw::legend(kit, &entries, None, right));

        rows.push(Vec::new());
        let fared = crew.fared;
        if fared.total() > 0 {
            rows.push(vec![kit.dim(format!(
                "how the {} fared in review",
                count(fared.total(), "task")
            ))]);
            rows.push(fared_bar(kit, fared, right));
            let entries = [
                (fared.first, Role::Success, "passed first time"),
                (fared.once, Role::Warning, "sent back once"),
                (fared.more, Role::Error, "sent back twice"),
            ]
            .into_iter()
            .map(|(n, role, name)| Entry {
                style: kit.fg(role),
                name: name.to_owned(),
                amount: n.to_string(),
                note: String::new(),
            })
            .collect::<Vec<_>>();
            rows.extend(draw::legend(kit, &entries, None, right));
        } else {
            rows.push(vec![kit.dim("no task reviewed yet")]);
        }

        if chart {
            let columns = self.chart(kit, rows.len().saturating_sub(1).min(CHART_ROWS));
            // The chart sits at the bottom, its labels on the last row.
            let pad = rows.len().saturating_sub(columns.len());
            let mut left: Vec<Vec<Span<'static>>> = vec![Vec::new(); pad];
            left.extend(columns);
            for (at, row) in rows.into_iter().enumerate() {
                let mut line = fit(
                    left.get(at).cloned().unwrap_or_default(),
                    chart_width + CHART_GAP,
                );
                line.extend(row);
                drawn.push(line);
            }
        } else {
            for row in rows {
                drawn.push(row);
            }
        }
        frame.draw(kit, drawn)
    }

    /// The period as the all-together card names it.
    fn period_words(&self) -> String {
        match self.crew.period {
            UsagePeriod::Today => "today".to_owned(),
            UsagePeriod::Week => {
                let first = self.crew.week_starts;
                format!("this week, {first} to {}", first.previous())
            }
            UsagePeriod::Month => "this month".to_owned(),
            UsagePeriod::All => "every day so far".to_owned(),
        }
    }

    /// `14 workflows · 63 tasks merged · 5.0M tokens · ~$11.20 · 12h 40m`.
    pub fn totals(&self) -> Vec<(String, &'static str)> {
        let crew = self.crew;
        let workflows = if crew.workflows == 1 {
            "workflow"
        } else {
            "workflows"
        };
        let mut pieces = vec![(crew.workflows.to_string(), workflows)];
        let merged = if crew.merged == 1 {
            "task merged"
        } else {
            "tasks merged"
        };
        pieces.push((crew.merged.to_string(), merged));
        pieces.push((self.tokens(crew.total.total_tokens()), "tokens"));
        if crew.total.cost_usd > 0.0 {
            pieces.push((format::spend(&crew.total), ""));
        }
        if crew.took_secs > 0 {
            pieces.push((took(crew.took_secs), ""));
        }
        pieces
    }

    /// The period's days or weeks as columns `rows` high, stacked by CLI from the bottom,
    /// over their labels: a week's days by their letters with today's in the accent, a
    /// month's by the dates its weeks start on, and weeks by the month each begins.
    fn chart(&self, kit: &Kit<'_>, rows: usize) -> Vec<Vec<Span<'static>>> {
        let crew = self.crew;
        let slots = crew.slots();
        let width = slot_width(slots.len());
        let columns: Vec<Vec<(u64, Style)>> = slots
            .iter()
            .map(|slot| {
                if slot.ahead {
                    return Vec::new();
                }
                crew.clis
                    .iter()
                    .map(|cli| {
                        let value = crew
                            .days
                            .iter()
                            .filter(|part| part.agent == cli.agent && slot.holds(part.day))
                            .map(|part| part.measured(self.measure))
                            .sum();
                        (value, kit.fg(cli.role()))
                    })
                    .collect()
            })
            .collect();
        let mut out = draw::columns(kit, &columns, rows, width);
        let mut labels: Vec<Span<'static>> = Vec::new();
        let mut used = 0;
        for (k, slot) in slots.iter().enumerate() {
            let label = match crew.period {
                UsagePeriod::All => {
                    let last = slot.first + time::Duration::days(slot.days - 1);
                    let begins = slot.first.day() == 1 || last.month() != slot.first.month();
                    match (k, begins) {
                        (0, _) => Some(short_month(slot.first.month())),
                        (_, true) => Some(short_month(last.month())),
                        _ => None,
                    }
                    .map(str::to_owned)
                }
                UsagePeriod::Month if width == 1 => {
                    (slot.first.day() % 7 == 1).then(|| slot.first.day().to_string())
                }
                _ => Some(letter(slot.first.weekday()).to_string()),
            };
            let at = k * width;
            // A label keeps a blank between it and the one before.
            let Some(label) = label.filter(|_| k == 0 || at > used) else {
                continue;
            };
            labels.push(Span::raw(" ".repeat(at - used)));
            used = at + str_width(&label);
            let today = slot.holds(crew.today) && crew.period != UsagePeriod::All;
            labels.push(if today {
                Span::styled(label, kit.bold(Role::Card))
            } else {
                kit.dim(label)
            });
        }
        out.push(labels);
        out
    }

    /// The CLIs' cards side by side, as many to a row as fit, two cells apart and all one
    /// height. `picked` is the card the keys are on.
    pub fn cli_cards(&self, kit: &Kit<'_>, width: usize, picked: Option<usize>) -> Vec<Drawn> {
        let clis = &self.crew.clis;
        if clis.is_empty() {
            return Vec::new();
        }
        let room = width.saturating_sub(2);
        let per_row = ((room + CARD_GAP) / (LEAST_CLI_CARD + CARD_GAP)).clamp(1, clis.len());
        let card = (room - CARD_GAP * (per_row - 1)) / per_row;
        let inner = card.saturating_sub(4);
        let layout = self.model_layout(inner);
        let mut out = Vec::new();
        for (row_at, row) in clis.chunks(per_row).enumerate() {
            let bodies: Vec<Drawn> = row
                .iter()
                .map(|cli| self.cli_body(kit, cli, inner, &layout))
                .collect();
            let height = bodies.iter().map(Drawn::height).max().unwrap_or(0);
            let mut line = Drawn::new();
            let mut at = 1;
            for (k, mut body) in bodies.into_iter().enumerate() {
                while body.height() < height {
                    body.push(Vec::new());
                }
                let frame = Frame {
                    left: 0,
                    width: card,
                    toggle: None,
                    active: picked == Some(row_at * per_row + k),
                };
                let drawn = frame.draw(kit, body);
                line = if k == 0 {
                    let mut first = drawn;
                    first.indent(1);
                    first
                } else {
                    crate::widgets::side_by_side(line, drawn, at)
                };
                at += card + CARD_GAP;
            }
            out.push(line);
        }
        out
    }

    /// How every card lays out its models' rows, alike so their bars line up: the names'
    /// column, the bars' width, and whether a count says what it counts.
    fn model_layout(&self, inner: usize) -> ModelLayout {
        let models = || self.crew.clis.iter().flat_map(|cli| cli.models.iter());
        let names = models()
            .map(|model| str_width(&model.name) + 2)
            .max()
            .unwrap_or(0);
        let names = (names + 1).clamp(LEAST_NAME, MOST_NAME);
        let widest = |units: bool| {
            models()
                .map(|model| str_width(&self.model_amount(model, units)))
                .max()
                .unwrap_or(0)
        };
        let with_units = inner.saturating_sub(names + widest(true) + SPARE);
        if with_units >= LEAST_MODEL_BAR {
            return ModelLayout {
                names,
                bar: with_units,
                units: true,
            };
        }
        ModelLayout {
            names,
            bar: inner
                .saturating_sub(names + widest(false))
                .max(LEAST_BARE_BAR),
            units: false,
        }
    }

    /// The width of `cli`'s model bars. Counts that say what they count leave each card's
    /// bars the room its own words leave, with a little to spare; bare counts keep every
    /// card's bars alike, so they line up.
    fn model_bar(&self, cli: &Cli, inner: usize, layout: &ModelLayout) -> usize {
        if !layout.units {
            return layout.bar;
        }
        let own = cli
            .models
            .iter()
            .map(|model| str_width(&self.model_amount(model, true)))
            .max()
            .unwrap_or(0);
        inner
            .saturating_sub(layout.names + own + SPARE)
            .max(LEAST_MODEL_BAR)
    }

    /// `  3.24M · 2 tasks` with the units, ` 2.1M · 24` without. Measured in tasks, the
    /// count comes first and the tokens after it.
    fn model_amount(&self, model: &Model, units: bool) -> String {
        let role = model.main_role();
        let did = role.map_or(0, |role| model.did(role));
        let did = match role {
            Some(role) if units => {
                let unit = match role {
                    CrewRole::Judge => "review",
                    role => role.unit(),
                };
                count(did, unit)
            }
            _ => did.to_string(),
        };
        let gap = if units { "  " } else { " " };
        match self.measure {
            StatsMeasure::Tasks => format!("{gap}{did} · {}", self.tokens(model.tokens())),
            _ => format!("{gap}{} · {did}", self.amount(model.measured(self.measure))),
        }
    }

    /// One CLI: its tokens, its work and plan, each model a thin bar, how its tasks and its
    /// reviews fared, its pace over the period, and its plan's windows.
    fn cli_body(&self, kit: &Kit<'_>, cli: &Cli, inner: usize, layout: &ModelLayout) -> Drawn {
        let mut drawn = Drawn::new();
        let dot = if cli.main { "●" } else { "○" };
        let left = vec![
            kit.span(dot, cli.role()),
            Span::raw(" "),
            kit.strong(cli.name.clone()),
        ];
        let tokens = self.tokens(cli.tokens());
        drawn.push(spread(
            left,
            vec![kit.strong(tokens), kit.dim(" tok")],
            inner,
        ));

        // A judge's sign-off is a review too, so a CLI that reviews and judges counts both.
        let mut did = Vec::new();
        let tasks = cli.did(CrewRole::Worker);
        let reviews = cli.did(CrewRole::Reviewer) + cli.did(CrewRole::Judge);
        if tasks > 0 {
            did.push(count(tasks, "task"));
        }
        if reviews > 0 {
            did.push(count(reviews, "review"));
        }
        if did.is_empty() {
            did.push("no tasks yet".to_owned());
        }
        let spend = format::spend(&cli.spent());
        let right = match self.plan_name(&cli.agent) {
            Some(plan) => format!("{plan} · {spend}"),
            None => spend,
        };
        let right = vec![kit.dim(right)];
        let room = inner.saturating_sub(spans_width(&right) + 1);
        let left = truncate_spans(&[kit.dim(did.join(" · "))], room);
        drawn.push(spread(left, right, inner));
        drawn.push(Vec::new());

        let whole = cli.measured(self.measure);
        let bar = self.model_bar(cli, inner, layout);
        for model in &cli.models {
            let style = model_style(kit, cli, model);
            let mut row = draw::swatch(kit, style);
            row.push(Span::raw(model.name.clone()));
            // A name cut to its column keeps a blank before the bar.
            let row = truncate_spans(&row, layout.names - 1);
            let mut row = fit(row, layout.names);
            row.extend(draw::thin(
                kit,
                model.measured(self.measure),
                whole,
                bar,
                style,
            ));
            row.push(kit.dim(self.model_amount(model, layout.units)));
            drawn.push(truncate_spans(&row, inner));
        }
        drawn.push(Vec::new());

        let back = cli.sent_back(CrewRole::Reviewer) + cli.sent_back(CrewRole::Judge);
        let accepted = Fared {
            first: reviews.saturating_sub(back),
            once: back.min(reviews),
            more: 0,
        };
        drawn.push(outcome_row(kit, "tasks", cli.fared(), "first pass"));
        drawn.push(outcome_row(kit, "reviews", accepted, "accepted"));

        let work = self
            .crew
            .series(StatsMeasure::Tasks, |part| part.agent == cli.agent);
        let top = self
            .crew
            .fullest(StatsMeasure::Tasks, |part| Some(part.agent.clone()));
        let mut line = draw::spark(kit, &work, top, kit.fg(cli.role()));
        let mut words = format!(" {}", self.span_words());
        let (n, secs, unit) = if tasks > 0 {
            (tasks, cli.took(CrewRole::Worker), "a task")
        } else {
            let secs = cli.took(CrewRole::Reviewer) + cli.took(CrewRole::Judge);
            (reviews, secs, "a review")
        };
        if n > 0 && secs > 0 {
            words.push_str(&format!(" · {} {unit}", took(secs / u64::from(n))));
        }
        line.push(kit.dim(words));
        drawn.push(truncate_spans(&line, inner));
        drawn.push(self.plan_line(kit, cli, inner));
        drawn
    }

    /// The period as a card's pace names it: `weeks`, `this week`.
    fn span_words(&self) -> &'static str {
        match self.crew.period {
            UsagePeriod::Today => "today",
            UsagePeriod::Week => "this week",
            UsagePeriod::Month => "this month",
            UsagePeriod::All => "weeks",
        }
    }

    /// `5h ━━━━━━  1%   week ━━━━━━ 56%`: the CLI's plan windows, their bars as wide as the
    /// card leaves them, or why there is no reading.
    pub(super) fn plan_line(&self, kit: &Kit<'_>, cli: &Cli, inner: usize) -> Vec<Span<'static>> {
        let Some(plan) = self.usage.plans.iter().find(|plan| plan.agent == cli.agent) else {
            return vec![kit.dim("no plan reading yet")];
        };
        if plan.pays_per_token {
            return vec![kit.dim("pays per token")];
        }
        let bar = (inner.saturating_sub(10) / 4).clamp(4, 10);
        match self.plan_windows(kit, cli, bar) {
            Some(windows) => truncate_spans(&windows, inner),
            // The Usage tab's own words; its page says why.
            None if plan.failed.is_some() => vec![kit.dim("failed to load usage data")],
            None => vec![kit.dim("no plan reading yet")],
        }
    }

    fn plan_name(&self, agent: &str) -> Option<String> {
        self.usage
            .plans
            .iter()
            .find(|plan| plan.agent == agent)
            .and_then(|plan| plan.plan.clone())
    }

    /// Each role with every model that filled it, a bar of what each did in it, and a
    /// legend of each, a blank row between roles. `picked` is the role the keys are on.
    pub fn who_did_what(&self, kit: &Kit<'_>, frame: Frame, picked: Option<CrewRole>) -> Drawn {
        let inner = frame.inner();
        let mut drawn = Drawn::new();
        let left = Header::new("who did what", Role::Card).left(kit);
        let note = vec![kit.dim("every model that filled the role · enter opens a role")];
        let room = inner.saturating_sub(spans_width(&left) + 2);
        drawn.push(spread(left, truncate_spans(&note, room), inner));
        let bar_cells = inner.saturating_sub(ROLE_COLUMN);
        for role in self.crew.roles() {
            drawn.push(Vec::new());
            let filled = self.filled_by(role);
            let parts: Vec<(u64, Style)> = filled
                .iter()
                .map(|(cli, model)| (u64::from(model.did(role)), model_style(kit, cli, model)))
                .collect();
            let label = if picked == Some(role) {
                vec![Span::styled(
                    format!("❯ {}", role.plural()),
                    kit.bold(Role::Card),
                )]
            } else {
                vec![Span::raw(role.plural())]
            };
            let mut row = fit(label, ROLE_COLUMN);
            row.extend(draw::stacked(kit, &parts, bar_cells));
            drawn.push(row);

            let did = self.crew.did(role);
            let single = filled.len() == 1;
            let entries: Vec<Entry> = filled
                .iter()
                .map(|(cli, model)| Entry {
                    style: model_style(kit, cli, model),
                    name: model.name.clone(),
                    amount: model.did(role).to_string(),
                    note: if single {
                        units(model.did(role), role.unit())
                    } else {
                        String::new()
                    },
                })
                .collect();
            let tail = (!single && did > 0).then(|| format!("of {}", count(did, role.unit())));
            for line in draw::legend(kit, &entries, tail, bar_cells) {
                let mut row = vec![Span::raw(" ".repeat(ROLE_COLUMN))];
                row.extend(line);
                drawn.push(row);
            }
        }
        if self.crew.roles().is_empty() {
            drawn.push(vec![kit.dim("No work recorded in this period yet.")]);
        }
        frame.draw(kit, drawn)
    }
}

/// Cells a column of the chart takes with the blank after it: two and one while the
/// columns fit the chart that way, else one.
fn slot_width(slots: usize) -> usize {
    if slots * 3 <= CHART + 1 { 3 } else { 1 }
}

/// `workflows` after `14`, `task` after `1`.
fn units(n: u32, unit: &str) -> String {
    if n == 1 {
        unit.to_owned()
    } else {
        format!("{unit}s")
    }
}

/// The weekday's letter: `S M T W T F S`.
fn letter(day: time::Weekday) -> char {
    match day {
        time::Weekday::Monday => 'M',
        time::Weekday::Tuesday | time::Weekday::Thursday => 'T',
        time::Weekday::Wednesday => 'W',
        time::Weekday::Friday => 'F',
        time::Weekday::Saturday | time::Weekday::Sunday => 'S',
    }
}

/// `Aug`.
fn short_month(month: time::Month) -> &'static str {
    match month {
        time::Month::January => "Jan",
        time::Month::February => "Feb",
        time::Month::March => "Mar",
        time::Month::April => "Apr",
        time::Month::May => "May",
        time::Month::June => "Jun",
        time::Month::July => "Jul",
        time::Month::August => "Aug",
        time::Month::September => "Sep",
        time::Month::October => "Oct",
        time::Month::November => "Nov",
        time::Month::December => "Dec",
    }
}

/// A plan window as a meter in the CLI's color and its share used, amber from 90% and red
/// with `out` once it's spent.
pub(super) fn window_meter(
    kit: &Kit<'_>,
    cli: &Cli,
    reading: &QuotaReading,
    cells: usize,
) -> Vec<Span<'static>> {
    let used = used_of(reading);
    let level = if used >= 1.0 {
        Level::Danger
    } else if used >= 0.9 {
        Level::Warn
    } else {
        Level::Ok
    };
    let fill = match level {
        Level::Ok => kit.fg(cli.role()),
        level => kit.fg(level.number_role()),
    };
    let percent = if reading.status == QuotaStatus::Rejected {
        "out".to_owned()
    } else {
        format::percent(used)
    };
    let mut meter = draw::meter(kit, (used * 1000.0) as u64, 1000, cells, fill);
    meter.push(Span::styled(
        format!(" {percent:>4}"),
        kit.style(level.number_role()),
    ));
    meter
}

/// The window's name as the Usage tab says it: `5h`, `week`.
pub(super) fn window_name(reading: &QuotaReading) -> String {
    match window_label(reading).as_str() {
        "7d" => "week".to_owned(),
        other => other.to_owned(),
    }
}

/// A plan window as a thin bar `cells` wide in the CLI's color and its share used after a
/// blank, amber from 90% and red with `out` once it's spent.
pub(super) fn window_bar(
    kit: &Kit<'_>,
    cli: &Cli,
    reading: &QuotaReading,
    cells: usize,
) -> Vec<Span<'static>> {
    let used = used_of(reading);
    let level = if used >= 1.0 {
        Level::Danger
    } else if used >= 0.9 {
        Level::Warn
    } else {
        Level::Ok
    };
    let (fill, words) = match level {
        Level::Ok => (kit.fg(cli.role()), kit.style(Role::Dim)),
        level => (kit.fg(level.number_role()), kit.style(level.number_role())),
    };
    let percent = if reading.status == QuotaStatus::Rejected {
        "out".to_owned()
    } else {
        format::percent(used)
    };
    let mut bar = draw::thin(kit, (used * 1000.0).round() as u64, 1000, cells, fill);
    bar.push(Span::styled(format!(" {percent:>3}"), words));
    bar
}

/// How every card lays out its models' rows.
struct ModelLayout {
    /// Cells of the swatch and the name.
    names: usize,
    bar: usize,
    /// Each count says what it counts.
    units: bool,
}

/// `tasks    ▪▪▪▪▪▪▪▪▪▪ 94% first pass`, the same row on every card: ten small squares, the
/// share that went the first way, and what that share is. With none of the work it says so.
pub(super) fn outcome_row(
    kit: &Kit<'_>,
    label: &str,
    fared: Fared,
    words: &str,
) -> Vec<Span<'static>> {
    let mut row = vec![kit.dim(format!("{label:<OUTCOME_LABEL$}"))];
    if fared.total() == 0 {
        row.push(kit.dim("none"));
        return row;
    }
    row.extend(draw::tenths(kit, fared, SQUARES));
    row.push(Span::raw(" "));
    row.push(kit.strong(format::percent(fared.first_pass())));
    row.push(kit.dim(format!(" {words}")));
    row
}

/// How tasks fared in review, as one bar: green, amber, red.
pub(super) fn fared_bar(kit: &Kit<'_>, fared: Fared, cells: usize) -> Vec<Span<'static>> {
    let parts = [
        (u64::from(fared.first), kit.fg(Role::Success)),
        (u64::from(fared.once), kit.fg(Role::Warning)),
        (u64::from(fared.more), kit.fg(Role::Error)),
    ];
    draw::stacked(kit, &parts, cells)
}
