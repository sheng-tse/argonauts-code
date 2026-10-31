//! Writing mod files: a page's changes into a mod's file, keeping its comments and order, and
//! a reset, which deletes a file so the one under it is in use again. A file is checked before
//! it's written, so a page can't leave one that doesn't load.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use argonauts_core::settings::mods::{check, save};
use toml_edit::{DocumentMut, Item, Table, TableLike, Value};

/// `<dir>/<name>.toml`.
pub(crate) fn mod_path(dir: &Path, name: &str) -> PathBuf {
    save::path_of(dir, name)
}

/// Keys of `[data]` that can't stand beside the one being written: a mod reads a built-in
/// source or a command, never both.
fn displaced_by(key: &str) -> &'static [&'static str] {
    match key {
        "data.source" => &["command", "every", "timeout"],
        "data.command" => &["source"],
        // The bottom line's list of CLIs stands in for `show`, so choosing how to pick them
        // drops the list.
        "plan-usage.show" => &["clis"],
        _ => &[],
    }
}

/// Puts `value` at `key`. A key that's there keeps its line as it was written: the spacing
/// before `=`, and the comment after the value.
fn put(table: &mut dyn TableLike, key: &str, mut value: Value) {
    match table.get_mut(key).and_then(Item::as_value_mut) {
        Some(old) => {
            *value.decor_mut() = old.decor().clone();
            *old = value;
        }
        None => {
            table.insert(key, Item::Value(value));
        }
    }
}

/// `text` with each pair written into it: a top-level key, or one key of a table, as
/// `data.every` is. The value is TOML text.
fn apply(text: &str, pairs: &[(String, String)]) -> anyhow::Result<String> {
    let mut doc: DocumentMut = text.parse().context("the file isn't valid TOML")?;
    for (key, value) in pairs {
        let value: Value = value
            .parse()
            .map_err(|err| anyhow::anyhow!("{key}: {value} isn't a TOML value: {err}"))?;
        match key.split_once('.') {
            None => put(doc.as_table_mut(), key, value),
            Some((table, leaf)) => {
                let slot = &mut doc[table];
                if slot.is_none() {
                    *slot = Item::Table(Table::new());
                }
                let Some(table_like) = slot.as_table_like_mut() else {
                    bail!("{table} isn't a table in this file");
                };
                put(table_like, leaf, value);
                for gone in displaced_by(key) {
                    table_like.remove(gone);
                }
            }
        }
    }
    Ok(doc.to_string())
}

/// Writes `pairs` into the mod's file in `dir`. A file that isn't there yet starts from
/// `start`, the text of the mod in use, so a copy of a built-in card keeps its comments.
/// Nothing is written when the result wouldn't load.
pub(crate) fn save(
    dir: &Path,
    name: &str,
    start: &str,
    pairs: &[(String, String)],
) -> anyhow::Result<PathBuf> {
    let path = mod_path(dir, name);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => start.to_owned(),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    let text = apply(&text, pairs)?;
    let found = check::problems(name, &text);
    if !found.is_empty() {
        bail!(
            "that would leave a file that doesn't load: {}",
            found.join("; ")
        );
    }
    write(&path, &text)?;
    Ok(path)
}

/// Writes through a draft beside the file, so a reader never sees half of it.
pub(crate) fn write(path: &Path, text: &str) -> anyhow::Result<()> {
    save::write_whole(path, text).with_context(|| format!("saving {}", path.display()))
}

