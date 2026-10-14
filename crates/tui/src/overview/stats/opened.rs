//! A model, a CLI or a role opened one level deeper with Enter. A model shows what it did in
//! each role and kind of work, its tokens split, its weeks, the tasks it took over as a
//! fallback, and its CLI's plan. A role shows each model in it, what reviewers sent back and
//! why, the checks they were given, its tokens split, and its weeks.

use std::collections::BTreeMap;

use argonauts_core::Spend;
use argonauts_core::api::UsagePeriod;
use argonauts_core::crew_stats::{CrewRole, Fallback, ReviewTally};
use argonauts_core::settings::stats::StatsMeasure;
use ratatui::style::Style;
use ratatui::text::Span;
use time::{Date, Month};

use super::cli::{SQUARES, Stats, count, model_style};
use super::data::{Cli, DayPart, Model, short_cli};
use super::draw::{self, Entry, Fared};
use super::table::{each, spend_cell};
use crate::format;
use crate::text::{str_width, truncate_spans};
use crate::theme::Role;
use crate::widgets::bars::fit_keeping_bars;
use crate::widgets::{Drawn, Frame, Kit, fit};

/// What Enter opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    Cli(String),
    Model(String, Option<String>),
    Role(CrewRole),
}

/// Cells between the opened card's two columns.
const COLUMN_GAP: usize = 3;
/// Cells a row's name takes at least, and what it did.
const NAME: usize = 10;
const DID: usize = 12;
/// The bars of the kinds of work and of the reasons for sending work back.
const KIND_BAR: usize = 14;
const REASON: usize = 22;
const REASON_BAR: usize = 16;
/// Squares beside a kind of work, and the most cells its name takes.
const KIND_SQUARES: usize = 8;
const MOST_KIND: usize = 16;
/// A model's or a role's name and its count.
const EACH_NAME: usize = 20;
const EACH_DID: usize = 13;
/// A plan window's bar.
const PLAN_BAR: usize = 6;
/// Where the second row of a fallback starts, under the words after `fallbacks`.
const FALLBACK_INDENT: usize = 10;

