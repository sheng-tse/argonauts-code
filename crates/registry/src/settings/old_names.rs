//! Setting names from older versions of Argonauts. A file that still uses them reads as if it
//! used today's names, and [`rename_old_names`] rewrites it in them once, keeping the file as
//! it was beside it.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use toml_edit::{Decor, DocumentMut, Formatted, Item, Key, TableLike};

use crate::SettingsError;
use crate::catalog::setting_meta;
use crate::write::{io_error, lock_folder, real_path, write_replacing};

/// Keys that moved, each old name beside its new one, in the order they apply: a later move
/// sees what an earlier one wrote, so `budget.lines.cards` becomes `limits.lines.tasks` before
/// the rest of `[budget]` becomes `[limits]`. `*` stands for one name and `widget_*` for one
/// that starts so; what it stood for fills the `*` of the new key.
const MOVED: &[(&str, &str)] = &[
    ("preset", "workflow"),
    ("default_agent", "main_agent"),
    ("triage.all_hands", "ultracode"),
    ("triage.use_recipes", "triage.use_learned"),
    ("triage", "solo_or_crew"),
    ("roles.default", "roles.main"),
    ("roles.owner", "roles.worker"),
    ("roles.coordinator", "roles.orchestrator"),
    ("roles.main.handoff", "fallback.main"),
    ("roles.orchestrator.handoff", "fallback.orchestrator"),
    ("roles.worker.handoff", "fallback.workers"),
    ("roles.reviewer.handoff", "fallback.reviewers"),
    ("roles.judge.handoff", "fallback.judge"),
    (
        "roles.*.sandbox.agents_see_other_copies",
        "roles.*.sandbox.agents_see_other_worktrees",
    ),
    (
        "visibility.status_summary.default",
        "visibility.status_summary.main",
    ),
    (
        "visibility.status_summary.owner",
        "visibility.status_summary.worker",
    ),
    (
        "visibility.status_summary.coordinator",
        "visibility.status_summary.orchestrator",
    ),
    ("approvals.full_auto", "approvals.autopilot"),
    ("approvals.answered_by", "approvals.answers"),
    ("approvals.*.rules", "approvals.rules.*"),
    ("approvals.escalation", "approvals.stuck"),
    ("approvals.budget", "approvals.limit"),
    ("approvals.recipe_promotion", "approvals.workflow_promotion"),
    ("approvals.rules.escalation", "approvals.rules.stuck"),
    ("approvals.rules.budget", "approvals.rules.limit"),
    (
        "approvals.rules.recipe_promotion",
        "approvals.rules.workflow_promotion",
    ),
    (
        "approvals.rules.merge.min_changed_line_coverage",
        "approvals.rules.merge.min_test_coverage_of_changed_lines",
    ),
    (
        "approvals.rules.merge.max_combined_diff_lines",
        "approvals.rules.merge.max_changed_lines",
    ),
    (
        "approvals.approver.different_family",
        "approvals.approver.different_provider",
    ),
    ("gates", "checks"),
    ("handoff", "fallback"),
    ("fallback.fallback", "fallback.order"),
    ("fallback.max_handoffs", "fallback.max_fallbacks"),
    ("fallback.race_count", "fallback.together"),
    ("fallback.*.fallback", "fallback.*.order"),
    ("fallback.*.max_handoffs", "fallback.*.max_fallbacks"),
    ("fallback.*.race_count", "fallback.*.together"),
    ("spikes", "experiments"),
    (
        "sandbox.agents_see_other_copies",
        "sandbox.agents_see_other_worktrees",
    ),
    ("review.other_family", "review.other_provider"),
    ("review.per_card", "review.per_task"),
    ("divide.max_cards", "divide.max_tasks"),
    ("memory.max_per_card", "memory.max_per_task"),
    ("limits.usd_per_day", "limits.spend_per_day"),
    ("budget.run_usd", "limits.spend_per_run"),
    ("budget.run_quota", "limits.run_plan_usage"),
    ("budget.per_card_usd", "limits.per_task_usd"),
    ("budget.per_card_tokens", "limits.per_task_tokens"),
    ("budget.per_card_quota", "limits.per_task_plan_usage"),
    ("budget.per_card_minutes", "limits.per_task_minutes"),
    ("budget.on_hit", "limits.when_reached"),
    ("budget.lines.cards", "limits.lines.tasks"),
    ("budget.lines.spikes", "limits.lines.experiments"),
    ("budget", "limits"),
    ("ui.above_input.when_card", "ui.above_input.when_question"),
    ("ui.above_input.widgets", "ui.above_input.mods"),
    ("ui.views.*.widgets", "ui.views.*.mods"),
    ("ui.widgets.cards", "ui.mods.tasks"),
    ("ui.widgets", "ui.mods"),
    ("ui.keymap.widget_*", "ui.keymap.mod_*"),
    ("ui.keymap.focus_card", "ui.keymap.focus_question"),
    ("ui.colors.widget-border", "ui.colors.mod-border"),
    ("ui.colors.mode-shadow", "ui.colors.mode-auto-preview"),
];

