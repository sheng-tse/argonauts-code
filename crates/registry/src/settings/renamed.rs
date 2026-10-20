//! The settings names that changed, each old line beside the line it becomes. The table words the start
//! question that offers to edit a file still using old names, and makes that edit. It never
//! reads an old name as its new one: until the file says the new name, its line does nothing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use toml::{Table, Value};
use toml_edit::{DocumentMut, Item, TableLike};

use crate::SettingsError;
use crate::resolve::is_safety_key;
use crate::write::{lock_folder, real_path, resolves_alone, write_replacing};

/// Tables that moved, with everything in them. Each keeps its parent, so a move is a new key in
/// the same place.
const TABLES: &[(&str, &str)] = &[
    ("approvals.goal", "approvals.understand"),
    ("approvals.approver", "approvals.judge"),
    ("notifications.chat", "notifications.conversation"),
    ("solo", "captain"),
    ("solo_or_crew", "captain_or_crew"),
    ("supervisor", "run_host"),
    ("roles.phase-lead", "roles.tie-breaker"),
    ("roles.restater", "roles.reader"),
    ("roles.differ", "roles.comparer"),
    ("roles.drafter", "roles.draft"),
    ("roles.mapper", "roles.map"),
    ("roles.spike", "roles.experiment"),
    ("roles.retro", "roles.run-review"),
    ("roles.adjudicator", "roles.judge"),
];

/// Keys that changed, spelled after their tables moved.
const KEYS: &[(&str, &str)] = &[
    ("main_agent", "crew.main"),
    ("crew.workers", "crew.worker"),
    ("crew.reviewers", "crew.reviewer"),
    ("fallback.workers", "fallback.worker"),
    ("fallback.reviewers", "fallback.reviewer"),
    ("captain.attempts", "captain.tries"),
    ("captain.restate_first", "captain.understand_first"),
    ("captain_or_crew.solo_kind", "captain_or_crew.captain_kind"),
    (
        "captain_or_crew.solo_max_files",
        "captain_or_crew.captain_max_files",
    ),
    (
        "captain_or_crew.solo_requires_tests",
        "captain_or_crew.captain_requires_tests",
    ),
    (
        "approvals.rules.stuck.max_extra_attempts",
        "approvals.rules.stuck.max_extra_tries",
    ),
    ("limits.lines.integration", "limits.lines.merge"),
    ("sessions.chat_resumes", "sessions.conversation_resumes"),
    ("ui.agents_in_chat", "ui.agents_in_conversation"),
    ("ui.heads_up", "ui.notices"),
    ("ui.mod_output", "ui.plugin_output"),
    ("ui.takeover", "ui.take_over"),
    (
        "visibility.reviews_hidden_until_adjudicated",
        "visibility.reviews_hidden_until_settled",
    ),
];

/// Values that changed under a key, spelled with its new name. `*` stands for any one part.
const VALUES: &[(&str, &str, &str)] = &[
    ("workflow", "solo", "captain"),
    (
        "captain_or_crew.otherwise",
        "solo_then_crew",
        "captain_then_crew",
    ),
    ("captain_or_crew.otherwise", "solo", "captain"),
    ("captain_or_crew.captain_kind", "as_solo", "as_captain"),
    ("captain_or_crew.force", "solo", "captain"),
    ("sandbox.mode", "off", "tool"),
    ("goal.default", "from_request", "from-request"),
    ("ui.views.*.open", "dialog", "view"),
];

const MAIN_AGENT: &str = "main_agent";
const CREW: &str = "crew";
/// Who the bare crew list names, as `[crew]` spells it out.
const CREW_LISTS: [&str; 3] = ["worker", "reviewer", "judge"];

