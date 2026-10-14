//! The crew's stats summed the ways Stats shows them: each CLI with its models, each model
//! across its roles, and each role with the models that filled it. Every view reads these,
//! so a model keeps its shade and its numbers wherever it shows. The main agent's conversations
//! are a role's work like any other, though no bar of tasks counts them.

use std::collections::BTreeMap;

use argonauts_core::Spend;
use argonauts_core::api::{PlanUsage, UsagePeriod, UsageSummary};
use argonauts_core::crew_stats::{CrewStats, Outcomes, ReviewTally, StatsRole, WorkRow};
use argonauts_core::display_names::{DisplayNames, Spellings};
use argonauts_core::settings::stats::StatsMeasure;
use time::{Date, Duration, Weekday};

use super::draw::Fared;
use crate::overview::cli_role;
use crate::overview::shades::ModelColors;
use crate::theme::Role;

/// A model, wherever it ran in the crew.
#[derive(Debug, Clone, Default)]
pub(super) struct Model {
    /// The CLI's id, such as `codex`.
    pub agent: String,
    pub model: Option<String>,
    /// Such as `GPT 5.6`, or the CLI's name when it ran its own default.
    pub name: String,
    /// Its place among its CLI's models by their tokens in all, which picks its shade.
    pub shade: usize,
    pub spent: Spend,
    /// What it did in each role it filled, by [`StatsRole::unit`].
    pub done: BTreeMap<StatsRole, u32>,
    /// Reviews and rulings of its that sent work back.
    pub sent_back: BTreeMap<StatsRole, u32>,
    /// Time in each role, together.
    pub took: BTreeMap<StatsRole, u64>,
    pub spent_as: BTreeMap<StatsRole, Spend>,
    /// The tasks it built, and how they fared in review.
    pub fared: Fared,
    /// The tasks it built, by kind of work, and how each kind fared.
    pub kinds: BTreeMap<String, Fared>,
    /// What its reviews and rulings were given and sent work back for, by role.
    pub reviews: BTreeMap<StatsRole, ReviewTally>,
    /// The sessions it ran in each role, and the tools they called.
    pub sessions: BTreeMap<StatsRole, u32>,
    pub tool_calls: BTreeMap<StatsRole, u32>,
    /// The turns of the conversations it held as the main agent.
    pub turns: u32,
    /// Times review sent its tasks back, and why, by how often.
    pub returns: u32,
    pub returned_for: BTreeMap<String, u32>,
}

impl Model {
    pub fn tokens(&self) -> u64 {
        self.spent.total_tokens()
    }

    pub fn did(&self, role: StatsRole) -> u32 {
        self.done.get(&role).copied().unwrap_or(0)
    }

    /// Pieces of work: tasks built, reviews, sign-offs, merges and workflows planned.
    /// Conversations aren't pieces of work.
    pub fn work(&self) -> u32 {
        self.done
            .iter()
            .filter(|(role, _)| **role != StatsRole::Main)
            .map(|(_, done)| done)
            .sum()
    }

    /// The roles it filled, in the who-did-what order.
    pub fn roles(&self) -> Vec<StatsRole> {
        StatsRole::BY_WORK
            .into_iter()
            .filter(|role| self.done.contains_key(role) || self.spent_as.contains_key(role))
            .collect()
    }

    /// The roles it filled, in the order By role lists them.
    pub fn filled(&self) -> Vec<StatsRole> {
        StatsRole::ALL
            .into_iter()
            .filter(|role| self.done.contains_key(role) || self.spent_as.contains_key(role))
            .collect()
    }

    /// The role it did most of its work in, which names what it did.
    pub fn main_role(&self) -> Option<StatsRole> {
        [
            StatsRole::Worker,
            StatsRole::Reviewer,
            StatsRole::Judge,
            StatsRole::Orchestrator,
            StatsRole::Merger,
            StatsRole::Main,
        ]
        .into_iter()
        .find(|role| self.did(*role) > 0)
    }

    pub fn spent_in(&self, role: StatsRole) -> Spend {
        self.spent_as.get(&role).copied().unwrap_or_default()
    }

    pub fn took_in(&self, role: StatsRole) -> u64 {
        self.took.get(&role).copied().unwrap_or(0)
    }

    /// The time its sessions took in every role, together.
    pub fn took_secs(&self) -> u64 {
        self.took.values().sum()
    }

