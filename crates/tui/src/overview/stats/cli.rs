//! The view Stats opens in, by CLI: every CLI together, one card for each CLI with its
//! models as bars, and who did what in each role.

use argonauts_core::api::{UsagePeriod, UsageSummary};
use argonauts_core::crew_stats::StatsRole;
use argonauts_core::limits::{PlanUsageReading, PlanUsageStatus};
use argonauts_core::logos::logo_for;
use argonauts_core::settings::stats::StatsMeasure;
use ratatui::text::Span;

use super::data::{Cli, Crew, DayPart, Model};
use super::draw::{self, Fared, Fill, Legend, Lit};
pub(super) use super::fills::{model_fill, model_style};
use super::grid::{self, Body};
use crate::format;
use crate::kit::legend::BEFORE_NUMBERS;
use crate::kit::logo::{ModelLabel, NoPicture};
use crate::kit::{Drawn, Frame, Header, Kit, Level, fit, spread};
use crate::overview::cards::{used_of, window_label};
use crate::text::{spans_width, str_width, truncate_spans};
use crate::theme::Role;

/// The chart's width: ten columns two cells wide, a cell apart.
const CHART: usize = 30;
/// Cells between the chart and the bars beside it.
const CHART_GAP: usize = 2;
/// The chart's rows of columns, under them its labels.
const CHART_ROWS: usize = 8;
/// From this inner width the all-together card draws its days chart.
const CHART_FROM: usize = 64;
/// The fewest cells a card's column of model names takes, and the most, a swatch and the
/// longest name with one blank before its bar: a longer name is cut with `…`.
const LEAST_NAME: usize = 16;
const MOST_NAME: usize = draw::MODEL_NAME + 3;
/// The narrowest a model's bar is drawn with the units of its count beside it, and without.
const LEAST_MODEL_BAR: usize = 8;
const LEAST_BARE_BAR: usize = 4;
/// Blank cells a card keeps after its model rows' words when they say what they count.
const SPARE: usize = 1;
/// Cells the labels of a card's rows of how work fared take: the longest, `reviews`, and the
/// blanks a meter or a count stands after its words.
const OUTCOME_LABEL: usize = "reviews".len() + BEFORE_NUMBERS;
/// The narrowest card inside that keeps `tasks` and its row whole with those labels, at
/// `100% first pass`. Narrower cards give their labels a blank less.
const ROOMY_CARD: usize = OUTCOME_LABEL + SQUARES + " 100% first pass".len();
/// The widest a plan window's bar is drawn on a card, and the cells of a row of two windows
/// beside their bars and the gap between them: `5h `, `week ` and a share after each.
const MOST_PLAN_BAR: usize = 12;
const PLAN_WORDS: usize = 18;
/// The who-did-what card's column of role names.
const ROLE_COLUMN: usize = 16;
/// Squares before they stand for more than one task each.
pub(super) const SQUARES: usize = 10;

/// What the cards draw from.
pub(super) struct Stats<'a> {
    pub crew: &'a Crew,
    pub usage: &'a UsageSummary,
    pub measure: StatsMeasure,
    /// The tokens of everything on this computer over every day, the crew's or not, when the
    /// Overview's numbers are at hand.
    pub on_this_computer: Option<u64>,
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

/// Tokens in millions with both decimals kept, as a table's column writes them, so its
/// numbers line up: `2.60M`, `0.07M`, `<0.01M`. From a thousand million on they're billions
/// with both decimals too, the `b` lower case as everywhere billions are written: `1.20b`.
pub(super) fn millions_fixed(count: u64) -> String {
    if count >= 1_000_000_000 {
        return format!("{:.2}b", count as f64 / 1_000_000_000.0);
    }
    let text = format!("{:.2}M", count as f64 / 1_000_000.0);
    if count > 0 && text == "0.00M" {
        return "<0.01M".to_owned();
    }
    text
}

