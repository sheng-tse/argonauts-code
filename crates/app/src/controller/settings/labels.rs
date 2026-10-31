//! What the models level shows of each choice, from the registry's model catalog: the display
//! name, description, "recommended" mark, "new" for a model no layer knows, the levels the
//! model takes with its default, the level set now, and for a CLI's default and its aliases
//! the model they run. The catalog also orders each CLI's models newest first; an endpoint's
//! list keeps its own order, since the catalog doesn't cover what an endpoint serves.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use argonauts_core::app::ModelChoice;
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::models::model_catalog::{ModelCatalog, display_name_from_id};
use argonauts_core::models::registry::{ModelEntry, RegistryKind};
use argonauts_core::settings::Settings;
use argonauts_registry::models::{CLAUDE_CODE_ALIASES, usual_efforts};

pub(super) struct Labels<'a> {
    catalog: ModelCatalog,
    listed: &'a [ModelEntry],
    /// (CLI, model) for each CLI whose default model is known from your settings or its
    /// own list.
    defaults: &'a [(String, String)],
    endpoints: BTreeSet<String>,
}

impl<'a> Labels<'a> {
    pub(super) fn new(
        settings: &Settings,
        catalog: ModelCatalog,
        listed: &'a [ModelEntry],
        defaults: &'a [(String, String)],
    ) -> Self {
        let endpoints = listed
            .iter()
            .filter(|entry| entry.kind == RegistryKind::Endpoint)
            .map(|entry| entry.id.agent.clone())
            .chain(settings.endpoints.keys().cloned())
            .collect();
        Self {
            catalog,
            listed,
            defaults,
            endpoints,
        }
    }

    pub(super) fn describe(
        &self,
        settings: &Settings,
        role: Option<RoleName>,
        choice: &mut ModelChoice,
    ) {
        let worker = choice.worker.clone();
        let cli = worker.agent.as_str();
        choice.alias = self.is_alias(&worker);
        let target = self.target(&worker);
        let (efforts, default_effort) = self.levels(Some(&worker), target.as_ref(), cli);
        choice.efforts = efforts;
        choice.default_effort = default_effort;
        choice.effort = effort_set(settings, role, cli);
        choice.runs = None;
        choice.recommended = false;
        choice.new = false;
        match worker.model.as_deref() {
            None => {
                choice.name = String::new();
                let own = self.catalog.models.iter().find(|entry| entry.id == worker);
                choice.recommended = own.and_then(|entry| entry.recommended) == Some(true);
                // The default's own words fit the model the catalog says it runs; once you
                // set another, that model's words do.
                let runs_its_own = own
                    .and_then(|entry| entry.runs.as_deref())
                    .is_none_or(|runs| {
                        target.as_ref().and_then(|t| t.model.as_deref()) == Some(runs)
                    });
                choice.description = own
                    .and_then(|entry| entry.description.clone())
                    .filter(|_| runs_its_own)
                    .or_else(|| self.description(target.as_ref()));
                choice.runs = target
                    .as_ref()
                    .map(|target| self.catalog.label(target).name);
            }
            Some(alias) if choice.alias => {
                choice.name = display_name_from_id(alias);
                choice.description = self.description(target.as_ref());
                choice.runs = target
                    .as_ref()
                    .map(|target| self.catalog.label(target).name);
            }
            Some(_) => {
                let label = self.catalog.label(&worker);
                choice.name = label.name;
                choice.description = label.description;
                choice.recommended = label.recommended;
                choice.new = label.new && !choice.older && !self.endpoints.contains(cli);
            }
        }
    }

    /// Within one CLI's or endpoint's models: its default, its aliases, its models, then its
    /// older ones. A CLI's models go newest first, and so do its older ones, though older
    /// ones the catalog hasn't ranked follow the ranked ones, since they can't be newer.
    pub(super) fn order(&self, a: &ModelChoice, b: &ModelChoice) -> Ordering {
        let place = place_in_group(a);
        place.cmp(&place_in_group(b)).then_with(|| {
            // OpenCode's list comes in its groups, in the order they show.
            if self.endpoints.contains(&a.worker.agent) || a.listed.is_some() {
                return Ordering::Equal;
            }
            let newest = || self.catalog.newest_first(&a.worker, &b.worker);
            match place.0 {
                2 => newest(),
                3 => self.ranked(b).cmp(&self.ranked(a)).then_with(newest),
                _ => Ordering::Equal,
            }
        })
    }

    fn ranked(&self, choice: &ModelChoice) -> bool {
        self.catalog
            .entry(&choice.worker)
            .is_some_and(|entry| entry.rank.is_some())
    }