/// Values that took new words, by the key that holds them, after the keys moved. A list has
/// each of its items renamed.
const RENAMED_VALUES: &[(&str, &str, &str)] = &[
    ("workflow", "peer-crew", "council"),
    ("workflow", "coordinated-crew", "orchestrator"),
    ("workflow", "solo-reviewed", "maker-checker"),
    ("phases", "peer-crew", "council"),
    ("phases", "coordinated-crew", "orchestrator"),
    ("phases", "solo-reviewed", "maker-checker"),
    ("drafting.by", "coordinator", "orchestrator"),
    ("judging.method", "coordinator", "orchestrator"),
    ("routing.algorithm", "coordinator", "orchestrator"),
    ("review.assign", "coordinator", "orchestrator"),
    ("approvals.answers", "lead", "main"),
    ("understand.when", "lead", "main"),
    ("ui.mods.context.session", "lead", "main"),
    ("approvals.*.mode", "shadow", "auto_preview"),
    ("approvals.*.mode", "full_auto", "autopilot"),
    ("goal.checker", "other_company", "other_provider"),
    ("sessions.lifetime", "card", "task"),
    ("roles.*.lifetime", "card", "task"),
    ("limits.when_reached", "handoff", "fallback"),
    (
        "approvals.rules.workflow_promotion.change_classes",
        "card_split",
        "task_split",
    ),
    (
        "approvals.rules.workflow_promotion.change_classes",
        "card_budget",
        "task_budget",
    ),
    ("ui.above_input.mods", "cards", "tasks"),
    ("ui.views.*.mods", "cards", "tasks"),
];

/// One old name a file used, and what it reads as now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OldName {
    pub old: String,
    pub new: String,
}

impl OldName {
    fn key(old: &[String], new: &[String]) -> Self {
        Self {
            old: old.join("."),
            new: new.join("."),
        }
    }

    fn value(key: &[String], old: &str, new: &str) -> Self {
        let key = key.join(".");
        Self {
            old: format!("{key} = \"{old}\""),
            new: format!("\"{new}\""),
        }
    }
}

impl fmt::Display for OldName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} → {}", self.old, self.new)
    }
}

/// A settings file [`rename_old_names`] rewrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renamed {
    /// The file as it was.
    pub backup: PathBuf,
    pub names: Vec<OldName>,
}

/// Reads a layer's old names as their new ones, and says which there were.
pub(crate) fn read_as_new(table: &mut toml::Table) -> Vec<OldName> {
    translate(table)
}

/// The warning for a layer that still uses old names, which still read.
pub(crate) fn warning(source: &str, names: &[OldName]) -> String {
    format!(
        "{source} uses older setting names, which still read as the new ones: {}",
        listed(names)
    )
}

impl Renamed {
    /// The names, the first few of them when there are many.
    pub fn listed(&self) -> String {
        listed(&self.names)
    }
}

fn listed(names: &[OldName]) -> String {
    const SHOWN: usize = 4;
    let mut text = names
        .iter()
        .take(SHOWN)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > SHOWN {
        text.push_str(&format!(", and {} more", names.len() - SHOWN));
    }
    text
}