impl Stats<'_> {
    /// The models `opened` covers, each with its CLI.
    fn covered(&self, opened: &Opened) -> Vec<(&Cli, &Model)> {
        match opened {
            Opened::Role(role) => self.filled_by(*role),
            _ => self
                .crew
                .models()
                .filter(|(cli, model)| match opened {
                    Opened::Cli(agent) => cli.agent == *agent,
                    Opened::Model(agent, name) => model.is(agent, name.as_deref()),
                    Opened::Role(_) => false,
                })
                .collect(),
        }
    }

    /// The opened card, or nothing when what was opened has no work in this period.
    pub fn opened(&self, kit: &Kit<'_>, frame: Frame, opened: &Opened) -> Option<Drawn> {
        let models = self.covered(opened);
        let (first_cli, first_model) = *models.first()?;
        let inner = frame.inner();
        let role = match opened {
            Opened::Role(role) => Some(*role),
            _ => None,
        };
        let spent = models.iter().fold(Spend::default(), |spent, (_, model)| {
            let part = match role {
                Some(role) => model.spent_as.get(&role).copied().unwrap_or_default(),
                None => model.spent,
            };
            spent.plus(&part)
        });

        let (title, pieces): (Vec<Span<'static>>, Vec<(String, &str)>) = match opened {
            Opened::Model(..) => {
                let mut title = draw::swatch(kit, model_style(kit, first_cli, first_model));
                title.push(kit.strong(first_model.name.clone()));
                title.push(kit.dim(format!(" on {} · opened with enter", first_cli.name)));
                (title, self.work_pieces(&models, &spent))
            }
            Opened::Cli(_) => {
                let title = vec![
                    kit.span("●", first_cli.role()),
                    Span::raw(" "),
                    kit.strong(first_cli.name.clone()),
                    kit.dim(" · opened with enter"),
                ];
                (title, self.work_pieces(&models, &spent))
            }
            Opened::Role(role) => {
                let title = vec![kit.strong(role.plural()), kit.dim(" · opened with enter")];
                let did = self.crew.did(*role);
                let mut pieces = vec![(did.to_string(), units(*role, did))];
                if matches!(role, CrewRole::Reviewer | CrewRole::Judge) {
                    let back: u32 = models
                        .iter()
                        .map(|(_, model)| model.sent_back.get(role).copied().unwrap_or(0))
                        .sum();
                    pieces.push((back.to_string(), "sent back"));
                }
                pieces.push((self.tokens_short(spent.total_tokens()), "tokens"));
                (title, pieces)
            }
        };
        let mut drawn = Drawn::new();
        drawn.push(super::cli::header_row(kit, title, &pieces, inner));
        drawn.push(Vec::new());

        let left_width = inner.saturating_sub(10) / 2;
        let right_width = inner.saturating_sub(left_width + COLUMN_GAP + 4);
        let (left, right) = match role {
            Some(role) => (
                self.role_left(kit, role, &models, left_width),
                self.role_right(kit, role, &models, &spent, right_width),
            ),
            None => (
                self.model_left(kit, opened, &models, left_width),
                self.model_right(kit, &models, &spent, right_width),
            ),
        };
        let rows = left.len().max(right.len());
        let mut lefts = left.into_iter();
        let mut rights = right.into_iter();
        for _ in 0..rows {
            let mut row = fit(lefts.next().unwrap_or_default(), left_width);
            row.push(Span::raw(" ".repeat(COLUMN_GAP)));
            row.extend(rights.next().unwrap_or_default());
            drawn.push(fit_keeping_bars(row, inner));
        }
        Some(frame.draw(kit, drawn))
    }

    /// `268 tasks · 118 reviews · 23.4M tokens · $31`.
    fn work_pieces(&self, models: &[(&Cli, &Model)], spent: &Spend) -> Vec<(String, &'static str)> {
        let mut pieces = Vec::new();
        for role in WORK_ORDER {
            let n: u32 = models.iter().map(|(_, model)| model.did(role)).sum();
            if n > 0 {
                pieces.push((n.to_string(), units(role, n)));
            }
        }
        pieces.push((self.tokens_short(spent.total_tokens()), "tokens"));
        pieces.push((spend_cell(spent), ""));
        pieces
    }

    /// By role, or by model for a CLI, then by kind of work.
    fn model_left(
        &self,
        kit: &Kit<'_>,
        opened: &Opened,
        models: &[(&Cli, &Model)],
        width: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let mut rows = Vec::new();
        let merged = merged(models);
        if matches!(opened, Opened::Cli(_)) {
            rows.push(vec![kit.dim("by model")]);
            let counts = models.iter().filter_map(|(_, model)| {
                let role = model.main_role()?;
                Some(count(model.did(role), role.unit()))
            });
            let did_width = column(counts, EACH_DID);
            for (cli, model) in models {
                let Some(role) = model.main_role() else {
                    continue;
                };
                let mut row = named(kit, cli, model);
                row.extend(fit(
                    vec![Span::raw(count(model.did(role), role.unit()))],
                    did_width,
                ));
                row.extend(work_words(kit, role, model));
                rows.push(fit_keeping_bars(row, width));
            }
        } else {
            rows.push(vec![kit.dim("by role")]);
            let roles: Vec<CrewRole> = WORK_ORDER
                .into_iter()
                .filter(|role| merged.did(*role) > 0)
                .collect();
            let name_width = roles
                .iter()
                .map(|role| str_width(role.word()) + 2)
                .max()
                .unwrap_or(0)
                .max(NAME);
            let counts = roles
                .iter()
                .map(|role| count(merged.did(*role), role.unit()));
            let did_width = column(counts, DID);
            for role in roles {
                let mut row = fit(vec![Span::raw(role.word())], name_width);
                row.extend(fit(
                    vec![Span::raw(count(merged.did(role), role.unit()))],
                    did_width,
                ));
                row.extend(work_words(kit, role, &merged));
                rows.push(fit_keeping_bars(row, width));
            }
        }
        let style = models.first().map_or(kit.fg(Role::Card), |(cli, model)| {
            model_style(kit, cli, model)
        });
        let kinds = kinds_rows(kit, &merged, style, width);
        if !kinds.is_empty() {
            rows.push(Vec::new());
            rows.push(vec![kit.dim("by kind of work, as a worker")]);
            rows.extend(kinds);
        }
        rows
    }

    /// Its tokens split, its weeks or days, the tasks it took over, and its CLI's plan.
    fn model_right(
        &self,
        kit: &Kit<'_>,
        models: &[(&Cli, &Model)],
        spent: &Spend,
        width: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let mut rows = self.tokens_split(kit, "its tokens", spent, width);
        rows.push(Vec::new());
        let style = models.first().map_or(kit.fg(Role::Card), |(cli, model)| {
            model_style(kit, cli, model)
        });
        let ours = |part: &DayPart| {
            models
                .iter()
                .any(|(_, model)| model.is(&part.agent, part.model.as_deref()))
        };
        let words = format!("{} {}", self.measure.word(), self.per());
        let top = self.crew.fullest(self.measure, |part| {
            Some((part.agent.clone(), part.model.clone()))
        });
        rows.push(self.spark_row(kit, self.measure, (&ours, top), style, &words, width));
        rows.extend(self.fallback_rows(kit, models, width));

        let mut clis: Vec<&Cli> = models.iter().map(|(cli, _)| *cli).collect();
        clis.dedup_by(|a, b| a.agent == b.agent);
        if let [cli] = clis.as_slice()
            && let Some(windows) = self.plan_windows(kit, cli, PLAN_BAR)
        {
            let mut row = vec![kit.dim(format!("{}'s plan  ", cli.name))];
            row.extend(windows);
            rows.push(fit_keeping_bars(row, width));
        }
        rows
    }

    /// Each model in the role, then what reviewers sent back and why, or for workers what
    /// they built.
    fn role_left(
        &self,
        kit: &Kit<'_>,
        role: CrewRole,
        models: &[(&Cli, &Model)],
        width: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let mut rows = vec![vec![kit.dim(format!("each {}", role.word()))]];
        let counts = models
            .iter()
            .map(|(_, model)| count(model.did(role), role.unit()));
        let did_width = column(counts, EACH_DID);
        for (cli, model) in models {
            let mut row = named(kit, cli, model);
            row.extend(fit(
                vec![Span::raw(count(model.did(role), role.unit()))],
                did_width,
            ));
            match role {
                CrewRole::Reviewer | CrewRole::Judge => {
                    let back = model.sent_back.get(&role).copied().unwrap_or(0);
                    row.extend([kit.dim("sent back "), kit.strong(back.to_string())]);
                }
                _ => row.extend(work_words(kit, role, model)),
            }
            rows.push(fit_keeping_bars(row, width));
        }
        let tally = tally(models, role);
        if !tally.reasons.is_empty() {
            rows.push(Vec::new());
            rows.push(vec![kit.dim("what they sent back for")]);
            let mut reasons: Vec<(&String, &u32)> = tally.reasons.iter().collect();
            reasons.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            let whole: u32 = reasons.iter().map(|(_, n)| **n).sum();
            for (why, n) in reasons {
                let mut row = fit(vec![Span::raw(why.clone())], REASON);
                row.extend(draw::thin(
                    kit,
                    u64::from(*n),
                    u64::from(whole),
                    REASON_BAR,
                    kit.fg(Role::Warning),
                ));
                row.push(kit.dim(format!("  {n}")));
                rows.push(fit_keeping_bars(row, width));
            }
        }
        if role == CrewRole::Worker {
            let style = models.first().map_or(kit.fg(Role::Card), |(cli, model)| {
                model_style(kit, cli, model)
            });
            let kinds = kinds_rows(kit, &merged(models), style, width);
            if !kinds.is_empty() {
                rows.push(Vec::new());
                rows.push(vec![kit.dim("by kind of work")]);
                rows.extend(kinds);
            }
        }
        rows
    }

    /// The checks the reviewers were given and how they came out, the role's tokens split,
    /// and its pieces of work a week or a day.
    fn role_right(
        &self,
        kit: &Kit<'_>,
        role: CrewRole,
        models: &[(&Cli, &Model)],
        spent: &Spend,
        width: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let mut rows = Vec::new();
        if matches!(role, CrewRole::Reviewer | CrewRole::Judge) {
            let tally = tally(models, role);
            let given = if tally.checks.is_empty() {
                "checks they were given · none".to_owned()
            } else {
                format!("checks they were given · {}", tally.checks.join(", "))
            };
            rows.push(truncate_spans(&[kit.dim(given)], width));
            rows.push(truncate_spans(
                &[
                    kit.dim("passed "),
                    kit.strong(tally.passed.to_string()),
                    kit.dim("   failed "),
                    kit.strong(tally.failed.to_string()),
                    kit.dim("   ran their own "),
                    kit.strong(tally.ran_own.to_string()),
                ],
                width,
            ));
            rows.push(Vec::new());
        }
        rows.extend(self.tokens_split(kit, "their tokens", spent, width));
        rows.push(Vec::new());
        let style = models.first().map_or(kit.fg(Role::Card), |(cli, model)| {
            model_style(kit, cli, model)
        });
        let in_role = |part: &DayPart| part.role == Some(role);
        let words = format!("{} {}", plural(role), self.per());
        let top = self.crew.fullest(StatsMeasure::Tasks, |part| part.role);
        rows.push(self.spark_row(
            kit,
            StatsMeasure::Tasks,
            (&in_role, top),
            style,
            &words,
            width,
        ));
        rows
    }

    /// `its tokens · 23.4M`, a bar of what they were, and a legend of each.
    fn tokens_split(
        &self,
        kit: &Kit<'_>,
        label: &str,
        spent: &Spend,
        width: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let cached = spent.cached_tokens;
        let new = spent.input_tokens.saturating_sub(cached);
        let output = spent.output_tokens;
        let kinds = [
            (cached, Role::TokensCached, "from cache"),
            (new, Role::TokensInput, "new input"),
            (output, Role::TokensOutput, "output"),
        ];
        let parts: Vec<(u64, Style)> = kinds
            .iter()
            .map(|(n, role, _)| (*n, kit.fg(*role)))
            .collect();
        let entries: Vec<Entry> = kinds
            .iter()
            .map(|(n, role, name)| Entry {
                style: kit.fg(*role),
                name: (*name).to_owned(),
                amount: self.tokens_short(*n),
                note: String::new(),
            })
            .collect();
        let mut rows = vec![vec![kit.dim(format!(
            "{label} · {}",
            self.tokens_short(spent.total_tokens())
        ))]];
        rows.push(draw::stacked(kit, &parts, width));
        rows.extend(draw::legend(kit, &entries, None, width));
        rows
    }

    /// `a week` when the period's slots are weeks, else `a day`.
    fn per(&self) -> &'static str {
        match self.crew.period {
            UsagePeriod::All => "a week",
            _ => "a day",
        }
    }

    /// A sparkline of the period's slots over the parts `pick` keeps, on the scale `top`
    /// that its kind shares, and its busiest day.
    fn spark_row(
        &self,
        kit: &Kit<'_>,
        measure: StatsMeasure,
        (pick, top): (&dyn Fn(&DayPart) -> bool, u64),
        style: Style,
        words: &str,
        width: usize,
    ) -> Vec<Span<'static>> {
        let series = self.crew.series(measure, pick);
        let mut row = draw::spark(kit, &series, top, style);
        let mut words = format!("  {words}");
        if let Some(day) = self.crew.busiest(measure, pick) {
            words.push_str(&format!(" · busiest {}", short_day(day)));
        }
        row.push(kit.dim(words));
        fit_keeping_bars(row, width)
    }

    /// `fallbacks took over 6 tasks from Opus 5.5`, and under it why the first stopped, for
    /// each model it took over from.
    fn fallback_rows(
        &self,
        kit: &Kit<'_>,
        models: &[(&Cli, &Model)],
        width: usize,
    ) -> Vec<Vec<Span<'static>>> {
        // By the CLI and model it took over from: how many tasks, and why the first stopped.
        let mut from: BTreeMap<(String, Option<String>), (u32, &Fallback)> = BTreeMap::new();
        for fallback in &self.crew.fallbacks {
            let ours = models
                .iter()
                .any(|(_, model)| model.is(&fallback.agent, fallback.model.as_deref()));
            if ours {
                let entry = from
                    .entry((fallback.from_agent.clone(), fallback.from_model.clone()))
                    .or_insert((0, fallback));
                entry.0 += 1;
            }
        }
        let mut rows = Vec::new();
        for ((agent, model), (n, first)) in from {
            let name = self
                .crew
                .models()
                .find(|(_, m)| m.is(&agent, model.as_deref()))
                .map_or_else(|| agent.clone(), |(_, m)| m.name.clone());
            let tasks = if n == 1 { "task" } else { "tasks" };
            rows.push(truncate_spans(
                &[
                    kit.dim("fallbacks "),
                    Span::raw("took over "),
                    kit.strong(n.to_string()),
                    Span::raw(format!(" {tasks} from {name}")),
                ],
                width,
            ));
            let why = match &first.window {
                Some(window) => {
                    let cli = self.crew.clis.iter().find(|cli| cli.agent == agent);
                    let cli = cli.map_or_else(|| short_cli(&agent, &agent), Cli::short);
                    format!("when {cli}'s {} window ran out", window_words(window))
                }
                None => first.why.clone(),
            };
            if !why.is_empty() {
                let indent = " ".repeat(FALLBACK_INDENT);
                rows.push(truncate_spans(&[kit.dim(format!("{indent}{why}"))], width));
            }
        }
        rows
    }

    /// `5h ━━━━━━  1%   week ━━━━━━ 11%`: the CLI's first two plan windows, each a bar `bar`
    /// cells wide in its color, amber from 90% and red once spent. `None` before a reading.
    pub(super) fn plan_windows(
        &self,
        kit: &Kit<'_>,
        cli: &Cli,
        bar: usize,
    ) -> Option<Vec<Span<'static>>> {
        let plan = self
            .usage
            .plans
            .iter()
            .find(|plan| plan.agent == cli.agent)?;
        if plan.windows.is_empty() {
            return None;
        }
        // The space between the windows grows with their bars, so wider ones still read
        // as two.
        let gap = " ".repeat(bar / 3 + 1);
        let mut row = Vec::new();
        for (k, reading) in plan.windows.iter().take(2).enumerate() {
            if k > 0 {
                row.push(Span::raw(gap.clone()));
            }
            row.push(kit.dim(format!("{} ", super::cli::window_name(reading))));
            row.extend(super::cli::window_bar(kit, cli, reading, bar));
        }
        Some(row)
    }
}