/// `1 task`, `24 tasks`, `14 sign-offs`.
pub(super) fn count(n: u32, unit: &str) -> String {
    match n {
        1 => format!("1 {unit}"),
        n => format!("{n} {unit}s"),
    }
}

/// `4m 12s`, `3m 05s`, `6m`, `41s`, `12h 40m`: the seconds take two digits beside minutes, so
/// a column of them lines up, and whole minutes go without them.
pub(super) fn took(secs: u64) -> String {
    match secs {
        3600.. => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
        60.. if secs.is_multiple_of(60) => format!("{}m", secs / 60),
        60.. => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{secs}s"),
    }
}

/// Time summed over many sessions, to the minute: `6h 54m`, `42m`, and `41s` under one.
pub(super) fn took_whole(secs: u64) -> String {
    match secs {
        3600.. => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
        60.. => format!("{}m", secs / 60),
        _ => format!("{secs}s"),
    }
}

/// What one of `n` pieces of work cost: `$0.14`. Under two cents it keeps a third decimal,
/// `$0.014`, so less than a cent still reads as something.
pub(super) fn cost_each(cost_usd: f64, n: u32) -> String {
    let each = cost_usd / f64::from(n.max(1));
    if each > 0.0 && each < 0.02 {
        format!("${each:.3}")
    } else {
        format::usd(each)
    }
}

/// One stack of the chart's columns: its fill, and which of the period's parts are its own.
pub(super) struct Stack<'a> {
    pub fill: Fill,
    pub holds: &'a dyn Fn(&DayPart) -> bool,
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