/// Rewrites `file` in today's setting names and keeps the file as it was beside it, as
/// `config.toml.bak`. Nothing is written when the file isn't there, doesn't parse (reading it
/// says what's wrong), or uses no old names, so a file is rewritten once.
pub fn rename_old_names(file: &Path) -> Result<Option<Renamed>, SettingsError> {
    let target = real_path(file)?;
    if !target.is_file() {
        return Ok(None);
    }
    let _turn = lock_folder(&target)?;
    let text = fs::read_to_string(&target).map_err(|err| io_error(&target, err))?;
    let Ok(mut doc) = text.parse::<DocumentMut>() else {
        return Ok(None);
    };
    let names = translate(&mut doc);
    if names.is_empty() {
        return Ok(None);
    }
    let backup = free_backup(&target);
    fs::copy(&target, &backup).map_err(|err| io_error(&backup, err))?;
    write_replacing(&target, &doc.to_string())?;
    Ok(Some(Renamed { backup, names }))
}

/// `config.toml.bak`, or `config.toml.bak2` and on when an earlier backup is there.
fn free_backup(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    (1..)
        .map(|n| match n {
            1 => target.with_file_name(format!("{name}.bak")),
            n => target.with_file_name(format!("{name}.bak{n}")),
        })
        .find(|path| !path.exists())
        .unwrap_or_else(|| target.with_file_name(format!("{name}.bak")))
}

/// The settings as a tree of named tables, so the same moves apply to a layer as it's read
/// and to a file as it's rewritten.
trait Tree {
    type Node;
    /// The names in the table at `path`; none when no table is there.
    fn names(&self, path: &[String]) -> Vec<String>;
    /// Removes what's at `path`, and the tables that leaves empty.
    fn take(&mut self, path: &[String]) -> Option<Self::Node>;
    /// Puts `node` at `path`. Where a table is there already the two merge, and wherever both
    /// set a key the one there already wins, since it's the new name.
    fn put(&mut self, path: &[String], node: Self::Node);
    /// Renames the string at `path`, or each item of the list there, that reads `from`.
    fn rename_value(&mut self, path: &[String], from: &str, to: &str) -> bool;
}

fn translate(tree: &mut impl Tree) -> Vec<OldName> {
    let mut found = Vec::new();
    // Before workflows had names of their own, `workflow` named the phases file a preset ran;
    // a preset is a workflow now.
    if tree.names(&[]).iter().any(|name| name == "preset") {
        move_key(
            tree,
            &["workflow".to_owned()],
            &["phases".to_owned()],
            &mut found,
        );
    }
    for (old, new) in MOVED {
        for (from, to) in matches(tree, old, new) {
            move_key(tree, &from, &to, &mut found);
        }
    }
    for (key, old, new) in RENAMED_VALUES {
        for (path, _) in matches(tree, key, key) {
            if tree.rename_value(&path, old, new) {
                found.push(OldName::value(&path, old, new));
            }
        }
    }
    found
}

fn move_key(tree: &mut impl Tree, from: &[String], to: &[String], found: &mut Vec<OldName>) {
    if let Some(node) = tree.take(from) {
        tree.put(to, node);
        found.push(OldName::key(from, to));
    }
}

/// Each key in `tree` that `pattern` names, with the key `new` names for it.
fn matches(tree: &impl Tree, pattern: &str, new: &str) -> Vec<(Vec<String>, Vec<String>)> {
    let pattern: Vec<&str> = pattern.split('.').collect();
    let mut found = Vec::new();
    walk(tree, &pattern, &mut Vec::new(), &mut Vec::new(), &mut found);
    found
        .into_iter()
        .map(|(path, stood_for)| (path, fill(new, &stood_for)))
        .collect()
}

fn walk(
    tree: &impl Tree,
    pattern: &[&str],
    path: &mut Vec<String>,
    stood_for: &mut Vec<String>,
    found: &mut Vec<(Vec<String>, Vec<String>)>,
) {
    let Some((segment, rest)) = pattern.split_first() else {
        found.push((path.clone(), stood_for.clone()));
        return;
    };
    for name in tree.names(path) {
        let Some(part) = fits(segment, &name) else {
            continue;
        };
        let wild = part.is_some();
        stood_for.extend(part);
        path.push(name);
        walk(tree, rest, path, stood_for, found);
        path.pop();
        if wild {
            stood_for.pop();
        }
    }
}

/// `None` when `name` isn't what `segment` names, else what its `*` stood for, if it has one.
fn fits(segment: &str, name: &str) -> Option<Option<String>> {
    match segment.strip_suffix('*') {
        Some(start) => name
            .strip_prefix(start)
            .filter(|rest| !rest.is_empty())
            .map(|rest| Some(rest.to_owned())),
        None => (segment == name).then_some(None),
    }
}

