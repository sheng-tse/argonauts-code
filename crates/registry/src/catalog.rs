//! What `/config` and the settings pickers know about each key: a description, the values a
//! picker offers, where a change is saved by default, and whether it's a safety key.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use argonauts_core::input::EFFORT_LEVELS;
use argonauts_core::settings::agents::EndpointSettings;
use argonauts_core::settings::roles::RoleSettings;
use argonauts_core::settings::{SettingMeta, SettingScope, Settings};
use toml::{Table, Value};

use crate::resolve::{is_safety_key, key_matches, leaves};

/// Keys that are personal choices in a repo, saved just for you by default.
const JUST_YOU: &[&str] = &[
    "default_agent",
    "agents.*.model",
    "agents.*.effort",
    "roles.*.workers",
    "roles.*.effort",
];

/// Keys about you or your machine, saved globally by default. A binary's path stays out of the
/// files your team shares, and the daily caps count across every repo.
const EVERYWHERE: &[&str] = &[
    "agents.*.auth",
    "agents.*.binary",
    "endpoints.*",
    "ui.*",
    "narrator.*",
    "notifications.*",
    "limits.usd_per_day",
    "limits.auto_merges_per_day",
];

/// The checkpoints `/approvals` covers, as they appear under `approvals.`.
const CHECKPOINTS: &[(&str, &str)] = &[
    ("goal", "The goal checkpoint"),
    ("plan", "The plan checkpoint"),
    ("merge", "The merge checkpoint"),
    ("escalation", "A stuck card"),
    ("budget", "More budget from the run's reserve"),
    ("change_request", "A change request sent up to you"),
    ("recipe_promotion", "Promoting a recipe change"),
];

const BOOL: &[&str] = &["true", "false"];

/// The keys the settings commands name come first, in picker order, then every other key. Map
/// keys you choose, such as harness ids and role names, are `*`.
pub fn settings_catalog() -> Vec<SettingMeta> {
    catalog().to_vec()
}

/// The catalog entry for a real key such as `agents.codex.model`, matching `*` in the catalog.
pub fn setting_meta(key: &str) -> Option<SettingMeta> {
    let catalog = catalog();
    catalog
        .iter()
        .find(|meta| meta.key == key)
        .or_else(|| catalog.iter().find(|meta| key_matches(&meta.key, key)))
        .cloned()
}

fn default_scope(key: &str) -> SettingScope {
    let any = |patterns: &[&str]| patterns.iter().any(|pattern| key_matches(pattern, key));
    if any(JUST_YOU) {
        SettingScope::ProjectLocal
    } else if any(EVERYWHERE) {
        SettingScope::Global
    } else {
        SettingScope::Project
    }
}

fn catalog() -> &'static [SettingMeta] {
    static CATALOG: OnceLock<Vec<SettingMeta>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut entries = named();
        let mut seen: BTreeSet<String> = entries.iter().map(|meta| meta.key.clone()).collect();
        for (key, value) in remaining() {
            if seen.insert(key.clone()) {
                let allowed = if value.is_bool() { BOOL } else { &[] };
                entries.push(meta(&key, "", allowed));
            }
        }
        entries
    })
}

fn meta(key: &str, description: &str, allowed: &[&str]) -> SettingMeta {
    SettingMeta {
        key: key.to_owned(),
        description: description.to_owned(),
        allowed: allowed.iter().map(|value| (*value).to_owned()).collect(),
        default_scope: default_scope(key),
        safety: is_safety_key(key),
    }
}

/// A key whose picker offers these words, quoted as TOML strings.
fn words(key: &str, description: &str, choices: &[&str]) -> SettingMeta {
    let quoted: Vec<String> = choices.iter().map(|word| format!("{word:?}")).collect();
    let quoted: Vec<&str> = quoted.iter().map(String::as_str).collect();
    meta(key, description, &quoted)
}