/// Deletes the mod's file in `dir`. False when there was none.
pub(crate) fn reset(dir: &Path, name: &str) -> anyhow::Result<bool> {
    let path = mod_path(dir, name);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| format!("deleting {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::mods::{ModBody, ModForm, ModSource, bottom_line, builtin};

    use super::*;

    #[test]
    fn choosing_how_the_line_picks_its_clis_drops_the_list() {
        let text = "[plan-usage]\nshow = \"all\"\nclis = [{ cli = \"codex\" }]\n";
        let pairs = [("plan-usage.show".to_owned(), "\"near-limit\"".to_owned())];
        let saved = apply(text, &pairs).unwrap();
        assert_eq!(saved, "[plan-usage]\nshow = \"near-limit\"\n");
    }

    #[test]
    fn an_automatic_choice_with_the_same_show_saves_without_the_list() {
        use argonauts_core::settings::mods::bottom_line::PageEdits as _;

        let text = "[plan-usage]\nshow = \"all\"\nclis = [{ cli = \"codex\" }]\n";
        let before = bottom_line::read(text).unwrap();
        let mut after = before.clone();
        after.plan_usage.clis = None;
        let pairs = after.changes_from(&before);
        assert!(!pairs.is_empty(), "the page has something to save");
        let saved = apply(text, &pairs).unwrap();
        assert_eq!(bottom_line::read(&saved).unwrap(), after, "{saved}");
        assert!(!saved.contains("clis"), "{saved}");
    }

    fn pair(key: &str, value: &str) -> (String, String) {
        (key.to_owned(), value.to_owned())
    }

    fn context() -> &'static str {
        builtin::builtin("context").unwrap().text
    }

    #[test]
    fn a_change_to_a_built_in_card_writes_a_commented_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = save(
            dir.path(),
            "context",
            context(),
            &[pair("form", "\"line\"")],
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# How full the main session's context is"),
            "{text}"
        );
        assert!(text.contains("# full · compact · line"), "{text}");
        let file = check::read("context", &text).unwrap();
        assert_eq!(file.form, ModForm::Line);
        assert_eq!(file.body, ModBody::Stack);
    }

    #[test]
    fn a_second_change_goes_into_the_file_that_is_there() {
        let dir = tempfile::tempdir().unwrap();
        save(
            dir.path(),
            "context",
            context(),
            &[pair("form", "\"line\"")],
        )
        .unwrap();
        let path = save(
            dir.path(),
            "context",
            "never read",
            &[pair("body", "\"meters\""), pair("levels.warn", "60.0")],
        )
        .unwrap();
        let file = check::read("context", &std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(file.form, ModForm::Line);
        assert_eq!(file.body, ModBody::Meters);
        assert_eq!(file.levels.warn, 60.0);
    }

    #[test]
    fn a_source_and_a_command_replace_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let checks = builtin::STARTERS
            .iter()
            .find(|(name, _)| *name == "checks")
            .unwrap()
            .1;
        let path = save(
            dir.path(),
            "checks",
            checks,
            &[pair("data.source", "\"spend\"")],
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let file = check::read("checks", &text).unwrap();
        assert!(matches!(file.data, ModSource::Builtin { .. }), "{text}");
        let pairs = [
            pair("data.command", "[\"./ci.sh\"]"),
            pair("data.every", "\"5m\""),
        ];
        let path = save(dir.path(), "checks", "", &pairs).unwrap();
        let file = check::read("checks", &std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(
            matches!(&file.data, ModSource::Command { command, .. } if command == &["./ci.sh"])
        );
    }

    #[test]
    fn nothing_is_written_when_the_result_wouldnt_load() {
        let dir = tempfile::tempdir().unwrap();
        let err = save(dir.path(), "context", context(), &[pair("body", "\"pie\"")]).unwrap_err();
        assert!(
            format!("{err:#}").contains("that would leave a file that doesn't load: body is"),
            "{err:#}"
        );
        assert!(!mod_path(dir.path(), "context").exists());
        let err = save(dir.path(), "context", context(), &[pair("form", "line")]).unwrap_err();
        assert!(format!("{err:#}").contains("isn't a TOML value"), "{err:#}");
    }

    #[test]
    fn the_bottom_line_s_file_is_checked_by_its_own_rules() {
        let dir = tempfile::tempdir().unwrap();
        let start = builtin::builtin(builtin::BOTTOM_LINE).unwrap().text;
        let path = save(
            dir.path(),
            builtin::BOTTOM_LINE,
            start,
            &[
                pair("plan-usage.as", "\"left\""),
                pair("right", "[\"plan-usage\"]"),
            ],
        )
        .unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("# The line under the input."), "{text}");
        let file = bottom_line::read(&text).unwrap();
        assert_eq!(file.right, ["plan-usage"]);
        assert_eq!(
            file.plan_usage.r#as,
            argonauts_core::settings::bottom_line::PlanUsageAs::Left
        );
        let err = save(
            dir.path(),
            builtin::BOTTOM_LINE,
            start,
            &[pair("plan-usage.windows", "\"all\"")],
        );
        let said = format!("{:#}", err.unwrap_err());
        assert!(
            said.contains("doesn't load: plan-usage.windows: unknown variant `all`"),
            "{said}"
        );
    }

    #[test]
    fn a_reset_deletes_the_file_once() {
        let dir = tempfile::tempdir().unwrap();
        save(
            dir.path(),
            "context",
            context(),
            &[pair("form", "\"line\"")],
        )
        .unwrap();
        assert!(reset(dir.path(), "context").unwrap());
        assert!(!reset(dir.path(), "context").unwrap());
    }
}
