//! Writes one setting into a TOML file, keeping its comments, order, and formatting.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use argonauts_core::layout::project_files;
use argonauts_core::paths::{LOCAL_CONFIG_EXCLUDE, Paths, exclude_from_git};
use argonauts_core::settings::{GLOBAL_ONLY_KEYS, SettingMeta, Settings, project_may_tighten};
use toml_edit::{Decor, DocumentMut, InlineTable, Item, Key, Table, TableLike, Value};

use crate::SettingsError;
use crate::catalog::{fits, quote, setting_meta, settings_catalog};
use crate::resolve::{SESSION_ENV, is_plain_key, is_safety_path, key_matches};
use crate::settings::fallback::entry_billing;
use crate::settings::limit_units::{LimitUnits, other_units};
use crate::settings::plain::{one_of, syntax_message};
use crate::settings::validate::{file_problems, type_message};
use crate::workflows::{is_workflow_name, not_a_workflow_name, workflow_choices_in};

/// Sets one dotted key in a TOML file. A new key gets its catalog description as a comment, a
/// bare word reads as a string as with `--set`, and a table value merges key by key. Picking a
/// limit unit drops the file's other unit for that limit. Nothing is written when the file
/// would then fail to resolve on its own, when the key is unknown or takes other values, when a
/// project file would hold a key only your global config may set, or when a safety key would
/// change inside an agent session. A repo's `config.local.toml` is git-excluded before it's
/// written.
pub fn write_setting(file: &Path, key: &str, toml_value: &str) -> Result<(), SettingsError> {
    let in_agent_session = std::env::var_os(SESSION_ENV).is_some();
    write(file, key, toml_value, in_agent_session)
}

/// Removes one dotted key so the layer below shows through, and says whether it was there. The
/// key's comment goes with it, and so does a table left empty unless a comment sits on it.
/// Safety keys are refused inside an agent session.
pub fn remove_setting(file: &Path, key: &str) -> Result<bool, SettingsError> {
    let in_agent_session = std::env::var_os(SESSION_ENV).is_some();
    remove(file, key, in_agent_session)
}

/// Checks keys and their TOML values the way [`write_setting`] checks a file that holds only
/// them, and writes nothing. For a change nobody has placed yet, such as a setup the main
/// session proposes. `workflows` names the workflows there are besides the built-in ones.
pub fn check_settings(
    pairs: &[(String, String)],
    workflows: &[String],
) -> Result<(), SettingsError> {
    let invalid = |message: String| SettingsError::Invalid {
        layer: PROPOSED.to_owned(),
        message,
    };
    let mut doc = DocumentMut::new();
    for (key, toml_value) in pairs {
        let path = key_path(key).map_err(invalid)?;
        let key = &path.join(".");
        let value =
            parse_value(toml_value).map_err(|message| invalid(format!("{key}: {message}")))?;
        if let Some(odd) = odd_name(&value) {
            return Err(invalid(format!(
                "{odd:?} in {key}'s value isn't a plain key name; use letters, digits, _ and -"
            )));
        }
        check(key, &value, &|| workflows.to_vec()).map_err(invalid)?;
        set(doc.as_table_mut(), &path, value, None, false)
            .map_err(|message| invalid(format!("{key}: {message}")))?;
    }
    resolves_alone(Path::new(PROPOSED), &doc.to_string())
}

/// What [`check_settings`] calls the change in its errors.
const PROPOSED: &str = "the proposed setup";

fn write(
    file: &Path,
    key: &str,
    toml_value: &str,
    in_agent_session: bool,
) -> Result<(), SettingsError> {
    let invalid = |message: String| SettingsError::Invalid {
        layer: shown(file),
        message,
    };
    let path = key_path(key).map_err(invalid)?;
    let key = &path.join(".");
    let value = parse_value(toml_value).map_err(|message| invalid(format!("{key}: {message}")))?;
    if let Some(odd) = odd_name(&value) {
        return Err(invalid(format!(
            "{odd:?} in {key}'s value isn't a plain key name; use letters, digits, _ and -"
        )));
    }
    if in_agent_session && let Some(safety) = safety_keys(key, &value).first() {
        return Err(refused_in_agent_session(safety));
    }
    let leaves = leaves_of(key, &value);
    let global = Paths::from_env().ok().map(|paths| paths.global_config());
    if project_config_repo(file, global.as_deref()).is_some()
        && let Some(global_only) = leaves
            .iter()
            .find(|leaf| is_global_only(leaf) && !project_may_tighten(leaf, toml_value))
    {
        return Err(global_only_in_project(global_only, file));
    }
    check(key, &value, &|| workflow_names(file)).map_err(invalid)?;

    let target = real_path(file)?;
    let _turn = lock_folder(&target)?;
    let original = read(&target)?;
    let mut doc: DocumentMut = original
        .parse()
        .map_err(|err: toml_edit::TomlError| invalid(err.to_string()))?;
    // A file of nothing but comments keeps them at the top, where new keys would otherwise
    // push past them.
    let only_comments = doc.is_empty() && !original.trim().is_empty();
    if only_comments {
        doc = DocumentMut::new();
    }
    let comment = setting_meta(key)
        .map(|meta| meta.description)
        .filter(|description| !description.is_empty());
    set(doc.as_table_mut(), &path, value, comment.as_deref(), false)
        .map_err(|message| invalid(format!("{key}: {message}")))?;
    // Two units for one limit in one file is an error, so the unit written replaces the other.
    for other in leaves.iter().flat_map(|leaf| other_units(leaf)) {
        if !leaves.contains(&other) {
            let other: Vec<&str> = other.split('.').collect();
            remove_at(doc.as_table_mut(), &other);
        }
    }
    let mut text = doc.to_string();
    if only_comments {
        text = format!("{}\n\n{text}", original.trim_end());
    }
    resolves_alone(file, &text)?;

    // Exclude before writing, so your own settings are never briefly committable.
    if let Some(repo_root) = local_config_repo(file) {
        exclude_from_git(repo_root, LOCAL_CONFIG_EXCLUDE)
            .map_err(|err| io_error(&repo_root.join(".git/info/exclude"), err))?;
    }
    write_replacing(&target, &text)
}

fn remove(file: &Path, key: &str, in_agent_session: bool) -> Result<bool, SettingsError> {
    let invalid = |message: String| SettingsError::Invalid {
        layer: shown(file),
        message,
    };
    let path = key_path(key).map_err(invalid)?;
    let key = &path.join(".");
    let target = real_path(file)?;
    // No folder holds no file, and locking would create one.
    if matches!(folder_of(&target).try_exists(), Ok(false)) {
        return Ok(false);
    }
    let _turn = lock_folder(&target)?;
    let original = read(&target)?;
    let mut doc: DocumentMut = original
        .parse()
        .map_err(|err: toml_edit::TomlError| invalid(err.to_string()))?;
    let header = file_header(&doc, &path);
    let Some(removed) = remove_at(doc.as_table_mut(), &path) else {
        return Ok(false);
    };
    if let Some(header) = header {
        keep_header(&mut doc, &header);
    }
    if in_agent_session {
        let path: Vec<String> = path.iter().map(|name| (*name).to_owned()).collect();
        let mut keys = vec![path.clone()];
        keys.extend(leaf_paths(&path, &removed));
        if let Some(safety) = keys.iter().find(|path| {
            let names: Vec<&str> = path.iter().map(String::as_str).collect();
            is_safety_path(&names)
        }) {
            return Err(refused_in_agent_session(&safety.join(".")));
        }
    }
    let text = doc.to_string();
    toml::from_str::<Settings>(&text)
        .map_err(|err| invalid(format!("{key}: {}", err.message())))?;
    write_replacing(&target, &text)?;
    Ok(true)
}

