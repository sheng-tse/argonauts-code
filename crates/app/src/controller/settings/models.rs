//! `Models { choices, role }`: the workers a role's rules allow, with provider, access, cost,
//! plan usage, efforts, and strengths, the current one marked, and what the model catalog says of
//! each (`labels`). Only agent CLIs that are installed show; `/agents` lists the rest with how
//! to add them. Each CLI's or endpoint's models come together, its default first, then its
//! aliases, its models newest first, and its older ones.

use std::collections::BTreeSet;

use argonauts_adapter_opencode::models as opencode_models;
use argonauts_core::app::ModelChoice;
use argonauts_core::limits::CostEstimate;
use argonauts_core::model_picker::ListedFacts;
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::registry::{ModelAccess, ModelEntry, RegistryKind};
use argonauts_core::session::AuthMode;
use argonauts_core::settings::Settings;
use argonauts_core::settings::endpoints::endpoint_settings;
use argonauts_registry::models::CLAUDE_CODE_ALIASES;

use super::labels::Labels;
use crate::controller::Ctx;

/// Roles whose steps edit files or run tools, so an endpoint, which only answers, can't fill
/// them. The main agent works as Captain.
const NEEDS_CLI: &[RoleName] = &[RoleName::Main, RoleName::Worker, RoleName::Experiment];

/// The CLI whose models come grouped by provider.
const OPENCODE: &str = "opencode";

/// The token counts a typical task spends, for the cost column.
const TASK_INPUT_TOKENS: u64 = 150_000;
const TASK_OUTPUT_TOKENS: u64 = 15_000;

