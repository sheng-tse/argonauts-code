//! The top-level `[fallback]` and each `[fallback.<role>]`.

use std::iter;

use argonauts_core::settings::Settings;
use argonauts_core::settings::handoff::{CrewRole, FallbackEntry, HandoffSettings, Pick};
use toml::{Table, Value};

use super::roles::{self, Workers};
use super::validate::Problem;

/// The keys with no per-vendor form. Each role table takes them from the top level.
const COPIED: [&str; 5] = [
    "mode",
    "ask_timeout",
    "allow_api_billing",
    "broken_work",
    "wrap_up",
];

/// Copies `mode`, `ask_timeout`, `allow_api_billing`, `broken_work`, and `wrap_up` from the
/// top-level `[fallback]` into each `[fallback.<role>]` you wrote that doesn't set them. It
/// works on the merged TOML, where an unset key can still be told apart from a default, and
/// copies nothing else: the other keys stay unset for `HandoffSettings::applied` to walk.
/// Returns each copy as `(from, to)` dotted keys, so the copy keeps the layer of the value it
/// came from.
pub(crate) fn merge_top_level(merged: &mut Table) -> Vec<(String, String)> {
    let Some(Value::Table(top)) = merged.get_mut("fallback") else {
        return Vec::new();
    };
    let defaults: Vec<(&str, Value)> = COPIED
        .iter()
        .filter_map(|key| Some((*key, top.get(*key)?.clone())))
        .collect();
    let mut copied = Vec::new();
    for (role, table) in role_tables(top) {
        for (key, value) in &defaults {
            if !table.contains_key(*key) {
                table.insert((*key).to_owned(), value.clone());
                copied.push((format!("fallback.{key}"), format!("fallback.{role}.{key}")));
            }
        }
    }
    copied
}

/// The `[fallback.<role>]` tables inside a `[fallback]` table, by the role's name. Anything
/// but a table there is a type error that deserializing reports.
fn role_tables(fallback: &mut Table) -> impl Iterator<Item = (&str, &mut Table)> {
    fallback.iter_mut().filter_map(|(name, value)| {
        let role = CrewRole::named(name)?;
        match value {
            Value::Table(table) => Some((role.as_str(), table)),
            _ => None,
        }
    })
}

/// Removes `allow_api_billing` from every fallback and replacement entry in a layer that isn't
/// yours to trust (a preset, or a project file you haven't trusted), and returns the dotted
/// keys whose `true` it removed, for the warning.
pub(crate) fn drop_entry_billing(layer: &mut Table) -> Vec<String> {
    let mut dropped = Vec::new();
    if let Some(Value::Table(fallback)) = layer.get_mut("fallback") {
        drop_in_handoff(fallback, "fallback", &mut dropped);
        for (role, table) in role_tables(fallback) {
            drop_in_handoff(table, &format!("fallback.{role}"), &mut dropped);
        }
    }
    dropped
}

/// The entries in `layer` that let a fallback bill an API, as dotted keys.
pub(crate) fn entry_billing(layer: &Table) -> Vec<String> {
    drop_entry_billing(&mut layer.clone())
}

fn drop_in_handoff(handoff: &mut Table, prefix: &str, dropped: &mut Vec<String>) {
    drop_in_list(
        handoff.get_mut("order"),
        &format!("{prefix}.order"),
        dropped,
    );
    if let Some(Value::Table(replace)) = handoff.get_mut("replace") {
        for (vendor, replacement) in replace.iter_mut() {
            if let Value::Table(replacement) = replacement {
                let key = format!("{prefix}.replace.{vendor}.with");
                drop_in_list(replacement.get_mut("with"), &key, dropped);
            }
        }
    }
}

fn drop_in_list(list: Option<&mut Value>, key: &str, dropped: &mut Vec<String>) {
    let Some(Value::Array(entries)) = list else {
        return;
    };
    for (at, entry) in entries.iter_mut().enumerate() {
        if let Value::Table(rules) = entry
            && rules.remove("allow_api_billing") == Some(Value::Boolean(true))
        {
            dropped.push(format!("{key}[{at}].allow_api_billing"));
        }
    }
}

