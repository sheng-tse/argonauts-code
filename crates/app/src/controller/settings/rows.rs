//! A picker's rows: `settings_catalog()` joined with the resolved entries, with real map keys,
//! the layer each value came from, and the command's default scope for each key.

use std::collections::BTreeSet;

use argonauts_core::app::SettingRow;
use argonauts_core::input::{PERMISSION_KEYS, ROLE_SANDBOX_KEYS};
use argonauts_core::names::RoleName;
use argonauts_core::settings::{SettingMeta, SettingScope, Settings};
use argonauts_registry::harnesses::agents_turned_on;
use argonauts_registry::{builtin_harnesses, setting_meta, settings_catalog};

use crate::app_settings::AppSettings;

/// Every role, in the order pickers list them.
pub(crate) const ROLES: &[RoleName] = &[
    RoleName::Default,
    RoleName::Coordinator,
    RoleName::PhaseLead,
    RoleName::Restater,
    RoleName::Differ,
    RoleName::Drafter,
    RoleName::Mapper,
    RoleName::Critic,
    RoleName::Spike,
    RoleName::Judge,
    RoleName::Owner,
    RoleName::Consultant,
    RoleName::Reviewer,
    RoleName::Adjudicator,
    RoleName::Retro,
    RoleName::Approver,
    RoleName::Narrator,
];

/// The roles whose model has a key of its own rather than `roles.<role>.workers`.
const OWN_MODEL_KEY: &[RoleName] = &[RoleName::Default, RoleName::Approver, RoleName::Narrator];

const AGENT_KEYS: &[&str] = &["enabled", "model", "effort", "auth", "binary"];
const NARRATOR_KEYS: &[&str] = &[
    "ui.narrator",
    "narrator.model",
    "narrator.interval",
    "narrator.spend_cap",
];
const THEME_KEYS: &[&str] = &["ui.theme", "ui.accent", "ui.color", "ui.motion"];

/// Rows for `command` (such as `effort`), or for `/config`'s `section` (a dotted key prefix).
/// `/preset` and `/workflows` list names rather than keys, so their rows are built elsewhere.
pub(crate) fn rows(
    settings: &AppSettings,
    command: Option<&str>,
    section: Option<&str>,
) -> Vec<SettingRow> {
    let command_name = command.unwrap_or("config");
    let keys = match command_name {
        "model" => model_keys(),
        "effort" => role_keys("effort"),
        "agents" => agents_keys(settings),
        "approvals" => under(catalog_keys(settings, &[]), "approvals"),
        "budget" => under(catalog_keys(settings, &[]), "budget"),
        "handoff" => {
            let roles: Vec<RoleName> = section.and_then(section_role).into_iter().collect();
            let keys: Vec<String> = catalog_keys(settings, &roles)
                .into_iter()
                .filter(|key| in_section(key, "handoff") || is_role_handoff(key))
                .collect();
            with_handoff_options(keys)
                .into_iter()
                .filter(|key| !is_summarizer(key) || summarizer_wanted(settings, key))
                .collect()
        }
        "permissions" => permission_keys(settings),
        "narrator" => NARRATOR_KEYS.iter().map(|key| (*key).to_owned()).collect(),
        "theme" => THEME_KEYS.iter().map(|key| (*key).to_owned()).collect(),
        "verbosity" => vec!["ui.verbosity".to_owned()],
        _ => catalog_keys(settings, &[]),
    };
    keys.into_iter()
        .filter(|key| section.is_none_or(|section| in_section(key, section)))
        .map(|key| {
            let scope = default_scope(command_name, &key);
            row(settings, key, scope)
        })
        .collect()
}

/// One row for `key`, with its catalog entry, its value in effect, and where it came from.
pub(crate) fn row(settings: &AppSettings, key: String, default_scope: SettingScope) -> SettingRow {
    // The catalog lists the top-level handoff options bare, so these words come first.
    let meta = handoff_option(&key)
        .or_else(|| setting_meta(&key))
        .unwrap_or_else(|| blank_meta(&key));
    SettingRow {
        value: settings
            .entry(&key)
            .map(|entry| entry.value.clone())
            .unwrap_or_default(),
        preset_from: settings.preset_from(&settings.layer(&key)),
        layer: settings.layer(&key),
        default_scope,
        safety: meta.safety,
        allowed: meta.allowed,
        description: meta.description,
        key,
    }
}

