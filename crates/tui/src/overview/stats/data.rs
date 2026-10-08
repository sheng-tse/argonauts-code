//! The crew's stats summed the ways Stats shows them: each CLI with its models, each model
//! across its roles, and each role with the models that filled it. Every view reads these,
//! so a model keeps its shade and its numbers wherever it shows.

use std::collections::BTreeMap;

use argonauts_core::Spend;
use argonauts_core::api::{PlanUsage, UsageSummary};
use argonauts_core::crew_stats::{CrewRole, CrewStats, WorkRow};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::settings::stats::StatsMeasure;
use time::Date;

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
#[derive(Debug, Clone, Default)]
pub(super) struct Crew {
    /// The main agent's CLI first, then the most tokens first.
    pub clis: Vec<Cli>,
    pub workflows: u32,
    pub took_secs: u64,
    /// Every task, and how it fared in review.
    pub fared: Fared,
    pub merged: u32,
    /// The last seven local days, oldest first, each CLI's models' tokens and spend.
    pub days: Vec<(Date, Vec<DayPart>)>,
    pub fallbacks: Vec<argonauts_core::crew_stats::Fallback>,
    pub total: Spend,
}

#[derive(Debug, Clone)]
pub(super) struct DayPart {
    pub agent: String,
    pub model: Option<String>,
    pub tokens: u64,
    pub cost_usd: f64,
}

impl DayPart {
    pub fn measured(&self, measure: StatsMeasure) -> u64 {
        match measure {
            StatsMeasure::Spend => (self.cost_usd * 100.0).round() as u64,
            _ => self.tokens,
        }
    }
}

impl Crew {
    pub fn of(usage: &UsageSummary, stats: &CrewStats, names: &DisplayNames, today: Date) -> Self {
        let mut models: BTreeMap<(String, Option<String>), Model> = BTreeMap::new();
        for row in &stats.work {
            let model = model_entry(&mut models, names, &row.agent, row.model.as_ref());
            add_row(model, row);
        }
        let mut fared = Fared::default();
        let mut merged = 0;
        for task in &stats.tasks {
            let model = model_entry(&mut models, names, &task.agent, task.model.as_ref());
            merged += u32::from(task.merged);
            // How a task fared in review counts once it was reviewed and merged, so the
            // squares and the first-pass share never count work nobody looked at.
            if !(task.reviewed && task.merged) {
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

        let days = (0..7)
            .rev()
            .map(|back| today.saturating_sub(time::Duration::days(back)))
            .map(|day| {
                let parts = stats
                    .days
                    .iter()
                    .filter(|part| part.day == day)
                    .map(|part| DayPart {
                        agent: part.agent.clone(),
                        model: part.model.clone(),
                        tokens: part.tokens,
                        cost_usd: part.cost_usd,
                    })
                    .collect();
                (day, parts)
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
        }
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

    /// Each day's tokens or spend for the models `pick` keeps, oldest first.
    pub fn daily(&self, measure: StatsMeasure, pick: impl Fn(&DayPart) -> bool) -> Vec<u64> {
        self.days
            .iter()
            .map(|(_, parts)| {
                parts
                    .iter()
                    .filter(|part| pick(part))
                    .map(|part| part.measured(measure))
                    .sum()
            })
            .collect()
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
}