/// One old name a settings file still uses, and the name the edit gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    /// The whole line as the file has it, such as `main_agent = "claude-code"` or
    /// `[approvals.goal] mode = "ask"`.
    pub old: String,
    /// The line as the edit writes it.
    pub new: String,
    /// What else the edit does, said under it.
    pub note: Option<String>,
    /// The dotted key the file sets now, and the one it will set.
    pub from: String,
    pub to: String,
    /// The value the edit writes, when the value's own name changed too.
    pub value_now: Option<String>,
    /// A safety setting, whose value starts to count once it has its new name.
    pub safety: bool,
}

/// What renaming a file did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renamed {
    pub renames: Vec<Rename>,
    /// The file as it was.
    pub backup: PathBuf,
}

/// Every old name in a settings file's text, in the file's order. Nothing when it doesn't read
/// as TOML, which the settings already warn of.
pub fn renames_in(text: &str) -> Vec<Rename> {
    let (Ok(table), Ok(doc)) = (text.parse::<Table>(), text.parse::<DocumentMut>()) else {
        return Vec::new();
    };
    let mut order = Vec::new();
    paths_in_order(&mut Vec::new(), doc.as_table(), &mut order);
    renames_of(&table, &order)
}

/// The old keys a file sets, each with what the start question will call it now.
pub(crate) fn old_keys(table: &Table) -> BTreeMap<String, String> {
    let mut paths = Vec::new();
    collect_leaves(&mut Vec::new(), table, &mut paths);
    let paths: Vec<Vec<String>> = paths.into_iter().map(|(path, _)| path).collect();
    renames_of(table, &paths)
        .into_iter()
        .filter(|rename| rename.from != rename.to)
        .map(|rename| {
            // A name that changed alone is said by itself; one whose value changed too, with it.
            let new = match rename.value_now {
                Some(_) => rename.new,
                None => shown_key(&parts(&rename.to)),
            };
            (rename.from, new)
        })
        .collect()
}

/// Gives the file's old names their new ones, keeping a copy of it beside it as `<name>.bak`,
/// and leaves every other line as it was. The file must still read whole afterwards, and each
/// value must sit under its new name, or nothing is written.
pub fn rename_in_file(file: &Path) -> Result<Renamed, SettingsError> {
    let target = real_path(file)?;
    let _turn = lock_folder(&target)?;
    let io = |err: std::io::Error| SettingsError::Io {
        path: target.clone(),
        message: err.to_string(),
    };
    let text = fs::read_to_string(&target).map_err(io)?;
    let renames = renames_in(&text);
    let invalid = |message: String| SettingsError::Invalid {
        layer: file.display().to_string(),
        message,
    };
    if renames.is_empty() {
        return Err(invalid("it uses no name that changed".to_owned()));
    }
    let edited = edited(&text, &renames).map_err(invalid)?;
    holds_every_value(&text, &edited, &renames).map_err(invalid)?;
    resolves_alone(file, &edited)?;
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let backup = target.with_file_name(format!("{name}.bak"));
    fs::write(&backup, &text).map_err(io)?;
    write_replacing(&target, &edited)?;
    Ok(Renamed { renames, backup })
}

/// The renames of the values at `paths`, which come in the file's order: names first, then
/// values, as a person reads down what changed.
fn renames_of(table: &Table, paths: &[Vec<String>]) -> Vec<Rename> {
    let crew_is_a_list = matches!(table.get(CREW), Some(Value::Array(_)));
    let mut renames: Vec<Rename> = paths
        .iter()
        .filter_map(|path| {
            let value = value_at(table, path)?;
            let set = paths;
            let to = moved(path);
            let value_now = value.as_str().and_then(|text| new_value(&to, text));
            // A new name the file already sets would be set twice, so this one stays as it is.
            let taken = to != *path && taken(set, &to, crew_is_a_list);
            if (to == *path && value_now.is_none()) || taken {
                return None;
            }
            let note = (*path == [MAIN_AGENT] && crew_is_a_list)
                .then(|| "your crew list joins it as worker, reviewer and judge".to_owned());
            let was = value.to_string();
            let now = value_now.map_or_else(|| was.clone(), quoted);
            Some(Rename {
                old: format!("{} = {was}", shown_key(path)),
                new: format!("{} = {now}", shown_key(&to)),
                note,
                safety: is_safety_key(&to.join(".")),
                from: path.join("."),
                to: to.join("."),
                value_now: value_now.map(str::to_owned),
            })
        })
        .collect();
    renames.sort_by_key(|rename| rename.from == rename.to);
    renames
}