/// [`numbers`] giving up pieces from the start instead, so the last ones stay.
fn last_numbers(kit: &Kit<'_>, pieces: &[(String, &str)], room: usize) -> Vec<Span<'static>> {
    for skip in 0..pieces.len() {
        let spans = numbers(kit, &pieces[skip..], usize::MAX);
        if spans_width(&spans) <= room {
            return spans;
        }
    }
    Vec::new()
}

/// [`header_row`] whose numbers give up their first pieces where the row is too narrow.
pub(super) fn header_row_keeping_last(
    kit: &Kit<'_>,
    left: Vec<Span<'static>>,
    pieces: &[(String, &str)],
    width: usize,
) -> Vec<Span<'static>> {
    let room = width.saturating_sub(spans_width(&left) + 2);
    spread(left, last_numbers(kit, pieces, room), width)
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

impl Stats<'_> {
    /// Tokens as the tab writes them. From a crew total of a million on, every count is in
    /// millions, so the numbers side by side read on one scale: `5.0M`, `1.55M`, `0.55M`.
    pub fn tokens(&self, count: u64) -> String {
        if self.crew.total.total_tokens() < 1_000_000 {
            return format::tokens(count);
        }
        millions(count)
    }

    /// Tokens as a table's column writes them: from a crew total of a million on, in
    /// [`millions_fixed`], so the column reads on one scale.
    pub fn table_tokens(&self, count: u64) -> String {
        if self.crew.total.total_tokens() < 1_000_000 {
            return format::tokens(count);
        }
        millions_fixed(count)
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
    /// tasks fared in review, a blank row between each, beside a chart of the period. A blank
    /// row under each bar, then a table of its parts, all three in the same columns: each part's swatch,
    /// name, amount and share, a CLI's logo first. `pointed` is the CLI the keys are on: its
    /// part stays lit in every bar and the chart, and the rest turns grey.
    pub fn all_together(&self, kit: &Kit<'_>, frame: Frame, pointed: Option<usize>) -> Drawn {
        let inner = frame.inner();
        let crew = self.crew;
        let mut drawn = Drawn::new();
        drawn.push(self.together_title(kit, inner));
        drawn.push(Vec::new());

        let right = self.beside_chart(inner);
        let lit = move |at: usize| pointed.is_none_or(|pointed| pointed == at);
        let pointed_cli = pointed.and_then(|at| crew.clis.get(at));
        let mut bars = Vec::new();

        let measured = crew.measured(self.measure);
        let clis = crew.clis.iter().enumerate().map(|(at, cli)| {
            let value = cli.measured(self.measure);
            let shown = if lit(at) { value } else { 0 };
            let fill = kit.fg(cli.role());
            Legend {
                logo: Some(logo_for(&cli.agent, None)),
                pointed: pointed == Some(at),
                ..Legend::of(
                    fill,
                    &cli.name,
                    (value, shown, measured),
                    self.table_amount(cli),
                )
            }
        });
        bars.push(Labelled {
            label: format!("{} by CLI · {}", self.measure.word(), self.amount(measured)),
            rows: clis.collect(),
            apart: true,
        });

        // The kinds of every CLI's tokens, of which the pointed CLI's stay lit.
        let names = ["from cache", "new input", "output"];
        let mut kinds = self.kinds(kit, &crew.total, names);
        if let Some(cli) = pointed_cli {
            let its = self.kinds(kit, &cli.spent(), names);
            for (kind, its) in kinds.iter_mut().zip(its) {
                kind.part.lit = its.part.value;
            }
        }
        bars.push(Labelled {
            label: "what those tokens were".to_owned(),
            rows: kinds,
            apart: false,
        });

        let fared = crew.fared;
        if fared.total() > 0 {
            let counts = |fared: Fared| [fared.first, fared.once, fared.more].map(u64::from);
            let all = counts(fared);
            let shown = pointed_cli.map_or(all, |cli| counts(cli.fared()));
            let whole = u64::from(fared.total());
            let named = [
                (Role::Success, "passed first time"),
                (Role::Warn, "sent back once"),
                (Role::Danger, "sent back twice"),
            ];
            let rows = (named.iter().enumerate()).map(|(k, (role, name))| {
                Legend::of(
                    kit.fg(*role),
                    name,
                    (all[k], shown[k], whole),
                    all[k].to_string(),
                )
            });
            bars.push(Labelled {
                label: format!("how the {} fared in review", count(fared.total(), "task")),
                rows: rows.collect(),
                apart: false,
            });
        }

        let mut rows = legends(kit, &bars, right);
        if fared.total() == 0 {
            rows.push(Vec::new());
            rows.push(vec![kit.dim("no task reviewed yet")]);
        }

        let own: Vec<_> = (crew.clis.iter().enumerate())
            .map(|(at, cli)| move |part: &DayPart| lit(at) && part.agent == cli.agent)
            .collect();
        let others = move |part: &DayPart| {
            (crew.clis.iter().enumerate()).any(|(at, cli)| !lit(at) && part.agent == cli.agent)
        };
        let mut stacks: Vec<Stack<'_>> = (crew.clis.iter().zip(&own))
            .map(|(cli, holds)| Stack {
                fill: kit.fg(cli.role()).into(),
                holds,
            })
            .collect();
        if pointed.is_some() {
            stacks.push(Stack {
                fill: kit.fg(Role::Faint).into(),
                holds: &others,
            });
        }
        for row in self.with_chart(kit, inner, rows, &stacks) {
            drawn.push(row);
        }
        frame.draw(kit, drawn)
    }

    /// A CLI's amount of what the bars measure, as its table row writes it: tokens with two
    /// decimals, its tasks, or its spend with `~` for an estimate.
    fn table_amount(&self, cli: &Cli) -> String {
        match self.measure {
            StatsMeasure::Tokens => self.table_tokens(cli.tokens()),
            StatsMeasure::Tasks => cli.work().to_string(),
            StatsMeasure::Spend => format::spend(&cli.spent()),
        }
    }

    /// The all-together card's first row. Over every day so far it says whose work Stats
    /// counts, and how much of everything on this computer that is, which the Overview
    /// counts. A shorter period is named, before its totals.
    fn together_title(&self, kit: &Kit<'_>, inner: usize) -> Vec<Span<'static>> {
        let title = |words: &str| {
            vec![
                kit.span(kit.glyphs.the_mod, Role::Question),
                Span::raw(" "),
                kit.strong("all together"),
                kit.dim(format!("  {words}")),
            ]
        };
        if self.crew.period != UsagePeriod::All {
            let left = title(&self.period_words());
            return header_row(kit, left, &self.totals(), inner);
        }
        let left = title("the crew's work through Argonauts Code");
        let mut right = vec![
            kit.strong(self.tokens(self.crew.total.total_tokens())),
            kit.dim(" tokens"),
        ];
        if let Some(all) = self.on_this_computer {
            let room = inner.saturating_sub(spans_width(&left) + spans_width(&right) + 2);
            let of = vec![
                kit.dim(" of "),
                kit.strong(millions(all)),
                kit.dim(" on this computer (Overview)"),
            ];
            if spans_width(&of) <= room {
                right.extend(of);
            }
        }
        let room = inner.saturating_sub(spans_width(&right) + 2);
        spread(truncate_spans(&left, room), right, inner)
    }

    /// The chart's width for the period's slots.
    fn chart_width(&self) -> usize {
        let slots = self.crew.slots().len();
        CHART.max(slots * slot_width(slots))
    }

    /// The cells the rows beside the chart have in a card `inner` wide: all of it where the
    /// card is too narrow for a chart.
    pub(super) fn beside_chart(&self, inner: usize) -> usize {
        if inner >= CHART_FROM {
            inner - self.chart_width() - CHART_GAP
        } else {
            inner
        }
    }

    /// Whether a card `inner` wide draws the period's chart with `least` cells beside it.
    pub(super) fn chart_fits(&self, inner: usize, least: usize) -> bool {
        inner >= self.chart_width() + CHART_GAP + least
    }

    /// `rows`, made for the width [`Stats::beside_chart`] gives, with the period's chart at
    /// their left: it sits at the bottom, its labels on the last row.
    pub(super) fn with_chart(
        &self,
        kit: &Kit<'_>,
        inner: usize,
        rows: Vec<Vec<Span<'static>>>,
        stacks: &[Stack<'_>],
    ) -> Vec<Vec<Span<'static>>> {
        if inner < CHART_FROM {
            return rows;
        }
        self.chart_beside(kit, rows, stacks)
    }

    /// [`Stats::with_chart`] at any width.
    pub(super) fn chart_beside(
        &self,
        kit: &Kit<'_>,
        rows: Vec<Vec<Span<'static>>>,
        stacks: &[Stack<'_>],
    ) -> Vec<Vec<Span<'static>>> {
        let columns = self.chart(kit, rows.len().saturating_sub(1).min(CHART_ROWS), stacks);
        let pad = rows.len().saturating_sub(columns.len());
        let mut left: Vec<Vec<Span<'static>>> = vec![Vec::new(); pad];
        left.extend(columns);
        let width = self.chart_width() + CHART_GAP;
        rows.into_iter()
            .enumerate()
            .map(|(at, row)| {
                let mut line = fit(left.get(at).cloned().unwrap_or_default(), width);
                line.extend(row);
                line
            })
            .collect()
    }

    /// The period as the all-together card names it.
    pub(super) fn period_words(&self) -> String {
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
        self.totals_taking(self.crew.took_secs, took)
    }

    /// [`Stats::totals`] with `took_secs` for the time, written by `write`, as a table whose
    /// time column adds up to something other than the runs' time gives its own sum.
    pub fn totals_taking(
        &self,
        took_secs: u64,
        write: fn(u64) -> String,
    ) -> Vec<(String, &'static str)> {
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
        if took_secs > 0 {
            pieces.push((write(took_secs), ""));
        }
        pieces
    }

    /// The period's days or weeks as columns `rows` high, each stacked from the bottom in the
    /// order of `stacks`, over their labels: a week's days by their letters with today's in
    /// the accent, a month's by the dates its weeks start on, and weeks by the month each
    /// begins.
    fn chart(&self, kit: &Kit<'_>, rows: usize, stacks: &[Stack<'_>]) -> Vec<Vec<Span<'static>>> {
        let crew = self.crew;
        let slots = crew.slots();
        let width = slot_width(slots.len());
        let columns: Vec<Vec<(u64, Fill)>> = slots
            .iter()
            .map(|slot| {
                if slot.ahead {
                    return Vec::new();
                }
                stacks
                    .iter()
                    .map(|stack| {
                        let value = crew
                            .days
                            .iter()
                            .filter(|part| slot.holds(part.day) && (stack.holds)(part))
                            .map(|part| part.measured(self.measure))
                            .sum();
                        (value, stack.fill)
                    })
                    .collect()
            })
            .collect();
        let mut out = draw::columns(kit, &columns, rows, width);
        let mut labels: Vec<Span<'static>> = Vec::new();
        let mut used = 0;
        let days_so_far = crew.period == UsagePeriod::All && crew.slots_are_days();
        for (k, slot) in slots.iter().enumerate() {
            let label = match crew.period {
                // Days so far are dated: the first by its month and day, then the first of a
                // month by its month, and every week's first day by its date.
                UsagePeriod::All if days_so_far => match (k, slot.first.day()) {
                    (0, day) => Some(format!("{} {day}", short_month(slot.first.month()))),
                    (_, 1) => Some(short_month(slot.first.month()).to_owned()),
                    (_, day) if k % 7 == 0 => Some(day.to_string()),
                    _ => None,
                },
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
                Span::styled(label, kit.bold(Role::Question))
            } else {
                kit.dim(label)
            });
        }
        out.push(labels);
        out
    }

    /// The CLIs' cards side by side, `picked` the one the keys are on, and how they're laid
    /// out.
    pub fn cli_cards(
        &self,
        kit: &Kit<'_>,
        width: usize,
        picked: Option<usize>,
    ) -> (Vec<Drawn>, grid::Layout) {
        let clis = &self.crew.clis;
        let cards = grid::Layout::of(width, clis.len());
        let layout = self.model_layout(cards.least_inner());
        let bodies = clis
            .iter()
            .enumerate()
            .map(|(at, cli)| self.cli_body(kit, cli, cards.inner(at), &layout))
            .collect();
        (grid::draw(kit, cards, bodies, picked), cards)
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
                labels: outcome_label(inner),
            };
        }
        ModelLayout {
            names,
            bar: inner
                .saturating_sub(names + widest(false))
                .max(LEAST_BARE_BAR),
            units: false,
            labels: outcome_label(inner),
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
                    StatsRole::Judge => "review",
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
    /// reviews fared, its pace over the period, and its plan's windows, a blank row between
    /// each part.
    fn cli_body(&self, kit: &Kit<'_>, cli: &Cli, inner: usize, layout: &ModelLayout) -> Body {
        // The CLI's logo where the terminal draws it, else the dot that says whether the main
        // agent runs on it.
        let dot = if cli.main { "● " } else { "○ " };
        let mark = NoPicture::Mark(kit.span(dot, cli.role()));
        let label = ModelLabel::cli_named(&cli.agent, &cli.name, kit.theme, mark);
        let mut left = label.logo;
        left.push(kit.strong(label.name));
        let tokens = self.tokens(cli.tokens());
        let title = spread(left, vec![kit.strong(tokens), kit.dim(" tok")], inner);

        // A judge's sign-off is a review too, so a CLI that reviews and judges counts both.
        let mut did = Vec::new();
        let tasks = cli.did(StatsRole::Worker);
        let reviews = cli.did(StatsRole::Reviewer) + cli.did(StatsRole::Judge);
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
        let did = spread(left, right, inner);

        let whole = cli.measured(self.measure);
        let bar = self.model_bar(cli, inner, layout);
        let bars = cli
            .models
            .iter()
            .map(|model| {
                let style = model_style(kit, cli, model);
                let mut row = draw::swatch(kit, model_fill(kit, cli, model));
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
                truncate_spans(&row, inner)
            })
            .collect();

        let back = cli.sent_back(StatsRole::Reviewer) + cli.sent_back(StatsRole::Judge);
        let accepted = Fared {
            first: reviews.saturating_sub(back),
            once: back.min(reviews),
            more: 0,
        };
        let mut rest = vec![
            Vec::new(),
            outcome_row(kit, ("tasks", layout.labels), cli.fared(), "first pass"),
            Vec::new(),
            outcome_row(kit, ("reviews", layout.labels), accepted, "accepted"),
            Vec::new(),
        ];
        rest.extend(self.pace_rows(kit, cli, inner));
        rest.push(Vec::new());
        rest.push(self.plan_line(kit, cli, inner));
        Body {
            head: vec![title, did, Vec::new()],
            bars,
            rest,
        }
    }

    /// The CLI's work over the period as a sparkline, before the period and the time a piece
    /// of the work took. A week is seven bars a cell apart, over the letters of their days.
    fn pace_rows(&self, kit: &Kit<'_>, cli: &Cli, inner: usize) -> Vec<Vec<Span<'static>>> {
        let crew = self.crew;
        let work = crew.series(StatsMeasure::Tasks, |part| part.agent == cli.agent);
        let top = crew.fullest(StatsMeasure::Tasks, |part| Some(part.agent.clone()));
        let style = kit.fg(cli.role());
        let tasks = cli.did(StatsRole::Worker);
        let (n, secs, unit) = if tasks > 0 {
            (tasks, cli.took(StatsRole::Worker), "a task")
        } else {
            let reviews = cli.did(StatsRole::Reviewer) + cli.did(StatsRole::Judge);
            let secs = cli.took(StatsRole::Reviewer) + cli.took(StatsRole::Judge);
            (reviews, secs, "a review")
        };
        let pace = (n > 0 && secs > 0).then(|| format!("{} {unit}", took(secs / u64::from(n))));
        let span = self.span_words();
        if crew.period != UsagePeriod::Week {
            let mut line = draw::spark(kit, &work, top, style);
            line.push(kit.dim(match pace {
                Some(pace) => format!(" {span} · {pace}"),
                None => format!(" {span}"),
            }));
            return vec![truncate_spans(&line, inner)];
        }
        let mut line = draw::spark_apart(kit, &work, top, style);
        // The letters under the bars say it's a week, so on a narrow card the pace alone
        // follows them.
        let room = inner.saturating_sub(spans_width(&line));
        let words = [
            pace.as_ref().map(|pace| format!("  {span} · {pace}")),
            pace.as_ref().map(|pace| format!("  {pace}")),
            Some(format!("  {span}")),
        ]
        .into_iter()
        .flatten()
        .find(|words| str_width(words) <= room);
        line.extend(words.map(|words| kit.dim(words)));
        vec![line, vec![kit.dim(self.weekday_letters())]]
    }

    /// `S M T W T F S`: the letters of a week's days from the day it starts on, a blank
    /// between them, under bars drawn the same way.
    pub(super) fn weekday_letters(&self) -> String {
        let mut day = self.crew.week_starts;
        let mut letters = Vec::new();
        for _ in 0..7 {
            letters.push(letter(day).to_string());
            day = day.next();
        }
        letters.join(" ")
    }

    /// The period as a card's pace names it: `weeks`, `this week`.
    pub(super) fn span_words(&self) -> &'static str {
        match self.crew.period {
            UsagePeriod::Today => "today",
            UsagePeriod::Week => "this week",
            UsagePeriod::Month => "this month",
            UsagePeriod::All if self.crew.slots_are_days() => "days",
            UsagePeriod::All => "weeks",
        }
    }

    /// `5h ━━━━━━   1%   week ━━━━━━  56%`: the CLI's plan windows, their bars as wide as
    /// the card leaves them, or why there is no reading.
    pub(super) fn plan_line(&self, kit: &Kit<'_>, cli: &Cli, inner: usize) -> Vec<Span<'static>> {
        let Some(plan) = self.usage.plans.iter().find(|plan| plan.agent == cli.agent) else {
            return vec![kit.dim("no plan reading yet")];
        };
        if plan.pays_per_token {
            return vec![kit.dim("pays per token")];
        }
        // Two windows share the row, half a bar's width apart.
        let bar = (LEAST_BARE_BAR..=MOST_PLAN_BAR)
            .rev()
            .find(|bar| PLAN_WORDS + 2 * bar + bar / 2 <= inner)
            .unwrap_or(LEAST_BARE_BAR);
        match self.plan_windows(kit, cli, bar) {
            Some(windows) => truncate_spans(&windows, inner),
            // The Usage tab's own words; its page says why.
            None if plan.failed.is_some() => vec![kit.dim("Failed to load usage data")],
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

    /// The models that did `role`'s work, the CLI that did the most of it first, its models
    /// together. A model that only spent in the role counts in its tokens, not in its list.
    pub(super) fn filled_by(&self, role: StatsRole) -> Vec<(&Cli, &Model)> {
        let mut filled: Vec<_> = (self.crew.in_role(role).into_iter())
            .filter(|(_, model)| model.did(role) > 0)
            .collect();
        filled.sort_by_key(|(cli, _)| std::cmp::Reverse(cli.did(role)));
        filled
    }

    /// Each role with every model that filled it, a bar of what each did in it, and a table
    /// of them under it, every role's in the same columns, a blank row between roles. Under
    /// the role's name, what the models' shares are of. `picked` is the role the keys are on.
    pub fn who_did_what(&self, kit: &Kit<'_>, frame: Frame, picked: Option<StatsRole>) -> Drawn {
        let inner = frame.inner();
        let mut drawn = Drawn::new();
        let left = Header::new("who did what", Role::Question).left(kit);
        let note = vec![kit.dim("every model that filled the role · enter opens a role")];
        let room = inner.saturating_sub(spans_width(&left) + 2);
        drawn.push(spread(left, truncate_spans(&note, room), inner));
        let bar_cells = inner.saturating_sub(ROLE_COLUMN);
        let roles = self.crew.roles();
        let tables: Vec<Vec<Legend>> = roles.iter().map(|role| self.did(kit, *role)).collect();
        let columns = draw::table_columns(tables.iter().flatten(), bar_cells, false);
        for (role, table) in roles.iter().zip(&tables) {
            drawn.push(Vec::new());
            let label = if picked == Some(*role) {
                vec![Span::styled(
                    format!("❯ {}", role.plural()),
                    kit.bold(Role::Question),
                )]
            } else {
                vec![Span::raw(role.plural())]
            };
            let mut row = fit(label, ROLE_COLUMN);
            let parts: Vec<(u64, Fill)> = (table.iter())
                .map(|row| (row.part.value, row.part.fill))
                .collect();
            row.extend(draw::stacked(kit, &parts, bar_cells));
            drawn.push(row);

            let whole = count(self.crew.did(*role), role.unit());
            for (k, line) in draw::table(kit, table, columns, true)
                .into_iter()
                .enumerate()
            {
                let left = if k == 0 {
                    kit.dim(whole.clone())
                } else {
                    Span::raw("")
                };
                let mut row = fit(vec![left], ROLE_COLUMN);
                row.extend(line);
                drawn.push(row);
            }
            if table.is_empty() {
                let none = format!("no {}s recorded", role.unit());
                drawn.push(vec![Span::raw(" ".repeat(ROLE_COLUMN)), kit.dim(none)]);
            }
        }
        if roles.is_empty() {
            drawn.push(vec![kit.dim("No work recorded in this period yet.")]);
        }
        frame.draw(kit, drawn)
    }

    /// The models that did `role`'s work, as the rows of who did what: the logo of whoever
    /// serves each, its texture, and what it did of the role's whole.
    fn did(&self, kit: &Kit<'_>, role: StatsRole) -> Vec<Legend> {
        let whole = u64::from(self.crew.did(role));
        (self.filled_by(role).into_iter())
            .map(|(cli, model)| {
                let did = u64::from(model.did(role));
                let fill = model_fill(kit, cli, model);
                let logo = logo_for(&cli.agent, model.model.as_deref());
                let label = ModelLabel::named(&logo, &model.name, kit.theme, NoPicture::Dot);
                let name = draw::model_name(&label.name);
                Legend {
                    logo: Some(logo),
                    ..Legend::of(fill, &name, (did, did, whole), did.to_string())
                }
            })
            .collect()
    }
}

/// One of the all-together card's bars: its label over it, its parts, and whether they sit a
/// blank cell apart, as CLIs do.
struct Labelled {
    label: String,
    rows: Vec<Legend>,
    apart: bool,
}

/// Each bar's label, the bar and its table, a blank row between bars, every table in the
/// same columns: the names as wide as the longest and the amounts as the widest.
fn legends(kit: &Kit<'_>, bars: &[Labelled], width: usize) -> Vec<Vec<Span<'static>>> {
    let every = bars.iter().flat_map(|bar| &bar.rows);
    let columns = draw::table_columns(every, width, true);
    let mut rows = Vec::new();
    for (at, bar) in bars.iter().enumerate() {
        if at > 0 {
            rows.push(Vec::new());
        }
        rows.push(vec![kit.dim(bar.label.clone())]);
        let parts: Vec<Lit> = bar.rows.iter().map(|row| row.part).collect();
        rows.push(draw::stacked_lit(kit, &parts, bar.apart, width));
        rows.push(Vec::new());
        rows.extend(draw::table(kit, &bar.rows, columns, true));
    }
    rows
}

/// Cells a column of the chart takes with the blank after it: two and one while the
/// columns fit the chart that way, else one.
fn slot_width(slots: usize) -> usize {
    if slots * 3 <= CHART + 1 { 3 } else { 1 }
}

/// The weekday's letter: `S M T W T F S`.
pub(in crate::overview) fn letter(day: time::Weekday) -> char {
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
    reading: &PlanUsageReading,
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
    let percent = if reading.status == PlanUsageStatus::Rejected {
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
pub(super) fn window_name(reading: &PlanUsageReading) -> String {
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
    reading: &PlanUsageReading,
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
    let percent = if reading.status == PlanUsageStatus::Rejected {
        "out".to_owned()
    } else {
        format::percent(used)
    };
    let mut bar = draw::thin(kit, (used * 1000.0).round() as u64, 1000, cells, fill);
    bar.push(Span::styled(format!(" {percent:>4}"), words));
    bar
}

/// How every card lays out its models' rows.
struct ModelLayout {
    /// Cells of the swatch and the name.
    names: usize,
    bar: usize,
    /// Each count says what it counts.
    units: bool,
    /// Cells of the labels of the rows of how work fared.
    labels: usize,
}

/// How wide the labels of the rows of how work fared are on cards whose narrowest is
/// `least_inner` cells inside, alike on every card so the rows line up across them.
pub(super) fn outcome_label(least_inner: usize) -> usize {
    if least_inner >= ROOMY_CARD {
        OUTCOME_LABEL
    } else {
        OUTCOME_LABEL - 1
    }
}

/// `tasks     ▪▪▪▪▪▪▪▪▪▪ 94% first pass`, the same row on every card, its label in a column
/// `width` wide: ten small squares, the share that went the first way, and what that share
/// is. With none of the work it says so.
pub(super) fn outcome_row(
    kit: &Kit<'_>,
    (label, width): (&str, usize),
    fared: Fared,
    words: &str,
) -> Vec<Span<'static>> {
    let mut row = vec![kit.dim(format!("{label:<width$}"))];
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
