//! Merges the settings layers. Later layers win key by key.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use argonauts_core::app::ConfigEntry;
use argonauts_core::events::run::OverrideRecord;
use argonauts_core::names::RoleName;
use argonauts_core::settings::budget::BudgetSettings;
use argonauts_core::settings::{SAFETY_KEYS, Settings};
use argonauts_core::{canonical_json, sha256_hex};
use toml::{Table, Value};

use crate::{ResolvedSettings, SettingsError, SettingsSources};

const BUILT_IN: &str = "built-in";
const GLOBAL: &str = "global";
const PROJECT: &str = "project";
const PROJECT_LOCAL: &str = "project-local";
const FLAG: &str = "flag";

const DEFAULT_PRESET: &str = "peer-crew";

/// Set in every agent session; safety keys never change from inside one.
pub(crate) const SESSION_ENV: &str = "ARGONAUTS_SESSION";

pub fn resolve(sources: &SettingsSources) -> Result<ResolvedSettings, SettingsError> {
    let defaults = to_table(&Settings::default(), BUILT_IN)?;
    let mut merged = defaults.clone();
    let mut set_by: BTreeMap<String, &'static str> = BTreeMap::new();

    let mut untrusted = Vec::new();
    // The budget unit the latest layer picked for each limit, such as `run_tokens`.
    let mut budget_units = BTreeMap::new();
    // Each project file is trusted on its own, so a safety key you saved just for yourself
    // applies even while the shared file waits for your trust.
    let layers = [
        (GLOBAL, &sources.global, true),
        (PROJECT, &sources.project, sources.project_trusted),
        (
            PROJECT_LOCAL,
            &sources.project_local,
            sources.project_local_trusted,
        ),
    ];
    for (layer, path, trusted) in layers {
        let Some(path) = path else { continue };
        let Some(mut table) = read_layer(path)? else {
            continue;
        };
        // A cloned repo may not loosen a safety key until you trust its files.
        if !trusted {
            for (key, _) in leaves(&table) {
                if is_safety_key(&key) {
                    remove_leaf(&mut table, &key);
                    untrusted.push((key, path));
                }
            }
        }
        for (key, _) in leaves(&table) {
            set_by.insert(key, layer);
        }
        note_budget_units(&path.display().to_string(), &table, &mut budget_units)?;
        merge(&mut merged, table);
    }

    for flag in &sources.overrides {
        let table = parse_flag(flag)?;
        for (key, _) in leaves(&table) {
            if is_safety_key(&key) {
                refuse_unless_person(&key, sources.interactive)?;
            }
            set_by.insert(key, FLAG);
        }
        note_budget_units(&format!("--set {flag}"), &table, &mut budget_units)?;
        merge(&mut merged, table);
    }

    let preset = merged
        .get("preset")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PRESET);
    if preset != DEFAULT_PRESET {
        return Err(SettingsError::NotImplemented(format!(
            "the {preset:?} preset (only {DEFAULT_PRESET:?} resolves until presets land)"
        )));
    }

    let mut settings: Settings =
        Value::Table(merged)
            .try_into()
            .map_err(|err: toml::de::Error| SettingsError::Invalid {
                layer: "the merged settings".to_owned(),
                message: err.to_string(),
            })?;
    // Only now, since `#[serde(default)]` would bring back a `run_usd` removed from the table.
    keep_one_budget_unit(&mut settings.budget, &budget_units);
    for (limit, keys) in BUDGET_UNITS {
        if let Some(unit) = budget_units.get(limit) {
            for key in keys.iter().filter(|key| *key != unit) {
                set_by.remove(&format!("budget.{key}"));
            }
        }
    }

    let resolved = leaves(&to_table(&settings, "the merged settings")?);
    let default_values: BTreeMap<String, Value> = leaves(&defaults).into_iter().collect();
    let known: BTreeSet<&str> = resolved.iter().map(|(key, _)| key.as_str()).collect();

    // Serde drops keys it doesn't know, so anything a layer set that didn't survive is unknown.
    let mut warnings: Vec<String> = set_by
        .iter()
        .filter(|(key, _)| !known.contains(key.as_str()))
        .map(|(key, layer)| format!("unknown setting {key} in the {layer} layer, ignored"))
        .collect();
    warnings.extend(untrusted.iter().map(|(key, path)| {
        format!(
            "{key} in {} is a safety setting and is ignored until you trust the file (/init, or \
             the trust question when it changes)",
            path.display()
        )
    }));
    if settings
        .roles
        .get(&RoleName::Default)
        .is_some_and(|role| !role.workers.is_empty())
    {
        warnings.push(
            "roles.default.workers is ignored: the default agent is default_agent, which /agent \
             and /model default set"
                .to_owned(),
        );
    }

    let entries = resolved
        .iter()
        .map(|(key, value)| ConfigEntry {
            key: key.clone(),
            value: value.to_string(),
            layer: set_by.get(key).copied().unwrap_or(BUILT_IN).to_owned(),
            safety: is_safety_key(key),
        })
        .collect();

    let overrides = resolved
        .iter()
        .filter_map(|(key, value)| {
            let layer = set_by.get(key)?;
            (default_values.get(key) != Some(value)).then(|| OverrideRecord {
                key: key.clone(),
                value: value.to_string(),
                from: (*layer).to_owned(),
            })
        })
        .collect();

    let json = serde_json::to_value(&settings).map_err(|err| SettingsError::Invalid {
        layer: "the merged settings".to_owned(),
        message: err.to_string(),
    })?;
    Ok(ResolvedSettings {
        sha256: sha256_hex(canonical_json(&json).as_bytes()),
        settings,
        entries,
        overrides,
        warnings,
    })
}