/// Each value's dotted path in the order the file has them. A list, even of tables, is one.
fn paths_in_order(prefix: &mut Vec<String>, table: &dyn TableLike, out: &mut Vec<Vec<String>>) {
    for (key, item) in table.iter() {
        prefix.push(key.to_owned());
        match item.as_table_like() {
            Some(inner) => paths_in_order(prefix, inner, out),
            None => out.push(prefix.clone()),
        }
        prefix.pop();
    }
}

/// Whether the file already sets `to` or something under it. The bare crew list stands for the
/// table it spells out, which has no `main` yet.
fn taken(set: &[Vec<String>], to: &[String], crew_is_a_list: bool) -> bool {
    set.iter().any(|path| {
        let at_or_under = path.starts_with(to) || to.starts_with(path);
        at_or_under && !(crew_is_a_list && path == &[CREW.to_owned()])
    })
}

/// Each value's dotted path, in the file's order. A list, even of tables, is one value.
fn collect_leaves(prefix: &mut Vec<String>, table: &Table, out: &mut Vec<(Vec<String>, Value)>) {
    for (key, value) in table {
        prefix.push(key.clone());
        match value {
            Value::Table(inner) => collect_leaves(prefix, inner, out),
            value => out.push((prefix.clone(), value.clone())),
        }
        prefix.pop();
    }
}

/// The path a value moves to: its table's new name, then its own.
fn moved(path: &[String]) -> Vec<String> {
    let mut path = path.to_vec();
    let table = TABLES
        .iter()
        .filter(|(old, _)| path.starts_with(&parts(old)))
        .max_by_key(|(old, _)| parts(old).len());
    if let Some((old, new)) = table {
        let rest = path.split_off(parts(old).len());
        path = parts(new);
        path.extend(rest);
    }
    if let Some((_, new)) = KEYS.iter().find(|(old, _)| path == parts(old)) {
        path = parts(new);
    }
    path
}

fn new_value(path: &[String], value: &str) -> Option<&'static str> {
    VALUES
        .iter()
        .find(|(key, old, _)| *old == value && matches_key(key, path))
        .map(|(_, _, new)| *new)
}

fn matches_key(pattern: &str, path: &[String]) -> bool {
    let pattern: Vec<&str> = pattern.split('.').collect();
    pattern.len() == path.len()
        && pattern
            .iter()
            .zip(path)
            .all(|(want, part)| *want == "*" || want == part)
}

fn parts(key: &str) -> Vec<String> {
    key.split('.').map(str::to_owned).collect()
}

/// A key as a file writes it: `main_agent`, or `[approvals.goal] mode` under its table.
fn shown_key(path: &[String]) -> String {
    match path.split_last() {
        Some((leaf, [])) => leaf.clone(),
        Some((leaf, table)) => format!("[{}] {leaf}", table.join(".")),
        None => String::new(),
    }
}