/// The roles in the order an opened model lists them.
const WORK_ORDER: [CrewRole; 4] = [
    CrewRole::Worker,
    CrewRole::Reviewer,
    CrewRole::Judge,
    CrewRole::Orchestrator,
];

/// `tasks`, `review`: the role's unit for `n` of them.
fn units(role: CrewRole, n: u32) -> &'static str {
    match (role, n) {
        (CrewRole::Worker, 1) => "task",
        (CrewRole::Worker, _) => "tasks",
        (CrewRole::Reviewer, 1) => "review",
        (CrewRole::Reviewer, _) => "reviews",
        (CrewRole::Judge, 1) => "sign-off",
        (CrewRole::Judge, _) => "sign-offs",
        (CrewRole::Main, 1) => "turn",
        (CrewRole::Main, _) => "turns",
        (_, 1) => "workflow",
        _ => "workflows",
    }
}

/// `reviews`, `tasks`: the role's work, counted.
fn plural(role: CrewRole) -> &'static str {
    units(role, 2)
}

/// The models summed into one, as a CLI or a role reads.
fn merged(models: &[(&Cli, &Model)]) -> Model {
    let mut out = Model::default();
    for (_, model) in models {
        for (role, n) in &model.done {
            *out.done.entry(*role).or_default() += n;
        }
        for (role, n) in &model.sent_back {
            *out.sent_back.entry(*role).or_default() += n;
        }
        for (role, n) in &model.took {
            *out.took.entry(*role).or_default() += n;
        }
        out.fared = fared_of([out.fared, model.fared]);
        for (kind, fared) in &model.kinds {
            let into = out.kinds.entry(kind.clone()).or_default();
            *into = fared_of([*into, *fared]);
        }
    }
    out
}

