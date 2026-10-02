//! The top-level `[handoff]` and each `[roles.<role>.handoff]`.

use std::iter;

use argonauts_core::settings::Settings;
use argonauts_core::settings::handoff::{FallbackEntry, HandoffSettings};
use toml::{Table, Value};

use super::roles::{self, Workers};
use super::validate::Problem;

/// The keys with no per-vendor form. Each role table takes them from the top level.
const COPIED: [&str; 4] = ["mode", "ask_timeout", "allow_api_billing", "broken_work"];

/// Copies `mode`, `ask_timeout`, `allow_api_billing`, and `broken_work` from the top-level
/// `[handoff]` into each role's handoff table that doesn't set them. It works on the merged
/// TOML, where an unset key can still be told apart from a default, and copies nothing else:
/// the other keys stay unset for `HandoffSettings::applied` to walk. Returns each copy as
/// `(from, to)` dotted keys, so the copy keeps the layer of the value it came from.
pub(crate) fn merge_top_level(merged: &mut Table) -> Vec<(String, String)> {
    let Some(Value::Table(top)) = merged.get("handoff") else {
        return Vec::new();
    };
    let defaults: Vec<(&str, Value)> = COPIED
        .iter()
        .filter_map(|key| Some((*key, top.get(*key)?.clone())))
        .collect();
    let Some(Value::Table(roles)) = merged.get_mut("roles") else {
        return Vec::new();
    };
    let mut copied = Vec::new();
    for (role, table) in roles.iter_mut() {
        // Anything but a table is a type error that deserializing reports.
        let Value::Table(table) = table else { continue };
        let handoff = table
            .entry("handoff")
            .or_insert_with(|| Value::Table(Table::new()));
        let Value::Table(handoff) = handoff else {
            continue;
        };
        for (key, value) in &defaults {
            if !handoff.contains_key(*key) {
                handoff.insert((*key).to_owned(), value.clone());
                copied.push((
                    format!("handoff.{key}"),
                    format!("roles.{role}.handoff.{key}"),
                ));
            }
        }
    }
    copied
}

/// Removes `allow_api_billing` from every fallback and replacement entry in a layer that isn't
/// yours to trust (a preset, or a project file you haven't trusted), and returns the dotted
/// keys whose `true` it removed, for the warning.
pub(crate) fn drop_entry_billing(layer: &mut Table) -> Vec<String> {
    let mut dropped = Vec::new();
    if let Some(Value::Table(handoff)) = layer.get_mut("handoff") {
        drop_in_handoff(handoff, "handoff", &mut dropped);
    }
    if let Some(Value::Table(roles)) = layer.get_mut("roles") {
        for (role, table) in roles.iter_mut() {
            if let Value::Table(table) = table
                && let Some(Value::Table(handoff)) = table.get_mut("handoff")
            {
                drop_in_handoff(handoff, &format!("roles.{role}.handoff"), &mut dropped);
            }
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
        handoff.get_mut("fallback"),
        &format!("{prefix}.fallback"),
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

/// The top-level `[handoff]` and each role's, with the dotted key each sits at.
fn tables(settings: &Settings) -> impl Iterator<Item = (String, &HandoffSettings)> {
    let roles = settings
        .roles
        .iter()
        .map(|(role, table)| (format!("roles.{}.handoff", role.as_str()), &table.handoff));
    iter::once(("handoff".to_owned(), &settings.handoff)).chain(roles)
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
    let mut warnings = Vec::new();
    for (prefix, table) in tables(settings) {
        for vendor in table.replace.keys() {
            if !workers.is_known(&vendor.0) {
                warnings.push(format!(
                    "{prefix}.replace.{vendor} names {vendor}, which isn't a harness or endpoint \
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

fn check_table(prefix: &str, table: &HandoffSettings, problems: &mut Vec<Problem>) {
    if let Some(count) = table.race_count
        && count < 2
    {
        let key = format!("{prefix}.race_count");
        problems.push(Problem::new(
            &key,
            format!("{key} is {count}, and a race needs at least 2 attempts at once"),
        ));
    }
    if table.max_handoffs == Some(0) {
        let key = format!("{prefix}.max_handoffs");
        problems.push(Problem::new(
            &key,
            format!("{key} must be at least 1; mode = \"wait\" is how a card waits instead"),
        ));
    }
    if let Err(message) = table.ask_timeout.parse() {
        let key = format!("{prefix}.ask_timeout");
        problems.push(Problem::new(&key, format!("{key}: {message}")));
    }
    check_entries(&format!("{prefix}.fallback"), &table.fallback, problems);
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
                format!("{key}[{at}].max_budget_usd must be more than 0 (it's {cap})"),
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
            "[handoff]\nmode = \"auto\"\nask_timeout = \"30s\"\nstrategy = \"race\"\n\
             [roles.owner.handoff]\nmode = \"wait\"\n[roles.reviewer]\ncount = 2\n",
        );
        let copied = merge_top_level(&mut merged);
        let roles = merged["roles"].as_table().unwrap();
        let owner = roles["owner"]["handoff"].as_table().unwrap();
        assert_eq!(owner["mode"].as_str(), Some("wait"));
        assert_eq!(owner["ask_timeout"].as_str(), Some("30s"));
        assert!(
            !owner.contains_key("strategy"),
            "only the keys with no vendor form"
        );
        let reviewer = roles["reviewer"]["handoff"].as_table().unwrap();
        assert_eq!(reviewer["mode"].as_str(), Some("auto"));
        assert!(copied.contains(&(
            "handoff.ask_timeout".to_owned(),
            "roles.reviewer.handoff.ask_timeout".to_owned()
        )));
        assert!(
            !copied
                .iter()
                .any(|(_, to)| to == "roles.owner.handoff.mode")
        );
    }

    #[test]
    fn entry_billing_is_dropped_everywhere_it_can_be() {
        let mut layer = table(
            "[handoff]\nfallback = [\"codex\", { worker = \"x/y\", allow_api_billing = true }]\n\
             [handoff.replace.codex]\nwith = [{ worker = \"x/y\", allow_api_billing = true }]\n\
             [roles.owner.handoff]\nfallback = [{ worker = \"x/y\", allow_api_billing = false }]\n\
             [roles.owner.handoff.replace.codex]\n\
             with = [{ worker = \"x/y\", allow_api_billing = true }]\n",
        );
        assert_eq!(entry_billing(&layer).len(), 3);
        let dropped = drop_entry_billing(&mut layer);
        assert_eq!(
            dropped,
            [
                "handoff.fallback[1].allow_api_billing",
                "handoff.replace.codex.with[0].allow_api_billing",
                "roles.owner.handoff.replace.codex.with[0].allow_api_billing",
            ]
        );
        assert!(entry_billing(&layer).is_empty());
        let text = toml::to_string(&layer).unwrap();
        assert!(!text.contains("allow_api_billing"), "{text}");
    }
}