fn quoted(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

/// The file's text with each rename made, every other line, comment and table in its place.
fn edited(text: &str, renames: &[Rename]) -> Result<String, String> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|err: toml_edit::TomlError| err.to_string())?;
    if renames.iter().any(|rename| rename.from == MAIN_AGENT) {
        spell_out_crew(&mut doc);
    }
    // A table moves whole where its new name is free, keeping its comments and its place.
    for (old, new) in TABLES {
        let (old, new) = (parts(old), parts(new));
        let wanted = renames
            .iter()
            .any(|rename| parts(&rename.from).starts_with(&old));
        if wanted && item_at(doc.as_table_mut(), &new).is_none() {
            let (parent, leaf) = old.split_at(old.len() - 1);
            let Some(table) = table_at(doc.as_table_mut(), parent) else {
                continue;
            };
            if let Some(item) = table.remove(&leaf[0]) {
                table.insert(&new[new.len() - 1], item);
            }
        }
    }
    for rename in renames {
        let to = parts(&rename.to);
        let from = parts(&rename.from);
        // Where its table's move left it.
        let now = if item_at(doc.as_table_mut(), &from).is_some() {
            from
        } else {
            moved_tables_only(&from)
        };
        if now != to {
            let item = take(doc.as_table_mut(), &now)
                .ok_or_else(|| format!("{} isn't where the file said it was", rename.from))?;
            put(doc.as_table_mut(), &to, item)?;
        }
        if let Some(value) = &rename.value_now
            && let Some(item) = item_at(doc.as_table_mut(), &to)
            && let Some(old) = item.as_value_mut()
        {
            let decor = old.decor().clone();
            *old = toml_edit::Value::from(value.as_str());
            *old.decor_mut() = decor;
        }
    }
    Ok(doc.to_string())
}

fn moved_tables_only(path: &[String]) -> Vec<String> {
    let mut path = path.to_vec();
    if let Some((old, new)) = TABLES
        .iter()
        .filter(|(old, _)| path.starts_with(&parts(old)))
        .max_by_key(|(old, _)| parts(old).len())
    {
        let rest = path.split_off(parts(old).len());
        path = parts(new);
        path.extend(rest);
    }
    path
}

/// `crew = [...]` becomes the `[crew]` table it stands for, in the first table's place, so the
/// main agent can join it.
fn spell_out_crew(doc: &mut DocumentMut) {
    let Some(list) = doc.get(CREW).and_then(Item::as_array).cloned() else {
        return;
    };
    let mut crew = toml_edit::Table::new();
    let first = doc
        .iter()
        .filter_map(|(_, item)| item.as_table().and_then(toml_edit::Table::position))
        .min();
    if let Some(first) = first {
        crew.set_position(first);
    }
    for list_name in CREW_LISTS {
        let mut list = list.clone();
        list.decor_mut().clear();
        crew.insert(list_name, toml_edit::value(list));
    }
    doc.insert(CREW, Item::Table(crew));
}

fn item_at<'a>(table: &'a mut dyn TableLike, path: &[String]) -> Option<&'a mut Item> {
    let (first, rest) = path.split_first()?;
    let item = table.get_mut(first)?;
    if rest.is_empty() {
        return Some(item);
    }
    item_at(item.as_table_like_mut()?, rest)
}

fn table_at<'a>(table: &'a mut dyn TableLike, path: &[String]) -> Option<&'a mut dyn TableLike> {
    match path.split_first() {
        None => Some(table),
        Some((first, rest)) => table_at(table.get_mut(first)?.as_table_like_mut()?, rest),
    }
}

/// Takes the item at `path` out, leaving the comments above it above whatever comes next.
fn take(table: &mut dyn TableLike, path: &[String]) -> Option<Item> {
    let (leaf, parent) = path.split_last()?;
    let table = table_at(table, parent)?;
    let comment = table
        .key(leaf)
        .and_then(|key| key.leaf_decor().prefix())
        .and_then(|prefix| prefix.as_str())
        .filter(|prefix| prefix.contains('#'))
        .map(str::to_owned);
    let next = table
        .iter()
        .map(|(key, _)| key.to_owned())
        .skip_while(|key| key != leaf)
        .nth(1);
    let item = table.remove(leaf)?;
    if let (Some(comment), Some(next)) = (comment, next)
        && let Some(mut key) = table.key_mut(&next)
    {
        let own = key
            .leaf_decor()
            .prefix()
            .and_then(|prefix| prefix.as_str())
            .unwrap_or("")
            .to_owned();
        key.leaf_decor_mut().set_prefix(format!("{comment}{own}"));
    }
    Some(item)
}

