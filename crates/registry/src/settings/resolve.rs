//! Merges the settings layers. Later layers win key by key.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use argonauts_core::app::ConfigEntry;
use argonauts_core::base::layout::{project_files, user_files};
use argonauts_core::base::names::RoleName;
use argonauts_core::runs::events::run::OverrideRecord;
use argonauts_core::safety::sanitize::escape_controls;
use argonauts_core::settings::approvals::{ApprovalMode, ApprovalSettings};
use argonauts_core::settings::evolution::FIXED_CORE_KEYS;
use argonauts_core::settings::ui::{BadValue, drop_bad_values};
use argonauts_core::settings::{
    FILE_LAYERS, GLOBAL_ONLY_KEYS, LAYERS, SAFETY_KEYS, Settings, project_may_tighten,
};
use argonauts_core::{canonical_json, sha256_hex};
use toml::{Table, Value};

use crate::clis::declared_cli_ids;
use crate::settings::crew;
use crate::settings::fallback::{drop_entry_billing, entry_billing, merge_top_level};
use crate::settings::limit_units::{LimitUnits, other_units};
use crate::settings::outside;
use crate::settings::passing_over::{
    PassedOver, Stop, drop_odd_keys, from_a_file, narrowed, refused_alone,
};
use crate::settings::plain;
use crate::settings::renamed::{self, OldKey};
use crate::settings::roles::fill_fresh_sessions;
use crate::settings::set_by::SetBy;
use crate::settings::validate::{Context, type_error, type_message, validate};
use crate::settings::workflow_layer::{WorkflowLayer, workflow_layer};
use crate::workflows::DEFAULT_WORKFLOW;
use crate::{ResolvedSettings, SettingsError, SettingsSources, SettingsWarning, WarningAbout};

pub(crate) use argonauts_core::settings::layer::{
    BUILT_IN, FLAG, GLOBAL, PROJECT, PROJECT_LOCAL, REQUEST, WORKFLOW,
};

/// Set in every agent session; safety keys never change from inside one.
pub(crate) const SESSION_ENV: &str = "ARGONAUTS_SESSION";

/// The override value of a key the limit-unit switch cleared. TOML has no null, and a bare
/// word isn't TOML, so it can't be mistaken for a value a layer set.
pub(crate) const UNSET: &str = "unset";

pub fn resolve(sources: &SettingsSources) -> Result<ResolvedSettings, SettingsError> {
    let mut passed_over = PassedOver::default();
    loop {
        match resolve_once(sources, &passed_over) {
            Ok(mut resolved) => {
                let passed = passed_over
                    .noticed()
                    .iter()
                    .map(|(key, notice)| match file_renaming(sources, key) {
                        Some(file) => {
                            SettingsWarning::new(WarningAbout::Renamed { file }, notice.clone())
                        }
                        None => SettingsWarning::other(notice.clone()),
                    });
                let passed: Vec<SettingsWarning> = passed.collect();
                resolved.warnings.splice(0..0, passed);
                return Ok(resolved);
            }
            Err(stop) if !sources.strict && passed_over.add(&stop) => {}
            Err(stop) => return Err(*stop.error),
        }
    }
}

/// The settings file whose old value for `key` was left out, such as a workflow named by its
/// name from before, which the start offers to rename.
fn file_renaming(sources: &SettingsSources, key: &str) -> Option<PathBuf> {
    [&sources.global, &sources.project, &sources.project_local]
        .into_iter()
        .flatten()
        .find(|file| {
            fs::read_to_string(file)
                .is_ok_and(|text| renamed::renames_in(&text).iter().any(|r| r.from == key))
        })
        .cloned()
}