/// The top-level `[fallback]` and each role's, with the dotted key each sits at.
fn tables(settings: &Settings) -> impl Iterator<Item = (String, &HandoffSettings)> {
    let roles = settings
        .fallback
        .roles
        .iter()
        .map(|(role, table)| (format!("fallback.{}", role.as_str()), table));
    iter::once(("fallback".to_owned(), &settings.fallback.every_role)).chain(roles)
}

/// Problems with the handoff settings, each naming its key: replacement lists, strategies, and
/// counts.
pub(crate) fn check(settings: &Settings) -> Vec<Problem> {
    let mut problems = Vec::new();
    for (prefix, table) in tables(settings) {
        check_table(&prefix, table, &mut problems);
    }
    problems
}

/// Each `replace.<vendor>` whose vendor isn't a harness or endpoint `workers` knows,
/// `[[harness]]` ids included, and each `summarizer` that's unknown or off. Like an unknown
/// role worker, these warn and never refuse.
pub(crate) fn warnings(settings: &Settings, workers: &Workers<'_>) -> Vec<String> {
    let mut warnings = endpoints_for_workers(settings, workers);
    warnings.extend(one_at_a_time(settings));
    for (prefix, table) in tables(settings) {
        for vendor in table.replace.keys() {
            if !workers.is_known(&vendor.0) {
                warnings.push(format!(
                    "{prefix}.replace.{vendor} names {vendor}, which isn't a CLI or endpoint \
                     Argonauts knows, and no models.toml declares it as a [[harness]]"
                ));
            }
        }
        if let Some(worker) = &table.summarizer {
            let key = format!("{prefix}.summarizer");
            if workers.is_off(&worker.worker) {
                warnings.push(roles::off(&key, worker));
            } else if !workers.is_known(&worker.worker) {
                warnings.push(roles::unknown(&key, worker));
            }
        }
    }
    warnings
}

/// The main role, the orchestrator and the judge each go to one fallback at a time:
/// two can't hold one conversation or settle one question. `together` of 2 or more in their
/// tables changes nothing, and a warning says so.
fn one_at_a_time(settings: &Settings) -> Vec<String> {
    let seats = [
        (CrewRole::Main, "the main role goes"),
        (CrewRole::Orchestrator, "the orchestrator role goes"),
        (CrewRole::Judge, "the judge role goes"),
    ];
    seats
        .into_iter()
        .filter_map(|(role, seat)| {
            let table = settings.fallback.roles.get(&role)?;
            let together = table.together.filter(|together| *together >= 2)?;
            Some(format!(
                "fallback.{}.together is {together}, and {seat} to one fallback at a time, so \
                 it's read as 1; several at once are for what the workers build and plan, \
                 and for reviews",
                role.as_str()
            ))
        })
        .collect()
}

/// Every role that follows the workers builds with tools, and an endpoint has none, so one in
/// `[fallback.workers]` is never picked. The other tables serve roles an endpoint can fill.
fn endpoints_for_workers(settings: &Settings, workers: &Workers<'_>) -> Vec<String> {
    let Some(table) = settings.fallback.roles.get(&CrewRole::Workers) else {
        return Vec::new();
    };
    let prefix = format!("fallback.{}", CrewRole::Workers.as_str());
    let replaced = table
        .replace
        .iter()
        .map(|(agent, replacement)| (format!("{prefix}.replace.{agent}.with"), &replacement.with));
    iter::once((format!("{prefix}.order"), &table.order))
        .chain(replaced)
        .filter_map(|(key, list)| {
            let endpoint = list
                .iter()
                .map(FallbackEntry::worker)
                .find(|worker| workers.is_endpoint(&worker.worker))?;
            Some(format!(
                "{key} names {endpoint}, an endpoint, and the workers need a CLI with tools, \
                 so it's never picked; endpoints can review, critique, judge, and narrate"
            ))
        })
        .collect()
}