/// Sets `item` at `path`, making the tables on the way as `[a.b]` headers, the way a person
/// would write them. The main agent goes first in `[crew]`, as setup writes it.
fn put(table: &mut dyn TableLike, path: &[String], item: Item) -> Result<(), String> {
    let (leaf, parent) = path.split_last().ok_or("an empty key")?;
    let mut here = table;
    for part in parent {
        if here.get(part).is_none() {
            let mut fresh = toml_edit::Table::new();
            fresh.set_implicit(true);
            here.insert(part, Item::Table(fresh));
        }
        here = here
            .get_mut(part)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| format!("{part} isn't a table"))?;
    }
    if path == [CREW, "main"] {
        let rest: Vec<(String, Item)> = here
            .iter()
            .map(|(key, item)| (key.to_owned(), item.clone()))
            .collect();
        here.clear();
        here.insert(leaf, item);
        for (key, item) in rest {
            here.insert(&key, item);
        }
    } else {
        here.insert(leaf, item);
    }
    Ok(())
}

/// Each value the file had, under its old name, is under its new one in `edited`, and the old
/// name is gone.
fn holds_every_value(text: &str, edited: &str, renames: &[Rename]) -> Result<(), String> {
    let before: Table = text
        .parse()
        .map_err(|err: toml::de::Error| err.to_string())?;
    let after: Table = edited
        .parse()
        .map_err(|err: toml::de::Error| err.to_string())?;
    for rename in renames {
        let was = value_at(&before, &parts(&rename.from));
        let now = value_at(&after, &parts(&rename.to));
        let kept = match (was, now) {
            (Some(Value::String(was)), Some(Value::String(now))) => {
                was == now || new_value(&parts(&rename.to), was) == Some(now.as_str())
            }
            (Some(was), Some(now)) => was == now,
            _ => false,
        };
        let gone = rename.from == rename.to || value_at(&after, &parts(&rename.from)).is_none();
        if !kept || !gone {
            return Err(format!(
                "renaming {} to {} didn't come out as asked, so the file is left as it was",
                rename.old, rename.new
            ));
        }
    }
    Ok(())
}