fn to_table(settings: &Settings, layer: &str) -> Result<Table, SettingsError> {
    Table::try_from(settings).map_err(|err| SettingsError::Invalid {
        layer: layer.to_owned(),
        message: err.to_string(),
    })
}

fn read_layer(path: &Path) -> Result<Option<Table>, SettingsError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(SettingsError::Io {
                path: path.to_owned(),
                message: err.to_string(),
            });
        }
    };
    text.parse::<Table>()
        .map(Some)
        .map_err(|err| SettingsError::Invalid {
            layer: path.display().to_string(),
            message: err.to_string(),
        })
}

/// `key=value` as a nested table. The value is TOML when it parses, else a plain string, so
/// `--set ui.keys=vim` works without quotes.
fn parse_flag(flag: &str) -> Result<Table, SettingsError> {
    let invalid = |message: &str| SettingsError::Invalid {
        layer: format!("--set {flag}"),
        message: message.to_owned(),
    };
    let (key, raw) = flag
        .split_once('=')
        .ok_or_else(|| invalid("expected key=value"))?;
    let key = key.trim();
    let segments: Vec<&str> = key.split('.').collect();
    if segments.iter().any(|segment| segment.trim().is_empty()) {
        return Err(invalid("the key needs a name between every dot"));
    }
    let raw = raw.trim();
    let value = format!("value = {raw}")
        .parse::<Table>()
        .ok()
        .and_then(|mut table| table.remove("value"))
        .unwrap_or_else(|| Value::String(raw.to_owned()));
    let mut table = Table::new();
    let mut current = &mut table;
    for segment in &segments[..segments.len() - 1] {
        current = match current
            .entry(segment.trim().to_owned())
            .or_insert_with(|| Value::Table(Table::new()))
        {
            Value::Table(next) => next,
            _ => return Err(invalid("the key goes through a value")),
        };
    }
    current.insert(segments[segments.len() - 1].trim().to_owned(), value);
    Ok(table)
}

fn refuse_unless_person(key: &str, interactive: bool) -> Result<(), SettingsError> {
    if std::env::var_os(SESSION_ENV).is_some() {
        return Err(SettingsError::Refused(format!(
            "{key} is a safety setting and can't be changed from inside an agent session"
        )));
    }
    if !interactive {
        return Err(SettingsError::Refused(format!(
            "{key} is a safety setting; change it from a terminal"
        )));
    }
    Ok(())
}

/// The budget keys that are one choice each: one run limit and one card limit, in one unit.
const BUDGET_UNITS: &[(&str, &[&str])] = &[
    (
        "run",
        &["run_usd", "run_tokens", "run_quota", "run_minutes"],
    ),
    (
        "per_card",
        &[
            "per_card_usd",
            "per_card_tokens",
            "per_card_quota",
            "per_card_minutes",
        ],
    ),
];