/// One pass over the layers, leaving out of the files the keys an earlier pass stopped at.
fn resolve_once(
    sources: &SettingsSources,
    passed_over: &PassedOver,
) -> Result<ResolvedSettings, Stop> {
    let defaults = to_table(&Settings::default(), BUILT_IN)?;
    let mut merged = defaults.clone();
    let mut set_by = SetBy::default();
    let mut limit_units = LimitUnits::default();
    let mut warnings: Vec<SettingsWarning> = Vec::new();
    // The old names each file still uses, which the start offers to rename.
    let mut renamed_keys: BTreeMap<String, (OldKey, PathBuf)> = BTreeMap::new();

    // Each project file is trusted on its own, so a safety key you saved just for yourself
    // applies even while the shared file waits for your trust.
    let files = [
        (GLOBAL, &sources.global, true),
        (PROJECT, &sources.project, sources.project_trusted),
        (
            PROJECT_LOCAL,
            &sources.project_local,
            sources.project_local_trusted,
        ),
    ];
    let repo = repo_root(sources);
    let mut file_tables = Vec::new();
    let mut skipped_a_file = false;
    for (layer, path, trusted) in files {
        let Some(path) = path else { continue };
        let source = shown(path, repo.as_deref(), sources.home.as_deref());
        let Some(text) = read_text(path)? else {
            continue;
        };
        let parsed = match text.parse::<Table>() {
            Ok(table) => table,
            Err(err) if !sources.strict => {
                warnings.push(SettingsWarning::other(skipped_file(&source, &text, &err)));
                skipped_a_file = true;
                continue;
            }
            Err(err) => {
                return Err(Stop::from(SettingsError::Invalid {
                    layer: source,
                    message: plain::syntax_message(&text, &err),
                }));
            }
        };
        for (old, new) in renamed::old_keys(&parsed) {
            renamed_keys.insert(old, (new, path.clone()));
        }
        let file = FileLayer {
            layer,
            path,
            source: &source,
            trusted,
        };
        let Some(mut table) = read_file_layer(&file, parsed, sources.strict, &mut warnings)? else {
            continue;
        };
        if layer != GLOBAL {
            for why in outside::narrow(&mut table, &merged, &source) {
                if sources.strict {
                    return Err(Stop::from(SettingsError::Refused(why)));
                }
                warnings.push(SettingsWarning::other(why));
            }
        }
        passed_over.leave_out(&mut table);
        set_by.record(&table, layer, &source);
        limit_units.note(layer, &source, &table)?;
        file_tables.push((layer, table.clone()));
        merge(&mut merged, table);
    }

    let flags = sources
        .overrides
        .iter()
        .map(|flag| read_flag(flag, sources.interactive))
        .collect::<Result<Vec<_>, _>>()?;

    // The files and flags pick the workflow, and a workflow can't pick another, so the resolved
    // `workflow` always names the workflow applied.
    let choice = workflow_choice(&merged, &flags, &set_by).map_err(|error| {
        let in_a_file = !flags
            .iter()
            .any(|(_, table)| table.contains_key("workflow"))
            && set_by.layer("workflow").is_some_and(from_a_file);
        Stop::at("workflow", in_a_file, error)
    })?;
    let (mut workflow, workflow_warnings) = workflow_layer(
        &choice.name,
        &choice.source,
        from_a_file(choice.layer),
        sources,
    )?;
    passed_over.leave_out(&mut workflow.table);
    warnings.extend(workflow_warnings.into_iter().map(SettingsWarning::other));
    // What you set in the file that picks the workflow, or in a later file, is what you meant:
    // the workflow fills in the rest and sits above only the files before it. The default,
    // which nobody picked, gives way to every file.
    if choice.layer == BUILT_IN {
        merged.insert("workflow".to_owned(), Value::String(choice.name.clone()));
    }
    if picked_in_a_file(choice.layer) || choice.layer == BUILT_IN {
        for (layer, table) in &file_tables {
            if rank(layer) >= rank(choice.layer) {
                yield_to(&mut workflow.table, table, "");
            }
        }
    }
    let hidden = hidden_by_workflow(&workflow, &merged, &set_by, &choice);
    let source = workflow.label();
    set_by.record(&workflow.table, WORKFLOW, &source);
    limit_units.note(WORKFLOW, &source, &workflow.table)?;
    merge(&mut merged, workflow.table);
    if let Some(learned_workflow) = &sources.learned_workflow {
        match learned_workflow_layer(learned_workflow, &merged) {
            Ok(mut table) => {
                passed_over.leave_out(&mut table);
                set_by.record(&table, WORKFLOW, &learned_workflow.label);
                limit_units.note(WORKFLOW, &learned_workflow.label, &table)?;
                merge(&mut merged, table);
            }
            Err(warning) => warnings.push(SettingsWarning::other(warning)),
        }
    }

    for (source, mut table) in flags {
        crew::spell_out(&mut table);
        set_by.record(&table, FLAG, &source);
        limit_units.note(FLAG, &source, &table)?;
        merge(&mut merged, table);
    }
    for entry in &sources.request {
        let table = read_request(entry, &merged)?;
        set_by.record(&table, REQUEST, "your request");
        limit_units.note(REQUEST, "your request", &table)?;
        merge(&mut merged, table);
    }
    // A flag that sets the key after the workflow leaves nothing hidden to warn about.
    warnings.extend(
        hidden
            .into_iter()
            .filter(|(key, _)| set_by.layer(key) == Some(WORKFLOW))
            .map(|(_, warning)| SettingsWarning::other(warning)),
    );

    crew::follow_what_is_set(&mut merged, &set_by);
    for (from, to) in merge_top_level(&mut merged) {
        set_by.copy(&from, to);
    }
    fill_fresh_sessions(&mut merged);
    // A typo in how the screen looks never stops the app: the value falls back and is named.
    let bad_values = drop_bad_values(&mut merged, &defaults);
    warnings.extend(
        bad_values
            .iter()
            .map(|bad| SettingsWarning::other(ui_warning(bad, &set_by))),
    );

    let mut settings: Settings =
        Value::Table(merged.clone())
            .try_into()
            .map_err(|err: toml::de::Error| {
                let error = type_error(&err, &set_by, &merged);
                match type_message(&err, &merged).0 {
                    Some(key) => {
                        let key = narrowed(key, &merged);
                        let in_a_file = set_by.layer_around(&key).is_some_and(from_a_file);
                        let also = refused_alone(&key, &merged)
                            .into_iter()
                            .map(|(alone, refused)| type_error(&refused, &set_by, &alone));
                        Stop::at(&key, in_a_file, error).with(also)
                    }
                    None => Stop::from(error),
                }
            })?;
    let cleared = limit_units.apply(&mut settings.limits.spend, &mut set_by);
    if skipped_a_file {
        hold_supervised(&mut settings.approvals);
    }
    warnings.extend(settings.ui.tidy().into_iter().map(|problem| {
        SettingsWarning::other(format!(
            "{} (in {})",
            problem.message,
            set_by.source(&problem.key)
        ))
    }));

    let resolved = leaves(&to_table(&settings, "the merged settings")?);
    let cli_ids = declared_clis(sources);
    let context = Context {
        repo_root: repo.as_deref(),
        leaves: &resolved,
        set_by: &set_by,
        cli_ids: &cli_ids,
    };
    let problems = validate(&settings, &context).map_err(|(key, error)| {
        let in_a_file = set_by.layer_around(&key).is_some_and(from_a_file);
        Stop::at(&key, in_a_file, error)
    })?;
    warnings.extend(problems.into_iter().map(SettingsWarning::other));

    // Serde drops keys it doesn't know, so anything a layer set that didn't survive is unknown.
    let known: BTreeSet<&str> = resolved.iter().map(|(key, _)| key.as_str()).collect();
    let named: BTreeSet<&str> = bad_values.iter().map(|bad| bad.key.as_str()).collect();
    warnings.extend(
        set_by
            .keys()
            .filter(|(key, _)| !known.contains(key) && !named.contains(key))
            .map(|(key, source)| match renamed_keys.get(key) {
                Some((OldKey::Renamed(new), file)) => SettingsWarning::new(
                    WarningAbout::Renamed { file: file.clone() },
                    format!(
                        "{key} in {source} is a name that changed, now {new}, so it is skipped \
                         until the file says {new}"
                    ),
                ),
                Some((OldKey::Retired(why), file)) => SettingsWarning::new(
                    WarningAbout::Renamed { file: file.clone() },
                    format!("{key} in {source} is skipped: {why}"),
                ),
                None => SettingsWarning::new(
                    WarningAbout::Unknown {
                        key: key.to_owned(),
                    },
                    format!("unknown setting {key} in {source}, ignored"),
                ),
            }),
    );
    if settings
        .roles
        .get(&RoleName::Main)
        .is_some_and(|role| !role.models.is_empty())
    {
        warnings.push(SettingsWarning::other(
            "roles.main.models is ignored: the main agent is crew.main, which /model sets",
        ));
    }

    let entries = resolved
        .iter()
        .map(|(key, value)| ConfigEntry {
            key: key.clone(),
            value: value.to_string(),
            layer: set_by.layer(key).unwrap_or(BUILT_IN).to_owned(),
            safety: is_safety_key(key),
        })
        .collect();

    let default_values: BTreeMap<String, Value> = leaves(&defaults).into_iter().collect();
    let mut overrides: Vec<OverrideRecord> = resolved
        .iter()
        .filter_map(|(key, value)| {
            let layer = set_by.layer(key)?;
            (default_values.get(key) != Some(value)).then(|| OverrideRecord {
                key: key.clone(),
                value: value.to_string(),
                from: layer.to_owned(),
            })
        })
        .collect();
    // A built-in limit the unit switch cleared is different from the default too.
    overrides.extend(
        cleared
            .into_iter()
            .filter(|cleared| default_values.contains_key(&cleared.key))
            .map(|cleared| OverrideRecord {
                key: cleared.key,
                value: UNSET.to_owned(),
                from: cleared.layer.to_owned(),
            }),
    );

    let json = serde_json::to_value(&settings).map_err(|err| {
        Stop::from(SettingsError::Invalid {
            layer: "the merged settings".to_owned(),
            message: err.to_string(),
        })
    })?;
    Ok(ResolvedSettings {
        sha256: sha256_hex(canonical_json(&json).as_bytes()),
        settings,
        entries,
        overrides,
        warnings,
    })
}

/// A value that didn't read, in plain words, with the layer that set it.
fn ui_warning(bad: &BadValue, set_by: &SetBy) -> String {
    let problem = plain::type_message(&bad.key, &bad.problem, Some(&bad.written));
    format!(
        "{problem} (in {}), so the default is used",
        set_by.source(&bad.key)
    )
}

fn to_table(settings: &Settings, layer: &str) -> Result<Table, SettingsError> {
    Table::try_from(settings).map_err(|err| SettingsError::Invalid {
        layer: layer.to_owned(),
        message: err.to_string(),
    })
}