fn named() -> Vec<SettingMeta> {
    let presets: Vec<String> = crate::presets().into_iter().map(|p| p.name).collect();
    let presets: Vec<&str> = presets.iter().map(String::as_str).collect();
    let mut entries = vec![
        // /agent and /model
        meta(
            "default_agent",
            "Who you chat with, and who works solo: a harness, or harness/model",
            &[],
        ),
        meta(
            "roles.*.workers",
            "Who fills this role, in order. Empty lets the router pick from the crew",
            &[],
        ),
        meta(
            "agents.*.model",
            "The model this harness runs unless a role names one. Unset uses the CLI's default",
            &[],
        ),
        // /effort
        words(
            "agents.*.effort",
            "Reasoning effort for this harness. Unset uses the CLI's default",
            EFFORT_LEVELS,
        ),
        words(
            "roles.*.effort",
            "Reasoning effort for this role",
            EFFORT_LEVELS,
        ),
        // /preset. The picker adds your own presets from `presets_in`.
        words(
            "preset",
            "How the crew works: peer-crew plans and judges as peers, coordinated-crew has one \
             coordinator",
            &presets,
        ),
        // /agents
        meta(
            "crew",
            "The harnesses and endpoints that work on crew runs",
            &[],
        ),
        meta(
            "agents.*.enabled",
            "Whether this harness may work at all",
            BOOL,
        ),
        words(
            "agents.*.auth",
            "Your own login (subscription), or an API key from the environment (api-key)",
            &["subscription", "api-key"],
        ),
        meta(
            "agents.*.binary",
            "The harness's program, as an absolute path outside the repo, when it isn't on PATH",
            &[],
        ),
        meta(
            "agents.*.version_pin",
            "The version you pinned. Doctor warns when the installed one differs",
            &[],
        ),
        meta(
            "endpoints.*.base_url",
            "The endpoint's OpenAI-compatible address, such as http://localhost:11434/v1",
            &[],
        ),
        meta(
            "endpoints.*.api_key_env",
            "The environment variable that holds its key, never the key itself",
            &[],
        ),
        words(
            "endpoints.*.api",
            "Which API it speaks",
            &["chat_completions", "responses"],
        ),
        words(
            "endpoints.*.structured_output",
            "How it's asked for checked output: a strict schema, a JSON object, or the prompt",
            &["json_schema", "json_object", "prompt"],
        ),
        meta(
            "endpoints.*.local",
            "A model server on your machine, so nothing leaves it",
            BOOL,
        ),
        meta(
            "endpoints.*.allow_api_billing",
            "Whether work may move onto this paid API without asking you",
            BOOL,
        ),
        // /approvals
        words(
            "approvals.goal_changes",
            "When an agent asks to change the approved goal: ask you, allow clarifications, or \
             deny",
            &["ask", "allow_clarifications", "deny"],
        ),
    ];
    for (checkpoint, label) in CHECKPOINTS {
        entries.push(words(
            &format!("approvals.{checkpoint}.mode"),
            &format!(
                "{label}: supervised waits for you, shadow also records what auto would do, \
                 auto lets your settings decide"
            ),
            &["supervised", "shadow", "auto"],
        ));
        entries.push(words(
            &format!("approvals.{checkpoint}.decide_with"),
            &format!("{label}: who decides in auto mode, your rules, your approver model, or both"),
            &["rules", "model", "rules_and_model", "rules_or_model"],
        ));
        let otherwise: &[&str] = if *checkpoint == "merge" {
            &["ask", "send_back", "stop", "open_pull_request"]
        } else {
            &["ask", "send_back", "stop"]
        };
        entries.push(words(
            &format!("approvals.{checkpoint}.otherwise"),
            &format!("{label}: what happens when auto doesn't approve"),
            otherwise,
        ));
    }
    entries.extend([
        meta(
            "approvals.plan.rules.max_score_spread",
            "Plan rules: the largest gap between one judge's two scores, out of 10",
            &[],
        ),
        meta(
            "approvals.plan.rules.min_winner_margin",
            "Plan rules: how far the winning plan must lead the runner-up",
            &[],
        ),
        meta(
            "approvals.merge.rules.min_changed_line_coverage",
            "Merge rules: the share of changed lines the tests must cover, from 0 to 1",
            &[],
        ),
        meta(
            "approvals.merge.rules.max_combined_diff_lines",
            "Merge rules: the most lines the run's combined diff may have",
            &[],
        ),
        meta(
            "approvals.escalation.rules.max_extra_attempts",
            "Stuck-card rules: extra attempts auto may grant, by an agent from a family that \
             hasn't tried",
            &[],
        ),
        meta(
            "approvals.budget.rules.use_reserve",
            "Budget rules: whether auto may release the run's reserve, never above a ceiling",
            BOOL,
        ),
        meta(
            "approvals.approver.model",
            "The approver model, any model in the registry. Unset means your rules alone decide",
            &[],
        ),
        meta(
            "approvals.approver.model_rules",
            "Your rules for the approver model, a file in .argonauts/",
            &[],
        ),
        meta(
            "approvals.approver.different_family",
            "The approver must come from a model family no author or owner used",
            BOOL,
        ),
        meta(
            "approvals.approver.second_model",
            "A second approver, from another vendor, that must also approve",
            &[],
        ),
        meta(
            "approvals.spot_check_sample",
            "Share of auto decisions queued for your spot check, from 0 to 1",
            &[],
        ),
        // /budget
        meta(
            "budget.enabled",
            "Whether budgets stop work. Off, the meter still counts",
            BOOL,
        ),
        meta(
            "budget.run_usd",
            "The most a run may spend, in dollars",
            &[],
        ),
        meta("budget.run_tokens", "The most tokens a run may use", &[]),
        meta(
            "budget.run_quota",
            "The most of a subscription window a run may use, from 0 to 1",
            &[],
        ),
        meta(
            "budget.run_minutes",
            "The longest a run may take, in minutes",
            &[],
        ),
        meta(
            "budget.per_card_usd",
            "Each card's budget in dollars, until similar past cards give a median",
            &[],
        ),
        meta(
            "budget.per_card_tokens",
            "Each card's budget in tokens",
            &[],
        ),
        meta(
            "budget.per_card_quota",
            "Each card's budget as a share of a subscription window",
            &[],
        ),
        meta(
            "budget.per_card_minutes",
            "Each card's budget in minutes",
            &[],
        ),
        meta(
            "budget.soft_stop",
            "Share of a budget at which the agent is asked to wrap up",
            &[],
        ),
        words(
            "budget.on_hit",
            "When a card hits its budget: ask you, extend it from the reserve, hand it off, or \
             escalate",
            &["ask", "extend", "handoff", "escalate"],
        ),
        // /handoff
        words(
            "roles.*.handoff.mode",
            "When this role's agent stops, such as at a usage limit: ask you, hand off to the \
             next fallback, or wait for the vendor",
            &["ask", "auto", "wait"],
        ),
        meta(
            "roles.*.handoff.fallback",
            "Who takes over, in order: an agent, or a table with its own rules",
            &[],
        ),
        meta(
            "roles.*.handoff.ask_timeout",
            "How long the handoff card waits before handing off by itself, or \"none\"",
            &[],
        ),
        meta(
            "roles.*.handoff.allow_api_billing",
            "Whether a handoff may move work onto a paid API without asking you",
            BOOL,
        ),
        meta(
            "roles.*.handoff.return_to_original",
            "Move the card back when the original vendor returns",
            BOOL,
        ),
        words(
            "roles.*.handoff.broken_work",
            "What the new owner does with broken work in progress: continue from it, or restart",
            &["continue", "restart"],
        ),
        // /permissions
        words(
            "sandbox.mode",
            "Where agents run: the OS sandbox, a container, or off (solo runs only, and every \
             checkpoint then asks you)",
            &["os", "container", "off"],
        ),
        meta(
            "sandbox.network",
            "Whether commands agents run can reach the network",
            BOOL,
        ),
        meta(
            "limits.protected_paths",
            "Paths no card may touch, as glob patterns, on top of .argonauts/**",
            &[],
        ),
        meta(
            "limits.dependency_allowlist",
            "Packages a plan may add without asking you",
            &[],
        ),
        meta(
            "limits.usd_per_day",
            "The most you spend a day across every run and repo, in dollars",
            &[],
        ),
        // /narrator
        words(
            "ui.narrator",
            "The narrator: off, on demand for /summary and /ask, or live updates",
            &["off", "on_demand", "live"],
        ),
        meta(
            "narrator.model",
            "The model that narrates, any in the registry. Unset uses the default agent, \
             read-only",
            &[],
        ),
        meta(
            "narrator.interval",
            "At most one live update this often, such as \"45s\"",
            &[],
        ),
        meta(
            "narrator.spend_cap",
            "The most the narrator spends per run, such as \"0.50 usd\" or \"200k tokens\"",
            &[],
        ),
        // /theme
        words(
            "ui.theme",
            "The palette. Auto follows your terminal's background",
            &["auto", "dark", "light"],
        ),
        meta(
            "ui.accent",
            "The working line's accent color, as \"#rrggbb\". Unset is the warm gold",
            &[],
        ),
        words(
            "ui.color",
            "Color. Auto follows NO_COLOR and TERM=dumb",
            &["auto", "always", "never"],
        ),
        words(
            "ui.motion",
            "Reduced keeps spinners still and streams text a line at a time",
            &["normal", "reduced"],
        ),
        // /vim, /verbose, /quiet
        words(
            "ui.keys",
            "Key bindings: the usual keys, or vim keys for lists and the prompt",
            &["default", "vim"],
        ),
        words(
            "ui.verbosity",
            "How much the transcript shows",
            &["full", "normal", "quiet"],
        ),
        meta(
            "ui.keymap.*",
            "Binds an action to another key, such as crew_view = \"ctrl+left\"",
            &[],
        ),
    ]);
    entries
}

