//! `Models { choices, role }`: the workers a role's rules allow, with family, access, cost,
//! quota, efforts, and strengths, the current one marked, and what the model catalog says of
//! each (`labels`). Only agent CLIs that are installed show; `/agents` lists the rest with how
//! to add them. Each CLI's or endpoint's models come together, its default first, then its
//! aliases, its models newest first, and its older ones.

use std::collections::BTreeSet;

use argonauts_core::app::ModelChoice;
use argonauts_core::limits::CostEstimate;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::registry::{ModelAccess, ModelEntry, RegistryKind};
use argonauts_core::session::AuthMode;
use argonauts_core::settings::Settings;
use argonauts_registry::models::CLAUDE_CODE_ALIASES;

use super::labels::Labels;
use crate::controller::Ctx;

/// Roles whose steps edit files or run tools, so an endpoint, which only answers, can't fill
/// them. The default agent works solo.
const NEEDS_HARNESS: &[RoleName] = &[RoleName::Default, RoleName::Owner, RoleName::Spike];

/// The token counts a typical card spends, for the cost column.
const CARD_INPUT_TOKENS: u64 = 150_000;
const CARD_OUTPUT_TOKENS: u64 = 15_000;

/// `None` is the default agent. Never an endpoint for a role whose step needs a harness, and
/// never a reviewer from the owner's family.
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
        .filter(|entry| {
            entry.kind == RegistryKind::Endpoint || installed.contains(&entry.id.worker)
        })
        .cloned()
        .collect();
    let with_models: BTreeSet<String> = entries
        .iter()
        .map(|entry| entry.id.worker.clone())
        .collect();
    // Harnesses the registry has no models for yet still run on their CLI's default model.
    let bare: Vec<WorkerRef> = settings
        .crew
        .iter()
        .map(|worker| worker.worker.clone())
        .chain(
            settings
                .agents
                .iter()
                .filter(|(_, agent)| agent.enabled)
                .map(|(id, _)| id.clone()),
        )
        .filter(|id| !with_models.contains(id) && !settings.endpoints.contains_key(id))
        .filter(|id| installed.contains(id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|id| WorkerRef::new(id, None))
        .collect();
    let wanted = role.unwrap_or(RoleName::Default);
    // Best fit first, then each harness's or endpoint's models together, in the order the
    // registry first names them.
    let mut order: Vec<String> = Vec::new();
    for entry in &entries {
        if !order.contains(&entry.id.worker) {
            order.push(entry.id.worker.clone());
        }
    }
    let group = |worker: &str| order.iter().position(|id| id == worker);
    entries.sort_by_key(|entry| (!entry.good_for.contains(&wanted), group(&entry.id.worker)));

    let mut choices: Vec<ModelChoice> = entries
        .into_iter()
        .map(|entry| from_entry(ctx, entry))
        .chain(bare.into_iter().map(|worker| bare_choice(ctx, worker)))
        .filter(|choice| refusal(ctx, role, &choice.worker).is_none())
        .collect();
    for choice in &mut choices {
        labels.describe(settings, role, choice);
    }
    let mut groups: Vec<String> = Vec::new();
    for choice in &choices {
        if !groups.contains(&choice.worker.worker) {
            groups.push(choice.worker.worker.clone());
        }
    }
    let group = |choice: &ModelChoice| groups.iter().position(|id| *id == choice.worker.worker);
    choices.sort_by(|a, b| group(a).cmp(&group(b)).then_with(|| labels.order(a, b)));
    for choice in &mut choices {
        choice.current = current
            .as_ref()
            .is_some_and(|current| same_worker(ctx, current, &choice.worker));
        if choice.worker.model.is_none() {
            choice.note = defaults
                .iter()
                .find(|(worker, _)| *worker == choice.worker.worker)
                .map(|(_, model)| format!("runs {model}"));
        }
    }
    choices
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
                .get(&entry.id.worker)
                .is_none_or(|a| a.model.is_none())
        })
        .filter_map(|entry| Some((entry.id.worker.clone(), entry.id.model.clone()?)));
    named.chain(marked).collect()
}

/// What an alias or a retired model stands for, in a few words.
fn entry_note(entry: &ModelEntry) -> Option<String> {
    let model = entry.id.model.as_deref()?;
    if alias_at(&entry.id.worker, model).is_some() {
        let mut letters = model.chars();
        let family: String = letters
            .next()
            .map(|first| first.to_uppercase().chain(letters).collect())
            .unwrap_or_default();
        return Some(format!("newest {family}"));
    }
    entry.retired.then(|| "retired".to_owned())
}