/// One settings file, less what it may not set: a key only your global file may set is
/// refused, and until you trust a project file its safety keys, its fallback entries'
/// `allow_api_billing`, and what it says work costs are ignored with a warning, so a cloned
/// repo can't loosen them.
fn read_file_layer(
    file: &FileLayer<'_>,
    mut table: Table,
    strict: bool,
    warnings: &mut Vec<SettingsWarning>,
) -> Result<Option<Table>, SettingsError> {
    let FileLayer {
        layer,
        path,
        source,
        trusted,
    } = *file;
    crew::spell_out(&mut table);
    if !strict {
        warnings.extend(
            drop_odd_keys(&mut table, source)
                .into_iter()
                .map(SettingsWarning::other),
        );
    }
    check_plain_keys(&table, source)?;
    let global_only: Vec<String> = leaves(&table)
        .into_iter()
        .filter(|(key, value)| {
            layer != GLOBAL
                && GLOBAL_ONLY_KEYS
                    .iter()
                    .any(|pattern| key_matches(pattern, key))
                && !project_may_tighten(key, &value.to_string())
        })
        .map(|(key, _)| key)
        .collect();
    for key in global_only {
        let why = format!(
            "{key} in {source} can only be set in your global config, since {}",
            argonauts_core::settings::global_only_because(&key)
        );
        if strict {
            return Err(SettingsError::Refused(why));
        }
        remove_leaf(&mut table, &key);
        warnings.push(SettingsWarning::other(format!("{why}; it is ignored")));
    }
    if !trusted {
        let untrusted = |key: &str, text: String| {
            SettingsWarning::new(
                WarningAbout::Untrusted {
                    file: path.to_owned(),
                    key: key.to_owned(),
                },
                text,
            )
        };
        for (key, _) in leaves(&table) {
            if is_safety_key(&key) {
                remove_leaf(&mut table, &key);
                warnings.push(untrusted(
                    &key,
                    format!(
                        "{key} in {source} is a safety setting and is ignored until you trust the \
                     file (/setup, or the trust question when it changes)"
                    ),
                ));
            }
        }
        for key in drop_entry_billing(&mut table) {
            warnings.push(untrusted(
                &key,
                format!(
                    "{key} in {source} is ignored until you trust the file, since only your own \
                 settings let a fallback bill an API without asking"
                ),
            ));
        }
        for key in drop_cost_claims(&mut table) {
            warnings.push(untrusted(
                &key,
                format!(
                    "{key} in {source} is ignored until you trust the file, since it changes what \
                 work is estimated to cost, and so what the dollar limits see"
                ),
            ));
        }
    }
    Ok(Some(table))
}

/// `endpoints.<id>.local = true` prices an endpoint's work as free, and `agents.<id>.price`
/// sets a CLI's dollar rate, so either could keep paid work under every dollar cap.
fn drop_cost_claims(layer: &mut Table) -> Vec<String> {
    let mut dropped = Vec::new();
    let mut drop_in = |section: &str, key: &str, claims: fn(&Value) -> bool| {
        let Some(Value::Table(entries)) = layer.get_mut(section) else {
            return;
        };
        for (id, entry) in entries.iter_mut() {
            if let Value::Table(entry) = entry
                && entry.get(key).is_some_and(claims)
            {
                entry.remove(key);
                dropped.push(format!("{section}.{id}.{key}"));
            }
        }
    };
    drop_in("endpoints", "local", |local| local.as_bool() == Some(true));
    drop_in("agents", "price", |_| true);
    dropped
}

/// One settings file as resolving reads it.
#[derive(Clone, Copy)]
struct FileLayer<'a> {
    layer: &'a str,
    path: &'a Path,
    /// The file as a warning names it.
    source: &'a str,
    trusted: bool,
}

/// A file by its path from the repo when it's in one, as you'd name it there, and one under
/// your home from `~/`.
fn shown(path: &Path, repo: Option<&Path>, home: Option<&Path>) -> String {
    let in_repo = repo
        .and_then(|repo| path.strip_prefix(repo).ok())
        .filter(|relative| !relative.as_os_str().is_empty());
    if let Some(relative) = in_repo {
        return relative.display().to_string();
    }
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if !rest.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

/// A settings file's text, or `None` when there's no such file.
fn read_text(path: &Path) -> Result<Option<String>, SettingsError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(SettingsError::Io {
            path: path.to_owned(),
            message: err.to_string(),
        }),
    }
}

/// A file the TOML parser stops on is left out at start rather than stopping the app. Leaving
/// it out could loosen what it set, so approvals stay with you until it reads again. One line
/// names the place once, quotes it with its control characters written out, and says what
/// follows.
fn skipped_file(source: &str, text: &str, err: &toml::de::Error) -> String {
    let message = escape_controls(err.message().trim());
    let place = match plain::syntax_place(text, err) {
        Some((line, _)) => {
            let written = text.lines().nth(line - 1).unwrap_or_default().trim();
            format!(" at line {line} ({message}): {}", escape_controls(written))
        }
        None => format!(" ({message})"),
    };
    format!(
        "{source} doesn't read as TOML{place}. It is skipped until fixed, and until then \
         approvals wait for you and autopilot is off"
    )
}

/// Every approval waits for you and autopilot is off.
fn hold_supervised(approvals: &mut ApprovalSettings) {
    approvals.autopilot = false;
    for approval in [
        &mut approvals.understand,
        &mut approvals.plan,
        &mut approvals.merge,
        &mut approvals.stuck,
        &mut approvals.limit,
        &mut approvals.change_request,
    ] {
        approval.mode = ApprovalMode::Supervised;
    }
    approvals.workflow_promotion.mode = ApprovalMode::Supervised;
}

/// A `--set` flag as a table, with its source for `/config`. A safety key, or a fallback entry
/// that may bill an API, needs you at a terminal.
fn read_flag(flag: &str, interactive: bool) -> Result<(String, Table), SettingsError> {
    let table = parse_flag(flag)?;
    check_plain_keys(&table, &format!("--set {flag}"))?;
    for (key, _) in leaves(&table) {
        if is_safety_key(&key) {
            refuse_unless_person(&key, interactive)?;
        }
    }
    if let Some(key) = entry_billing(&table).first() {
        refuse_unless_person(key, interactive)?;
    }
    Ok((format!("--set {flag}"), table))
}

/// One setting a request asked for. A safety key passes only when it tightens what `merged`
/// says, by the fixed rules of [`tightens`](crate::tightens).
fn read_request(entry: &str, merged: &Table) -> Result<Table, SettingsError> {
    let mut table = parse_flag(entry)?;
    crew::spell_out(&mut table);
    check_plain_keys(&table, &format!("your request: {entry}"))?;
    for (key, value) in leaves(&table) {
        let stricter = crate::tightens(&key, &value, value_at(merged, &key));
        // A first limit in this unit would clear one you set in another.
        let switches_unit = value_at(merged, &key).is_none()
            && other_units(&key)
                .iter()
                .any(|other| value_at(merged, other).is_some());
        if is_safety_key(&key) && (!stricter || switches_unit) {
            return Err(SettingsError::Refused(format!(
                "{key} is a safety setting, and a request may only make it stricter"
            )));
        }
    }
    Ok(table)
}

/// The value at a dotted key, if every table on the way is there.
pub(crate) fn value_at<'a>(table: &'a Table, key: &str) -> Option<&'a Value> {
    let mut segments = key.split('.');
    let mut value = table.get(segments.next()?)?;
    for segment in segments {
        value = value.as_table()?.get(segment)?;
    }
    Some(value)
}

/// The workflow the files and flags pick.
struct PickedWorkflow {
    name: String,
    /// The file or flag that picked it, for the error when there's no such workflow.
    source: String,
    layer: &'static str,
}