/// Refuses a file that `resolve` would refuse on its own, with the message it would give. A
/// value of the wrong type never reaches the file.
pub(crate) fn resolves_alone(file: &Path, text: &str) -> Result<(), SettingsError> {
    let invalid = |message: String| SettingsError::Invalid {
        layer: shown(file),
        message,
    };
    let table: toml::Table = text
        .parse()
        .map_err(|err: toml::de::Error| invalid(syntax_message(text, &err)))?;
    let settings: Settings = toml::Value::Table(table.clone())
        .try_into()
        .map_err(|err: toml::de::Error| invalid(type_message(&err, &table).1))?;
    // The layer name only labels what a later unit clears, and nothing is cleared here.
    LimitUnits::default().note("file", &shown(file), &table)?;
    match file_problems(&settings).into_iter().next() {
        Some(problem) => Err(invalid(problem.message)),
        None => Ok(()),
    }
}

/// The dotted key of every value a write sets. A list is one value, as in `resolve`.
fn leaves_of(key: &str, value: &Value) -> Vec<String> {
    match value {
        Value::InlineTable(table) => table
            .iter()
            .flat_map(|(name, inner)| leaves_of(&format!("{key}.{name}"), inner))
            .collect(),
        _ => vec![key.to_owned()],
    }
}

fn is_global_only(key: &str) -> bool {
    GLOBAL_ONLY_KEYS
        .iter()
        .any(|pattern| key_matches(pattern, key))
}

/// The refusal `resolve` gives for the file once it holds `key`.
fn global_only_in_project(key: &str, file: &Path) -> SettingsError {
    SettingsError::Refused(format!(
        "{key} in {} can only be set in your global config, since it names a program for \
         Argonauts Code to run",
        shown(file)
    ))
}

/// A repo's settings file by its path from the repo, as `resolve` names it, and any other file
/// by its full path.
fn shown(file: &Path) -> String {
    let global = Paths::from_env().ok().map(|paths| paths.global_config());
    project_config_repo(file, global.as_deref())
        .and_then(|repo| file.strip_prefix(repo).ok())
        .unwrap_or(file)
        .display()
        .to_string()
}

fn remove_at(table: &mut dyn TableLike, path: &[&str]) -> Option<Item> {
    let (name, rest) = path.split_first()?;
    if rest.is_empty() {
        return table.remove(name).filter(|item| !item.is_none());
    }
    let (removed, now_empty) = {
        let item = table.get_mut(name)?;
        let commented = match item {
            Item::Table(child) => has_comment(child.decor()),
            _ => false,
        };
        let child = item.as_table_like_mut()?;
        let removed = remove_at(child, rest)?;
        (removed, child.is_empty() && !commented)
    };
    if now_empty {
        table.remove(name);
    }
    Some(removed)
}

/// The comment that opens the file, when `path` is the file's first key. It belongs to the file,
/// so it stays when the key goes; only the key's own description goes with it.
fn file_header(doc: &DocumentMut, path: &[&str]) -> Option<String> {
    let [name] = path else {
        return None;
    };
    let root = doc.as_table();
    let (first, item) = root.iter().next()?;
    if first != *name || !item.is_value() {
        return None;
    }
    let prefix = root.key(name)?.leaf_decor().prefix()?.as_str()?;
    let own = setting_meta(name)
        .map(|meta| format!("# {}", meta.description.replace(['\n', '\r'], " ")))
        .unwrap_or_default();
    let header: String = prefix
        .split_inclusive('\n')
        .filter(|line| line.trim_end() != own)
        .collect();
    header.contains('#').then_some(header)
}

/// Puts the file's opening comment above whatever now comes first: a key, or else a table.
fn keep_header(doc: &mut DocumentMut, header: &str) {
    let root = doc.as_table_mut();
    let first_key = root
        .iter()
        .find(|(_, item)| item.is_value())
        .map(|(name, _)| name.to_owned());
    if let Some(name) = first_key {
        if let Some(mut key) = root.key_mut(&name) {
            prepend(key.leaf_decor_mut(), header);
        }
        return;
    }
    let first_table = root
        .iter_mut()
        .filter_map(|(_, item)| item.as_table_mut())
        .filter(|table| !table.is_implicit())
        .min_by_key(|table| table.position());
    if let Some(table) = first_table {
        prepend(table.decor_mut(), header);
    }
}

fn prepend(decor: &mut Decor, text: &str) {
    let below = decor
        .prefix()
        .and_then(|prefix| prefix.as_str())
        .unwrap_or("");
    let joined = format!("{text}{below}");
    decor.set_prefix(joined);
}

fn has_comment(decor: &Decor) -> bool {
    decor
        .prefix()
        .and_then(|prefix| prefix.as_str())
        .is_some_and(|prefix| prefix.contains('#'))
}

/// Every key under `prefix` in `item`, each as its names, since a hand-written file may quote
/// a name that holds a dot.
fn leaf_paths(prefix: &[String], item: &Item) -> Vec<Vec<String>> {
    let Some(table) = item.as_table_like() else {
        return Vec::new();
    };
    table
        .iter()
        .flat_map(|(name, inner)| {
            let mut path = prefix.to_vec();
            path.push(name.to_owned());
            let mut paths = leaf_paths(&path, inner);
            paths.push(path);
            paths
        })
        .collect()
}

/// A dotted key's names. Each must be a plain TOML key (letters, digits, `_`, `-`), so a quoted
/// or odd name can't slip past the safety rules as a key they don't recognize.
fn key_path(key: &str) -> Result<Vec<&str>, String> {
    let path: Vec<&str> = key.split('.').map(str::trim).collect();
    if path.iter().any(|segment| segment.is_empty()) {
        return Err(format!("{key:?} needs a name between every dot"));
    }
    if let Some(odd) = path.iter().find(|segment| !is_plain_key(segment)) {
        return Err(format!(
            "{odd:?} in {key:?} isn't a plain key name; use letters, digits, _ and -"
        ));
    }
    Ok(path)
}