/// Records the unit `table` picked for each budget limit. Two units for one limit in one layer
/// is an error, since nothing says which one you meant.
fn note_budget_units(
    layer: &str,
    table: &Table,
    chosen: &mut BTreeMap<&'static str, &'static str>,
) -> Result<(), SettingsError> {
    let Some(Value::Table(budget)) = table.get("budget") else {
        return Ok(());
    };
    for (limit, keys) in BUDGET_UNITS {
        let set: Vec<&'static str> = keys
            .iter()
            .copied()
            .filter(|key| budget.contains_key(*key))
            .collect();
        match set.as_slice() {
            [] => {}
            [unit] => {
                chosen.insert(limit, unit);
            }
            _ => {
                return Err(SettingsError::Invalid {
                    layer: layer.to_owned(),
                    message: format!(
                        "budget.{} are one choice; set one of them",
                        set.join(" and budget.")
                    ),
                });
            }
        }
    }
    Ok(())
}

/// A layer that sets one `run_*` (or `per_card_*`) key clears the others it inherits, the
/// built-in `run_usd` and `per_card_usd` included, since TOML can't unset a key.
fn keep_one_budget_unit(budget: &mut BudgetSettings, chosen: &BTreeMap<&str, &str>) {
    let clear = |limit: &str, key: &str| chosen.get(limit).is_some_and(|unit| *unit != key);
    if clear("run", "run_usd") {
        budget.run_usd = None;
    }
    if clear("run", "run_tokens") {
        budget.run_tokens = None;
    }
    if clear("run", "run_quota") {
        budget.run_quota = None;
    }
    if clear("run", "run_minutes") {
        budget.run_minutes = None;
    }
    if clear("per_card", "per_card_usd") {
        budget.per_card_usd = None;
    }
    if clear("per_card", "per_card_tokens") {
        budget.per_card_tokens = None;
    }
    if clear("per_card", "per_card_quota") {
        budget.per_card_quota = None;
    }
    if clear("per_card", "per_card_minutes") {
        budget.per_card_minutes = None;
    }
}

/// Lists that join across layers instead of replacing: a layer can hide more paths from a
/// vendor, never show one that another layer hid.
const JOINED: &[&str] = &["agents.*.exclude_paths"];

/// Overlays `layer` on `base`, key by key; tables merge, and anything else replaces, except the
/// lists in [`JOINED`].
fn merge(base: &mut Table, layer: Table) {
    merge_under(base, layer, "");
}

