//! The crew's stats summed the ways Stats shows them: each CLI with its models, each model
//! across its roles, and each role with the models that filled it. Every view reads these,
//! so a model keeps its shade and its numbers wherever it shows. The main session's turns are
//! the Overview's: Stats counts the crew's work.

use std::collections::BTreeMap;

use argonauts_core::Spend;
use argonauts_core::api::{PlanUsage, UsagePeriod, UsageSummary};
use argonauts_core::crew_stats::{CrewRole, CrewStats, ReviewTally, WorkRow};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::settings::stats::StatsMeasure;
use time::{Date, Duration, Weekday};

use super::draw::Fared;
use crate::overview::cli_role;
use crate::theme::Role;

/// A model, wherever it ran in the crew.
#[derive(Debug, Clone, Default)]
pub(super) struct Model {
    /// The CLI's id, such as `codex`.
    pub agent: String,
    pub model: Option<String>,
    /// Such as `GPT 5.6`, or the CLI's name when it ran its own default.
    pub name: String,
    /// Its place among its CLI's models by tokens, which picks its shade.
    pub shade: usize,
    pub spent: Spend,
    /// What it did in each role it filled, by [`CrewRole::unit`].
    pub done: BTreeMap<CrewRole, u32>,
    /// Reviews and rulings of its that sent work back.
    pub sent_back: BTreeMap<CrewRole, u32>,
    /// Time in each role, together.
    pub took: BTreeMap<CrewRole, u64>,
    pub spent_as: BTreeMap<CrewRole, Spend>,
    /// The tasks it built, and how they fared in review.
    pub fared: Fared,
    /// The tasks it built, by kind of work, and how each kind fared.
    pub kinds: BTreeMap<String, Fared>,
    /// What its reviews and rulings were given and sent work back for, by role.
    pub reviews: BTreeMap<CrewRole, ReviewTally>,
}

impl Model {
    pub fn tokens(&self) -> u64 {
        self.spent.total_tokens()
    }

    pub fn did(&self, role: CrewRole) -> u32 {
        self.done.get(&role).copied().unwrap_or(0)
    }

    /// Pieces of work: tasks built, reviews, sign-offs and workflows planned. Turns of the
    /// conversation aren't pieces of work.
    pub fn work(&self) -> u32 {
        self.done
            .iter()
            .filter(|(role, _)| **role != CrewRole::Main)
            .map(|(_, done)| done)
            .sum()
    }

    /// The roles it filled, in the who-did-what order.
    pub fn roles(&self) -> Vec<CrewRole> {
        CrewRole::ALL
            .into_iter()
            .filter(|role| self.done.contains_key(role) || self.spent_as.contains_key(role))
            .collect()
    }

    /// The role it did most of its work in, which names what it did.
    pub fn main_role(&self) -> Option<CrewRole> {
        [
            CrewRole::Worker,
            CrewRole::Reviewer,
            CrewRole::Judge,
            CrewRole::Orchestrator,
            CrewRole::Main,
        ]
        .into_iter()
        .find(|role| self.did(*role) > 0)
    }

    pub fn measured(&self, measure: StatsMeasure) -> u64 {
        measured(&self.spent, self.work(), measure)
    }

    pub fn is(&self, agent: &str, model: Option<&str>) -> bool {
        self.agent == agent && self.model.as_deref() == model
    }
}

/// What a bar measures: tokens, pieces of work, or cents spent.
pub(super) fn measured(spent: &Spend, work: u32, measure: StatsMeasure) -> u64 {
    match measure {
        StatsMeasure::Tokens => spent.total_tokens(),
        StatsMeasure::Tasks => u64::from(work),
        StatsMeasure::Spend => (spent.cost_usd * 100.0).round() as u64,
    }
}

/// A CLI and the models it ran.
#[derive(Debug, Clone, Default)]
pub(super) struct Cli {
    pub agent: String,
    /// Such as `Antigravity CLI`.
    pub name: String,
    /// The main agent runs on it.
    pub main: bool,
    /// Its models, the most tokens first.
    pub models: Vec<Model>,
}