/// Refuses a key the catalog doesn't know, naming the nearest one it does, and a value outside
/// the key's allowed list, naming the values it takes. A table value is checked key by key.
/// `workflows` lists the workflows `workflow` may name beyond the built-ins.
fn check(key: &str, value: &Value, workflows: &dyn Fn() -> Vec<String>) -> Result<(), String> {
    let catalog = settings_catalog();
    let inner = catalog.iter().find(|meta| holds(&meta.key, key));
    if let (Value::InlineTable(table), Some(_)) = (value, inner) {
        for (name, value) in table.iter() {
            check(&format!("{key}.{name}"), value, workflows)?;
        }
        return Ok(());
    }
    match (setting_meta(key), inner) {
        (Some(meta), _) => {
            let mut allowed = meta.allowed;
            if key == "workflow" {
                if let Some(name) = value.as_str()
                    && !is_workflow_name(name)
                {
                    return Err(not_a_workflow_name(name));
                }
                for name in workflows() {
                    let name = quote(&name);
                    if !allowed.contains(&name) {
                        allowed.push(name);
                    }
                }
            }
            // A theme file's name can't be listed, so the type check decides it.
            if key == "ui.theme"
                && value
                    .as_str()
                    .is_some_and(|name| name.starts_with("custom:"))
            {
                return Ok(());
            }
            check_allowed(key, value, &allowed)
        }
        (None, Some(inner)) => Err(format!(
            "{key} is a table of settings, such as {}; set one of them",
            filled(&inner.key, key)
        )),
        (None, None) => Err(match nearest(key, &catalog) {
            Some(near) => format!("{key} isn't a setting. Did you mean {near}?"),
            None => format!("{key} isn't a setting"),
        }),
    }
}

fn check_allowed(key: &str, value: &Value, allowed: &[String]) -> Result<(), String> {
    // A value that can't be compared, such as a table, is left to the type check.
    let Some(given) = plain(&value.to_string()) else {
        return Ok(());
    };
    if allowed.is_empty()
        || allowed
            .iter()
            .any(|text| plain(text).as_ref() == Some(&given))
    {
        return Ok(());
    }
    Err(format!(
        "{key} can't be {}; it takes {}",
        value.to_string().trim(),
        one_of(allowed)
    ))
}

/// A TOML value as `toml` reads it, so `'auto'` and `"auto"` compare equal.
fn plain(text: &str) -> Option<toml::Value> {
    format!("value = {text}")
        .parse::<toml::Table>()
        .ok()?
        .remove("value")
}

/// Whether the catalog key `pattern` lies inside the table `key` names.
fn holds(pattern: &str, key: &str) -> bool {
    let (parts, names): (Vec<&str>, Vec<&str>) =
        (pattern.split('.').collect(), key.split('.').collect());
    parts.len() > names.len() && fits(&parts[..names.len()].join("."), key)
}

/// The catalog key `pattern` with each `*` filled by the name `key` has there.
fn filled(pattern: &str, key: &str) -> String {
    let names: Vec<&str> = key.split('.').collect();
    pattern
        .split('.')
        .enumerate()
        .map(|(at, part)| match (part, names.get(at)) {
            ("*", Some(name)) => *name,
            _ => part,
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// The known key, or table of keys, closest to `key` by edit distance. Ties go to the one the
/// catalog lists first, so picker keys win.
fn nearest(key: &str, catalog: &[SettingMeta]) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for meta in catalog {
        let full = filled(&meta.key, key);
        let parts: Vec<&str> = full.split('.').collect();
        for end in 1..=parts.len() {
            let candidate = parts[..end].join(".");
            let distance = edit_distance(key, &candidate);
            if best.as_ref().is_none_or(|(closest, _)| distance < *closest) {
                best = Some((distance, candidate));
            }
        }
    }
    best.map(|(_, near)| near)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, left) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, right) in b.iter().enumerate() {
            let replaced = diagonal + usize::from(left != *right);
            diagonal = row[j + 1];
            row[j + 1] = replaced.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[b.len()]
}

/// The workflows a file's `workflow` may name besides the built-ins: the files in the `workflows`
/// folder beside it and in your global one.
fn workflow_names(file: &Path) -> Vec<String> {
    let beside = folder_of(file).join("workflows");
    let global = Paths::from_env().map(|paths| paths.global_workflows());
    let all = match &global {
        Ok(global) => workflow_choices_in(global, Some(&beside)),
        Err(_) => workflow_choices_in(&beside, None),
    };
    all.into_iter().map(|workflow| workflow.name).collect()
}

fn refused_in_agent_session(safety: &str) -> SettingsError {
    SettingsError::Refused(format!(
        "{safety} is a safety setting and can't be changed from inside an agent session"
    ))
}

/// Every safety key that writing `toml_value` at `key` would change, including keys inside a
/// table value such as `endpoints.ollama = { base_url = "..." }`. The confirm question names them.
pub fn safety_keys_in(key: &str, toml_value: &str) -> Vec<String> {
    let key: Vec<&str> = key.split('.').map(str::trim).collect();
    let value = parse_value(toml_value).unwrap_or_else(|_| Value::from(toml_value));
    safety_keys(&key.join("."), &value)
}

/// [`safety_keys_in`] for a key as it will be written, so spaces around a dot can't dodge the
/// rule. Names inside the value are matched one by one, so a quoted name with a dot can't
/// either. A fallback entry's `allow_api_billing` counts, as it does for `--set`.
fn safety_keys(key: &str, value: &Value) -> Vec<String> {
    let mut found = safety_leaves(key, value);
    found.extend(entry_billing(&as_layer(key, value)));
    found
}

fn safety_leaves(key: &str, value: &Value) -> Vec<String> {
    fn walk<'a>(path: &mut Vec<&'a str>, value: &'a Value, found: &mut Vec<String>) {
        if is_safety_path(path) {
            found.push(path.join("."));
        }
        if let Value::InlineTable(table) = value {
            for (name, inner) in table.iter() {
                path.push(name);
                walk(path, inner, found);
                path.pop();
            }
        }
    }
    let mut path: Vec<&str> = key.split('.').collect();
    let mut found = Vec::new();
    walk(&mut path, value, &mut found);
    found
}

/// `value` nested at its dotted `key`, as a layer that sets only it.
fn as_layer(key: &str, value: &Value) -> toml::Table {
    let mut layer = plain(&value.to_string());
    for name in key.rsplit('.') {
        let mut table = toml::Table::new();
        table.extend(layer.map(|inner| (name.to_owned(), inner)));
        layer = Some(toml::Value::Table(table));
    }
    match layer {
        Some(toml::Value::Table(table)) => table,
        _ => toml::Table::new(),
    }
}

/// The first name inside a table value that isn't a plain key name, which the settings loader
/// would refuse.
fn odd_name(value: &Value) -> Option<String> {
    let Value::InlineTable(table) = value else {
        return None;
    };
    table.iter().find_map(|(name, inner)| {
        if is_plain_key(name) {
            odd_name(inner)
        } else {
            Some(name.to_owned())
        }
    })
}