fn fill(pattern: &str, stood_for: &[String]) -> Vec<String> {
    let mut parts = stood_for.iter();
    pattern
        .split('.')
        .map(|segment| match segment.strip_suffix('*') {
            Some(start) => format!("{start}{}", parts.next().map_or("", String::as_str)),
            None => segment.to_owned(),
        })
        .collect()
}

impl Tree for toml::Table {
    type Node = toml::Value;

    fn names(&self, path: &[String]) -> Vec<String> {
        table_at(self, path)
            .map(|table| table.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn take(&mut self, path: &[String]) -> Option<toml::Value> {
        let (last, parents) = path.split_last()?;
        let node = table_at_mut(self, parents)?.remove(last)?;
        prune(self, parents);
        Some(node)
    }

    fn put(&mut self, path: &[String], node: toml::Value) {
        let Some((last, parents)) = path.split_last() else {
            return;
        };
        let mut table = self;
        for name in parents {
            let child = table
                .entry(name.clone())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            let toml::Value::Table(child) = child else {
                return;
            };
            table = child;
        }
        merge_value(table, last, node);
    }

    fn rename_value(&mut self, path: &[String], from: &str, to: &str) -> bool {
        let Some((last, parents)) = path.split_last() else {
            return false;
        };
        table_at_mut(self, parents)
            .and_then(|table| table.get_mut(last))
            .is_some_and(|value| rename_in_value(value, from, to))
    }
}

fn table_at<'a>(table: &'a toml::Table, path: &[String]) -> Option<&'a toml::Table> {
    path.iter()
        .try_fold(table, |table, name| table.get(name)?.as_table())
}

fn table_at_mut<'a>(table: &'a mut toml::Table, path: &[String]) -> Option<&'a mut toml::Table> {
    path.iter()
        .try_fold(table, |table, name| table.get_mut(name)?.as_table_mut())
}

fn prune(table: &mut toml::Table, path: &[String]) {
    let Some((name, rest)) = path.split_first() else {
        return;
    };
    if let Some(toml::Value::Table(child)) = table.get_mut(name) {
        prune(child, rest);
        if child.is_empty() {
            table.remove(name);
        }
    }
}

fn merge_value(table: &mut toml::Table, name: &str, node: toml::Value) {
    match (table.get_mut(name), node) {
        (None, node) => {
            table.insert(name.to_owned(), node);
        }
        (Some(toml::Value::Table(there)), toml::Value::Table(moved)) => {
            for (inner, value) in moved {
                merge_value(there, &inner, value);
            }
        }
        _ => {}
    }
}

fn rename_in_value(value: &mut toml::Value, from: &str, to: &str) -> bool {
    match value {
        toml::Value::String(text) if text == from => {
            *text = to.to_owned();
            true
        }
        toml::Value::Array(items) => {
            // Every item, not just the first that matches.
            let mut renamed = false;
            for item in items.iter_mut() {
                renamed |= rename_in_value(item, from, to);
            }
            renamed
        }
        _ => false,
    }
}

impl Tree for DocumentMut {
    type Node = (Key, Item);

    fn names(&self, path: &[String]) -> Vec<String> {
        like_at(self.as_table(), path)
            .map(|table| table.iter().map(|(name, _)| name.to_owned()).collect())
            .unwrap_or_default()
    }

    fn take(&mut self, path: &[String]) -> Option<(Key, Item)> {
        let (last, parents) = path.split_last()?;
        let parent = like_at_mut(self.as_table_mut(), parents)?;
        let key = parent.key(last)?.clone();
        let item = parent.remove(last)?;
        prune_doc(self.as_table_mut(), parents);
        Some((key, item))
    }

    fn put(&mut self, path: &[String], (key, item): (Key, Item)) {
        let Some((last, parents)) = path.split_last() else {
            return;
        };
        let mut table: &mut dyn TableLike = self.as_table_mut();
        for name in parents {
            if !table.contains_key(name) {
                let mut child = toml_edit::Table::new();
                child.set_implicit(true);
                table.insert(name, Item::Table(child));
            }
            let Some(child) = table.get_mut(name).and_then(Item::as_table_like_mut) else {
                return;
            };
            table = child;
        }
        merge_item(table, last, &key, item, &path.join("."));
    }

