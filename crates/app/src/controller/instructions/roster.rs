//! The agents a request may name, and their models and efforts, as Argonauts Code knows them now.

use argonauts_core::cheapest::{cheapest_listed, price_of};
use argonauts_core::input::EFFORT_LEVELS;
use argonauts_core::names::ModelRef;
use argonauts_core::registry::{ModelRegistry, RegistryKind, known_efforts};

use super::words::{first_word, key};
use crate::controller::Ctx;

/// The agents a request may name, as Argonauts Code knows them right now.
#[derive(Debug, Clone, Default)]
pub(crate) struct Roster {
    pub agents: Vec<Agent>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Agent {
    pub id: String,
    /// `Claude Code`.
    pub name: String,
    /// What review rules compare, such as `claude` or `gpt`.
    pub provider: String,
    /// An endpoint only answers, so it can't build or be the main agent.
    pub endpoint: bool,
    pub state: AgentState,
    /// Its models as its live list gives them. Empty when the list isn't known.
    pub models: Vec<Model>,
    /// The model and effort its cheapest work runs on.
    pub cheapest: Option<(String, Option<String>)>,
    /// The models it lists that the catalog prices, cheapest first, each once, as it names
    /// them.
    pub by_price: Vec<String>,
    /// The model it runs when nothing names one, when that is known.
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum AgentState {
    #[default]
    Ready,
    NotInstalled,
    TurnedOff,
    /// Out of plan usage, or its agent is down.
    Down,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Model {
    pub id: String,
    /// `Opus 5.5`.
    pub name: String,
    pub aliases: Vec<String>,
    /// The effort levels it takes. `None` when nothing says, and then any level goes.
    pub efforts: Option<Vec<String>>,
}

impl Roster {
    /// Every CLI and endpoint the settings, the adapters, and the registry know of, with
    /// whether each can work now.
    pub(crate) fn new(ctx: &Ctx) -> Self {
        let settings = ctx.settings();
        let registry = &ctx.services.registry;
        let adapters = crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved())
            .unwrap_or_default();
        let available = ctx.services.workers.available();
        let listed = registry.list();
        let mut ids: Vec<String> = Vec::new();
        let known = adapters
            .iter()
            .map(|adapter| adapter.id().to_owned())
            .chain(settings.agents.keys().cloned())
            .chain(settings.endpoints.keys().cloned())
            .chain(available.iter().map(|worker| worker.agent.clone()))
            .chain(
                listed
                    .iter()
                    .filter(|entry| entry.kind == RegistryKind::Endpoint)
                    .map(|entry| entry.id.agent.clone()),
            );
        for id in known {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let agents = ids
            .into_iter()
            .map(|id| {
                let bare = ModelRef::new(id.clone(), None);
                let endpoint = settings.endpoints.contains_key(&id)
                    || listed
                        .iter()
                        .any(|entry| entry.id.agent == id && entry.kind == RegistryKind::Endpoint);
                let enabled = settings.agents.get(&id).is_none_or(|agent| agent.enabled);
                let state = if available.iter().any(|worker| worker.agent == id) {
                    if crate::controller::turn::agent_out(ctx, &bare) {
                        AgentState::Down
                    } else {
                        AgentState::Ready
                    }
                } else if enabled {
                    AgentState::NotInstalled
                } else {
                    AgentState::TurnedOff
                };
                let models = listed
                    .iter()
                    .filter(|entry| entry.id.agent == id)
                    .filter_map(|entry| {
                        let model = entry.id.model.clone()?;
                        Some(Model {
                            name: ctx
                                .names
                                .model(&id, &model)
                                .unwrap_or_else(|| model.clone()),
                            aliases: entry.aliases.clone(),
                            efforts: known_efforts(registry.as_ref(), &entry.id),
                            id: model,
                        })
                    })
                    .collect();
                // The CLI's own list decides, and the adapter's pick stands in until it's read.
                let cheapest = cheapest_listed(registry.as_ref(), &id)
                    .map(|cheapest| (cheapest.model, cheapest.effort))
                    .or_else(|| {
                        adapters
                            .iter()
                            .find(|adapter| adapter.id() == id)
                            .map(|adapter| {
                                let (model, effort) = adapter.cheapest_model();
                                (model.to_owned(), effort.map(str::to_owned))
                            })
                    });
                Agent {
                    name: ctx.names.cli(&id),
                    provider: registry.provider(&bare),
                    endpoint,
                    state,
                    models,
                    cheapest,
                    by_price: by_price(registry.as_ref(), &id),
                    default_model: ctx.names.names().defaults.get(&id).cloned(),
                    id,
                }
            })
            .collect();
        Self { agents }
    }

    pub(super) fn agent(&self, id: &str) -> Option<&Agent> {
        self.agents.iter().find(|agent| agent.id == id)
    }

    pub(super) fn name(&self, id: &str) -> String {
        self.agent(id)
            .map_or_else(|| id.to_owned(), |agent| agent.name.clone())
    }

    /// The agent `who` names, by its id, its name, a short name, its provider or maker, or one of
    /// its models, with the model when it names one. Agent CLIs come before endpoints, and ones
    /// that can work before ones that can't.
    pub(super) fn find(&self, who: &str) -> Option<(&Agent, Option<String>)> {
        let wanted = key(who);
        if wanted.is_empty() {
            return None;
        }
        let best = |matches: &dyn Fn(&Agent) -> bool| {
            self.agents
                .iter()
                .filter(|agent| matches(agent))
                .min_by_key(|agent| (agent.endpoint, agent.state != AgentState::Ready))
        };
        let exact = |agent: &Agent| key(&agent.id) == wanted || key(&agent.name) == wanted;
        let short = |agent: &Agent| {
            NICKNAMES.contains(&(wanted.as_str(), agent.id.as_str()))
                || key(first_word(&agent.id)) == wanted
                || key(first_word(&agent.name)) == wanted
        };
        let maker = |agent: &Agent| !agent.provider.is_empty() && key(&agent.provider) == wanted;
        if let Some(agent) = best(&exact)
            .or_else(|| best(&short))
            .or_else(|| best(&maker))
        {
            return Some((agent, None));
        }
        let with_model = |agent: &Agent| known_model(agent, who).is_some();
        let agent = best(&with_model)?;
        Some((agent, known_model(agent, who)))
    }
}

/// The models `cli` lists that the catalog prices, cheapest first. A model listed under an
/// alias and its own id counts once, by the first name it was listed under.
fn by_price(registry: &dyn ModelRegistry, cli: &str) -> Vec<String> {
    let catalog = registry.catalog();
    let mut listed: Vec<ModelRef> = registry
        .list()
        .into_iter()
        .filter(|entry| entry.id.agent == cli && !entry.retired)
        .map(|entry| entry.id)
        .collect();
    // The catalog keeps the lists a CLI shows only in its own picker, as Claude Code's.
    listed.extend(
        catalog
            .models
            .iter()
            .filter(|entry| entry.id.agent == cli)
            .map(|entry| entry.id.clone()),
    );
    let mut priced: Vec<(f64, String, String)> = Vec::new();
    for id in &listed {
        let Some(model) = id.model.as_deref() else {
            continue;
        };
        let runs = catalog.runs(id).unwrap_or_else(|| model.to_owned());
        if priced.iter().any(|(_, known, _)| *known == runs) {
            continue;
        }
        let Some(price) = price_of(&catalog, &runs) else {
            continue;
        };
        priced.push((price.input.0 + price.output.0, runs, model.to_owned()));
    }
    priced.sort_by(|a, b| a.0.total_cmp(&b.0));
    priced.into_iter().map(|(_, _, model)| model).collect()
}

/// Short names people use that aren't an agent's id or name.
const NICKNAMES: &[(&str, &str)] = &[
    ("agy", "antigravity-cli"),
    ("gemini", "antigravity-cli"),
    ("gpt", "codex"),
];

/// The model of `agent` a request's word names, or why the agent stays on its default model.
/// Without its live list nothing says the agent has the model, so the word isn't passed on:
/// a run would start the CLI on a model it may not have.
pub(super) fn model_named(roster: &Roster, agent: &Agent, asked: &str) -> Result<String, String> {
    let asked = asked.trim();
    let missing = || format!("{} has no model named {asked}", agent.name);
    if key(asked).is_empty() {
        return Err(missing());
    }
    if !agent.models.is_empty() {
        return known_model(agent, asked).ok_or_else(missing);
    }
    let anothers = roster
        .agents
        .iter()
        .any(|other| other.id != agent.id && known_model(other, asked).is_some());
    if anothers {
        return Err(missing());
    }
    Err(format!(
        "{}'s models couldn't be checked, so it stays on its default model",
        agent.name
    ))
}

/// The model of `agent`'s live list a word names: by id, alias, or name, or a word of its id
/// such as `opus`.
pub(super) fn known_model(agent: &Agent, asked: &str) -> Option<String> {
    let wanted = key(asked);
    if wanted.is_empty() {
        return None;
    }
    let exact = agent.models.iter().find(|model| {
        key(&model.id) == wanted
            || key(&model.name) == wanted
            || model.aliases.iter().any(|alias| key(alias) == wanted)
    });
    let by_word = || {
        agent.models.iter().find(|model| {
            model
                .id
                .split(['-', '_', ' ', '.', '/'])
                .chain(model.name.split(' '))
                .any(|word| key(word) == wanted)
        })
    };
    exact.or_else(by_word).map(|model| model.id.clone())
}

pub(super) fn display_model(agent: &Agent, model: &str) -> String {
    agent
        .models
        .iter()
        .find(|known| known.id == model)
        .map_or_else(|| model.to_owned(), |known| known.name.clone())
}

/// `level`, or the nearest level the model takes, with a note when it is different. `None` for a
/// model that takes no effort setting.
pub(super) fn clamp(
    level: &str,
    agent: Option<&Agent>,
    model: Option<&str>,
) -> (Option<String>, Option<String>) {
    let known = agent.and_then(|agent| {
        let model = model?;
        let found = agent.models.iter().find(|known| known.id == model)?;
        Some((found.name.clone(), found.efforts.clone()?))
    });
    let Some((name, efforts)) = known else {
        return (Some(level.to_owned()), None);
    };
    if efforts.iter().any(|taken| taken == level) {
        return (Some(level.to_owned()), None);
    }
    if efforts.is_empty() {
        return (None, Some(format!("{name} takes no effort setting")));
    }
    let rank = |level: &str| EFFORT_LEVELS.iter().position(|known| *known == level);
    let wanted = rank(level).unwrap_or(0);
    let mut ranked: Vec<(usize, &String)> = efforts
        .iter()
        .filter_map(|taken| Some((rank(taken)?, taken)))
        .collect();
    ranked.sort();
    let below = ranked.iter().rev().find(|(at, _)| *at <= wanted);
    let Some((_, taken)) = below.or(ranked.first()) else {
        return (Some(level.to_owned()), None);
    };
    let top = ranked
        .last()
        .map(|(_, top)| top.as_str())
        .unwrap_or_default();
    let note = if rank(top).unwrap_or(0) < wanted {
        format!("{name} takes up to {top}, so {taken}")
    } else {
        format!("{name} doesn't take {level}, so {taken}")
    };
    (Some((*taken).clone()), Some(note))
}