    /// The levels `worker` takes and its default. An alias or a default with levels of its own
    /// in the registry, as your `models.toml` may give it, takes those. Otherwise they're the
    /// levels of `target`, the model it runs, as the catalog says, else as the registry entry
    /// does, else its CLI's usual ones. The catalog's list may be empty, for a model that
    /// takes none.
    pub(super) fn levels(
        &self,
        worker: Option<&ModelRef>,
        target: Option<&ModelRef>,
        cli: &str,
    ) -> (Vec<String>, Option<String>) {
        let own = worker
            .filter(|worker| Some(*worker) != target)
            .and_then(|worker| self.listed.iter().find(|entry| entry.id == *worker))
            .filter(|entry| !entry.efforts.is_empty());
        if let Some(own) = own {
            // Levels of its own with no default of their own start where the model it runs does.
            let default = own
                .default_effort
                .clone()
                .or_else(|| self.catalog.entry(target?)?.default_effort.clone())
                .filter(|level| own.efforts.contains(level));
            return (own.efforts.clone(), default);
        }
        let described = target.and_then(|target| self.catalog.entry(target));
        let listed = target.and_then(|target| self.listed_entry(target));
        // A CLI that lists each model's variants, as OpenCode does, says which levels it
        // takes, and that one with none takes none. It names no default.
        if let Some(variants) = listed.and_then(|entry| entry.variants.clone()) {
            return (variants, None);
        }
        let efforts = described
            .and_then(|entry| entry.efforts.clone())
            .or_else(|| {
                listed
                    .map(|entry| entry.efforts.clone())
                    .filter(|levels| !levels.is_empty())
            })
            .unwrap_or_else(|| {
                usual_efforts(cli)
                    .iter()
                    .map(|level| (*level).to_owned())
                    .collect()
            });
        let default = described
            .and_then(|entry| entry.default_effort.clone())
            .or_else(|| listed.and_then(|entry| entry.default_effort.clone()))
            .filter(|level| efforts.contains(level));
        (efforts, default)
    }

    /// The model `worker` runs: a CLI's default runs the model your settings or its own list
    /// name, else the one the catalog says; an alias runs its provider's newest; a model runs
    /// itself.
    pub(super) fn target(&self, worker: &ModelRef) -> Option<ModelRef> {
        let model = match worker.model.as_deref() {
            Some(_) if self.is_alias(worker) => return self.alias_target(worker),
            Some(_) => return Some(worker.clone()),
            None => self
                .defaults
                .iter()
                .find(|(cli, _)| *cli == worker.agent)
                .map(|(_, model)| model.clone())
                .or_else(|| self.catalog.entry(worker)?.runs.clone())?,
        };
        let runs = ModelRef::new(worker.agent.clone(), Some(model));
        if self.is_alias(&runs) {
            return self.alias_target(&runs).or(Some(runs));
        }
        Some(runs)
    }

    /// Claude Code's aliases always are, though discovery lists them as models of their own.
    /// Any other name is an alias when the catalog says so and the CLI doesn't list it.
    fn is_alias(&self, worker: &ModelRef) -> bool {
        let Some(model) = worker.model.as_deref() else {
            return false;
        };
        if worker.agent == "claude-code" && CLAUDE_CODE_ALIASES.contains(&model) {
            return true;
        }
        self.catalog.is_alias(worker)
            && !self
                .listed
                .iter()
                .any(|entry| entry.id == *worker && entry.discovered)
    }

    /// The current model the registry lists under that alias or of that provider, which follows
    /// Anthropic's list day to day, else the one the catalog names.
    fn alias_target(&self, alias: &ModelRef) -> Option<ModelRef> {
        let name = alias.model.as_deref()?;
        let current = |entry: &&ModelEntry| {
            entry.id.agent == alias.agent
                && entry.id != *alias
                && entry.discovered
                && !entry.retired
                && entry.successor.is_none()
        };
        let provider_word = |entry: &&ModelEntry| {
            entry
                .id
                .model
                .as_deref()
                .is_some_and(|model| model.split(['-', '.', ':']).any(|word| word == name))
        };
        let listed = self
            .listed
            .iter()
            .filter(current)
            .find(|entry| entry.aliases.iter().any(|known| known == name))
            .or_else(|| self.listed.iter().filter(current).find(provider_word));
        listed
            .map(|entry| entry.id.clone())
            .or_else(|| self.catalog.entry(alias).map(|entry| entry.id.clone()))
    }

    fn listed_entry(&self, worker: &ModelRef) -> Option<&ModelEntry> {
        self.listed
            .iter()
            .find(|entry| entry.id == *worker)
            .or_else(|| {
                let model = worker.model.as_deref()?;
                self.listed.iter().find(|entry| {
                    entry.id.agent == worker.agent && entry.aliases.iter().any(|a| a == model)
                })
            })
    }

    fn description(&self, target: Option<&ModelRef>) -> Option<String> {
        self.catalog.label(target?).description
    }
}

/// Default first, then aliases in the order Claude Code names them, then models, then older.
fn place_in_group(choice: &ModelChoice) -> (u8, usize) {
    match choice.worker.model.as_deref() {
        None => (0, 0),
        Some(model) if choice.alias => {
            let at = CLAUDE_CODE_ALIASES.iter().position(|alias| *alias == model);
            (1, at.unwrap_or(CLAUDE_CODE_ALIASES.len()))
        }
        Some(_) if choice.older => (3, 0),
        Some(_) => (2, 0),
    }
}

/// The level set now: the role's, else, as a conversation turn reads it, the agent's.
pub(super) fn effort_set(settings: &Settings, role: Option<RoleName>, cli: &str) -> Option<String> {
    settings
        .roles
        .get(&role.unwrap_or(RoleName::Main))
        .and_then(|role| role.effort.clone())
        .or_else(|| {
            settings
                .agents
                .get(cli)
                .and_then(|agent| agent.effort.clone())
        })
}