    fn rename_value(&mut self, path: &[String], from: &str, to: &str) -> bool {
        let Some((last, parents)) = path.split_last() else {
            return false;
        };
        let Some(parent) = like_at_mut(self.as_table_mut(), parents) else {
            return false;
        };
        let renamed = parent
            .get_mut(last)
            .and_then(Item::as_value_mut)
            .is_some_and(|value| rename_in_edit(value, from, to));
        if renamed && let Some(mut key) = parent.key_mut(last) {
            describe(key.leaf_decor_mut(), &path.join("."));
        }
        renamed
    }
}

fn like_at<'a>(table: &'a dyn TableLike, path: &[String]) -> Option<&'a dyn TableLike> {
    path.iter()
        .try_fold(table, |table, name| table.get(name)?.as_table_like())
}

fn like_at_mut<'a>(table: &'a mut dyn TableLike, path: &[String]) -> Option<&'a mut dyn TableLike> {
    path.iter().try_fold(table, |table, name| {
        table.get_mut(name)?.as_table_like_mut()
    })
}

/// A table emptied by a move goes, unless a comment of yours sits on it.
fn prune_doc(table: &mut dyn TableLike, path: &[String]) {
    let Some((name, rest)) = path.split_first() else {
        return;
    };
    let Some(item) = table.get_mut(name) else {
        return;
    };
    let commented = item
        .as_table()
        .is_some_and(|child| comment_of(child.decor()).is_some());
    let Some(child) = item.as_table_like_mut() else {
        return;
    };
    prune_doc(child, rest);
    if child.is_empty() && !commented {
        table.remove(name);
    }
}

fn merge_item(table: &mut dyn TableLike, name: &str, key: &Key, item: Item, dotted: &str) {
    let Some(there) = table.get_mut(name) else {
        let renamed = Key::new(name).with_leaf_decor(key.leaf_decor().clone());
        table.entry_format(&renamed).or_insert(item);
        describe_all(table, name, dotted);
        return;
    };
    let (Some(there), Some(moved)) = (there.as_table_like_mut(), item.as_table_like()) else {
        return;
    };
    for (inner, value) in moved.iter() {
        let Some(inner_key) = moved.key(inner) else {
            continue;
        };
        merge_item(
            there,
            inner,
            inner_key,
            value.clone(),
            &format!("{dotted}.{inner}"),
        );
    }
}

/// What a moved key's comment says, in today's words: a comment of one line above a key is
/// the description an older Argonauts wrote there, so it's replaced with today's.
fn describe_all(table: &mut dyn TableLike, name: &str, dotted: &str) {
    if let Some(mut key) = table.key_mut(name) {
        describe(key.leaf_decor_mut(), dotted);
    }
    let Some(child) = table.get_mut(name).and_then(Item::as_table_like_mut) else {
        return;
    };
    let names: Vec<String> = child.iter().map(|(inner, _)| inner.to_owned()).collect();
    for inner in names {
        describe_all(child, &inner, &format!("{dotted}.{inner}"));
    }
}

fn describe(decor: &mut Decor, key: &str) {
    let Some(prefix) = comment_of(decor) else {
        return;
    };
    let Some(meta) = setting_meta(key).filter(|meta| !meta.description.is_empty()) else {
        return;
    };
    let comments = prefix
        .lines()
        .filter(|line| line.trim_start().starts_with('#'))
        .count();
    if comments != 1 {
        return;
    }
    let blank: String = prefix
        .split_inclusive('\n')
        .take_while(|line| line.trim().is_empty())
        .collect();
    let description = meta.description.replace(['\n', '\r'], " ");
    decor.set_prefix(format!("{blank}# {description}\n"));
}

fn comment_of(decor: &Decor) -> Option<String> {
    decor
        .prefix()
        .and_then(|prefix| prefix.as_str())
        .filter(|prefix| prefix.contains('#'))
        .map(str::to_owned)
}

fn rename_in_edit(value: &mut toml_edit::Value, from: &str, to: &str) -> bool {
    match value {
        toml_edit::Value::String(text) if text.value() == from => {
            let decor = text.decor().clone();
            *text = Formatted::new(to.to_owned());
            *text.decor_mut() = decor;
            true
        }
        toml_edit::Value::Array(items) => {
            // Every item, not just the first that matches.
            let mut renamed = false;
            for item in items.iter_mut() {
                renamed |= rename_in_edit(item, from, to);
            }
            renamed
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests;