/// Why `worker` can't fill `role`, when it can't.
pub(crate) fn refusal(ctx: &Ctx, role: Option<RoleName>, worker: &WorkerRef) -> Option<String> {
    let role = role.unwrap_or(RoleName::Default);
    if NEEDS_HARNESS.contains(&role) && is_endpoint(ctx, worker) {
        let what = match role {
            RoleName::Default => "the default agent".to_owned(),
            role => format!("the {} role", role.as_str()),
        };
        return Some(format!(
            "{worker} is an endpoint, and {what} needs an agent CLI that can edit files"
        ));
    }
    if role == RoleName::Reviewer {
        let family = ctx.services.registry.family(worker);
        if owner_families(ctx).contains(&family) {
            return Some(format!(
                "{worker} is from the {family} family, like the owner, and a reviewer comes from \
                 another family"
            ));
        }
    }
    None
}

/// The worker that fills `role` now, `None` when the router picks.
pub(crate) fn current(settings: &Settings, role: Option<RoleName>) -> Option<WorkerRef> {
    match role.unwrap_or(RoleName::Default) {
        RoleName::Default => Some(settings.default_agent.clone()),
        RoleName::Narrator => settings.narrator.model.clone(),
        RoleName::Approver => settings.approvals.approver.model.clone(),
        role => settings
            .roles
            .get(&role)
            .and_then(|role| role.workers.first().cloned()),
    }
}

/// The effort levels the model filling `role` takes, as the catalog or its CLI says, else its
/// harness's usual levels, and every level when the router picks the worker. None for a model
/// that takes no effort setting.
pub(crate) fn efforts(ctx: &Ctx, role: Option<RoleName>) -> (Option<WorkerRef>, Vec<String>) {
    let worker = current(ctx.settings(), role).map(|worker| with_model(ctx.settings(), worker));
    let levels = levels_of(ctx, worker.as_ref());
    (worker, levels)
}

/// The effort levels `worker` takes, worked out as the models level shows them.
pub(crate) fn levels_of(ctx: &Ctx, worker: Option<&WorkerRef>) -> Vec<String> {
    levels_and_default(ctx, worker).0
}

/// The level `worker` starts at when no effort is set, as the models level shows it: a CLI's
/// default starts where the model it runs does, such as xhigh for Opus 5.5.
pub(crate) fn default_effort(ctx: &Ctx, worker: &WorkerRef) -> Option<String> {
    levels_and_default(ctx, Some(worker)).1
}

fn levels_and_default(ctx: &Ctx, worker: Option<&WorkerRef>) -> (Vec<String>, Option<String>) {
    let settings = ctx.settings();
    let registry = &ctx.services.registry;
    let listed = registry.list();
    let defaults = cli_defaults(settings, &listed);
    let labels = Labels::new(settings, registry.catalog(), &listed, &defaults);
    let target = worker.and_then(|worker| labels.target(worker));
    let harness = worker.map_or("", |worker| worker.worker.as_str());
    labels.levels(worker, target.as_ref(), harness)
}

/// The harnesses that are on and whose CLI is installed, as the worker directory finds them,
/// with the endpoints it offers.
fn installed(ctx: &Ctx) -> BTreeSet<String> {
    ctx.services
        .workers
        .available()
        .into_iter()
        .map(|worker| worker.worker)
        .collect()
}

/// A bare harness runs the model its `agents.<id>.model` names, when one does.
fn with_model(settings: &Settings, worker: WorkerRef) -> WorkerRef {
    if worker.model.is_some() {
        return worker;
    }
    let model = settings
        .agents
        .get(&worker.worker)
        .and_then(|agent| agent.model.clone());
    WorkerRef::new(worker.worker, model)
}

fn same_worker(ctx: &Ctx, current: &WorkerRef, choice: &WorkerRef) -> bool {
    current == choice || with_model(ctx.settings(), current.clone()) == *choice
}

fn is_endpoint(ctx: &Ctx, worker: &WorkerRef) -> bool {
    ctx.settings().endpoints.contains_key(&worker.worker)
        || ctx
            .services
            .registry
            .get(worker)
            .is_some_and(|entry| entry.kind == RegistryKind::Endpoint)
}

fn owner_families(ctx: &Ctx) -> BTreeSet<String> {
    ctx.settings()
        .roles
        .get(&RoleName::Owner)
        .map(|owner| {
            owner
                .workers
                .iter()
                .map(|worker| ctx.services.registry.family(worker))
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
            .estimate(&entry.id, CARD_INPUT_TOKENS, CARD_OUTPUT_TOKENS),
        quota: ctx.services.quota.get(&entry.id.vendor()),
        worker: entry.id,
        family: entry.family,
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
    }
}

fn bare_choice(ctx: &Ctx, worker: WorkerRef) -> ModelChoice {
    let access = match ctx.settings().agents.get(&worker.worker) {
        Some(agent) if agent.auth == AuthMode::ApiKey => ModelAccess::ApiKey,
        _ => ModelAccess::Subscription,
    };
    ModelChoice {
        family: ctx.services.registry.family(&worker),
        access,
        cost: CostEstimate::Unknown,
        quota: ctx.services.quota.get(&worker.vendor()),
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
    }
}
