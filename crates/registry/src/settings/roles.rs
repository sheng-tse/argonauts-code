//! `[roles.<role>]`: fresh-session defaults, which models a role may name, and roles that
//! never run.

use argonauts_core::adapter::KNOWN_AGENT_IDS;
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::Settings;
use toml::{Table, Value};

use super::validate::Problem;
use crate::clis::{builtin_counts, every_builtin_cli};
use crate::models::builtin_endpoint;

/// Roles that start from a fresh session unless their table says otherwise, so nothing they
/// saw before sways a verdict.
const FRESH_BY_DEFAULT: [&str; 1] = ["judge"];

/// Roles whose steps need a CLI with tools. An endpoint has none, so it can review,
/// critique, judge, and narrate, but not these.
const CLI_ROLES: [RoleName; 5] = [
    RoleName::Reader,
    RoleName::Draft,
    RoleName::Experiment,
    RoleName::Worker,
    RoleName::Main,
];

/// Gives a judge table that doesn't set `fresh_session` a fresh
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

/// Which ids name a CLI and which an endpoint. An id nobody knows is a warning, never an
/// error, since a `models.toml` that doesn't parse yet may be meant to declare it.
pub(crate) struct AgentIds<'a> {
    settings: &'a Settings,
    builtin_clis: Vec<String>,
    /// `[[cli]]` ids from your `models.toml` files.
    declared: &'a [String],
    /// Agents a settings layer turned on with `enabled = true`.
    turned_on: Vec<String>,
}

impl<'a> AgentIds<'a> {
    pub fn new(settings: &'a Settings) -> Self {
        let builtin_clis = every_builtin_cli()
            .into_iter()
            .map(|manifest| manifest.id)
            .collect();
        Self {
            settings,
            builtin_clis,
            declared: &[],
            turned_on: Vec::new(),
        }
    }

    /// With what resolving knows beyond the settings: the declared CLIs and the agents
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
            || self.builtin_clis.iter().any(|known| known == id)
            || self.settings.agents.contains_key(id)
            || self.is_endpoint(id)
            || self.is_declared(id)
    }
}

/// An endpoint where a step needs a CLI: the main agent, or a model of a CLI role.
/// The `[fallback]` lists each serve several roles, so an endpoint there is fine: the fallback
/// filters the candidates for each role when it happens.
pub(crate) fn endpoint_problems(settings: &Settings, agents: &AgentIds<'_>) -> Vec<Problem> {
    let mut problems = Vec::new();
    let endpoint = |worker: &ModelRef| agents.is_endpoint(&worker.agent);
    if endpoint(&settings.crew.main) {
        problems.push(Problem::new(
            "crew.main",
            format!(
                "crew.main is {}, an endpoint; the main agent needs a CLI with tools",
                settings.crew.main
            ),
        ));
    }
    for role in CLI_ROLES {
        let Some(table) = settings.roles.get(&role) else {
            continue;
        };
        let name = role.as_str();
        let mut lists: Vec<(String, Vec<&ModelRef>)> = Vec::new();
        // `roles.default.models` is ignored, and says so in a warning of its own.
        if role != RoleName::Main {
            lists.push((
                format!("roles.{name}.models"),
                table.models.iter().collect(),
            ));
        }
        for (key, list) in lists {
            if let Some(worker) = list.into_iter().find(|worker| endpoint(worker)) {
                problems.push(Problem::new(
                    &key,
                    format!(
                        "{key} names {worker}, an endpoint, and the {name} role needs a CLI \
                         with tools; endpoints can review, critique, judge, and narrate"
                    ),
                ));
            }
        }
    }
    problems
}

/// A run finds no CLI for an agent that's off, so it says so now rather than then.
pub(crate) fn off(key: &str, worker: &ModelRef) -> String {
    let id = &worker.agent;
    format!(
        "{key} names {worker}, which is off, so a run finds no CLI for it; turn it on with \
         agents.{id}.enabled = true"
    )
}

/// A model may name a CLI from a `models.toml` that doesn't parse yet, so it only warns.
pub(crate) fn unknown(key: &str, worker: &ModelRef) -> String {
    format!(
        "{key} names {worker}, which isn't a CLI or endpoint Argonauts Code knows, and no \
         models.toml declares it as a [[cli]]"
    )
}

/// Role models nobody knows or that are off.
pub(crate) fn warnings(settings: &Settings, agents: &AgentIds<'_>) -> Vec<String> {
    let mut warnings = Vec::new();
    if agents.is_off(&settings.crew.main.agent) {
        warnings.push(off("crew.main", &settings.crew.main));
    }
    for (role, table) in &settings.roles {
        if *role == RoleName::Main {
            continue;
        }
        let key = format!("roles.{}.models", role.as_str());
        for worker in &table.models {
            if agents.is_off(&worker.agent) {
                warnings.push(off(&key, worker));
            } else if !agents.is_known(&worker.agent) {
                warnings.push(unknown(&key, worker));
            }
        }
    }
    warnings
}