    /// Its reviews that sent work back in `role`, or its sign-offs that refused it.
    pub fn back_in(&self, role: StatsRole) -> u32 {
        self.sent_back.get(&role).copied().unwrap_or(0)
    }

    /// Its reviews or sign-offs in `role`: those that let the work through first, those that
    /// sent it back after.
    pub fn accepted_as(&self, role: StatsRole) -> Fared {
        let (done, back) = (self.did(role), self.back_in(role));
        Fared {
            first: done.saturating_sub(back),
            once: back.min(done),
            more: 0,
        }
    }

    /// Its reviews and its sign-offs together, a judge's sign-off being a review too.
    pub fn reviewed(&self) -> Fared {
        let (reviews, signed) = (
            self.accepted_as(StatsRole::Reviewer),
            self.accepted_as(StatsRole::Judge),
        );
        Fared {
            first: reviews.first + signed.first,
            once: reviews.once + signed.once,
            more: 0,
        }
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

    pub fn did(&self, role: StatsRole) -> u32 {
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

    pub fn sent_back(&self, role: StatsRole) -> u32 {
        self.models
            .iter()
            .map(|model| model.sent_back.get(&role).copied().unwrap_or(0))
            .sum()
    }

    pub fn took(&self, role: StatsRole) -> u64 {
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
    /// How the runs ended, and what the judge and the merger settled in them.
    pub outcomes: Outcomes,
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
    pub role: Option<StatsRole>,
    pub tokens: u64,
    pub cost_usd: f64,
    pub done: u32,
}

impl DayPart {
    /// What a column or a sparkline measures of it. A conversation isn't a piece of work, as
    /// in [`Model::work`].
    pub fn measured(&self, measure: StatsMeasure) -> u64 {
        match measure {
            StatsMeasure::Tokens => self.tokens,
            StatsMeasure::Tasks if self.role == Some(StatsRole::Main) => 0,
            StatsMeasure::Tasks => u64::from(self.done),
            StatsMeasure::Spend => (self.cost_usd * 100.0).round() as u64,
        }
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
        // The records spell a model several ways, as the alias `haiku`, as `claude-haiku-5-5`
        // and with a date, and each screen shows it once.
        let mut spellings = Spellings::new(names);
        let met = (stats.work.iter().map(|row| (&row.agent, &row.model)))
            .chain(stats.tasks.iter().map(|task| (&task.agent, &task.model)))
            .chain(stats.days.iter().map(|part| (&part.agent, &part.model)))
            .chain(stats.fallbacks.iter().flat_map(|fallback| {
                [
                    (&fallback.agent, &fallback.model),
                    (&fallback.from_agent, &fallback.from_model),
                ]
            }));
        for (agent, model) in met {
            spellings.meet(agent, model.as_deref());
        }
        let mut spelled =
            |agent: &str, model: &Option<String>| spellings.of(agent, model.as_deref());

        let mut models: BTreeMap<(String, Option<String>), Model> = BTreeMap::new();
        for row in &stats.work {
            let model = spelled(&row.agent, &row.model);
            add_row(model_entry(&mut models, names, &row.agent, model), row);
        }
        let mut fared = Fared::default();
        let mut merged = 0;
        for task in &stats.tasks {
            let model = spelled(&task.agent, &task.model);
            let model = model_entry(&mut models, names, &task.agent, model);
            merged += u32::from(task.merged);
            // How a task fared counts once a reviewer or the judge looked at it, merged or
            // not, so a task sent back and never merged still lowers the first-pass share.
            if !task.reviewed {
                continue;
            }
            model.fared.add(task.sent_back);
            model.returns += task.sent_back;
            for why in &task.reasons {
                *model.returned_for.entry(why.clone()).or_default() += 1;
            }
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
                name: names.cli(&agent),
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
        }
        let shown = clis.iter().flat_map(|cli| &cli.models);
        let shades = ModelColors::new(&usage.all_time_models, names)
            .with(shown.map(|model| (model.agent.as_str(), model.model.as_deref())));
        for model in clis.iter_mut().flat_map(|cli| &mut cli.models) {
            model.shade = shades.place(&model.agent, model.model.as_deref());
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
            .map(|part| DayPart {
                day: part.day,
                agent: part.agent.clone(),
                model: spelled(&part.agent, &part.model),
                role: part.role,
                tokens: part.tokens,
                cost_usd: part.cost_usd,
                done: part.done,
            })
            .collect();

        let fallbacks = stats
            .fallbacks
            .iter()
            .map(|fallback| argonauts_core::crew_stats::Fallback {
                model: spelled(&fallback.agent, &fallback.model),
                from_model: spelled(&fallback.from_agent, &fallback.from_model),
                ..fallback.clone()
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
            fallbacks,
            outcomes: stats.outcomes,
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

    pub fn did(&self, role: StatsRole) -> u32 {
        self.clis.iter().map(|cli| cli.did(role)).sum()
    }

    pub fn measured(&self, measure: StatsMeasure) -> u64 {
        measured(&self.total, self.work(), measure)
    }

    /// The roles any model filled, in the who-did-what order.
    pub fn roles(&self) -> Vec<StatsRole> {
        StatsRole::BY_WORK
            .into_iter()
            .filter(|role| self.models().any(|(_, model)| model.roles().contains(role)))
            .collect()
    }

    /// The roles any model filled, in the order By role lists them.
    pub fn filled(&self) -> Vec<StatsRole> {
        StatsRole::ALL
            .into_iter()
            .filter(|role| self.models().any(|(_, model)| model.roles().contains(role)))
            .collect()
    }

    /// The models that filled `role`, in the order the cards have them.
    pub fn in_role(&self, role: StatsRole) -> Vec<(&Cli, &Model)> {
        self.models()
            .filter(|(_, model)| model.roles().contains(&role))
            .collect()
    }

    /// Why review sent the crew's tasks back, by how often.
    pub fn returned_for(&self) -> BTreeMap<String, u32> {
        let mut all = BTreeMap::new();
        for (_, model) in self.models() {
            for (why, n) in &model.returned_for {
                *all.entry(why.clone()).or_default() += n;
            }
        }
        all
    }

    /// Each slot's pieces of work over the parts `pick` keeps, conversations among them;
    /// `None` for a slot ahead of today.
    pub fn done_series(&self, pick: impl Fn(&DayPart) -> bool) -> Vec<Option<u64>> {
        self.slots()
            .iter()
            .map(|slot| {
                (!slot.ahead).then(|| {
                    self.days
                        .iter()
                        .filter(|part| slot.holds(part.day) && pick(part))
                        .map(|part| u64::from(part.done))
                        .sum()
                })
            })
            .collect()
    }

    /// The most pieces of work any one group's slot holds, the parts grouped by `key`: the
    /// scale the sparklines of [`Crew::done_series`] share.
    pub fn fullest_done<K: Ord>(&self, key: impl Fn(&DayPart) -> Option<K>) -> u64 {
        let mut sums: BTreeMap<(K, Date), u64> = BTreeMap::new();
        for slot in self.slots().iter().filter(|slot| !slot.ahead) {
            for part in self.days.iter().filter(|part| slot.holds(part.day)) {
                if let Some(key) = key(part) {
                    *sums.entry((key, slot.first)).or_default() += u64::from(part.done);
                }
            }
        }
        sums.into_values().max().unwrap_or(0)
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
    model: Option<String>,
) -> &'a mut Model {
    models
        .entry((agent.to_owned(), model.clone()))
        .or_insert_with(|| Model {
            agent: agent.to_owned(),
            name: names
                .model(agent, model.as_deref())
                .unwrap_or_else(|| names.cli(agent)),
            model,
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
    *model.sessions.entry(row.role).or_default() += row.sessions;
    *model.tool_calls.entry(row.role).or_default() += row.tool_calls;
    model.turns += row.turns;
    if !row.review.is_empty() {
        model.reviews.entry(row.role).or_default().add(&row.review);
    }
}

/// The share of its input `spent` read from cache, when it read any.
pub(super) fn cache_hit(spent: &Spend) -> Option<f64> {
    (spent.input_tokens > 0).then(|| spent.cached_tokens as f64 / spent.input_tokens as f64)
}

/// The tokens `spent` wrote in each of the `secs` seconds its sessions ran, to the nearest
/// one, when it wrote any and the time is known.
pub(super) fn written_a_second(spent: &Spend, secs: u64) -> Option<f64> {
    (secs > 0 && spent.output_tokens > 0)
        .then(|| (spent.output_tokens as f64 / secs as f64).round())
}

/// The reason with the highest count, the first by name among equals.
pub(super) fn most(reasons: Option<&BTreeMap<String, u32>>) -> Option<&str> {
    reasons?
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
        .map(|(why, _)| why.as_str())
}