impl Cli {
    pub fn role(&self) -> Role {
        cli_role(&self.agent)
    }

    pub fn spent(&self) -> Spend {
        let mut spent = Spend::default();
        for model in &self.models {
            add(&mut spent, &model.spent);
        }
        spent
    }

    pub fn tokens(&self) -> u64 {
        self.spent().total_tokens()
    }

    pub fn did(&self, role: CrewRole) -> u32 {
        self.models.iter().map(|model| model.did(role)).sum()
    }

    pub fn work(&self) -> u32 {
        self.models.iter().map(Model::work).sum()
    }

    pub fn measured(&self, measure: StatsMeasure) -> u64 {
        measured(&self.spent(), self.work(), measure)
    }

    pub fn fared(&self) -> Fared {
        let mut fared = Fared::default();
        for model in &self.models {
            fared.first += model.fared.first;
            fared.once += model.fared.once;
            fared.more += model.fared.more;
        }
        fared
    }

    pub fn sent_back(&self, role: CrewRole) -> u32 {
        self.models
            .iter()
            .map(|model| model.sent_back.get(&role).copied().unwrap_or(0))
            .sum()
    }

    pub fn took(&self, role: CrewRole) -> u64 {
        self.models
            .iter()
            .map(|model| model.took.get(&role).copied().unwrap_or(0))
            .sum()
    }

    /// `Claude`, `Codex`, `agy`: the short name the card above the input and the by-model
    /// rows use.
    pub fn short(&self) -> String {
        short_cli(&self.agent, &self.name)
    }
}

pub(super) fn short_cli(agent: &str, name: &str) -> String {
    match agent {
        "claude-code" => "Claude".to_owned(),
        "codex" => "Codex".to_owned(),
        "antigravity-cli" => "agy".to_owned(),
        _ => name.to_owned(),
    }
}

/// Everything Stats draws, summed once from the period's rows.
#[derive(Debug, Clone)]
pub(super) struct Crew {
    /// The main agent's CLI first, then the most tokens first.
    pub clis: Vec<Cli>,
    pub workflows: u32,
    pub took_secs: u64,
    /// Every task, and how it fared in review.
    pub fared: Fared,
    pub merged: u32,
    /// Each model's work in each role on each day of the period.
    pub days: Vec<DayPart>,
    pub fallbacks: Vec<argonauts_core::crew_stats::Fallback>,
    pub total: Spend,
    pub period: UsagePeriod,
    pub today: Date,
    /// The day a week starts on, for the weeks and the days of the week the charts show.
    pub week_starts: Weekday,
}

#[derive(Debug, Clone)]
pub(super) struct DayPart {
    pub day: Date,
    pub agent: String,
    pub model: Option<String>,
    pub role: Option<CrewRole>,
    pub tokens: u64,
    pub cost_usd: f64,
    pub done: u32,
}

impl DayPart {
    pub fn measured(&self, measure: StatsMeasure) -> u64 {
        measured_day(self.tokens, self.cost_usd, self.done, measure)
    }
}

fn measured_day(tokens: u64, cost_usd: f64, done: u32, measure: StatsMeasure) -> u64 {
    match measure {
        StatsMeasure::Tokens => tokens,
        StatsMeasure::Tasks => u64::from(done),
        StatsMeasure::Spend => (cost_usd * 100.0).round() as u64,
    }
}

/// One column of a chart or one cell of a sparkline: a day, or a week for every day so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Slot {
    pub first: Date,
    /// Days it covers: 1, or 7 for a week.
    pub days: i64,
    /// It starts after today, so nothing can be in it yet.
    pub ahead: bool,
}

impl Slot {
    pub fn holds(&self, day: Date) -> bool {
        day >= self.first && day < self.first + Duration::days(self.days)
    }
}

/// Weeks a chart of every day so far shows at most.
pub(super) const WEEKS: usize = 10;