fn merge_under(base: &mut Table, layer: Table, prefix: &str) {
    for (key, value) in layer {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        let joined = JOINED.iter().any(|pattern| key_matches(pattern, &path));
        match (base.get_mut(&key), value) {
            (Some(Value::Table(existing)), Value::Table(more)) => {
                merge_under(existing, more, &path);
            }
            (Some(Value::Array(existing)), Value::Array(more)) if joined => {
                for item in more {
                    if !existing.contains(&item) {
                        existing.push(item);
                    }
                }
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

fn remove_leaf(table: &mut Table, key: &str) {
    match key.split_once('.') {
        Some((head, rest)) => {
            if let Some(Value::Table(inner)) = table.get_mut(head) {
                remove_leaf(inner, rest);
            }
        }
        None => {
            table.remove(key);
        }
    }
}

/// Every non-table value, with its dotted key. Arrays count as one value.
pub(crate) fn leaves(table: &Table) -> Vec<(String, Value)> {
    fn walk(prefix: &str, table: &Table, out: &mut Vec<(String, Value)>) {
        for (key, value) in table {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            match value {
                Value::Table(inner) => walk(&path, inner, out),
                other => out.push((path, other.clone())),
            }
        }
    }
    let mut out = Vec::new();
    walk("", table, &mut out);
    out
}

pub(crate) fn is_safety_key(key: &str) -> bool {
    SAFETY_KEYS.iter().any(|pattern| key_matches(pattern, key))
}

/// Whether a dotted `key` falls under `pattern`, where `*` matches one segment and a trailing
/// `.*` covers the key itself and everything below, so a whole section set at once counts too.
pub(crate) fn key_matches(pattern: &str, key: &str) -> bool {
    let segments: Vec<&str> = key.split('.').collect();
    let (pattern, subtree) = match pattern.strip_suffix(".*") {
        Some(prefix) => (prefix, true),
        None => (pattern, false),
    };
    let parts: Vec<&str> = pattern.split('.').collect();
    let length_fits = if subtree {
        segments.len() >= parts.len()
    } else {
        segments.len() == parts.len()
    };
    length_fits
        && parts
            .iter()
            .zip(&segments)
            .all(|(part, segment)| *part == "*" || part == segment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::names::RoleName;
    use argonauts_core::settings::ui::NarratorMode;

    fn write(dir: &Path, name: &str, text: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn built_in_defaults_alone_are_peer_crew_with_nothing_overridden() {
        let resolved = resolve(&SettingsSources::default()).unwrap();
        assert_eq!(resolved.settings, Settings::default());
        assert!(resolved.overrides.is_empty());
        assert!(resolved.warnings.is_empty());
        assert!(resolved.entries.iter().all(|entry| entry.layer == BUILT_IN));
        assert_eq!(resolved.sha256.len(), 64);
        let again = resolve(&SettingsSources::default()).unwrap();
        assert_eq!(again.sha256, resolved.sha256);
    }

    #[test]
    fn later_layers_win_key_by_key() {
        let dir = tempfile::tempdir().unwrap();
        let global = write(
            dir.path(),
            "global.toml",
            "[ui]\nnarrator = \"live\"\nkeys = \"vim\"\n",
        );
        let project = write(
            dir.path(),
            "project.toml",
            "[ui]\nnarrator = \"off\"\n[roles.reviewer]\nworkers = [\"codex\"]\n",
        );
        let resolved = resolve(&SettingsSources {
            global: Some(global),
            project: Some(project),
            overrides: vec!["critique.rounds=0".into(), "ui.verbosity=quiet".into()],
            ..SettingsSources::default()
        })
        .unwrap();
        let settings = &resolved.settings;
        assert_eq!(settings.ui.narrator, NarratorMode::Off);
        assert_eq!(settings.roles[&RoleName::Reviewer].workers.len(), 1);
        let layer = |key: &str| {
            resolved
                .entries
                .iter()
                .find(|entry| entry.key == key)
                .map(|entry| entry.layer.clone())
        };
        assert_eq!(layer("ui.narrator").as_deref(), Some(PROJECT));
        assert_eq!(layer("ui.keys").as_deref(), Some(GLOBAL));
        assert_eq!(layer("critique.rounds").as_deref(), Some(FLAG));
        assert_eq!(layer("preset").as_deref(), Some(BUILT_IN));
        let overridden: Vec<&str> = resolved.overrides.iter().map(|o| o.key.as_str()).collect();
        assert!(overridden.contains(&"critique.rounds"), "{overridden:?}");
        assert!(overridden.contains(&"ui.keys"));
        assert_ne!(
            resolved.sha256,
            resolve(&SettingsSources::default()).unwrap().sha256
        );
    }

    #[test]
    fn unknown_keys_warn_and_missing_files_are_fine() {
        let dir = tempfile::tempdir().unwrap();
        let project = write(dir.path(), "project.toml", "[ui]\nnarrattor = \"live\"\n");
        let resolved = resolve(&SettingsSources {
            global: Some(dir.path().join("missing.toml")),
            project: Some(project),
            ..SettingsSources::default()
        })
        .unwrap();
        assert_eq!(resolved.warnings.len(), 1, "{:?}", resolved.warnings);
        assert!(resolved.warnings[0].contains("ui.narrattor"));
    }

    #[test]
    fn bad_files_and_flags_name_their_layer() {
        let dir = tempfile::tempdir().unwrap();
        let project = write(dir.path(), "project.toml", "[ui\n");
        let err = resolve(&SettingsSources {
            project: Some(project.clone()),
            ..SettingsSources::default()
        })
        .unwrap_err();
        assert!(err.to_string().contains("project.toml"), "{err}");
        let err = resolve(&SettingsSources {
            overrides: vec!["no-equals".into()],
            ..SettingsSources::default()
        })
        .unwrap_err();
        assert!(err.to_string().contains("--set no-equals"), "{err}");
        let err = resolve(&SettingsSources {
            overrides: vec!["critique.rounds=\"many\"".into()],
            ..SettingsSources::default()
        })
        .unwrap_err();
        assert!(matches!(err, SettingsError::Invalid { .. }), "{err}");
    }

    #[test]
    fn safety_keys_need_a_person_at_a_terminal() {
        let flag = SettingsSources {
            overrides: vec!["sandbox.network=true".into()],
            interactive: false,
            ..SettingsSources::default()
        };
        assert!(matches!(resolve(&flag), Err(SettingsError::Refused(_))));
        for key in [
            "limits.protected_paths",
            "approvals.merge.mode",
            "budget.run_usd",
            "agents.codex.auth",
            "endpoints.openrouter.base_url",
            "endpoints.openrouter.header_env.Authorization",
            "roles.owner.handoff.allow_api_billing",
            "agents.codex.extra_args",
            "agents.codex.binary",
            "mcp.status_tool",
            "merge.mode",
            "gates.run",
        ] {
            assert!(is_safety_key(key), "{key}");
        }
        for key in [
            "budget.per_card_usd",
            "agents.codex.model",
            "ui.keys",
            "roles.owner.workers",
        ] {
            assert!(!is_safety_key(key), "{key}");
        }
    }

    #[test]
    fn project_safety_keys_need_trust() {
        let dir = tempfile::tempdir().unwrap();
        let project = write(
            dir.path(),
            "project.toml",
            "ui.keys = \"vim\"\n[sandbox]\nnetwork = true\n[approvals.merge]\nmode = \"auto\"\n",
        );
        let untrusted = resolve(&SettingsSources {
            project: Some(project.clone()),
            ..SettingsSources::default()
        })
        .unwrap();
        assert!(!untrusted.settings.sandbox.network);
        assert_eq!(
            untrusted.settings.ui.keys,
            argonauts_core::KeyScheme::Vim,
            "ordinary keys still apply"
        );
        assert_eq!(untrusted.warnings.len(), 2, "{:?}", untrusted.warnings);
        assert!(untrusted.warnings.iter().all(|w| w.contains("trust")));

        let trusted = resolve(&SettingsSources {
            project: Some(project),
            project_trusted: true,
            ..SettingsSources::default()
        })
        .unwrap();
        assert!(trusted.settings.sandbox.network);
        assert!(trusted.warnings.is_empty());
    }

    #[test]
    fn local_layer_between_project_and_preset() {
        let dir = tempfile::tempdir().unwrap();
        let global = write(
            dir.path(),
            "global.toml",
            "[ui]\nkeys = \"vim\"\nbell = false\n",
        );
        let project = write(
            dir.path(),
            "project.toml",
            "[ui]\nbell = true\nmotion = \"reduced\"\n",
        );
        let local = write(
            dir.path(),
            "local.toml",
            "[ui]\nmotion = \"normal\"\n[sandbox]\nnetwork = true\n",
        );
        let sources = SettingsSources {
            global: Some(global),
            project: Some(project),
            project_local: Some(local),
            overrides: vec!["ui.keys=\"default\"".into()],
            ..SettingsSources::default()
        };
        let resolved = resolve(&sources).unwrap();
        let layer = |key: &str| {
            resolved
                .entries
                .iter()
                .find(|entry| entry.key == key)
                .map(|entry| entry.layer.clone())
                .unwrap()
        };
        assert_eq!(layer("ui.bell"), "project");
        assert_eq!(layer("ui.motion"), "project-local");
        assert_eq!(layer("ui.keys"), "flag");
        assert!(!resolved.settings.sandbox.network);
        let shared_only = resolve(&SettingsSources {
            project_trusted: true,
            ..sources.clone()
        })
        .unwrap();
        assert!(!shared_only.settings.sandbox.network);
        let trusted = resolve(&SettingsSources {
            project_local_trusted: true,
            ..sources
        })
        .unwrap();
        assert!(trusted.settings.sandbox.network);
    }

    #[test]
    fn budget_unit_switch_in_a_later_layer() {
        let dir = tempfile::tempdir().unwrap();
        let global = write(dir.path(), "global.toml", "[budget]\nrun_minutes = 30.0\n");
        let project = write(
            dir.path(),
            "project.toml",
            "[budget]\nrun_tokens = 200000\nper_card_minutes = 10.0\n",
        );
        let sources = SettingsSources {
            global: Some(global),
            project: Some(project),
            project_trusted: true,
            ..SettingsSources::default()
        };
        let resolved = resolve(&sources).unwrap();
        let budget = &resolved.settings.budget;
        assert_eq!((budget.run_usd, budget.run_minutes), (None, None));
        assert_eq!(budget.run_tokens, Some(200_000));
        assert_eq!(
            (budget.per_card_usd, budget.per_card_minutes),
            (None, Some(10.0))
        );
        assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
        let layer = |key: &str| {
            resolved
                .entries
                .iter()
                .find(|entry| entry.key == key)
                .map(|entry| entry.layer.clone())
        };
        assert_eq!(layer("budget.run_tokens").as_deref(), Some(PROJECT));
        assert_eq!(layer("budget.run_minutes"), None);
        let flagged = resolve(&SettingsSources {
            overrides: vec!["budget.run_usd=5".into()],
            interactive: true,
            ..sources
        })
        .unwrap();
        assert_eq!(flagged.settings.budget.run_usd, Some(5.0));
        assert_eq!(flagged.settings.budget.run_tokens, None);
        let built_in = resolve(&SettingsSources::default()).unwrap();
        assert_eq!(built_in.settings.budget.run_usd, Some(20.0));
    }

    #[test]
    fn two_budget_units_in_one_layer_are_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let global = write(
            dir.path(),
            "global.toml",
            "[budget]\nrun_usd = 5.0\nrun_tokens = 100\n",
        );
        let err = resolve(&SettingsSources {
            global: Some(global.clone()),
            ..SettingsSources::default()
        })
        .unwrap_err();
        assert!(matches!(err, SettingsError::Invalid { .. }), "{err}");
        assert!(err.to_string().contains("global.toml"), "{err}");
        assert!(err.to_string().contains("one choice"), "{err}");
    }

    #[test]
    fn exclude_paths_join_across_layers() {
        let dir = tempfile::tempdir().unwrap();
        let global = write(
            dir.path(),
            "global.toml",
            "[agents.codex]\nexclude_paths = [\"secrets/**\"]\n",
        );
        let project = write(
            dir.path(),
            "project.toml",
            "[agents.codex]\nexclude_paths = []\n",
        );
        let local = write(
            dir.path(),
            "local.toml",
            "[agents.codex]\nexclude_paths = [\"notes/**\", \"secrets/**\"]\n",
        );
        let resolved = resolve(&SettingsSources {
            global: Some(global),
            project: Some(project),
            project_local: Some(local),
            overrides: vec!["agents.codex.exclude_paths=[]".into()],
            ..SettingsSources::default()
        })
        .unwrap();
        assert_eq!(
            resolved.settings.agents["codex"].exclude_paths,
            ["secrets/**", "notes/**"]
        );
    }

    #[test]
    fn roles_default_workers_warns_that_default_agent_decides() {
        let resolved = resolve(&SettingsSources {
            overrides: vec!["roles.default.workers=[\"codex\"]".into()],
            ..SettingsSources::default()
        })
        .unwrap();
        assert_eq!(resolved.warnings.len(), 1, "{:?}", resolved.warnings);
        assert!(resolved.warnings[0].contains("default_agent"));
        assert_eq!(resolved.settings.default_agent.worker, "claude-code");
    }

    #[test]
    fn extra_args_is_safety_key() {
        assert!(is_safety_key("agents.claude-code.extra_args"));
        assert!(!is_safety_key("agents.claude-code.model"));
    }

    #[test]
    fn presets_other_than_the_default_are_not_implemented_yet() {
        let err = resolve(&SettingsSources {
            overrides: vec!["preset=coordinated-crew".into()],
            ..SettingsSources::default()
        })
        .unwrap_err();
        assert!(matches!(err, SettingsError::NotImplemented(_)), "{err}");
    }
}