/// Where `command` saves `key` unless you pick otherwise.
pub(crate) fn default_scope(command: &str, key: &str) -> SettingScope {
    use SettingScope::{Global, Project, ProjectLocal};
    match command {
        "model" | "effort" => match key {
            "narrator.model" => Global,
            "approvals.approver.model" => Project,
            _ => ProjectLocal,
        },
        "preset" | "workflow" | "approvals" | "budget" | "handoff" | "permissions" => {
            if key == "limits.usd_per_day" {
                Global
            } else {
                Project
            }
        }
        "agents" => {
            let segments: Vec<&str> = key.split('.').collect();
            match segments.as_slice() {
                ["endpoints", ..] | ["agents", _, "auth" | "binary"] => Global,
                _ => Project,
            }
        }
        "narrator" | "theme" | "vim" | "verbosity" => Global,
        // A folder of yours, and often one for now only.
        "add-dir" => SettingScope::Session,
        _ => setting_meta(key).map_or(Project, |meta| meta.default_scope),
    }
}

/// Whether `key` is `section` itself or under it.
pub(crate) fn in_section(key: &str, section: &str) -> bool {
    key == section
        || key
            .strip_prefix(section)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// `default_agent`, each role's workers, then the narrator's and the approver's model.
fn model_keys() -> Vec<String> {
    let mut keys = vec!["default_agent".to_owned()];
    keys.extend(
        ROLES
            .iter()
            .filter(|role| !OWN_MODEL_KEY.contains(role))
            .map(|role| format!("roles.{}.workers", role.as_str())),
    );
    keys.push("narrator.model".to_owned());
    keys.push("approvals.approver.model".to_owned());
    keys
}

/// `roles.<role>.<leaf>` for the default agent and every role with a worker list.
fn role_keys(leaf: &str) -> Vec<String> {
    ROLES
        .iter()
        .filter(|role| !matches!(role, RoleName::Approver | RoleName::Narrator))
        .map(|role| format!("roles.{}.{leaf}", role.as_str()))
        .collect()
}

/// `crew`, each harness's enabled, model, effort, auth, and binary, and every endpoint key.
/// Each agent's keys: the ones your settings name, then the built-in profiles in play, such as
/// OpenCode's over ACP and Hermes once you turn it on.
fn agents_keys(app: &AppSettings) -> Vec<String> {
    let settings = app.settings();
    let mut keys = vec!["crew".to_owned()];
    let profiles = builtin_harnesses(&agents_turned_on(app.resolved()));
    let ids = settings
        .agents
        .keys()
        .cloned()
        .chain(profiles.into_iter().map(|profile| profile.id));
    for id in ids {
        keys.extend(AGENT_KEYS.iter().map(|leaf| format!("agents.{id}.{leaf}")));
    }
    for meta in settings_catalog() {
        if meta.key.starts_with("endpoints.") {
            keys.extend(expand(&meta.key, settings, &[]));
        }
    }
    dedup(keys)
}

/// `/permissions`: the sandbox every agent gets, its keys in [`PERMISSION_KEYS`] first and the
/// rest of `[sandbox]` after them, then the limits, then each role's own sandbox, unset unless
/// that role sets it.
fn permission_keys(settings: &AppSettings) -> Vec<String> {
    let (sandbox, limits): (Vec<String>, Vec<String>) = PERMISSION_KEYS
        .iter()
        .map(|key| (*key).to_owned())
        .partition(|key| in_section(key, "sandbox"));
    let mut keys = sandbox;
    keys.extend(under(catalog_keys(settings, &[]), "sandbox"));
    keys.extend(limits);
    for role in ROLES {
        keys.extend(
            ROLE_SANDBOX_KEYS
                .iter()
                .map(|leaf| format!("roles.{}.sandbox.{leaf}", role.as_str())),
        );
    }
    dedup(keys)
}

fn under(keys: Vec<String>, section: &str) -> Vec<String> {
    keys.into_iter()
        .filter(|key| in_section(key, section))
        .collect()
}

fn is_role_handoff(key: &str) -> bool {
    matches!(
        key.split('.').collect::<Vec<_>>().as_slice(),
        ["roles", _, "handoff", ..]
    )
}

/// `handoff.summarizer`, or a role's.
fn is_summarizer(key: &str) -> bool {
    matches!(
        key.split('.').collect::<Vec<_>>().as_slice(),
        ["handoff", "summarizer"] | ["roles", _, "handoff", "summarizer"]
    )
}

/// Whether `/handoff` lists a summarizer: it's set, so you can change or reset it, or a
/// handoff it would serve is told `note+summary`. The top-level one serves every role without
/// its own, so any handoff context counts for it; a role's serves that role, whose handoffs
/// may also follow the top-level context.
fn summarizer_wanted(settings: &AppSettings, key: &str) -> bool {
    let section = key.strip_suffix(".summarizer").unwrap_or(key);
    if settings.entry(key).is_some() {
        return true;
    }
    settings.resolved().entries.iter().any(|entry| {
        let serves = if section == "handoff" {
            in_section(&entry.key, "handoff") || is_role_handoff(&entry.key)
        } else {
            in_section(&entry.key, section) || in_section(&entry.key, "handoff")
        };
        serves && entry.key.ends_with(".context") && entry.value == NOTE_AND_SUMMARY
    })
}

/// `note+summary` as TOML text, the context that needs a summarizer.
const NOTE_AND_SUMMARY: &str = "\"note+summary\"";

/// The role a section such as `roles.reviewer.handoff` is about.
fn section_role(section: &str) -> Option<RoleName> {
    let name = section.strip_prefix("roles.")?.split('.').next()?;
    role_named(name)
}

pub(crate) fn role_named(name: &str) -> Option<RoleName> {
    ROLES.iter().copied().find(|role| role.as_str() == name)
}

/// Every key `/config` shows: the catalog with each `*` filled in from the settings, then any
/// key the settings hold that the catalog doesn't name. `extra_roles` fill `roles.*` beyond the
/// default agent and the roles your settings configure.
fn catalog_keys(settings: &AppSettings, extra_roles: &[RoleName]) -> Vec<String> {
    let resolved = settings.settings();
    let mut roles: Vec<RoleName> = vec![RoleName::Default];
    roles.extend(resolved.roles.keys().copied());
    roles.extend(extra_roles.iter().copied());
    let mut keys = Vec::new();
    for meta in settings_catalog() {
        keys.extend(expand(&meta.key, resolved, &roles));
    }
    keys.extend(settings.entries().into_iter().map(|entry| entry.key));
    dedup(keys)
}

fn dedup(keys: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    keys.into_iter()
        .filter(|key| seen.insert(key.clone()))
        .collect()
}

/// A catalog key with each `*` replaced by the map keys the settings hold there.
fn expand(pattern: &str, settings: &Settings, roles: &[RoleName]) -> Vec<String> {
    let mut keys = vec![String::new()];
    for segment in pattern.split('.') {
        keys = keys
            .into_iter()
            .flat_map(|prefix| {
                let names = if segment == "*" {
                    map_keys(settings, &prefix, roles)
                } else {
                    vec![segment.to_owned()]
                };
                names.into_iter().map(move |name| {
                    if prefix.is_empty() {
                        name
                    } else {
                        format!("{prefix}.{name}")
                    }
                })
            })
            .collect();
    }
    keys
}

fn map_keys(settings: &Settings, prefix: &str, roles: &[RoleName]) -> Vec<String> {
    let names: Vec<String> = match prefix {
        "agents" => settings.agents.keys().cloned().collect(),
        "endpoints" => settings.endpoints.keys().cloned().collect(),
        "roles" => roles.iter().map(|role| role.as_str().to_owned()).collect(),
        "ui.keymap" => settings.ui.keymap.keys().cloned().collect(),
        _ => match prefix.split('.').collect::<Vec<_>>().as_slice() {
            ["endpoints", id, "header_env"] => settings
                .endpoints
                .get(*id)
                .map(|endpoint| endpoint.header_env.keys().cloned().collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        },
    };
    // A map key with a dot in it can't be written as one dotted key.
    names
        .into_iter()
        .filter(|name| !name.contains('.'))
        .collect()
}

/// The handoff options unset by default, which the catalog doesn't list: each with what it
/// does and the values it takes.
const HANDOFF_OPTIONS: &[(&str, &str, &[&str])] = &[
    (
        "strategy",
        "Who takes over a stopped agent's cards: one replacement, spread across several, or a race",
        &["first", "spread", "race"],
    ),
    (
        "on_return",
        "What happens when the original agent comes back",
        &["ask", "keep", "review", "hand_back", "redo"],
    ),
    ("checkpoint", "Tag the card's state at every handoff", &[]),
    (
        "merge_rule",
        "Whether a stronger model reviews work a weaker one finished",
        &["upgrade_review", "none"],
    ),
    (
        "unknown_tier",
        "How to treat a model with no tier or track record",
        &["weaker", "equal"],
    ),
    (
        "max_handoffs",
        "Handoffs per card before it escalates to you",
        &[],
    ),
];

/// Adds the handoff options each handoff section in `keys` lacks, after its own keys.
fn with_handoff_options(keys: Vec<String>) -> Vec<String> {
    let sections: Vec<String> = dedup(
        keys.iter()
            .filter_map(|key| {
                let at = key.find("handoff.")?;
                Some(key[..at + "handoff".len()].to_owned())
            })
            .collect(),
    );
    let mut keys = keys;
    for section in sections {
        for (name, ..) in HANDOFF_OPTIONS {
            keys.push(format!("{section}.{name}"));
        }
    }
    dedup(keys)
}

fn handoff_option(key: &str) -> Option<SettingMeta> {
    let (section, name) = key.rsplit_once('.')?;
    if section != "handoff" && !is_role_handoff(&format!("{section}.")) {
        return None;
    }
    let (_, description, words) = HANDOFF_OPTIONS.iter().find(|(known, ..)| *known == name)?;
    let allowed = if name == "checkpoint" {
        vec!["true".to_owned(), "false".to_owned()]
    } else {
        words.iter().map(|word| format!("{word:?}")).collect()
    };
    Some(SettingMeta {
        key: key.to_owned(),
        description: (*description).to_owned(),
        allowed,
        default_scope: SettingScope::Project,
        safety: false,
    })
}

fn blank_meta(key: &str) -> SettingMeta {
    SettingMeta {
        key: key.to_owned(),
        description: String::new(),
        allowed: Vec::new(),
        default_scope: SettingScope::Project,
        safety: !argonauts_registry::safety_keys_in(key, "\"\"").is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_command_saves_where_the_table_says() {
        use SettingScope::*;
        let cases = [
            ("model", "roles.reviewer.workers", ProjectLocal),
            ("model", "default_agent", ProjectLocal),
            ("model", "narrator.model", Global),
            ("model", "approvals.approver.model", Project),
            ("effort", "roles.default.effort", ProjectLocal),
            ("preset", "preset", Project),
            ("preset", "roles.coordinator.workers", Project),
            ("workflow", "workflow", Project),
            ("approvals", "approvals.merge.mode", Project),
            ("budget", "budget.run_usd", Project),
            ("handoff", "roles.owner.handoff.mode", Project),
            ("permissions", "sandbox.network", Project),
            ("permissions", "limits.usd_per_day", Global),
            ("agents", "agents.codex.model", Project),
            ("agents", "agents.codex.auth", Global),
            ("agents", "agents.codex.binary", Global),
            ("agents", "endpoints.ollama.base_url", Global),
            ("narrator", "ui.narrator", Global),
            ("theme", "ui.theme", Global),
            ("verbosity", "ui.verbosity", Global),
            ("config", "roles.owner.effort", ProjectLocal),
            ("config", "ui.keys", Global),
            ("config", "approvals.merge.mode", Project),
        ];
        for (command, key, scope) in cases {
            assert_eq!(default_scope(command, key), scope, "/{command} {key}");
        }
    }

    #[test]
    fn the_handoff_options_take_the_values_their_types_do() {
        use argonauts_core::settings::handoff::{
            HandoffStrategy, MergeRule, OnReturn, UnknownTier,
        };
        fn parses<T: serde::de::DeserializeOwned>(key: &str) {
            let meta = handoff_option(key).unwrap();
            for value in meta.allowed {
                let text = format!("value = {value}");
                let table: toml::Table = text.parse().unwrap();
                let parsed = table["value"].clone().try_into::<T>();
                assert!(parsed.is_ok(), "{key} = {value}");
            }
        }
        parses::<HandoffStrategy>("handoff.strategy");
        parses::<OnReturn>("roles.owner.handoff.on_return");
        parses::<MergeRule>("handoff.merge_rule");
        parses::<UnknownTier>("handoff.unknown_tier");
        parses::<bool>("handoff.checkpoint");
        assert!(handoff_option("budget.strategy").is_none());
        let keys = with_handoff_options(vec!["roles.owner.handoff.mode".to_owned()]);
        assert!(keys.contains(&"roles.owner.handoff.max_handoffs".to_owned()));
    }

    #[test]
    fn only_the_handoff_summarizers_count_as_summarizers() {
        use argonauts_core::settings::handoff::HandoffContext;
        assert!(is_summarizer("handoff.summarizer"));
        assert!(is_summarizer("roles.owner.handoff.summarizer"));
        assert!(!is_summarizer("handoff.context"));
        assert!(!is_summarizer("narrator.summarizer"));
        assert!(!is_summarizer("roles.owner.summarizer"));
        assert!(!is_summarizer("handoff.replace.codex.summarizer"));
        let text = format!("value = {NOTE_AND_SUMMARY}");
        let table: toml::Table = text.parse().unwrap();
        let context: HandoffContext = table["value"].clone().try_into().unwrap();
        assert_eq!(context, HandoffContext::NoteAndSummary);
    }

    #[test]
    fn map_keys_are_filled_in() {
        let mut settings = Settings::default();
        settings
            .endpoints
            .insert("ollama".to_owned(), Default::default());
        let keys = expand("endpoints.*.base_url", &settings, &[]);
        assert_eq!(keys, vec!["endpoints.ollama.base_url".to_owned()]);
        let keys = expand("roles.*.effort", &settings, &[RoleName::PhaseLead]);
        assert_eq!(keys, vec!["roles.phase-lead.effort".to_owned()]);
        assert!(in_section("budget.run_usd", "budget"));
        assert!(!in_section("budgets.x", "budget"));
    }
}