impl Crew {
    pub fn of(usage: &UsageSummary, stats: &CrewStats, names: &DisplayNames, today: Date) -> Self {
        let mut models: BTreeMap<(String, Option<String>), Model> = BTreeMap::new();
        for row in stats.work.iter().filter(|row| row.role != CrewRole::Main) {
            let model = model_entry(&mut models, names, &row.agent, row.model.as_ref());
            add_row(model, row);
        }
        let mut fared = Fared::default();
        let mut merged = 0;
        for task in &stats.tasks {
            let model = model_entry(&mut models, names, &task.agent, task.model.as_ref());
            merged += u32::from(task.merged);
            // How a task fared counts once a reviewer or the judge looked at it, merged or
            // not, so a task sent back and never merged still lowers the first-pass share.
            if !task.reviewed {
                continue;
            }
            model.fared.add(task.sent_back);
            let kind = task.kind.clone().unwrap_or_else(|| "build".to_owned());
            model.kinds.entry(kind).or_default().add(task.sent_back);
            fared.add(task.sent_back);
        }

        let in_use = |agent: &str| {
            usage
                .plans
                .iter()
                .any(|plan: &PlanUsage| plan.agent == agent && plan.in_use)
        };
        let mut clis: BTreeMap<String, Cli> = BTreeMap::new();
        for model in models.into_values() {
            let agent = model.agent.clone();
            let cli = clis.entry(agent.clone()).or_insert_with(|| Cli {
                name: names.harness(&agent),
                main: in_use(&agent),
                agent,
                models: Vec::new(),
            });
            cli.models.push(model);
        }
        let mut clis: Vec<Cli> = clis.into_values().collect();
        for cli in &mut clis {
            cli.models
                .sort_by(|a, b| b.tokens().cmp(&a.tokens()).then(a.name.cmp(&b.name)));
            for (nth, model) in cli.models.iter_mut().enumerate() {
                model.shade = nth;
            }
        }
        clis.sort_by(|a, b| {
            b.main
                .cmp(&a.main)
                .then(b.tokens().cmp(&a.tokens()))
                .then(a.name.cmp(&b.name))
        });

        let days = stats
            .days
            .iter()
            .filter(|part| part.role != Some(CrewRole::Main))
            .map(|part| DayPart {
                day: part.day,
                agent: part.agent.clone(),
                model: part.model.clone(),
                role: part.role,
                tokens: part.tokens,
                cost_usd: part.cost_usd,
                done: part.done,
            })
            .collect();

        let mut total = Spend::default();
        for cli in &clis {
            add(&mut total, &cli.spent());
        }
        Self {
            clis,
            workflows: stats.workflows,
            took_secs: stats.took_secs,
            fared,
            merged,
            days,
            fallbacks: stats.fallbacks.clone(),
            total,
            period: usage.period,
            today,
            week_starts: usage.week_starts.weekday(),
        }
    }

    /// The first day of the week `day` is in.
    pub fn week_of(&self, day: Date) -> Date {
        let back = (7 + day.weekday().number_days_from_sunday()
            - self.week_starts.number_days_from_sunday())
            % 7;
        day - Duration::days(i64::from(back))
    }

    /// The chart's columns for the period: every day so far as weeks, up to the last
    /// [`WEEKS`] of them; the week and the month as their days, those after today ahead; and
    /// today by itself.
    pub fn slots(&self) -> Vec<Slot> {
        let today = self.today;
        let day = |first: Date| Slot {
            first,
            days: 1,
            ahead: first > today,
        };
        match self.period {
            UsagePeriod::Today => vec![day(today)],
            UsagePeriod::Week => {
                let first = self.week_of(today);
                (0..7).map(|k| day(first + Duration::days(k))).collect()
            }
            UsagePeriod::Month => {
                let first = today.replace_day(1).unwrap_or(today);
                let days = i64::from(first.month().length(first.year()));
                (0..days).map(|k| day(first + Duration::days(k))).collect()
            }
            UsagePeriod::All => {
                let this_week = self.week_of(today);
                let earliest = self.days.iter().map(|part| part.day).min().unwrap_or(today);
                let weeks = ((this_week - self.week_of(earliest)).whole_days() / 7 + 1)
                    .clamp(1, WEEKS as i64);
                (0..weeks)
                    .rev()
                    .map(|back| Slot {
                        first: this_week - Duration::days(7 * back),
                        days: 7,
                        ahead: false,
                    })
                    .collect()
            }
        }
    }