/// Every leaf key of the defaults, plus a role's and an endpoint's, with map keys as `*`.
fn remaining() -> Vec<(String, Value)> {
    let sections = [
        ("", Table::try_from(Settings::default())),
        ("roles.*.", Table::try_from(RoleSettings::default())),
        ("endpoints.*.", Table::try_from(EndpointSettings::default())),
    ];
    sections
        .into_iter()
        .filter_map(|(prefix, table)| table.ok().map(|table| (prefix, table)))
        .flat_map(|(prefix, table)| {
            leaves(&table)
                .into_iter()
                .map(move |(key, value)| (normalize(&format!("{prefix}{key}")), value))
        })
        .collect()
}

/// `agents.codex.model` becomes `agents.*.model`.
fn normalize(key: &str) -> String {
    let mut segments: Vec<&str> = key.split('.').collect();
    match segments.as_slice() {
        ["agents" | "endpoints" | "roles", _, _, ..] => segments[1] = "*",
        ["ui", "keymap", _, ..] => segments[2] = "*",
        _ => {}
    }
    if let ["endpoints", _, "header_env", _, ..] = segments.as_slice() {
        segments[3] = "*";
    }
    segments.join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real key for a catalog key, with each `*` filled in.
    fn concrete(key: &str) -> String {
        let mut segments: Vec<&str> = key.split('.').collect();
        for (at, segment) in segments.clone().iter().enumerate() {
            if *segment == "*" {
                segments[at] = match segments[0] {
                    "agents" => "codex",
                    "endpoints" if at == 1 => "ollama",
                    "roles" => "reviewer",
                    _ => "crew_view",
                };
            }
        }
        segments.join(".")
    }

    /// Settings with `key` set to the TOML text `value`, or why they don't deserialize.
    fn settings_with(key: &str, value: &str) -> Result<Table, String> {
        let value: Value = format!("value = {value}")
            .parse::<Table>()
            .map_err(|err| err.to_string())?
            .remove("value")
            .ok_or("no value")?;
        let mut table = Table::try_from(Settings::default()).map_err(|err| err.to_string())?;
        let segments: Vec<&str> = key.split('.').collect();
        let mut current = &mut table;
        for segment in &segments[..segments.len() - 1] {
            current = current
                .entry((*segment).to_owned())
                .or_insert_with(|| Value::Table(Table::new()))
                .as_table_mut()
                .ok_or("not a table")?;
        }
        current.insert(segments[segments.len() - 1].to_owned(), value);
        let settings: Settings = Value::Table(table)
            .try_into()
            .map_err(|err: toml::de::Error| err.to_string())?;
        Table::try_from(settings).map_err(|err| err.to_string())
    }

    /// A value to try for keys whose pickers offer no list.
    fn sample(key: &str) -> &'static str {
        match key {
            "default_agent" | "approvals.approver.model" | "approvals.approver.second_model" => {
                "\"codex/gpt-x\""
            }
            "roles.*.workers" | "crew" | "roles.*.handoff.fallback" => "[\"codex\"]",
            "agents.*.model" => "\"opus\"",
            "agents.*.binary" => "\"/opt/codex/bin/codex\"",
            "agents.*.version_pin" => "\"0.158.0\"",
            "endpoints.*.base_url" => "\"http://localhost:11434/v1\"",
            "endpoints.*.api_key_env" => "\"OLLAMA_API_KEY\"",
            "approvals.approver.model_rules" => "\"approval-rules.md\"",
            "budget.run_tokens" | "budget.per_card_tokens" => "200000",
            "approvals.merge.rules.max_combined_diff_lines"
            | "approvals.escalation.rules.max_extra_attempts" => "2",
            "limits.protected_paths" | "limits.dependency_allowlist" => "[\"docs/**\"]",
            "narrator.model" => "\"ollama/llama3.2\"",
            "narrator.interval" | "roles.*.handoff.ask_timeout" => "\"30s\"",
            "narrator.spend_cap" => "\"0.50 usd\"",
            "ui.accent" => "\"#d4a017\"",
            "ui.keymap.*" => "\"ctrl+left\"",
            _ => "0.5",
        }
    }

    #[test]
    fn every_named_key_is_real_and_its_values_are_accepted() {
        for meta in named() {
            let key = concrete(&meta.key);
            let values: Vec<&str> = if meta.allowed.is_empty() {
                vec![sample(&meta.key)]
            } else {
                meta.allowed.iter().map(String::as_str).collect()
            };
            for value in values {
                let table = settings_with(&key, value)
                    .unwrap_or_else(|err| panic!("{key} = {value}: {err}"));
                assert!(
                    leaves(&table).iter().any(|(leaf, _)| leaf == &key),
                    "{key} isn't a settings key"
                );
            }
            assert!(!meta.description.is_empty(), "{}", meta.key);
            assert!(!meta.description.ends_with('.'), "{}", meta.key);
        }
    }

    #[test]
    fn each_key_is_listed_once_and_every_default_key_is_there() {
        let catalog = settings_catalog();
        let keys: BTreeSet<&str> = catalog.iter().map(|meta| meta.key.as_str()).collect();
        assert_eq!(keys.len(), catalog.len(), "a key is listed twice");
        for (key, _) in remaining() {
            assert!(keys.contains(key.as_str()), "{key}");
        }
        for key in [
            "agents.*.binary",
            "runs.remind_parked_days",
            "roles.*.on_demand",
        ] {
            assert!(keys.contains(key), "{key}");
        }
        assert!(!keys.iter().any(|key| key.contains("claude-code")));
    }

    #[test]
    fn default_scopes_are_the_ones_each_setting_is_given() {
        let scope = |key: &str| setting_meta(key).map(|meta| meta.default_scope);
        for key in [
            "default_agent",
            "agents.codex.model",
            "agents.codex.effort",
            "roles.reviewer.workers",
            "roles.owner.effort",
        ] {
            assert_eq!(scope(key), Some(SettingScope::ProjectLocal), "{key}");
        }
        for key in [
            "preset",
            "crew",
            "approvals.merge.mode",
            "budget.run_usd",
            "roles.owner.handoff.mode",
            "sandbox.network",
            "limits.protected_paths",
            "agents.codex.enabled",
        ] {
            assert_eq!(scope(key), Some(SettingScope::Project), "{key}");
        }
        for key in [
            "agents.codex.auth",
            "agents.codex.binary",
            "endpoints.ollama.base_url",
            "ui.theme",
            "ui.keys",
            "ui.keymap.crew_view",
            "narrator.model",
            "notifications.os",
            "limits.usd_per_day",
            "limits.auto_merges_per_day",
        ] {
            assert_eq!(scope(key), Some(SettingScope::Global), "{key}");
        }
    }

    #[test]
    fn real_keys_find_their_entry() {
        let meta = setting_meta("roles.reviewer.handoff.allow_api_billing").unwrap();
        assert_eq!(meta.key, "roles.*.handoff.allow_api_billing");
        assert!(meta.safety);
        let meta = setting_meta("approvals.merge.mode").unwrap();
        assert!(meta.safety);
        assert_eq!(meta.allowed, ["\"supervised\"", "\"shadow\"", "\"auto\""]);
        assert!(!setting_meta("ui.keys").unwrap().safety);
        assert!(
            setting_meta("preset")
                .unwrap()
                .allowed
                .contains(&"\"coordinated-crew\"".to_owned())
        );
        assert_eq!(setting_meta("nothing.here"), None);
    }
}