/// `None` is the main agent. Never an endpoint for a role whose step needs a CLI, and
/// never a reviewer from the worker's provider.
pub(crate) fn choices(ctx: &Ctx, role: Option<RoleName>) -> Vec<ModelChoice> {
    let settings = ctx.settings();
    let current = current(settings, role);
    let registry = &ctx.services.registry;
    let installed = installed(ctx);
    let listed = registry.list();
    let defaults = cli_defaults(settings, &listed);
    let labels = Labels::new(settings, registry.catalog(), &listed, &defaults);
    let mut entries: Vec<ModelEntry> = listed
        .iter()
        .filter(|entry| entry.kind == RegistryKind::Endpoint || installed.contains(&entry.id.agent))
        .cloned()
        .collect();
    // A CLI listed with no Default of its own, as OpenCode's list is, still has one.
    let with_default: BTreeSet<String> = entries
        .iter()
        .filter(|entry| entry.id.model.is_none())
        .map(|entry| entry.id.agent.clone())
        .collect();
    // CLIs the registry has no models for yet still run on their CLI's default model.
    let bare: Vec<ModelRef> = settings
        .crew
        .members()
        .iter()
        .map(|worker| worker.agent.clone())
        .chain(
            settings
                .agents
                .iter()
                .filter(|(_, agent)| agent.enabled)
                .map(|(id, _)| id.clone()),
        )
        .filter(|id| !with_default.contains(id) && !settings.endpoints.contains_key(id))
        .filter(|id| installed.contains(id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|id| ModelRef::new(id, None))
        .collect();
    let wanted = role.unwrap_or(RoleName::Main);
    // Best fit first, then each CLI's or endpoint's models together, in the order the
    // registry first names them.
    let mut order: Vec<String> = Vec::new();
    for entry in &entries {
        if !order.contains(&entry.id.agent) {
            order.push(entry.id.agent.clone());
        }
    }
    let group = |worker: &str| order.iter().position(|id| id == worker);
    entries.sort_by_key(|entry| (!entry.good_for.contains(&wanted), group(&entry.id.agent)));

    let mut choices: Vec<ModelChoice> = entries
        .into_iter()
        .map(|entry| from_entry(ctx, entry))
        .chain(bare.into_iter().map(|worker| bare_choice(ctx, worker)))
        .filter(|choice| refusal(ctx, role, &choice.worker).is_none())
        .collect();
    for choice in &mut choices {
        labels.describe(settings, role, choice);
        choice.listed = listed_facts(&choice.worker, &listed);
        // OpenCode lists every one of these itself, so none is new to it.
        if choice.listed.is_some() {
            choice.new = false;
        }
    }
    let mut groups: Vec<String> = Vec::new();
    for choice in &choices {
        if !groups.contains(&choice.worker.agent) {
            groups.push(choice.worker.agent.clone());
        }
    }
    let group = |choice: &ModelChoice| groups.iter().position(|id| *id == choice.worker.agent);
    choices.sort_by(|a, b| group(a).cmp(&group(b)).then_with(|| labels.order(a, b)));
    for choice in &mut choices {
        choice.current = current
            .as_ref()
            .is_some_and(|current| same_worker(ctx, current, &choice.worker));
        if choice.worker.model.is_none() {
            choice.note = defaults
                .iter()
                .find(|(worker, _)| *worker == choice.worker.agent)
                .map(|(_, model)| format!("runs {model}"));
        }
    }
    choices
}

/// What OpenCode's list says of one of its models, for its row: the provider's group, the
/// window, tool use, and where a hosted one runs. A local tag's window is the least of
/// OpenCode's figure and the one its own server holds it to, since the server cuts the prompt
/// there.
fn listed_facts(worker: &ModelRef, listed: &[ModelEntry]) -> Option<ListedFacts> {
    if worker.agent != OPENCODE {
        return None;
    }
    let route = worker.model.as_deref()?;
    let (provider, tag) = route.split_once('/')?;
    let entry = listed.iter().find(|entry| entry.id == *worker)?;
    let local = opencode_models::is_local(provider);
    let served = listed
        .iter()
        .filter(|other| local && other.kind == RegistryKind::Endpoint)
        .filter(|other| other.access == ModelAccess::Local)
        .filter(|other| other.id.model.as_deref() == Some(tag))
        .find_map(|other| other.window);
    let window = [entry.window.or(entry.context_window), served]
        .into_iter()
        .flatten()
        .min();
    Some(ListedFacts {
        group: Some(opencode_models::provider_heading(provider)),
        window,
        no_tools: !entry.tools,
        thinking: entry.thinking == Some(true),
        hosted_by: (!local).then(|| opencode_models::provider_heading(provider)),
        memory: None,
    })
}

fn alias_at(worker: &str, model: &str) -> Option<usize> {
    (worker == "claude-code")
        .then(|| CLAUDE_CODE_ALIASES.iter().position(|alias| *alias == model))
        .flatten()
}

/// The model each CLI runs when you name none, where that's known: the one
/// `agents.<id>.model` names, else the one the CLI's own list marks as its default.
fn cli_defaults(settings: &Settings, listed: &[ModelEntry]) -> Vec<(String, String)> {
    let named = settings
        .agents
        .iter()
        .filter_map(|(id, agent)| Some((id.clone(), agent.model.clone()?)));
    let marked = listed
        .iter()
        .filter(|entry| entry.cli_default)
        .filter(|entry| {
            settings
                .agents
                .get(&entry.id.agent)
                .is_none_or(|a| a.model.is_none())
        })
        .filter_map(|entry| Some((entry.id.agent.clone(), entry.id.model.clone()?)));
    named.chain(marked).collect()
}

/// What an alias or a retired model stands for, in a few words.
fn entry_note(entry: &ModelEntry) -> Option<String> {
    let model = entry.id.model.as_deref()?;
    if alias_at(&entry.id.agent, model).is_some() {
        let mut letters = model.chars();
        let line: String = letters
            .next()
            .map(|first| first.to_uppercase().chain(letters).collect())
            .unwrap_or_default();
        return Some(format!("newest {line}"));
    }
    entry.retired.then(|| "retired".to_owned())
}

/// Why `worker` can't fill `role`, when it can't.
pub(crate) fn refusal(ctx: &Ctx, role: Option<RoleName>, worker: &ModelRef) -> Option<String> {
    let role = role.unwrap_or(RoleName::Main);
    if NEEDS_CLI.contains(&role) && is_endpoint(ctx, worker) {
        let what = match role {
            RoleName::Main => "the main agent".to_owned(),
            role => format!("the {} role", role.as_str()),
        };
        return Some(format!(
            "{worker} is an endpoint, and {what} needs an agent CLI that can edit files"
        ));
    }
    if role == RoleName::Reviewer {
        let provider = ctx.services.registry.provider(worker);
        if worker_providers(ctx).contains(&provider) {
            return Some(format!(
                "{worker} is from the {provider} provider, like the worker, and a reviewer comes \
                 from another provider"
            ));
        }
    }
    None
}

/// The worker that fills `role` now, `None` when the router picks.
pub(crate) fn current(settings: &Settings, role: Option<RoleName>) -> Option<ModelRef> {
    match role.unwrap_or(RoleName::Main) {
        RoleName::Main => Some(settings.crew.main.clone()),
        RoleName::Narrator => settings.narrator.model.clone(),

        role => settings
            .roles
            .get(&role)
            .and_then(|role| role.models.first().cloned()),
    }
}

/// The effort levels the model filling `role` takes, as the catalog or its CLI says, else its
/// CLI's usual levels, and every level when the router picks the worker. None for a model
/// that takes no effort setting.
pub(crate) fn efforts(ctx: &Ctx, role: Option<RoleName>) -> (Option<ModelRef>, Vec<String>) {
    let worker = current(ctx.settings(), role).map(|worker| with_model(ctx.settings(), worker));
    let levels = levels_of(ctx, worker.as_ref());
    (worker, levels)
}

/// The effort levels `worker` takes, worked out as the models level shows them.
pub(crate) fn levels_of(ctx: &Ctx, worker: Option<&ModelRef>) -> Vec<String> {
    levels_and_default(ctx, worker).0
}

/// The level `worker` starts at when no effort is set, as the models level shows it: a CLI's
/// default starts where the model it runs does, such as xhigh for Opus 5.5.
pub(crate) fn default_effort(ctx: &Ctx, worker: &ModelRef) -> Option<String> {
    levels_and_default(ctx, Some(worker)).1
}

fn levels_and_default(ctx: &Ctx, worker: Option<&ModelRef>) -> (Vec<String>, Option<String>) {
    let settings = ctx.settings();
    let registry = &ctx.services.registry;
    let listed = registry.list();
    let defaults = cli_defaults(settings, &listed);
    let labels = Labels::new(settings, registry.catalog(), &listed, &defaults);
    let target = worker.and_then(|worker| labels.target(worker));
    let cli = worker.map_or("", |worker| worker.agent.as_str());
    labels.levels(worker, target.as_ref(), cli)
}

/// The CLIs that are on and whose CLI is installed, as the worker directory finds them,
/// with the endpoints it offers.
fn installed(ctx: &Ctx) -> BTreeSet<String> {
    ctx.services
        .workers
        .available()
        .into_iter()
        .map(|worker| worker.agent)
        .collect()
}

/// A bare CLI runs the model its `agents.<id>.model` names, when one does.
fn with_model(settings: &Settings, worker: ModelRef) -> ModelRef {
    if worker.model.is_some() {
        return worker;
    }
    let model = settings
        .agents
        .get(&worker.agent)
        .and_then(|agent| agent.model.clone());
    ModelRef::new(worker.agent, model)
}

fn same_worker(ctx: &Ctx, current: &ModelRef, choice: &ModelRef) -> bool {
    current == choice || with_model(ctx.settings(), current.clone()) == *choice
}

fn is_endpoint(ctx: &Ctx, worker: &ModelRef) -> bool {
    ctx.settings().endpoints.contains_key(&worker.agent)
        || ctx
            .services
            .registry
            .get(worker)
            .is_some_and(|entry| entry.kind == RegistryKind::Endpoint)
}

fn worker_providers(ctx: &Ctx) -> BTreeSet<String> {
    ctx.settings()
        .roles
        .get(&RoleName::Worker)
        .map(|worker| {
            worker
                .models
                .iter()
                .map(|model| ctx.services.registry.provider(model))
                .collect()
        })
        .unwrap_or_default()
}

/// What the registry says; `Labels::describe` fills in the catalog's part, levels included.
fn from_entry(ctx: &Ctx, entry: ModelEntry) -> ModelChoice {
    let note = entry_note(&entry);
    let older = entry.retired || entry.successor.is_some();
    ModelChoice {
        note,
        older,
        cost: ctx
            .services
            .registry
            .estimate(&entry.id, TASK_INPUT_TOKENS, TASK_OUTPUT_TOKENS),
        plan_usage: ctx.services.plan_usage.get(&entry.id.agent_id()),
        worker: entry.id,
        provider: entry.provider,
        access: entry.access,
        efforts: Vec::new(),
        strengths: entry.strengths,
        current: false,
        name: String::new(),
        description: None,
        recommended: false,
        new: false,
        default_effort: None,
        effort: None,
        alias: false,
        runs: None,
        listed: None,
    }
}

fn bare_choice(ctx: &Ctx, worker: ModelRef) -> ModelChoice {
    // A server on this computer, such as Ollama, runs its models here.
    let local =
        endpoint_settings(ctx.settings(), &worker.agent).is_some_and(|endpoint| endpoint.local);
    let access = match ctx.settings().agents.get(&worker.agent) {
        _ if local => ModelAccess::Local,
        Some(agent) if agent.auth == AuthMode::ApiKey => ModelAccess::ApiKey,
        _ => ModelAccess::Subscription,
    };
    ModelChoice {
        provider: ctx.services.registry.provider(&worker),
        access,
        cost: CostEstimate::Unknown,
        plan_usage: ctx.services.plan_usage.get(&worker.agent_id()),
        efforts: Vec::new(),
        strengths: Vec::new(),
        current: false,
        note: None,
        older: false,
        name: String::new(),
        description: None,
        recommended: false,
        new: false,
        default_effort: None,
        effort: None,
        alias: false,
        runs: None,
        worker,
        listed: None,
    }
}