    /// Each slot's sum of what `measure` counts over the parts `pick` keeps; `None` for a slot
    /// ahead of today.
    pub fn series(
        &self,
        measure: StatsMeasure,
        pick: impl Fn(&DayPart) -> bool,
    ) -> Vec<Option<u64>> {
        self.slots()
            .iter()
            .map(|slot| {
                (!slot.ahead).then(|| {
                    self.days
                        .iter()
                        .filter(|part| slot.holds(part.day) && pick(part))
                        .map(|part| part.measured(measure))
                        .sum()
                })
            })
            .collect()
    }

    /// The most any one group's slot holds, the parts grouped by `key`: the scale the
    /// sparklines of a kind share, so the ones side by side compare.
    pub fn fullest<K: Ord>(
        &self,
        measure: StatsMeasure,
        key: impl Fn(&DayPart) -> Option<K>,
    ) -> u64 {
        let mut sums: BTreeMap<(K, Date), u64> = BTreeMap::new();
        for slot in self.slots().iter().filter(|slot| !slot.ahead) {
            for part in self.days.iter().filter(|part| slot.holds(part.day)) {
                if let Some(key) = key(part) {
                    *sums.entry((key, slot.first)).or_default() += part.measured(measure);
                }
            }
        }
        sums.into_values().max().unwrap_or(0)
    }

    /// The day of the period with the most of what `measure` counts over the parts `pick`
    /// keeps.
    pub fn busiest(&self, measure: StatsMeasure, pick: impl Fn(&DayPart) -> bool) -> Option<Date> {
        let mut by_day: BTreeMap<Date, u64> = BTreeMap::new();
        for part in self.days.iter().filter(|part| pick(part)) {
            *by_day.entry(part.day).or_default() += part.measured(measure);
        }
        by_day
            .into_iter()
            .filter(|(_, value)| *value > 0)
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
            .map(|(day, _)| day)
    }

    pub fn models(&self) -> impl Iterator<Item = (&Cli, &Model)> {
        self.clis
            .iter()
            .flat_map(|cli| cli.models.iter().map(move |model| (cli, model)))
    }

    pub fn work(&self) -> u32 {
        self.clis.iter().map(Cli::work).sum()
    }

    pub fn did(&self, role: CrewRole) -> u32 {
        self.clis.iter().map(|cli| cli.did(role)).sum()
    }

    pub fn measured(&self, measure: StatsMeasure) -> u64 {
        measured(&self.total, self.work(), measure)
    }

    /// The roles any model filled, in the who-did-what order.
    pub fn roles(&self) -> Vec<CrewRole> {
        CrewRole::ALL
            .into_iter()
            .filter(|role| self.models().any(|(_, model)| model.roles().contains(role)))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.clis.is_empty()
    }
}

/// The model's row, made the first time it's asked for.
fn model_entry<'a>(
    models: &'a mut BTreeMap<(String, Option<String>), Model>,
    names: &DisplayNames,
    agent: &str,
    model: Option<&String>,
) -> &'a mut Model {
    models
        .entry((agent.to_owned(), model.cloned()))
        .or_insert_with(|| Model {
            agent: agent.to_owned(),
            model: model.cloned(),
            name: names
                .model(agent, model.map(String::as_str))
                .unwrap_or_else(|| names.harness(agent)),
            ..Model::default()
        })
}

fn add(total: &mut Spend, more: &Spend) {
    *total = total.plus(more);
}

fn add_row(model: &mut Model, row: &WorkRow) {
    add(&mut model.spent, &row.spent);
    add(model.spent_as.entry(row.role).or_default(), &row.spent);
    if row.done > 0 {
        *model.done.entry(row.role).or_default() += row.done;
    }
    if row.sent_back > 0 {
        *model.sent_back.entry(row.role).or_default() += row.sent_back;
    }
    *model.took.entry(row.role).or_default() += row.took_secs;
    if !row.review.is_empty() {
        model.reviews.entry(row.role).or_default().add(&row.review);
    }
}