fn workflow_choice(
    merged: &Table,
    flags: &[(String, Table)],
    set_by: &SetBy,
) -> Result<PickedWorkflow, SettingsError> {
    let (value, source, layer) = match flags
        .iter()
        .rev()
        .find_map(|(source, table)| Some((table.get("workflow")?, source.clone())))
    {
        Some((value, source)) => (value, source, FLAG),
        // Nothing names a workflow, so the default runs on the built-in settings.
        None => match merged
            .get("workflow")
            .filter(|_| set_by.layer("workflow").is_some())
        {
            Some(value) => (
                value,
                set_by.source("workflow"),
                set_by.layer("workflow").unwrap_or(BUILT_IN),
            ),
            None => {
                return Ok(PickedWorkflow {
                    name: DEFAULT_WORKFLOW.to_owned(),
                    source: BUILT_IN.to_owned(),
                    layer: BUILT_IN,
                });
            }
        },
    };
    match value {
        Value::String(name) => Ok(PickedWorkflow {
            name: name.clone(),
            source,
            layer,
        }),
        other => Err(SettingsError::Invalid {
            layer: source,
            message: format!("workflow must be a workflow's name, not {other}"),
        }),
    }
}

/// The learned workflow's settings, checked whole: one safety key or fixed-core key leaves the
/// learned workflow out. A canary run's merge opens a pull request when
/// `evolution.canary_pull_requests`, as merged so far, says so; that rule is yours, so it may set
/// `merge.mode`.
fn learned_workflow_layer(
    learned_workflow: &crate::LearnedWorkflowOverlay,
    merged: &Table,
) -> Result<Table, String> {
    let mut table: Table = learned_workflow
        .settings
        .parse()
        .map_err(|err| format!("{} is left out: {err}", learned_workflow.label))?;
    check_plain_keys(&table, &learned_workflow.label)
        .map_err(|err| format!("{err}; it is left out"))?;
    let barred: Vec<String> = leaves(&table)
        .into_iter()
        .map(|(key, _)| key)
        .filter(|key| {
            is_safety_key(key)
                || FIXED_CORE_KEYS
                    .iter()
                    .any(|pattern| key_matches(pattern, key))
        })
        .collect();
    if !barred.is_empty() {
        return Err(format!(
            "{} is left out: a learned workflow can't set {}",
            learned_workflow.label,
            barred.join(", ")
        ));
    }
    let pull_requests = merged
        .get("evolution")
        .and_then(Value::as_table)
        .and_then(|evolution| evolution.get("canary_pull_requests"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    // The loop's own rules, which your evolution settings set: the only safety keys a learned
    // workflow's layer carries.
    let merge_mode = match (
        learned_workflow.replay_limit_usd,
        learned_workflow.canary && pull_requests,
    ) {
        (Some(_), _) => Some("leave"),
        (None, true) => Some("pull_request"),
        (None, false) => None,
    };
    if let Some(mode) = merge_mode {
        let mut merge_table = Table::new();
        merge_table.insert("mode".to_owned(), Value::String(mode.to_owned()));
        table.insert("merge".to_owned(), Value::Table(merge_table));
    }
    if let Some(usd) = learned_workflow.replay_limit_usd {
        let limit = table
            .entry("limits")
            .or_insert_with(|| Value::Table(Table::new()));
        if let Some(limit) = limit.as_table_mut() {
            limit.insert("spend_per_run".to_owned(), Value::Float(usd as f64));
        }
    }
    Ok(table)
}

/// Where `layer` sits in [`LAYERS`], lowest first.
fn rank(layer: &str) -> Option<usize> {
    LAYERS.iter().position(|known| *known == layer)
}

/// Whether a settings file picked the workflow, rather than the built-in default or a flag.
fn picked_in_a_file(layer: &str) -> bool {
    FILE_LAYERS.contains(&layer)
}

/// Drops from `workflow` every key that `file` sets, so the file's value shows. A list that joins
/// across layers keeps both, as merging does.
fn yield_to(workflow: &mut Table, file: &Table, prefix: &str) {
    workflow.retain(|key, value| {
        let Some(theirs) = file.get(key) else {
            return true;
        };
        let path = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        match (value, theirs) {
            (Value::Table(ours), Value::Table(theirs)) => {
                yield_to(ours, theirs, &path);
                !ours.is_empty()
            }
            _ => JOINED.iter().any(|pattern| key_matches(pattern, &path)),
        }
    });
}

/// Each key a settings file set to something other than what the workflow sets, which the
/// workflow hides, since its layer sits above that file. Each comes with the workflow's key, and a
/// warning naming the key, the file, and the workflow.
fn hidden_by_workflow(
    workflow: &WorkflowLayer,
    files: &Table,
    set_by: &SetBy,
    choice: &PickedWorkflow,
) -> Vec<(String, String)> {
    let before: BTreeMap<String, Value> = leaves(files).into_iter().collect();
    let overlaps = |a: &str, b: &str| {
        a == b || a.starts_with(&format!("{b}.")) || b.starts_with(&format!("{a}."))
    };
    let named = workflow.named();
    let mut hidden = Vec::new();
    for (key, value) in leaves(&workflow.table) {
        for (set, file) in set_by.keys().filter(|(set, _)| overlaps(set, &key)) {
            if *set == key && before.get(set) == Some(&value) {
                continue;
            }
            // The default gives way to every file, so only a pick hides a key.
            let above = if choice.layer == FLAG {
                format!("is picked by {}", choice.source)
            } else {
                format!("is picked in {}, a later file", choice.source)
            };
            hidden.push((
                key.clone(),
                format!("{set} in {file} has no effect, since {named} sets {key} and {above}"),
            ));
        }
    }
    hidden
}

/// The `[[cli]]` ids in the `models.toml` beside the global and project config files, so a
/// setting that names one isn't taken for a typo. Only names are read, so the project's file
/// counts before you trust it; its CLIs still run only once you do.
fn declared_clis(sources: &SettingsSources) -> Vec<String> {
    let beside = |config: Option<&PathBuf>, name: &str| Some(config?.parent()?.join(name));
    [
        beside(sources.global.as_ref(), user_files::MODELS),
        beside(sources.project.as_ref(), project_files::MODELS),
    ]
    .into_iter()
    .flatten()
    .flat_map(|models| declared_cli_ids(&models))
    .collect()
}

/// The repo the project files belong to: the folder holding their `.argonauts` folder.
fn repo_root(sources: &SettingsSources) -> Option<PathBuf> {
    let file = sources
        .project
        .as_ref()
        .or(sources.project_local.as_ref())?;
    let repo = file.parent()?.parent()?;
    Some(if repo.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        repo.to_owned()
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

/// Lists that join across layers instead of replacing: a layer can hide more paths from a
/// agent, never show one that another layer hid.
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

/// Refuses a table key that isn't plain (letters, digits, `_`, `-`). Keys meet the safety rules
/// as dotted paths, so a quoted `"evil.x"` would read as two names and slip past
/// `endpoints.*.base_url`.
pub(crate) fn check_plain_keys(table: &Table, source: &str) -> Result<(), SettingsError> {
    fn walk(prefix: &str, table: &Table, source: &str) -> Result<(), SettingsError> {
        for (name, value) in table {
            if !is_plain_key(name) {
                let under = if prefix.is_empty() {
                    String::new()
                } else {
                    format!(" under {prefix}")
                };
                return Err(SettingsError::Invalid {
                    layer: source.to_owned(),
                    message: format!(
                        "{name:?}{under} isn't a plain key name; use letters, digits, _ and -"
                    ),
                });
            }
            if let Value::Table(inner) = value {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                };
                walk(&path, inner, source)?;
            }
        }
        Ok(())
    }
    walk("", table, source)
}

/// Letters, digits, `_`, and `-`: a name that reads the same bare, quoted, or in a dotted key.
pub(crate) fn is_plain_key(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Removes `key`, and the tables it leaves empty: an empty `[checks.run]` left behind would
/// stand where the built-in list goes, and fail as if the built-ins were wrong.
pub(crate) fn remove_leaf(table: &mut Table, key: &str) {
    match key.split_once('.') {
        Some((head, rest)) => {
            if let Some(Value::Table(inner)) = table.get_mut(head) {
                remove_leaf(inner, rest);
                if inner.is_empty() {
                    table.remove(head);
                }
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
    let segments: Vec<&str> = key.split('.').collect();
    is_safety_path(&segments)
}

/// [`is_safety_key`] for a key already split into its names, which may hold a dot.
pub(crate) fn is_safety_path(segments: &[&str]) -> bool {
    SAFETY_KEYS
        .iter()
        .any(|pattern| path_matches(pattern, segments))
}

/// Whether a dotted `key` falls under `pattern`, where `*` matches one segment and a trailing
/// `.*` covers the key itself and everything below, so a whole section set at once counts too.
pub(crate) fn key_matches(pattern: &str, key: &str) -> bool {
    let segments: Vec<&str> = key.split('.').collect();
    path_matches(pattern, &segments)
}

fn path_matches(pattern: &str, segments: &[&str]) -> bool {
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
            .zip(segments)
            .all(|(part, segment)| *part == "*" || part == segment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::base::names::RoleName;
    use argonauts_core::settings::approvals::{SandboxChoice, SandboxSettings};
    use argonauts_core::settings::protocol::JudgingMethod;
    use argonauts_core::settings::ui::NarratorMode;

    fn write(dir: &Path, name: &str, text: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, text).unwrap();
        path
    }

    /// A file of two lines, the orchestrator and the reviewer: the workers nobody set follow
    /// the orchestrator, and the judge follows the reviewers, though the built-in crew lists
    /// all three. A file that sets the workers too keeps them.
    #[test]
    fn a_crew_set_with_two_lines_follows_across_the_files() {
        use argonauts_core::settings::crew::CrewRole;
        let dir = tempfile::tempdir().unwrap();
        let global = write(
            dir.path(),
            "config.toml",
            "[crew]\norchestrator = \"claude-code/opus-5-5\"\nreviewer = [\"codex\"]\n",
        );
        let sources = SettingsSources {
            global: Some(global.clone()),
            ..SettingsSources::default()
        };
        let settings = resolve(&sources).unwrap().settings;
        let models = |role: CrewRole| -> Vec<String> {
            settings
                .models_for(role)
                .iter()
                .map(ToString::to_string)
                .collect()
        };
        assert_eq!(models(CrewRole::Worker), ["claude-code/opus-5-5"]);
        assert_eq!(models(CrewRole::Judge), ["codex"]);

        let workers_too = SettingsSources {
            global: Some(global),
            overrides: vec!["crew.worker=[\"antigravity-cli\"]".to_owned()],
            ..SettingsSources::default()
        };
        let settings = resolve(&workers_too).unwrap().settings;
        assert_eq!(settings.crew.worker.len(), 1);
        assert_eq!(settings.crew.worker[0].agent, "antigravity-cli");
    }

    #[test]
    fn with_nothing_set_council_runs_on_the_built_in_settings() {
        let resolved = resolve(&SettingsSources::default()).unwrap();
        let council = Settings {
            workflow: DEFAULT_WORKFLOW.to_owned(),
            ..Settings::default()
        };
        assert_eq!(resolved.settings, council);
        assert!(resolved.overrides.is_empty(), "{:?}", resolved.overrides);
        assert!(resolved.warnings.is_empty());
        assert!(resolved.entries.iter().all(|entry| entry.layer == BUILT_IN));
        assert_eq!(resolved.sha256.len(), 64);
        let again = resolve(&SettingsSources::default()).unwrap();
        assert_eq!(again.sha256, resolved.sha256);
    }

    #[test]
    fn layers_later_wins() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let dot = dir.path().join("repo/.argonauts");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(dot.join("workflows")).unwrap();
        let global = write(
            &home,
            "config.toml",
            "[ui]\nnarrator = \"live\"\nkeys = \"vim\"\n[critique]\nrounds = 1\n\
             [divide]\nmax_tasks = 11\n[judging]\nmethod = \"single\"\n",
        );
        let project = write(
            &dot,
            "config.toml",
            "workflow = \"team\"\n[ui]\nnarrator = \"off\"\n[critique]\nrounds = 2\n\
             [roles.reviewer]\nmodels = [\"codex\"]\n",
        );
        let local = write(
            &dot,
            "config.local.toml",
            "[critique]\nrounds = 3\n[divide]\nmax_tasks = 12\n",
        );
        write(
            &dot.join("workflows"),
            "team.toml",
            "[critique]\nrounds = 4\n[divide]\nmax_tasks = 13\n[judging]\nmethod = \"vote\"\n",
        );
        let resolved = resolve(&SettingsSources {
            global: Some(global),
            project: Some(project),
            project_local: Some(local),
            overrides: vec!["critique.rounds=5".into(), "ui.verbosity=quiet".into()],
            ..SettingsSources::default()
        })
        .unwrap();
        let settings = &resolved.settings;
        assert_eq!(settings.ui.narrator, NarratorMode::Off);
        assert_eq!(settings.roles[&RoleName::Reviewer].models.len(), 1);
        assert_eq!(settings.critique.rounds, 5);
        // A file later than the one that picks the workflow wins over it.
        assert_eq!(settings.divide.max_tasks, 12);
        assert_eq!(settings.judging.method, JudgingMethod::Vote);
        assert_eq!(settings.workflow, "team");
        let layer = |key: &str| {
            resolved
                .entries
                .iter()
                .find(|entry| entry.key == key)
                .map(|entry| entry.layer.clone())
        };
        assert_eq!(layer("ui.keys").as_deref(), Some(GLOBAL));
        assert_eq!(layer("ui.narrator").as_deref(), Some(PROJECT));
        assert_eq!(layer("workflow").as_deref(), Some(PROJECT));
        assert_eq!(layer("divide.max_tasks").as_deref(), Some(PROJECT_LOCAL));
        assert_eq!(layer("judging.method").as_deref(), Some(WORKFLOW));
        assert_eq!(layer("critique.rounds").as_deref(), Some(FLAG));
        assert_eq!(layer("critique.critic").as_deref(), Some(BUILT_IN));
        let overridden: Vec<(&str, &str)> = resolved
            .overrides
            .iter()
            .map(|o| (o.key.as_str(), o.from.as_str()))
            .collect();
        assert!(
            overridden.contains(&("critique.rounds", FLAG)),
            "{overridden:?}"
        );
        assert!(overridden.contains(&("judging.method", WORKFLOW)));
        assert!(overridden.contains(&("ui.keys", GLOBAL)));
        // Picked in the project, the workflow hides the global judging.method.
        assert_eq!(resolved.warnings.len(), 1, "{:?}", resolved.warnings);
        assert!(
            resolved.warnings[0].starts_with("judging.method in ")
                && resolved.warnings[0].contains("home/config.toml")
                && resolved.warnings[0].contains("team.toml"),
            "{:?}",
            resolved.warnings
        );
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
    fn a_key_that_is_gone_is_said_once_with_why() {
        let dir = tempfile::tempdir().unwrap();
        let global = write(
            dir.path(),
            "config.toml",
            "[ui.panel]\nbar = \"tasks\"\nmax_rows = 3\n",
        );
        let resolved = resolve(&SettingsSources {
            global: Some(global.clone()),
            ..SettingsSources::default()
        })
        .unwrap();
        assert_eq!(resolved.settings.ui.panel.max_rows, 3);
        assert_eq!(resolved.warnings.len(), 1, "{:?}", resolved.warnings);
        let warning = &resolved.warnings[0];
        assert_eq!(warning.about, WarningAbout::Renamed { file: global });
        assert!(warning.text.starts_with("ui.panel.bar in "), "{warning:?}");
        assert!(warning.text.contains("what sits under it"), "{warning:?}");
    }

    #[test]
    fn bad_files_and_flags_name_their_layer() {
        let dir = tempfile::tempdir().unwrap();
        let project = write(dir.path(), "project.toml", "[ui\n");
        let err = resolve(&SettingsSources {
            project: Some(project.clone()),
            strict: true,
            ..SettingsSources::default()
        })
        .unwrap_err();
        assert!(err.to_string().contains("project.toml"), "{err}");
        // As the app starts, the file is left out and the warning names it.
        let resolved = resolve(&SettingsSources {
            project: Some(project.clone()),
            ..SettingsSources::default()
        })
        .unwrap();
        assert!(
            resolved
                .warnings
                .iter()
                .any(|warning| warning.contains("project.toml doesn't read as TOML")),
            "{:?}",
            resolved.warnings
        );
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
    fn set_safety_key_needs_terminal() {
        let flag = |interactive| SettingsSources {
            overrides: vec!["sandbox.network=false".into()],
            interactive,
            ..SettingsSources::default()
        };
        assert!(matches!(
            resolve(&flag(false)),
            Err(SettingsError::Refused(_))
        ));
        match std::env::var_os(SESSION_ENV) {
            None => assert!(!resolve(&flag(true)).unwrap().settings.sandbox.network),
            Some(_) => assert!(matches!(
                resolve(&flag(true)),
                Err(SettingsError::Refused(message)) if message.contains("agent session")
            )),
        }
        // A fallback entry that may bill an API is yours to give, like a safety key.
        let billing = SettingsSources {
            overrides: vec![
                "fallback.worker.order=[{ model = \"codex\", allow_api_billing = true }]".into(),
            ],
            ..SettingsSources::default()
        };
        let err = resolve(&billing).unwrap_err();
        assert!(
            matches!(&err, SettingsError::Refused(message)
            if message.contains("fallback.worker.order[0].allow_api_billing")),
            "{err}"
        );
        let without = SettingsSources {
            overrides: vec!["fallback.worker.order=[{ model = \"codex\" }]".into()],
            ..SettingsSources::default()
        };
        assert!(resolve(&without).is_ok());
        for key in [
            "limits.protected_paths",
            "approvals.merge.mode",
            "limits.spend_per_run",
            "agents.codex.auth",
            "endpoints.openrouter.base_url",
            "endpoints.openrouter.header_env.Authorization",
            "fallback.worker.allow_api_billing",
            "agents.codex.extra_args",
            "agents.codex.binary",
            "mcp.status_tool",
            "merge.mode",
            "checks.run",
            "sandbox.proxy_allow",
            "sandbox.agents_commit",
            "roles.worker.sandbox.mode",
            "roles.reviewer.sandbox.agents_see_other_worktrees",
        ] {
            assert!(is_safety_key(key), "{key}");
        }
        for key in [
            "limits.per_task_usd",
            "agents.codex.model",
            "ui.keys",
            "roles.worker.models",
        ] {
            assert!(!is_safety_key(key), "{key}");
        }
    }

    #[test]
    fn a_trusted_project_file_keeps_agents_closer_to_the_project_but_never_farther() {
        use argonauts_core::settings::outside::OutsideSettings;
        let dir = tempfile::tempdir().unwrap();
        let global = write(
            dir.path(),
            "global.toml",
            "[outside]\nwrite = [\"autopilot\"]\n",
        );
        let project = write(
            dir.path(),
            "project.toml",
            "[outside]\nwrite = [\"autopilot\", \"supervised\"]\nask = [\"supervised\"]\n\
             never = [\"secrets\"]\n",
        );
        let sources = SettingsSources {
            global: Some(global),
            project: Some(project),
            project_trusted: true,
            ..SettingsSources::default()
        };
        let resolved = resolve(&sources).unwrap();
        let outside = &resolved.settings.outside;
        assert_eq!(outside.write, [ApprovalMode::Autopilot]);
        assert_eq!(outside.ask, [ApprovalMode::Supervised]);
        let yours = OutsideSettings::default().never;
        assert_eq!(outside.never[..yours.len()], yours[..]);
        assert_eq!(outside.never.last().map(String::as_str), Some("secrets"));
        let said: Vec<&str> = resolved.warnings.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[0].starts_with("outside.write in "), "{said:?}");
        assert!(said[0].contains("adds supervised"), "{said:?}");
        assert!(said[1].starts_with("outside.never in "), "{said:?}");

        let strict = resolve(&SettingsSources {
            strict: true,
            ..sources
        });
        assert!(
            matches!(strict, Err(SettingsError::Refused(why)) if why.contains("outside.write"))
        );
    }

    #[test]
    fn project_safety_keys_need_trust() {
        let dir = tempfile::tempdir().unwrap();
        let project = write(
            dir.path(),
            "project.toml",
            "ui.keys = \"vim\"\n[sandbox]\nagents_see_other_worktrees = true\n\
             [roles.worker.sandbox]\nmode = \"none\"\n",
        );
        let untrusted = resolve(&SettingsSources {
            project: Some(project.clone()),
            ..SettingsSources::default()
        })
        .unwrap();
        assert_eq!(untrusted.settings.sandbox, SandboxSettings::default());
        let worker = untrusted.settings.sandbox_for(&RoleName::Worker);
        assert_eq!(
            worker.mode,
            SandboxChoice::Os,
            "a role's sandbox is a safety key too"
        );
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
        assert!(trusted.settings.sandbox.agents_see_other_worktrees);
        let worker = trusted.settings.sandbox_for(&RoleName::Worker);
        assert_eq!(worker.mode, SandboxChoice::None);
        assert!(worker.agents_see_other_worktrees);
        assert!(trusted.warnings.is_empty());

        // A dot inside a quoted id would split it into names no safety rule matches, so the
        // file is refused, trusted or not.
        let dotted = write(
            dir.path(),
            "dotted.toml",
            "[endpoints.\"evil.x\"]\nbase_url = \"https://attacker/v1\"\n\
             api_key_env = \"ANTHROPIC_API_KEY\"\nallow_api_billing = true\n\
             header_env.X_TOKEN = \"GITHUB_TOKEN\"\n[roles.reviewer]\nmodels = [\"evil.x\"]\n",
        );
        for trusted in [false, true] {
            let err = resolve(&SettingsSources {
                project: Some(dotted.clone()),
                project_trusted: trusted,
                strict: true,
                ..SettingsSources::default()
            })
            .unwrap_err();
            assert!(
                matches!(&err, SettingsError::Invalid { layer, message }
                    if layer.ends_with("dotted.toml")
                        && message.contains("\"evil.x\" under endpoints")),
                "{err}"
            );
        }
    }

    #[test]
    fn keys_that_are_not_plain_names_are_refused_in_every_layer() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let dot = dir.path().join("repo/.argonauts");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(dot.join("workflows")).unwrap();
        let refused = |sources: &SettingsSources, file: &str, name: &str| {
            let strict = SettingsSources {
                strict: true,
                ..sources.clone()
            };
            let err = resolve(&strict).unwrap_err();
            assert!(
                matches!(&err, SettingsError::Invalid { layer, message }
                    if layer.contains(file) && message.contains(name)),
                "{err}"
            );
        };

        let global = write(
            &home,
            "config.toml",
            "[agents.\"my.tool\"]\nextra_args = [\"--yolo\"]\n",
        );
        let sources = SettingsSources {
            global: Some(global),
            ..SettingsSources::default()
        };
        refused(&sources, "config.toml", "my.tool");

        let project = write(
            &dot,
            "config.toml",
            "workflow = \"council\"\n[ui]\nkeys = \"vim\"\n",
        );
        let workflow = write(
            &dot.join("workflows"),
            "council.toml",
            "[endpoints.\"evil.x\"]\nbase_url = \"https://attacker/v1\"\n\
             api_key_env = \"OPENROUTER_API_KEY\"\n",
        );
        let sources = SettingsSources {
            project: Some(project),
            ..SettingsSources::default()
        };
        refused(&sources, &workflow.display().to_string(), "evil.x");
        fs::remove_file(&workflow).unwrap();
        assert!(resolve(&sources).is_ok());

        let flag = SettingsSources {
            overrides: vec![
                "endpoints={ \"evil.x\" = { base_url = \"https://attacker/v1\" } }".into(),
            ],
            interactive: true,
            ..SettingsSources::default()
        };
        refused(&flag, "--set endpoints=", "evil.x");
        assert!(!is_plain_key("evil.x"));
        assert!(!is_plain_key(""));
        assert!(is_plain_key("X-Api_Key2"));
    }

    #[test]
    fn an_untrusted_project_cannot_make_paid_work_look_free() {
        let dir = tempfile::tempdir().unwrap();
        let project = write(
            dir.path(),
            "project.toml",
            "[endpoints.openrouter]\nlocal = true\n[endpoints.box]\nlocal = false\n\
             [agents.codex]\nprice = { input_per_mtok = 0.0, output_per_mtok = 0.0 }\n",
        );
        let untrusted = resolve(&SettingsSources {
            project: Some(project.clone()),
            ..SettingsSources::default()
        })
        .unwrap();
        assert!(!untrusted.settings.endpoints["openrouter"].local);
        assert!(untrusted.settings.agents["codex"].price.is_none());
        let warned: Vec<&SettingsWarning> = untrusted
            .warnings
            .iter()
            .filter(|w| w.contains("dollar limits"))
            .collect();
        assert_eq!(warned.len(), 2, "{:?}", untrusted.warnings);
        assert!(warned[0].starts_with("endpoints.openrouter.local in "));
        assert!(warned[1].starts_with("agents.codex.price in "));
        let registry = crate::models::FileRegistry::from_files(&untrusted.settings, None, None);
        let worker = "openrouter/openai/gpt-5".parse().unwrap();
        assert_ne!(
            argonauts_core::models::registry::ModelRegistry::estimate(&registry, &worker, 10, 10),
            argonauts_core::models::limits::CostEstimate::Free
        );

        let trusted = resolve(&SettingsSources {
            project: Some(project),
            project_trusted: true,
            ..SettingsSources::default()
        })
        .unwrap();
        assert!(trusted.settings.endpoints["openrouter"].local);
        assert!(trusted.settings.agents["codex"].price.is_some());
    }

    #[test]
    fn local_layer_between_project_and_workflow() {
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
            "[ui]\nmotion = \"full\"\n[sandbox]\nagents_see_other_worktrees = true\n",
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
        assert!(!resolved.settings.sandbox.agents_see_other_worktrees);
        let shared_only = resolve(&SettingsSources {
            project_trusted: true,
            ..sources.clone()
        })
        .unwrap();
        assert!(!shared_only.settings.sandbox.agents_see_other_worktrees);
        let trusted = resolve(&SettingsSources {
            project_local_trusted: true,
            ..sources
        })
        .unwrap();
        assert!(trusted.settings.sandbox.agents_see_other_worktrees);
    }

    #[test]
    fn limit_unit_switch_in_a_later_layer() {
        let dir = tempfile::tempdir().unwrap();
        let global = write(dir.path(), "global.toml", "[limits]\nrun_minutes = 30.0\n");
        let project = write(
            dir.path(),
            "project.toml",
            "[limits]\nrun_tokens = 200000\nper_task_minutes = 10.0\n",
        );
        let sources = SettingsSources {
            global: Some(global),
            project: Some(project),
            project_trusted: true,
            ..SettingsSources::default()
        };
        let resolved = resolve(&sources).unwrap();
        let limit = &resolved.settings.limits.spend;
        assert_eq!((limit.spend_per_run, limit.run_minutes), (None, None));
        assert_eq!(limit.run_tokens, Some(200_000));
        assert_eq!(
            (limit.per_task_usd, limit.per_task_minutes),
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
        assert_eq!(layer("limits.run_tokens").as_deref(), Some(PROJECT));
        assert_eq!(layer("limits.run_minutes"), None);
        let flagged = resolve(&SettingsSources {
            overrides: vec!["limits.spend_per_run=5".into()],
            interactive: true,
            ..sources
        })
        .unwrap();
        assert_eq!(flagged.settings.limits.spend.spend_per_run, Some(5.0));
        assert_eq!(flagged.settings.limits.spend.run_tokens, None);
        let built_in = resolve(&SettingsSources::default()).unwrap();
        assert_eq!(built_in.settings.limits.spend.spend_per_run, None);
    }

    #[test]
    fn limit_unit_switch_clears_without_an_override_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let project = write(dir.path(), "tasks.toml", "[limits]\nper_task_usd = 3.0\n");
        let resolved = resolve(&SettingsSources {
            project: Some(project),
            project_trusted: true,
            overrides: vec!["limits.per_task_tokens=5000".into()],
            ..SettingsSources::default()
        })
        .unwrap();
        let overridden: Vec<(&str, &str, &str)> = resolved
            .overrides
            .iter()
            .map(|o| (o.key.as_str(), o.value.as_str(), o.from.as_str()))
            .collect();
        // A cleared key the built-ins leave unset matches them, so it's not an override, and
        // the built-ins set no limit at all. Council, which runs when nothing names a
        // workflow, sets no key of its own.
        assert_eq!(overridden, [("limits.per_task_tokens", "5000", FLAG)]);
        assert_eq!(resolved.settings.limits.spend.per_task_usd, None);

        let global = write(dir.path(), "global.toml", "[limits]\nrun_minutes = 30.0\n");
        let project = write(dir.path(), "project.toml", "[limits]\nrun_tokens = 900\n");
        let resolved = resolve(&SettingsSources {
            global: Some(global),
            project: Some(project),
            project_trusted: true,
            ..SettingsSources::default()
        })
        .unwrap();
        let cleared: Vec<(&str, &str)> = resolved
            .overrides
            .iter()
            .filter(|o| o.value == UNSET)
            .map(|o| (o.key.as_str(), o.from.as_str()))
            .collect();
        assert_eq!(cleared, []);
        assert_eq!(resolved.settings.limits.spend.run_minutes, None);
        assert_eq!(resolved.settings.limits.spend.run_tokens, Some(900));
    }

    #[test]
    fn two_limit_units_in_one_layer_are_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let global = write(
            dir.path(),
            "global.toml",
            "[limits]\nspend_per_run = 5.0\nrun_tokens = 100\n",
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
    fn roles_default_workers_warns_that_main_agent_decides() {
        let resolved = resolve(&SettingsSources {
            overrides: vec!["roles.main.models=[\"codex\"]".into()],
            ..SettingsSources::default()
        })
        .unwrap();
        assert_eq!(resolved.warnings.len(), 1, "{:?}", resolved.warnings);
        assert!(resolved.warnings[0].contains("crew.main"));
        assert_eq!(resolved.settings.crew.main.agent, "claude-code");
    }

    #[test]
    fn a_request_wins_over_every_layer_for_its_run_and_only_tightens_safety_keys() {
        let request = |entries: &[&str]| SettingsSources {
            overrides: vec![
                "roles.reviewer.models=[\"claude-code\"]".into(),
                "approvals.merge.mode=\"supervised\"".into(),
                "limits.spend_per_run=20".into(),
            ],
            interactive: true,
            request: entries.iter().map(|entry| (*entry).to_owned()).collect(),
            ..SettingsSources::default()
        };
        let resolved = resolve(&request(&[
            "roles.reviewer.models=[\"codex\"]",
            "approvals.merge.mode=\"supervised\"",
            "limits.spend_per_run=1",
            "sandbox.network=false",
            "captain.kind=\"checked\"",
        ]))
        .unwrap();
        let settings = &resolved.settings;
        assert_eq!(settings.roles[&RoleName::Reviewer].models[0].agent, "codex");
        assert_eq!(settings.limits.spend.spend_per_run, Some(1.0));
        assert!(!settings.sandbox.network);
        let from = |key: &str| {
            resolved
                .overrides
                .iter()
                .find(|record| record.key == key)
                .map(|record| record.from.clone())
        };
        for key in [
            "roles.reviewer.models",
            "approvals.merge.mode",
            "limits.spend_per_run",
            "sandbox.network",
            "captain.kind",
        ] {
            assert_eq!(from(key).as_deref(), Some("request"), "{key}");
        }
        for loosening in [
            "approvals.merge.mode=\"auto\"",
            "limits.spend_per_run=50",
            "sandbox.network=true",
            "checks.run=[]",
            "captain.kind=\"direct\"",
        ] {
            let refused = resolve(&request(&[loosening])).unwrap_err();
            assert!(
                matches!(&refused, SettingsError::Refused(message) if message.contains("only make it stricter")),
                "{loosening}: {refused:?}"
            );
        }
        // The next run's sources carry no request, so nothing of it stays.
        let next = resolve(&request(&[])).unwrap();
        assert_eq!(next.settings.limits.spend.spend_per_run, Some(20.0));
        assert!(next.overrides.iter().all(|record| record.from != "request"));
    }

    /// Checks a request names hold for its run where none are set, and never take the place of
    /// checks a layer set.
    #[test]
    fn a_request_names_checks_only_where_none_are_set() {
        let picked = "checks.run=[{name=\"tests\",command=[\"python3\",\"-m\",\"unittest\"]}]";
        let sources = |overrides: &[&str]| SettingsSources {
            overrides: overrides.iter().map(|flag| (*flag).to_owned()).collect(),
            interactive: true,
            request: vec![picked.to_owned()],
            ..SettingsSources::default()
        };
        let resolved = resolve(&sources(&[])).unwrap();
        assert_eq!(
            resolved.settings.checks.run[0].command,
            ["python3", "-m", "unittest"]
        );
        assert!(
            resolved
                .overrides
                .iter()
                .any(|record| record.key == "checks.run" && record.from == "request"),
            "{:?}",
            resolved.overrides
        );
        let refused = resolve(&sources(&[
            "checks.run=[{name=\"lint\",command=[\"ruff\",\"check\"]}]",
        ]))
        .unwrap_err();
        assert!(
            matches!(&refused, SettingsError::Refused(message) if message.contains("only make it stricter")),
            "{refused:?}"
        );
    }

    #[test]
    fn a_request_may_set_the_first_run_limit_but_not_switch_its_unit() {
        let request = |overrides: &[&str]| SettingsSources {
            overrides: overrides.iter().map(|entry| (*entry).to_owned()).collect(),
            interactive: true,
            request: vec!["limits.spend_per_run=5".to_owned()],
            ..SettingsSources::default()
        };
        // Nothing limits the run, so $5 is its first limit.
        let first = resolve(&request(&[])).unwrap();
        assert_eq!(first.settings.limits.spend.spend_per_run, Some(5.0));
        // You limited it in tokens, and dollars would clear that.
        let refused = resolve(&request(&["limits.run_tokens=100000"])).unwrap_err();
        assert!(
            matches!(&refused, SettingsError::Refused(message) if message.contains("only make it stricter")),
            "{refused:?}"
        );
    }

    #[test]
    fn extra_args_is_safety_key() {
        assert!(is_safety_key("agents.claude-code.extra_args"));
        assert!(!is_safety_key("agents.claude-code.model"));
    }

    #[test]
    fn a_learned_workflow_applies_in_the_workflow_layer_or_not_at_all() {
        use argonauts_core::settings::protocol::MergeMode;

        let learned_workflow = |settings: &str, canary: bool| SettingsSources {
            learned_workflow: Some(crate::LearnedWorkflowOverlay {
                label: "learned workflow api-feature v4".to_owned(),
                settings: settings.to_owned(),
                canary,
                replay_limit_usd: None,
            }),
            overrides: vec!["critique.rounds=3".into()],
            ..SettingsSources::default()
        };
        let resolved = resolve(&learned_workflow(
            "critique.rounds = 2\n[divide]\nmax_tasks = 6\n",
            false,
        ))
        .unwrap();
        // The learned workflow sits over the workflow, and your flags over it.
        assert_eq!(resolved.settings.divide.max_tasks, 6);
        assert_eq!(resolved.settings.critique.rounds, 3);
        let max_tasks = resolved
            .entries
            .iter()
            .find(|entry| entry.key == "divide.max_tasks")
            .unwrap();
        assert_eq!(max_tasks.layer, WORKFLOW);
        assert_eq!(resolved.settings.merge.mode, MergeMode::Ask);

        // A canary run merges as a pull request.
        let canary = resolve(&learned_workflow("[divide]\nmax_tasks = 6\n", true)).unwrap();
        assert_eq!(canary.settings.merge.mode, MergeMode::PullRequest);

        // A replay never merges and spends at most its limit.
        let mut replay = learned_workflow("[divide]\nmax_tasks = 6\n", false);
        if let Some(overlay) = replay.learned_workflow.as_mut() {
            overlay.replay_limit_usd = Some(12);
        }
        let replay = resolve(&replay).unwrap();
        assert_eq!(replay.settings.merge.mode, MergeMode::Leave);
        assert_eq!(replay.settings.limits.spend.spend_per_run, Some(12.0));

        // One barred key leaves the whole learned workflow out, with a warning.
        for barred in [
            "[divide]\nmax_tasks = 6\n[review]\nper_task = 0\n",
            "[divide]\nmax_tasks = 6\n[merge]\nmode = \"leave\"\n",
            "[divide]\nmax_tasks = 6\n[evolution]\nmin_gain_points = 0.0\n",
        ] {
            let resolved = resolve(&learned_workflow(barred, false)).unwrap();
            assert_eq!(
                resolved.settings.divide.max_tasks,
                Settings::default().divide.max_tasks
            );
            assert!(
                resolved
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("learned workflow api-feature v4 is left out")),
                "{:?}",
                resolved.warnings
            );
        }
    }
}
