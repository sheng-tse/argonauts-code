//! `[roles.<role>]`: fresh-session defaults, which workers a role may name, and roles that
//! never run.

use argonauts_core::adapter::KNOWN_AGENT_IDS;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::settings::handoff::FallbackEntry;
use toml::{Table, Value};

use super::validate::Problem;
use crate::harnesses::{builtin_counts, every_builtin_harness};
use crate::models::builtin_endpoint;

/// Roles that start from a fresh session unless their table says otherwise, so nothing they
/// saw before sways a verdict.
const FRESH_BY_DEFAULT: [&str; 3] = ["judge", "adjudicator", "approver"];

/// Roles whose steps need a harness with tools. An endpoint has none, so it can review,
/// critique, judge, and narrate, but not these.
const HARNESS_ROLES: [RoleName; 5] = [
    RoleName::Restater,
    RoleName::Drafter,
    RoleName::Spike,
    RoleName::Owner,
    RoleName::Default,
];

/// Gives each judge, adjudicator, and approver table that doesn't set `fresh_session` a fresh
/// session. A role with no table gets the same default where it runs.
pub(crate) fn fill_fresh_sessions(merged: &mut Table) {
    let Some(Value::Table(roles)) = merged.get_mut("roles") else {
        return;
    };
    for role in FRESH_BY_DEFAULT {
        if let Some(Value::Table(table)) = roles.get_mut(role) {
            table.entry("fresh_session").or_insert(Value::Boolean(true));
        }
    }
}

/// Which ids name a harness and which an endpoint. An id nobody knows is a warning, never an
/// error, since a `models.toml` that doesn't parse yet may be meant to declare it.
pub(crate) struct Workers<'a> {
    settings: &'a Settings,
    builtin_harnesses: Vec<String>,
    /// `[[harness]]` ids from your `models.toml` files.
    declared: &'a [String],
    /// Agents a settings layer turned on with `enabled = true`.
    turned_on: Vec<String>,
}

impl<'a> Workers<'a> {
    pub fn new(settings: &'a Settings) -> Self {
        let builtin_harnesses = every_builtin_harness()
            .into_iter()
            .map(|manifest| manifest.id)
            .collect();
        Self {
            settings,
            builtin_harnesses,
            declared: &[],
            turned_on: Vec::new(),
        }
    }

    /// With what resolving knows beyond the settings: the declared harnesses and the agents
    /// turned on.
    pub fn resolving(self, declared: &'a [String], turned_on: Vec<String>) -> Self {
        Self {
            declared,
            turned_on,
            ..self
        }
    }

    fn is_declared(&self, id: &str) -> bool {
        self.declared.iter().any(|declared| declared == id)
    }

    /// A built-in profile that stays off until `agents.<id>.enabled = true`, and that no
    /// `models.toml` declares in its place.
    pub fn is_off(&self, id: &str) -> bool {
        !builtin_counts(id, &self.turned_on) && !self.is_declared(id)
    }

    pub fn is_endpoint(&self, id: &str) -> bool {
        self.settings.endpoints.contains_key(id) || builtin_endpoint(id).is_some()
    }

    pub fn is_known(&self, id: &str) -> bool {
        KNOWN_AGENT_IDS.contains(&id)
            || self.builtin_harnesses.iter().any(|known| known == id)
            || self.settings.agents.contains_key(id)
            || self.is_endpoint(id)
            || self.is_declared(id)
    }
}

/// An endpoint where a step needs a harness: the default agent, a worker of a harness role, or
/// an entry in such a role's own handoff lists. The top-level `[handoff]` lists serve every
/// role, so an endpoint there is fine: the handoff filters the candidates for each role when it
/// happens.
pub(crate) fn endpoint_problems(settings: &Settings, workers: &Workers<'_>) -> Vec<Problem> {
    let mut problems = Vec::new();
    let endpoint = |worker: &WorkerRef| workers.is_endpoint(&worker.worker);
    if endpoint(&settings.default_agent) {
        problems.push(Problem::new(
            "default_agent",
            format!(
                "default_agent is {}, an endpoint; the default agent needs a harness with tools",
                settings.default_agent
            ),
        ));
    }
    for role in HARNESS_ROLES {
        let Some(table) = settings.roles.get(&role) else {
            continue;
        };
        let name = role.as_str();
        let mut lists: Vec<(String, Vec<&WorkerRef>)> = Vec::new();
        // `roles.default.workers` is ignored, and says so in a warning of its own.
        if role != RoleName::Default {
            lists.push((
                format!("roles.{name}.workers"),
                table.workers.iter().collect(),
            ));
        }
        lists.push((
            format!("roles.{name}.handoff.fallback"),
            entry_workers(&table.handoff.fallback),
        ));
        for (vendor, replacement) in &table.handoff.replace {
            lists.push((
                format!("roles.{name}.handoff.replace.{vendor}.with"),
                entry_workers(&replacement.with),
            ));
        }
        for (key, list) in lists {
            if let Some(worker) = list.into_iter().find(|worker| endpoint(worker)) {
                problems.push(Problem::new(
                    &key,
                    format!(
                        "{key} names {worker}, an endpoint, and the {name} role needs a harness \
                         with tools; endpoints can review, critique, judge, and narrate"
                    ),
                ));
            }
        }
    }
    problems
}

/// A run finds no harness for a worker that's off, so it says so now rather than then.
pub(crate) fn off(key: &str, worker: &WorkerRef) -> String {
    let id = &worker.worker;
    format!(
        "{key} names {worker}, which is off, so a run finds no harness for it; turn it on with \
         agents.{id}.enabled = true"
    )
}

/// A worker may be a harness from a `models.toml` that doesn't parse yet, so it only warns.
pub(crate) fn unknown(key: &str, worker: &WorkerRef) -> String {
    format!(
        "{key} names {worker}, which isn't a harness or endpoint Argonauts knows, and no \
         models.toml declares it as a [[harness]]"
    )
}

fn entry_workers(list: &[FallbackEntry]) -> Vec<&WorkerRef> {
    list.iter().map(FallbackEntry::worker).collect()
}

/// Role workers nobody knows or that are off, and an adjudicator that can never run.
pub(crate) fn warnings(settings: &Settings, workers: &Workers<'_>) -> Vec<String> {
    let mut warnings = Vec::new();
    if workers.is_off(&settings.default_agent.worker) {
        warnings.push(off("default_agent", &settings.default_agent));
    }
    for (role, table) in &settings.roles {
        if *role == RoleName::Default {
            continue;
        }
        let key = format!("roles.{}.workers", role.as_str());
        for worker in &table.workers {
            if workers.is_off(&worker.worker) {
                warnings.push(off(&key, worker));
            } else if !workers.is_known(&worker.worker) {
                warnings.push(unknown(&key, worker));
            }
        }
    }
    let reviewers = settings
        .roles
        .get(&RoleName::Reviewer)
        .and_then(|role| role.count);
    let most_reviewers = settings.review.per_card.max(reviewers.unwrap_or(0));
    if settings.roles.contains_key(&RoleName::Adjudicator) && most_reviewers < 2 {
        warnings.push(format!(
            "roles.adjudicator never runs: it settles reviewers who disagree, and no card gets \
             two reviewers (review.per_card is {})",
            settings.review.per_card
        ));
    }
    warnings
}