fn value_at<'a>(table: &'a Table, path: &[String]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let value = table.get(first)?;
    match (rest.is_empty(), value) {
        (true, value) => Some(value),
        (false, Value::Table(inner)) => value_at(inner, rest),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::settings::Settings;

    /// A global file as an older build wrote it.
    const OLDER: &str = r#"# mine
main_agent = "codex"
workflow = "solo"
crew = ["claude-code", "codex"]

[approvals]
autopilot = false

[approvals.goal]
mode = "supervised" # read it back to me
"#;

    fn names(renames: &[Rename]) -> Vec<(&str, &str)> {
        renames
            .iter()
            .map(|rename| (rename.old.as_str(), rename.new.as_str()))
            .collect()
    }

    #[test]
    fn an_older_file_lists_each_old_line_with_the_line_it_becomes() {
        let renames = renames_in(OLDER);
        assert_eq!(
            names(&renames),
            [
                ("main_agent = \"codex\"", "[crew] main = \"codex\""),
                (
                    "[approvals.goal] mode = \"supervised\"",
                    "[approvals.understand] mode = \"supervised\""
                ),
                ("workflow = \"solo\"", "workflow = \"captain\""),
            ]
        );
        assert_eq!(
            renames[0].note.as_deref(),
            Some("your crew list joins it as worker, reviewer and judge")
        );
        assert!(renames[1].safety);
        assert_eq!(renames[1].note, None);
    }

    #[test]
    fn renaming_keeps_every_value_comment_and_place() {
        let edited = edited(OLDER, &renames_in(OLDER)).unwrap();
        assert_eq!(
            edited,
            r#"# mine
workflow = "captain"

[crew]
main = "codex"
worker = ["claude-code", "codex"]
reviewer = ["claude-code", "codex"]
judge = ["claude-code", "codex"]

[approvals]
autopilot = false

[approvals.understand]
mode = "supervised" # read it back to me
"#
        );
        holds_every_value(OLDER, &edited, &renames_in(OLDER)).unwrap();
        assert!(renames_in(&edited).is_empty());
    }

    #[test]
    fn the_new_names_are_read_as_settings() {
        let edited = edited(OLDER, &renames_in(OLDER)).unwrap();
        let settings: Settings = toml::Value::Table(edited.parse().unwrap())
            .try_into()
            .unwrap();
        assert_eq!(settings.crew.main.agent, "codex");
        assert_eq!(settings.workflow, "captain");
        assert_eq!(
            settings.approvals.understand.mode,
            argonauts_core::settings::approvals::ApprovalMode::Supervised
        );
    }

    #[test]
    fn a_table_inside_a_moved_table_and_its_renamed_keys_move_together() {
        let text = "[solo]\nattempts = 2\nkind = \"checked\"\n\n[solo_or_crew]\nsolo_max_files = 4\notherwise = \"solo_then_crew\"\n";
        let renames = renames_in(text);
        assert_eq!(
            names(&renames),
            [
                ("[solo] attempts = 2", "[captain] tries = 2"),
                ("[solo] kind = \"checked\"", "[captain] kind = \"checked\""),
                (
                    "[solo_or_crew] solo_max_files = 4",
                    "[captain_or_crew] captain_max_files = 4"
                ),
                (
                    "[solo_or_crew] otherwise = \"solo_then_crew\"",
                    "[captain_or_crew] otherwise = \"captain_then_crew\""
                ),
            ]
        );
        let edited = edited(text, &renames).unwrap();
        assert_eq!(
            edited,
            "[captain]\nkind = \"checked\"\ntries = 2\n\n[captain_or_crew]\notherwise = \"captain_then_crew\"\ncaptain_max_files = 4\n"
        );
    }

    #[test]
    fn a_new_name_the_file_already_sets_is_left_alone() {
        let text = "main_agent = \"codex\"\n\n[crew]\nmain = \"claude-code\"\n";
        assert!(renames_in(text).is_empty());
    }

    #[test]
    fn the_main_agent_joins_a_crew_table_first() {
        let text = "main_agent = \"codex\"\n\n[crew]\nworker = [\"codex\"]\n";
        let edited = edited(text, &renames_in(text)).unwrap();
        assert_eq!(edited, "\n[crew]\nmain = \"codex\"\nworker = [\"codex\"]\n");
    }

    #[test]
    fn current_names_and_files_that_do_not_read_need_nothing() {
        assert!(renames_in("workflow = \"captain\"\n[crew]\nmain = \"codex\"\n").is_empty());
        assert!(renames_in("main_agent = ").is_empty());
    }

    #[test]
    fn every_new_name_is_a_setting() {
        let defaults: Table = toml::Table::try_from(Settings::default()).unwrap();
        let mut known = Vec::new();
        collect_leaves(&mut Vec::new(), &defaults, &mut known);
        let known: Vec<String> = known.into_iter().map(|(path, _)| path.join(".")).collect();
        // Roles and fallback lists are keyed by name, so they have no defaults to look in.
        let keyed = |key: &str| key.starts_with("roles.") || key.starts_with("fallback.");
        for (_, new) in KEYS.iter().filter(|(_, new)| !keyed(new)) {
            assert!(known.iter().any(|key| key == new), "{new} is no setting");
        }
        for (_, new) in TABLES.iter().filter(|(_, new)| !keyed(new)) {
            let under = format!("{new}.");
            assert!(
                known.iter().any(|key| key.starts_with(&under)),
                "{new} holds no setting"
            );
        }
    }
}