/// A bare word that isn't TOML reads as a string, as with `--set`, so `vim` works without quotes.
fn parse_value(text: &str) -> Result<Value, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("needs a value".to_owned());
    }
    if let Ok(doc) = format!("value = {text}").parse::<DocumentMut>() {
        if doc.len() != 1 {
            return Err(format!("{text:?} is more than one value"));
        }
        if let Some(value) = doc.get("value").and_then(Item::as_value) {
            let mut value = value.clone();
            value.decor_mut().clear();
            return Ok(value);
        }
    }
    if text.contains(['\n', '\r']) {
        return Err("a value is TOML text or a single word".to_owned());
    }
    Ok(Value::from(text))
}

/// Creates tables on the way. A new key gets `comment` on the line above, where one can go.
fn set(
    table: &mut dyn TableLike,
    path: &[&str],
    value: Value,
    comment: Option<&str>,
    inline: bool,
) -> Result<(), String> {
    let Some((name, rest)) = path.split_first() else {
        return Err("the key is empty".to_owned());
    };
    if rest.is_empty() {
        // A table merges key by key, as the layers do, so a write never drops keys the file
        // already holds, safety keys among them.
        if let Value::InlineTable(entries) = &value
            && let Some(item) = table.get_mut(name).filter(|item| item.is_table_like())
        {
            let inline = matches!(item, Item::Value(Value::InlineTable(_)));
            let child = item
                .as_table_like_mut()
                .ok_or_else(|| format!("{name} isn't a table"))?;
            for (inner, inner_value) in entries.iter() {
                set(child, &[inner], inner_value.clone(), None, inline)?;
            }
            return Ok(());
        }
        return match table.get_mut(name) {
            Some(Item::Value(old)) => {
                // Keep what surrounds the old value, such as a comment after it on the line.
                let decor = old.decor().clone();
                *old = value;
                *old.decor_mut() = decor;
                Ok(())
            }
            None | Some(Item::None) => {
                insert(table, name, Item::Value(value), comment, inline);
                Ok(())
            }
            // A key such as `native_fanout` takes a word or a table. The type check after this
            // refuses a value where only a table fits.
            Some(item) => {
                *item = Item::Value(value);
                // A table header's key had no space after it; `key = value` needs one.
                if let Some(mut key) = table.key_mut(name) {
                    key.leaf_decor_mut().set_suffix(" ");
                }
                Ok(())
            }
        };
    }
    if matches!(table.get(name), None | Some(Item::None)) {
        let child = if inline {
            Item::Value(Value::InlineTable(InlineTable::new()))
        } else {
            let mut child = Table::new();
            child.set_implicit(true);
            child.set_dotted(table.is_dotted());
            Item::Table(child)
        };
        table.insert(name, child);
    }
    let item = table
        .get_mut(name)
        .ok_or_else(|| format!("{name} went missing"))?;
    let child_inline = matches!(item, Item::Value(Value::InlineTable(_)));
    let child = item
        .as_table_like_mut()
        .ok_or_else(|| format!("{name} isn't a table, so it has no {}", rest.join(".")))?;
    set(child, rest, value, comment, child_inline)
}

fn insert(table: &mut dyn TableLike, name: &str, item: Item, comment: Option<&str>, inline: bool) {
    let comment = comment.map(|comment| format!("# {}\n", comment.replace(['\n', '\r'], " ")));
    match comment {
        // A comment can't sit inside an inline table or between the parts of a dotted key.
        // Keys that share one description, as each color under `[ui.colors]` does, say it
        // once, above the first of them.
        Some(comment) if !inline && !table.is_dotted() && !says(table, &comment) => {
            let key = Key::new(name).with_leaf_decor(Decor::new(comment, " "));
            table.entry_format(&key).or_insert(item);
        }
        _ => {
            table.insert(name, item);
        }
    }
}

/// Whether a key of `table` already has `comment` above it.
fn says(table: &dyn TableLike, comment: &str) -> bool {
    table.iter().any(|(name, _)| {
        let above = table.key(name).and_then(|key| key.leaf_decor().prefix());
        above
            .and_then(|prefix| prefix.as_str())
            .is_some_and(|prefix| prefix.contains(comment))
    })
}

/// The repo root when `file` is a repo's `.argonauts/config.local.toml`.
fn local_config_repo(file: &Path) -> Option<&Path> {
    if file.file_name()? != project_files::CONFIG_LOCAL {
        return None;
    }
    repo_of(file)
}

/// The repo root when `file` is a repo's `.argonauts/config.toml` or `config.local.toml`. Your
/// global config, `~/.argonauts/config.toml`, has the same shape, so `global` tells it apart.
fn project_config_repo<'a>(file: &'a Path, global: Option<&Path>) -> Option<&'a Path> {
    let name = file.file_name()?;
    if name != project_files::CONFIG && name != project_files::CONFIG_LOCAL {
        return None;
    }
    if global.is_some_and(|global| same_file(file, global)) {
        return None;
    }
    repo_of(file)
}

/// The folder holding `file`'s `.argonauts` folder.
fn repo_of(file: &Path) -> Option<&Path> {
    let folder = file.parent()?;
    if folder.file_name()? != ".argonauts" {
        return None;
    }
    folder.parent()
}

/// Whether two paths name one file, through links and whether or not it exists yet.
fn same_file(a: &Path, b: &Path) -> bool {
    let real = |path: &Path| {
        real_path(path)
            .ok()
            .and_then(|path| std::path::absolute(path).ok())
    };
    matches!((real(a), real(b)), (Some(a), Some(b)) if a == b)
}

/// The file to write. A config that's a link, such as one kept in a dotfiles repo, is written
/// through, so the link stays, even when what it points at doesn't exist yet.
pub(crate) fn real_path(file: &Path) -> Result<PathBuf, SettingsError> {
    match fs::canonicalize(file) {
        Ok(path) => Ok(path),
        Err(err) if err.kind() == io::ErrorKind::NotFound => follow_links(file),
        Err(err) => Err(io_error(file, err)),
    }
}

/// Where a chain of links ends, whether or not a file is there yet.
fn follow_links(file: &Path) -> Result<PathBuf, SettingsError> {
    let mut path = file.to_owned();
    // The same limit the kernel uses, so a loop of links ends with an error.
    for _ in 0..40 {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = fs::read_link(&path).map_err(|err| io_error(&path, err))?;
                // A relative target is relative to the link's folder; `join` keeps an absolute one.
                path = folder_of(&path).join(target);
            }
            Ok(_) => return Ok(path),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(path),
            Err(err) => return Err(io_error(&path, err)),
        }
    }
    Err(io_error(file, io::Error::other("too many links")))
}

fn folder_of(file: &Path) -> &Path {
    match file.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

/// Locks the folder while its file is read, edited, and replaced, so two windows saving at once
/// never lose a change. It creates the folder first, so the first writes into it take turns too.
pub(crate) fn lock_folder(target: &Path) -> Result<fs::File, SettingsError> {
    let dir = folder_of(target);
    fs::create_dir_all(dir).map_err(|err| io_error(dir, err))?;
    let handle = fs::File::open(dir).map_err(|err| io_error(dir, err))?;
    handle.lock().map_err(|err| io_error(dir, err))?;
    Ok(handle)
}

fn read(file: &Path) -> Result<String, SettingsError> {
    match fs::read_to_string(file) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(io_error(file, err)),
    }
}