/// What the role's reviews were given and sent back for, across its models.
fn tally(models: &[(&Cli, &Model)], role: CrewRole) -> ReviewTally {
    let mut tally = ReviewTally::default();
    for (_, model) in models {
        if let Some(more) = model.reviews.get(&role) {
            tally.add(more);
        }
    }
    tally
}

/// What it did in `role`: `▪▪▪▪▪▪▪▪▪▪ 90% first pass` for a builder, `sent back 9 · 52s each`
/// for a reviewer, `1m 50s each` otherwise.
fn work_words(kit: &Kit<'_>, role: CrewRole, model: &Model) -> Vec<Span<'static>> {
    let took = each(model.took.get(&role).copied().unwrap_or(0), model.did(role));
    match role {
        CrewRole::Worker if model.fared.total() > 0 => {
            let mut words = draw::tenths(kit, model.fared, SQUARES);
            words.push(Span::raw(" "));
            words.push(kit.strong(format::percent(model.fared.first_pass())));
            words.push(kit.dim(" first pass"));
            words
        }
        CrewRole::Reviewer | CrewRole::Judge => {
            let back = model.sent_back.get(&role).copied().unwrap_or(0);
            let mut words = vec![kit.dim("sent back "), kit.strong(back.to_string())];
            if !took.is_empty() {
                words.push(kit.dim(format!(" · {took} each")));
            }
            words
        }
        _ if !took.is_empty() => vec![kit.dim(format!("{took} each"))],
        _ => Vec::new(),
    }
}

/// `build     ━━━━━━━━━━━━━━  152 · ▪▪▪▪▪▪▪▪`: each kind of work its share of the tasks, and
/// how it fared.
fn kinds_rows(kit: &Kit<'_>, model: &Model, style: Style, width: usize) -> Vec<Vec<Span<'static>>> {
    let mut kinds: Vec<(&String, &Fared)> = model.kinds.iter().collect();
    kinds.sort_by(|a, b| b.1.total().cmp(&a.1.total()).then(a.0.cmp(b.0)));
    let whole: u32 = kinds.iter().map(|(_, fared)| fared.total()).sum();
    let names = column(kinds.iter().map(|(kind, _)| (*kind).clone()), NAME).min(MOST_KIND);
    kinds
        .into_iter()
        .map(|(kind, fared)| {
            let name = truncate_spans(&[Span::raw(kind.clone())], names - 1);
            let mut row = fit(name, names);
            row.extend(draw::thin(
                kit,
                u64::from(fared.total()),
                u64::from(whole),
                KIND_BAR,
                style,
            ));
            row.push(kit.dim(format!("  {:>3} · ", fared.total())));
            row.extend(draw::tenths(kit, *fared, KIND_SQUARES));
            fit_keeping_bars(row, width)
        })
        .collect()
}

/// A column as wide as its widest words and a blank after them, `least` at least.
fn column(words: impl Iterator<Item = String>, least: usize) -> usize {
    words
        .map(|words| str_width(&words) + 1)
        .max()
        .unwrap_or(0)
        .max(least)
}

/// A model's swatch and name in its column, the name cut so a blank always follows it.
fn named(kit: &Kit<'_>, cli: &Cli, model: &Model) -> Vec<Span<'static>> {
    let mut row = draw::swatch(kit, model_style(kit, cli, model));
    row.push(Span::raw(model.name.clone()));
    fit(truncate_spans(&row, EACH_NAME - 1), EACH_NAME)
}

/// How a plan window reads in a sentence: `five_hour` is the `5-hour` window.
fn window_words(window: &str) -> String {
    match window {
        "five_hour" => "5-hour".to_owned(),
        "seven_day" | "weekly" => "weekly".to_owned(),
        "seven_day_opus" => "weekly Opus".to_owned(),
        "seven_day_sonnet" => "weekly Sonnet".to_owned(),
        "daily" | "one_day" => "daily".to_owned(),
        other => other.replace('_', " "),
    }
}

/// `Oct 5`.
pub(super) fn short_day(day: Date) -> String {
    let month = match day.month() {
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
    };
    format!("{month} {}", day.day())
}

fn fared_of(each: impl IntoIterator<Item = Fared>) -> Fared {
    let mut fared = Fared::default();
    for one in each {
        fared.first += one.first;
        fared.once += one.once;
        fared.more += one.more;
    }
    fared
}