fn check_table(prefix: &str, table: &HandoffSettings, problems: &mut Vec<Problem>) {
    if table.together == Some(0) {
        let key = format!("{prefix}.together");
        problems.push(Problem::new(
            &key,
            format!("{key} must be at least 1: one fallback at a time, or 2 or more at once"),
        ));
    }

    let workers = format!("fallback.{}", CrewRole::Workers.as_str());
    if prefix == workers && table.pick == Some(Pick::Merge) {
        let key = format!("{prefix}.pick");
        problems.push(Problem::new(
            &key,
            format!(
                "{key} is \"merge\", which keeps every finding, and what the workers build has \
                 none; use \"first_passing\" or \"judge\""
            ),
        ));
    }
    let reviewers = format!("fallback.{}", CrewRole::Reviewers.as_str());
    if prefix == reviewers && table.pick == Some(Pick::FirstPassing) {
        let key = format!("{prefix}.pick");
        problems.push(Problem::new(
            &key,
            format!(
                "{key} is \"first_passing\", which keeps the first to pass your checks, and a \
                 review passes none; use \"merge\" or \"judge\""
            ),
        ));
    }
    if table.max_fallbacks == Some(0) {
        let key = format!("{prefix}.max_fallbacks");
        problems.push(Problem::new(
            &key,
            format!("{key} must be at least 1; mode = \"wait\" is how a task waits instead"),
        ));
    }
    if let Err(message) = table.ask_timeout.parse() {
        let key = format!("{prefix}.ask_timeout");
        problems.push(Problem::new(&key, format!("{key}: {message}")));
    }
    check_entries(&format!("{prefix}.order"), &table.order, problems);
    for (vendor, replacement) in &table.replace {
        let key = format!("{prefix}.replace.{vendor}.with");
        check_entries(&key, &replacement.with, problems);
    }
}

fn check_entries(key: &str, entries: &[FallbackEntry], problems: &mut Vec<Problem>) {
    for (at, entry) in entries.iter().enumerate() {
        if let FallbackEntry::WithRules(rules) = entry
            && let Some(cap) = rules.max_budget_usd
            && !(cap.is_finite() && cap > 0.0)
        {
            problems.push(Problem::new(
                key,
                format!("{key}[{at}].max_spend_usd must be more than 0 (it's {cap})"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Table {
        text.parse().unwrap()
    }

    #[test]
    fn top_level_keys_fill_only_what_a_role_leaves_unset() {
        let mut merged = table(
            "[fallback]\nmode = \"auto\"\nask_timeout = \"30s\"\nstrategy = \"spread\"\n\
             [fallback.workers]\nmode = \"wait\"\n[fallback.reviewers]\ntogether = 2\n\
             [fallback.replace.codex]\nwith = [\"claude-code\"]\n",
        );
        let copied = merge_top_level(&mut merged);
        let fallback = merged["fallback"].as_table().unwrap();
        let workers = fallback["workers"].as_table().unwrap();
        assert_eq!(workers["mode"].as_str(), Some("wait"));
        assert_eq!(workers["ask_timeout"].as_str(), Some("30s"));
        assert!(
            !workers.contains_key("strategy"),
            "only the keys with no agent form"
        );
        let reviewers = fallback["reviewers"].as_table().unwrap();
        assert_eq!(reviewers["mode"].as_str(), Some("auto"));
        assert!(copied.contains(&(
            "fallback.ask_timeout".to_owned(),
            "fallback.reviewers.ask_timeout".to_owned()
        )));
        assert!(!copied.iter().any(|(_, to)| to == "fallback.workers.mode"));
        // A role with no table of its own goes by the top-level one, and `replace` is no role.
        assert!(!fallback.contains_key("judge"));
        assert!(!fallback["replace"].as_table().unwrap().contains_key("mode"));
    }

    #[test]
    fn entry_billing_is_dropped_everywhere_it_can_be() {
        let mut layer = table(
            "[fallback]\norder = [\"codex\", { worker = \"x/y\", allow_api_billing = true }]\n\
             [fallback.replace.codex]\nwith = [{ worker = \"x/y\", allow_api_billing = true }]\n\
             [fallback.workers]\norder = [{ worker = \"x/y\", allow_api_billing = false }]\n\
             [fallback.workers.replace.codex]\n\
             with = [{ worker = \"x/y\", allow_api_billing = true }]\n",
        );
        assert_eq!(entry_billing(&layer).len(), 3);
        let dropped = drop_entry_billing(&mut layer);
        assert_eq!(
            dropped,
            [
                "fallback.order[1].allow_api_billing",
                "fallback.replace.codex.with[0].allow_api_billing",
                "fallback.workers.replace.codex.with[0].allow_api_billing",
            ]
        );
        assert!(entry_billing(&layer).is_empty());
        let text = toml::to_string(&layer).unwrap();
        assert!(!text.contains("allow_api_billing"), "{text}");
    }
}