/// Writes through a temporary file and a rename, so a crash never leaves half a file, and keeps
/// the old file's permissions.
pub(crate) fn write_replacing(target: &Path, text: &str) -> Result<(), SettingsError> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let dir = folder_of(target);
    fs::create_dir_all(dir).map_err(|err| io_error(dir, err))?;
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&temp, text).map_err(|err| io_error(&temp, err))?;
    if let Ok(metadata) = fs::metadata(target) {
        // Best effort: the file is still correct if this fails.
        let _ = fs::set_permissions(&temp, metadata.permissions());
    }
    fs::rename(&temp, target).map_err(|err| {
        let _ = fs::remove_file(&temp);
        io_error(target, err)
    })
}

pub(crate) fn io_error(path: &Path, err: io::Error) -> SettingsError {
    SettingsError::Io {
        path: path.to_owned(),
        message: err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    use super::*;

    fn git(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn write_setting_keeps_comments() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        let before = "\
# My settings. Leave them alone, robots.
workflow = \"council\"   # the default

[ui]
# I like vim.
keys = \"default\"
";
        fs::write(&file, before).unwrap();
        write(&file, "ui.keys", "vim", false).unwrap();
        write(&file, "workflow", "\"orchestrator\"", false).unwrap();
        write(&file, "ui.theme", "\"light\"", false).unwrap();
        write(&file, "approvals.merge.mode", "\"auto\"", false).unwrap();
        let after = fs::read_to_string(&file).unwrap();
        assert!(
            after.starts_with("# My settings. Leave them alone, robots.\n"),
            "{after}"
        );
        assert!(
            after.contains("workflow = \"orchestrator\"   # the default\n"),
            "{after}"
        );
        assert!(after.contains("# I like vim.\nkeys = \"vim\"\n"), "{after}");
        assert!(after.contains("# The palette. Auto follows"), "{after}");
        assert!(
            after.contains("[approvals.merge]\n# The merge approval: supervised waits"),
            "{after}"
        );
        assert!(after.contains("mode = \"auto\"\n"), "{after}");
        assert!(!after.contains("[approvals]\n"), "{after}");
        let settings: Settings = toml::from_str(&after).unwrap();
        assert_eq!(settings.workflow, "orchestrator");
        assert_eq!(settings.ui.keys, argonauts_core::KeyScheme::Vim);
    }

    #[test]
    fn keys_that_share_a_description_say_it_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        write(&file, "ui.colors.agent-blue", "\"agent-purple\"", false).unwrap();
        write(&file, "ui.colors.agent-cyan", "\"agent-green\"", false).unwrap();
        write(&file, "ui.theme", "\"light\"", false).unwrap();
        let after = fs::read_to_string(&file).unwrap();
        assert_eq!(
            after.matches("# A role and its color").count(),
            1,
            "{after}"
        );
        assert!(
            after.contains("agent-blue = \"agent-purple\"\nagent-cyan = \"agent-green\"\n"),
            "{after}"
        );
        // A key with a description of its own still gets it.
        assert!(after.contains("# The palette. Auto follows"), "{after}");
    }

    #[test]
    fn write_setting_creates_the_file_and_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("home/.argonauts/config.toml");
        write(&file, "agents.codex.model", "gpt-x", false).unwrap();
        write(&file, "roles.reviewer.models", "[\"codex\"]", false).unwrap();
        let settings: Settings = toml::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(settings.agents["codex"].model.as_deref(), Some("gpt-x"));
        let reviewer = &settings.roles[&argonauts_core::names::RoleName::Reviewer];
        assert_eq!(reviewer.models.len(), 1);
    }

    #[test]
    fn dotted_and_inline_tables_stay_valid() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        fs::write(
            &file,
            "ui.keys = \"vim\"\nnarrator = { interval = \"45s\" }\n",
        )
        .unwrap();
        write(&file, "ui.theme", "light", false).unwrap();
        write(&file, "narrator.spend_cap", "\"1.00 usd\"", false).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        let settings: Settings = toml::from_str(&text).unwrap();
        assert_eq!(
            settings.ui.theme,
            argonauts_core::settings::ui::Theme::Light
        );
        assert_eq!(settings.narrator.spend_cap, "1.00 usd");
        assert_eq!(settings.narrator.interval.0, "45s");
        assert!(text.contains("ui.theme = \"light\""), "{text}");
    }

    #[test]
    fn a_word_can_replace_a_table_where_the_key_takes_either() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        fs::write(&file, "[roles.worker.native_fanout]\nmax = 3\n").unwrap();
        write(&file, "roles.worker.native_fanout", "off", false).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.contains("native_fanout = \"off\""), "{text}");
        let settings: Settings = toml::from_str(&text).unwrap();
        let worker = &settings.roles[&argonauts_core::names::RoleName::Worker];
        use argonauts_core::settings::sessions::{FanoutSwitch, NativeFanout};
        assert_eq!(
            worker.native_fanout,
            Some(NativeFanout::Switch(FanoutSwitch::Off))
        );
    }

    #[test]
    fn wrong_values_never_reach_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        fs::write(&file, "[limits]\nspend_per_run = 20.0\n").unwrap();
        for (key, value) in [
            ("limits.spend_per_run", "\"lots\""),
            ("limits..spend_per_run", "5"),
            ("limits", "5"),
            ("limits.spend_per_run.x", "5"),
            ("limits.spend_per_run", "5\nsandbox.network = true"),
            ("ui.keys", ""),
            // A quoted or odd name would dodge the safety rules as a key they don't know.
            ("\"approvals\".merge.mode", "auto"),
            ("sandbox.net work", "true"),
            ("ui.ke'ys", "vim"),
        ] {
            let err = write(&file, key, value, false).unwrap_err();
            assert!(matches!(err, SettingsError::Invalid { .. }), "{key}: {err}");
        }
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "[limits]\nspend_per_run = 20.0\n"
        );
    }

    #[test]
    fn safety_keys_never_change_inside_an_agent_session() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        for (key, value) in [
            ("sandbox.network", "true"),
            (" sandbox . network ", "true"),
            ("approvals.merge.mode", "auto"),
            (
                "endpoints.ollama",
                "{ base_url = \"http://elsewhere\", local = true }",
            ),
            ("agents.codex", "{ auth = \"api-key\" }"),
            ("approvals", "{ merge = { mode = \"auto\" } }"),
        ] {
            let err = write(&file, key, value, true).unwrap_err();
            assert!(matches!(err, SettingsError::Refused(_)), "{key}: {err}");
        }
        assert!(!file.exists());
        fs::write(
            &file,
            "[limits]\nspend_per_run = 5.0\n[agents.codex]\nbinary = \"/opt/codex\"\n\
             [fallback.worker]\nallow_api_billing = true\n",
        )
        .unwrap();
        write(&file, "limits", "{}", true).unwrap();
        write(&file, "agents.codex", "{ model = \"x\" }", true).unwrap();
        write(&file, "fallback.worker", "{ mode = \"auto\" }", true).unwrap();
        let settings: Settings = toml::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(settings.limits.spend.spend_per_run, Some(5.0));
        let codex = &settings.agents["codex"];
        assert_eq!(codex.binary.as_deref(), Some(Path::new("/opt/codex")));
        assert_eq!(codex.model.as_deref(), Some("x"));
        let workers = &settings.fallback.roles[&argonauts_core::settings::crew::CrewRole::Worker];
        assert!(workers.allow_api_billing);
        fs::remove_file(&file).unwrap();
        assert_eq!(
            safety_keys_in(
                "endpoints.ollama",
                "{ base_url = \"x\", api_key_env = \"K\" }"
            ),
            ["endpoints.ollama.base_url", "endpoints.ollama.api_key_env"]
        );
        assert!(safety_keys_in("endpoints.ollama", "{ local = true }").is_empty());
        assert_eq!(
            safety_keys_in("checks.run", "[\"cargo test\"]"),
            ["checks.run"]
        );
        assert!(safety_keys_in("ui.keys", "vim").is_empty());
        write(&file, "ui.keys", "vim", true).unwrap();
        write(&file, "sandbox.network", "true", false).unwrap();
    }

    #[test]
    fn fallback_billing_never_changes_inside_an_agent_session() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        let billing = "{ model = \"codex\", allow_api_billing = true }";
        for (key, value) in [
            ("fallback.order", format!("[{billing}]")),
            (
                "fallback.replace.codex.with",
                format!("[\"claude-code\", {billing}]"),
            ),
            ("fallback.worker", format!("{{ order = [{billing}] }}")),
            (
                "fallback",
                format!("{{ worker = {{ replace = {{ codex = {{ with = [{billing}] }} }} }} }}"),
            ),
        ] {
            let err = write(&file, key, &value, true).unwrap_err();
            assert!(matches!(err, SettingsError::Refused(_)), "{key}: {err}");
        }
        assert!(!file.exists());
        assert_eq!(
            safety_keys_in("fallback.order", &format!("[\"codex\", {billing}]")),
            ["fallback.order[1].allow_api_billing"]
        );
        assert_eq!(
            safety_keys_in(
                "fallback.worker",
                &format!("{{ replace = {{ codex = {{ with = [{billing}] }} }} }}")
            ),
            ["fallback.worker.replace.codex.with[0].allow_api_billing"]
        );
        let no_billing = "[\"claude-code\", { model = \"codex\", allow_api_billing = false }]";
        assert!(safety_keys_in("fallback.order", no_billing).is_empty());
        write(&file, "fallback.order", no_billing, true).unwrap();
    }

    fn resolve_alone(file: &Path) -> Result<crate::ResolvedSettings, SettingsError> {
        crate::resolve(&crate::SettingsSources {
            global: Some(file.to_owned()),
            strict: true,
            ..Default::default()
        })
    }

    #[test]
    fn a_limit_unit_replaces_the_other_units_of_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        fs::write(
            &file,
            "[limits]\n# Dollars.\nspend_per_run = 20.0\nper_task_usd = 2.0\nsoft_stop = 0.8\n",
        )
        .unwrap();
        write(&file, "limits.run_tokens", "2000000", false).unwrap();
        write(&file, "limits", "{ per_task_minutes = 30 }", false).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(!text.contains("usd") && !text.contains("Dollars"), "{text}");
        let limit = resolve_alone(&file).unwrap().settings.limits.spend;
        assert_eq!(limit.run_tokens, Some(2_000_000));
        assert_eq!(limit.spend_per_run, None);
        assert_eq!(limit.per_task_usd, None);
        assert_eq!(limit.soft_stop, 0.8);

        let message = refusal(
            &file,
            "limits",
            "{ spend_per_run = 5.0, run_plan_usage = 0.5 }",
        );
        assert!(
            message.contains("limits.spend_per_run and limits.run_plan_usage are one choice"),
            "{message}"
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), text);
    }

    #[test]
    fn a_value_resolve_would_refuse_never_reaches_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        let before = "[ui]\nkeys = \"vim\"\n";
        fs::write(&file, before).unwrap();
        for (key, value, wanted) in [
            (
                "workflows.max_depth",
                "3",
                "workflows.max_depth is at most 2",
            ),
            ("sessions.rotate_at", "0", "sessions.rotate_at"),
            ("critique.critic", "\"not a model!\"", "critique.critic is"),
            ("crew.main", "ollama", "crew.main is ollama, an endpoint"),
            (
                "limits.soft_stop",
                "1.5",
                "limits.soft_stop must be more than 0",
            ),
            ("checks.timeout", "soon", "checks.timeout: "),
            (
                "roles.worker.native_fanout",
                "{ max = 0 }",
                "roles.worker.native_fanout.max must be at least 1",
            ),
        ] {
            let message = refusal(&file, key, value);
            assert!(message.contains(wanted), "{key}: {message}");
        }
        assert_eq!(fs::read_to_string(&file).unwrap(), before);

        // The refusal is the one resolve gives for the file.
        let err = write(&file, "workflows.max_depth", "3", false).unwrap_err();
        fs::write(&file, format!("{before}[workflows]\nmax_depth = 3\n")).unwrap();
        assert_eq!(resolve_alone(&file).unwrap_err(), err);
    }

    #[test]
    fn a_project_file_never_takes_a_global_only_key() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let shared = argonauts_core::paths::project_config_path(&repo);
        let local = argonauts_core::paths::project_local_config_path(&repo);
        for file in [&shared, &local] {
            for (key, value) in [
                ("notifications.conversation.on", "[\"finished\"]"),
                (
                    "notifications",
                    "{ conversation = { on = [\"finished\"] } }",
                ),
            ] {
                let err = write(file, key, value, false).unwrap_err();
                assert!(matches!(err, SettingsError::Refused(_)), "{key}: {err}");
            }
        }
        assert!(!shared.exists() && !local.exists());

        // The refusal is the one resolve gives for the file, trusted or not.
        let err = write(
            &shared,
            "notifications.conversation.on",
            "[\"finished\"]",
            false,
        )
        .unwrap_err();
        fs::create_dir_all(shared.parent().unwrap()).unwrap();
        fs::write(
            &shared,
            "[notifications.conversation]\non = [\"finished\"]\n",
        )
        .unwrap();
        let sources = crate::SettingsSources {
            project: Some(shared.clone()),
            project_trusted: true,
            strict: true,
            ..Default::default()
        };
        assert_eq!(crate::resolve(&sources).unwrap_err(), err);

        // Your global config lives in a folder of the same name.
        let global = dir.path().join("home/.argonauts/config.toml");
        assert_eq!(
            project_config_repo(&global, None),
            Some(global.parent().unwrap().parent().unwrap())
        );
        assert_eq!(project_config_repo(&global, Some(&global)), None);
        assert_eq!(
            project_config_repo(&local, Some(&global)),
            Some(repo.as_path())
        );
        assert_eq!(
            project_config_repo(&dir.path().join("config.toml"), None),
            None
        );
        write(
            &dir.path().join("config.toml"),
            "notifications.conversation.on",
            "[\"finished\"]",
            false,
        )
        .unwrap();
    }

    #[test]
    fn a_dotted_name_in_a_value_is_still_a_safety_key_and_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        let dotted = "{ \"evil.x\" = { base_url = \"https://attacker/v1\" } }";
        assert_eq!(
            safety_keys_in("endpoints", dotted),
            ["endpoints.evil.x.base_url"]
        );
        for in_agent_session in [true, false] {
            let err = write(&file, "endpoints", dotted, in_agent_session).unwrap_err();
            assert!(err.to_string().contains("\"evil.x\""), "{err}");
        }
        assert!(!file.exists());
        fs::write(
            &file,
            "[endpoints.\"evil.x\"]\nbase_url = \"https://attacker/v1\"\n",
        )
        .unwrap();
        let err = remove(&file, "endpoints", true).unwrap_err();
        assert!(matches!(err, SettingsError::Refused(_)), "{err}");
    }

    #[test]
    fn local_setting_added_to_git_exclude() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "--quiet"]);
        let local = argonauts_core::paths::project_local_config_path(repo);
        let shared = argonauts_core::paths::project_config_path(repo);
        write(&local, "agents.codex.effort", "high", false).unwrap();
        write(&local, "agents.codex.model", "gpt-x", false).unwrap();
        write(&shared, "workflow", "captain", false).unwrap();
        let exclude = fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
        assert_eq!(
            exclude.matches(".argonauts/config.local.toml").count(),
            1,
            "{exclude}"
        );
        let untracked = git(repo, &["status", "--porcelain", "--untracked-files=all"]);
        assert!(untracked.contains(".argonauts/config.toml"), "{untracked}");
        assert!(!untracked.contains("config.local.toml"), "{untracked}");
    }

    #[test]
    fn a_linked_file_stays_a_link_and_keeps_its_mode() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("dotfiles/argonauts.toml");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::write(&real, "# mine\n").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write(&link, "ui.keys", "vim", false).unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let text = fs::read_to_string(&real).unwrap();
        assert!(
            text.starts_with("# mine\n") && text.contains("keys = \"vim\""),
            "{text}"
        );
        let mode = fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_table_merges_into_an_inline_table_too() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        fs::write(
            &file,
            "endpoints.ollama = { base_url = \"http://localhost:11434/v1\", local = true }\n",
        )
        .unwrap();
        write(&file, "endpoints.ollama", "{ timeout = \"2m\" }", false).unwrap();
        let settings: Settings = toml::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        let ollama = &settings.endpoints["ollama"];
        assert_eq!(
            ollama.base_url.as_deref(),
            Some("http://localhost:11434/v1")
        );
        assert!(ollama.local);
        assert_eq!(ollama.timeout.0, "2m");
    }

    #[test]
    fn remove_setting_keeps_comments() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        fs::write(
            &file,
            "\
# My settings. Leave them alone, robots.
workflow = \"council\"   # the default

[ui]
# I like vim.
keys = \"vim\"
# The palette.
theme = \"light\"

[narrator]
model = \"ollama/llama3.2\"
",
        )
        .unwrap();
        assert!(remove(&file, "ui.theme", false).unwrap());
        assert!(remove(&file, "narrator.model", false).unwrap());
        assert!(!remove(&file, "ui.accent", false).unwrap());
        assert!(!remove(&file, "notifications.os", false).unwrap());
        let after = fs::read_to_string(&file).unwrap();
        assert!(
            after.starts_with("# My settings. Leave them alone, robots.\n"),
            "{after}"
        );
        assert!(
            after.contains("workflow = \"council\"   # the default\n"),
            "{after}"
        );
        assert!(after.contains("# I like vim.\nkeys = \"vim\"\n"), "{after}");
        assert!(
            !after.contains("theme") && !after.contains("# The palette."),
            "{after}"
        );
        assert!(!after.contains("[narrator]"), "{after}");
        let settings: Settings = toml::from_str(&after).unwrap();
        assert_eq!(settings.ui.theme, Settings::default().ui.theme);
        assert_eq!(settings.narrator.model, None);
        let missing = dir.path().join("nowhere/config.toml");
        assert!(!remove(&missing, "ui.keys", false).unwrap());
        assert!(!missing.parent().unwrap().exists());
    }

    #[test]
    fn removing_the_first_key_keeps_the_files_opening_comment() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        fs::write(
            &file,
            "# My settings. Leave them alone, robots.\nultracode = true\nworkflow = \"captain\"\n",
        )
        .unwrap();
        assert!(remove(&file, "ultracode", false).unwrap());
        let after = fs::read_to_string(&file).unwrap();
        assert_eq!(
            after,
            "# My settings. Leave them alone, robots.\nworkflow = \"captain\"\n"
        );

        // With no key left, the first table carries it, and the key's description goes.
        write_setting(&file, "ui.theme", "dark").unwrap();
        remove(&file, "workflow", false).unwrap();
        write_setting(&file, "ultracode", "true").unwrap();
        let text = fs::read_to_string(&file).unwrap();
        fs::write(&file, text.replace("ultracode", "# opening\nultracode")).unwrap();
        assert!(remove(&file, "ultracode", false).unwrap());
        let after = fs::read_to_string(&file).unwrap();
        assert!(
            after.starts_with("# opening\n# My settings. Leave them alone, robots.\n\n[ui]\n"),
            "{after}"
        );
        assert!(!after.contains("ultracode") && !after.contains("Ultracode:"));
    }

    #[test]
    fn removing_a_safety_key_is_refused_inside_an_agent_session() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        let before = "[sandbox]\nnetwork = true\n[endpoints.ollama]\nbase_url = \"http://x\"\n\
                      [ui]\nkeys = \"vim\"\n";
        fs::write(&file, before).unwrap();
        for key in [
            "sandbox.network",
            "sandbox",
            "endpoints.ollama",
            "endpoints",
        ] {
            let err = remove(&file, key, true).unwrap_err();
            assert!(matches!(err, SettingsError::Refused(_)), "{key}: {err}");
        }
        assert_eq!(fs::read_to_string(&file).unwrap(), before);
        assert!(remove(&file, "ui.keys", true).unwrap());
        assert!(remove(&file, "sandbox.network", false).unwrap());
    }

    #[test]
    fn a_link_to_a_file_not_there_yet_stays_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("dotfiles/argonauts.toml");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink("dotfiles/argonauts.toml", &link).unwrap();
        write(&link, "ui.keys", "vim", false).unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::read_to_string(&real)
                .unwrap()
                .contains("keys = \"vim\"")
        );
    }

    #[test]
    fn writers_to_one_folder_take_turns() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        fs::write(&file, "# mine\n").unwrap();
        // The first writes into a folder not there yet take turns too.
        let fresh = dir.path().join("repo/.argonauts/config.toml");
        let keys: Vec<String> = (0..8).map(|n| format!("ui.keymap.action_{n}")).collect();
        for file in [&file, &fresh] {
            std::thread::scope(|scope| {
                for key in &keys {
                    scope.spawn(move || write(file, key, "\"ctrl+x\"", false).unwrap());
                }
            });
            let settings: Settings = toml::from_str(&fs::read_to_string(file).unwrap()).unwrap();
            assert_eq!(settings.ui.keymap.len(), keys.len(), "a change was lost");
        }
    }

    fn refusal(file: &Path, key: &str, value: &str) -> String {
        match write(file, key, value, false) {
            Err(SettingsError::Invalid { message, .. }) => message,
            other => panic!("{key} = {value}: {other:?}"),
        }
    }

    #[test]
    fn a_check_written_as_a_string_is_refused_with_the_table_to_write() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("repo/.argonauts/config.toml");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let err = write(&file, "checks.run", "[\"cargo test\"]", false).unwrap_err();
        assert_eq!(
            err,
            SettingsError::Invalid {
                layer: ".argonauts/config.toml".to_owned(),
                message: "checks.run can't be a list of strings; it takes a list of checks, each \
                          a [[checks.run]] table with a name and a command. Write it like this:\n\
                          [[checks.run]]\nname = \"test\"\ncommand = [\"cargo\", \"test\"]"
                    .to_owned(),
            }
        );
        assert!(!file.exists());
    }

    #[test]
    fn write_setting_names_the_nearest_key() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        let before = "[ui]\nkeys = \"default\"\n";
        fs::write(&file, before).unwrap();
        for (key, value, near) in [
            ("ui.them", "light", "ui.theme"),
            ("agents.codex.modle", "gpt-x", "agents.codex.model"),
            ("limts.spend_per_run", "5", "limits.spend_per_run"),
            ("limts", "{ spend_per_run = 5 }", "limits"),
            (
                "fallback.worker.stratgy",
                "race",
                "fallback.worker.strategy",
            ),
            (
                "fallback.replace.codex.wth",
                "[\"claude-code\"]",
                "fallback.replace.codex.with",
            ),
            (
                "agents.codex",
                "{ modle = \"gpt-x\" }",
                "agents.codex.model",
            ),
        ] {
            let message = refusal(&file, key, value);
            assert!(
                message.ends_with(&format!("Did you mean {near}?")),
                "{key}: {message}"
            );
        }
        let message = refusal(&file, "limits", "5");
        assert!(
            message.contains("limits is a table of settings"),
            "{message}"
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), before);

        // Map keys you choose are known wherever the catalog has a `*`.
        for (key, value) in [
            ("roles.worker.native_fanout.max", "2"),
            ("fallback.worker.replace.codex.strategy", "spread"),
            ("visibility.status_summary.worker", "false"),
            ("endpoints.ollama.header_env.X-Team", "TEAM_HEADER"),
            ("ui.keymap.agent_panel_view", "\"ctrl+left\""),
            (
                "agents.codex",
                "{ model = \"gpt-x\", price = { input_per_mtok = 1.5, output_per_mtok = 6.0 } }",
            ),
        ] {
            write(&file, key, value, false).unwrap_or_else(|err| panic!("{key}: {err}"));
        }
    }

    #[test]
    fn write_setting_refuses_a_value_not_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".argonauts/config.toml");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let before = "# mine\nworkflow = \"captain\"\n";
        fs::write(&file, before).unwrap();
        for (key, value, wanted) in [
            (
                "ui.theme",
                "blue",
                "ui.theme can't be \"blue\"; it takes \"auto\", \"dark\", \"light\", \"dark-daltonized\"",
            ),
            (
                "sandbox.network",
                "yes",
                "sandbox.network can't be \"yes\"; it takes true or false",
            ),
            (
                "ui.ambiguous_width",
                "3",
                "ui.ambiguous_width can't be 3; it takes 1 or 2",
            ),
            (
                "approvals.merge",
                "{ mode = \"sometimes\" }",
                "approvals.merge.mode can't be \"sometimes\"",
            ),
            (
                "fallback.worker.on_return",
                "later",
                "\"ask\", \"keep\", \"review\", \"give_back\", or \"redo\"",
            ),
        ] {
            let message = refusal(&file, key, value);
            assert!(message.contains(wanted), "{key}: {message}");
        }
        let chosen = "a-workflow-only-this-test-writes";
        let message = refusal(&file, "workflow", chosen);
        assert_eq!(message.matches("\"council\"").count(), 1, "{message}");
        assert_eq!(fs::read_to_string(&file).unwrap(), before);

        // A workflow file beside the settings makes its name a value `workflow` takes.
        let workflows = dir.path().join(".argonauts/workflows");
        fs::create_dir_all(&workflows).unwrap();
        fs::write(
            workflows.join(format!("{chosen}.toml")),
            "[review]\nper_task = 2\n",
        )
        .unwrap();
        write(&file, "workflow", chosen, false).unwrap();
        // A file whose name the lookup refuses doesn't make that name a value `workflow` takes.
        fs::write(workflows.join("my team.toml"), "[review]\nper_task = 2\n").unwrap();
        let message = refusal(&file, "workflow", "\"my team\"");
        assert!(message.contains("isn't a workflow name"), "{message}");
        for (key, value) in [
            ("ui.theme", "'light'"),
            ("ui.ambiguous_width", "2"),
            ("roles.worker.native_fanout", "off"),
            ("agents.codex.effort", "xhigh"),
        ] {
            write(&file, key, value, false).unwrap_or_else(|err| panic!("{key}: {err}"));
        }
        let settings: Settings = toml::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(settings.workflow, chosen);
    }

    #[test]
    fn a_proposed_setup_is_checked_without_a_file() {
        let pairs = |list: &[(&str, &str)]| -> Vec<(String, String)> {
            list.iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect()
        };
        check_settings(
            &pairs(&[
                ("ui.theme", "\"light\""),
                ("agents.codex.effort", "\"high\""),
            ]),
            &[],
        )
        .expect("a valid setup");
        let unknown = check_settings(&pairs(&[("ui.themes", "\"light\"")]), &[]).unwrap_err();
        assert!(unknown.to_string().contains("isn't a setting"), "{unknown}");
        let wrong = check_settings(&pairs(&[("ui.theme", "\"plaid\"")]), &[]).unwrap_err();
        assert!(wrong.to_string().contains("ui.theme"), "{wrong}");
        let typed = check_settings(&pairs(&[("ui.ambiguous_width", "\"wide\"")]), &[]).unwrap_err();
        assert!(typed.to_string().contains("the proposed setup"), "{typed}");
        let yours = pairs(&[("workflow", "\"tight-loop\"")]);
        assert!(check_settings(&yours, &[]).is_err(), "no such workflow");
        check_settings(&yours, &["tight-loop".to_owned()]).expect("a workflow of yours");
    }
}
