//! Writes one setting into a TOML file, keeping its comments, order, and formatting.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use argonauts_core::layout::project_files;
use argonauts_core::paths::{LOCAL_CONFIG_EXCLUDE, exclude_from_git};
use argonauts_core::settings::Settings;
use toml_edit::{Decor, DocumentMut, InlineTable, Item, Key, Table, TableLike, Value};

use crate::SettingsError;
use crate::catalog::setting_meta;
use crate::resolve::{SESSION_ENV, is_safety_key};

/// Sets one dotted key in a TOML file. A new key gets its catalog description as a comment, a
/// bare word reads as a string as with `--set`, and a table value merges key by key. Safety keys
/// are refused inside an agent session, and a repo's `config.local.toml` is git-excluded before
/// it's written. Unknown keys and disallowed values aren't refused yet.
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

fn write(
    file: &Path,
    key: &str,
    toml_value: &str,
    in_agent_session: bool,
) -> Result<(), SettingsError> {
    let invalid = |message: String| SettingsError::Invalid {
        layer: file.display().to_string(),
        message,
    };
    let path = key_path(key).map_err(invalid)?;
    let key = &path.join(".");
    let value = parse_value(toml_value).map_err(|message| invalid(format!("{key}: {message}")))?;
    if in_agent_session && let Some(safety) = safety_keys(key, &value).first() {
        return Err(refused_in_agent_session(safety));
    }

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
    let mut text = doc.to_string();
    if only_comments {
        text = format!("{}\n\n{text}", original.trim_end());
    }
    // A value of the wrong type never reaches the file.
    toml::from_str::<Settings>(&text)
        .map_err(|err| invalid(format!("{key}: {}", err.message())))?;

    // Exclude before writing, so your own settings are never briefly committable.
    if let Some(repo_root) = local_config_repo(file) {
        exclude_from_git(repo_root, LOCAL_CONFIG_EXCLUDE)
            .map_err(|err| io_error(&repo_root.join(".git/info/exclude"), err))?;
    }
    write_replacing(&target, &text)
}

fn remove(file: &Path, key: &str, in_agent_session: bool) -> Result<bool, SettingsError> {
    let invalid = |message: String| SettingsError::Invalid {
        layer: file.display().to_string(),
        message,
    };
    let path = key_path(key).map_err(invalid)?;
    let key = &path.join(".");
    let target = real_path(file)?;
    let _turn = lock_folder(&target)?;
    let original = read(&target)?;
    let mut doc: DocumentMut = original
        .parse()
        .map_err(|err: toml_edit::TomlError| invalid(err.to_string()))?;
    let Some(removed) = remove_at(doc.as_table_mut(), &path) else {
        return Ok(false);
    };
    if in_agent_session {
        let mut keys = vec![key.clone()];
        keys.extend(leaf_keys(key, &removed));
        if let Some(safety) = keys.iter().find(|key| is_safety_key(key)) {
            return Err(refused_in_agent_session(safety));
        }
    }
    let text = doc.to_string();
    toml::from_str::<Settings>(&text)
        .map_err(|err| invalid(format!("{key}: {}", err.message())))?;
    write_replacing(&target, &text)?;
    Ok(true)
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

fn has_comment(decor: &Decor) -> bool {
    decor
        .prefix()
        .and_then(|prefix| prefix.as_str())
        .is_some_and(|prefix| prefix.contains('#'))
}

fn leaf_keys(prefix: &str, item: &Item) -> Vec<String> {
    let Some(table) = item.as_table_like() else {
        return Vec::new();
    };
    table
        .iter()
        .flat_map(|(name, inner)| {
            let key = format!("{prefix}.{name}");
            let mut keys = leaf_keys(&key, inner);
            keys.push(key);
            keys
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
    let plain = |segment: &&str| {
        segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    };
    if let Some(odd) = path.iter().find(|segment| !plain(segment)) {
        return Err(format!(
            "{odd:?} in {key:?} isn't a plain key name; use letters, digits, _ and -"
        ));
    }
    Ok(path)
}

fn refused_in_agent_session(safety: &str) -> SettingsError {
    SettingsError::Refused(format!(
        "{safety} is a safety setting and can't be changed from inside an agent session"
    ))
}

/// Every safety key that writing `toml_value` at `key` would change, including keys inside a
/// table value such as `endpoints.ollama = { base_url = "..." }`. The confirm card names them.
pub fn safety_keys_in(key: &str, toml_value: &str) -> Vec<String> {
    let key: Vec<&str> = key.split('.').map(str::trim).collect();
    let value = parse_value(toml_value).unwrap_or_else(|_| Value::from(toml_value));
    safety_keys(&key.join("."), &value)
}

/// [`safety_keys_in`] for a key as it will be written, so spaces around a dot can't dodge the
/// rule.
fn safety_keys(key: &str, value: &Value) -> Vec<String> {
    let mut found = Vec::new();
    if is_safety_key(key) {
        found.push(key.to_owned());
    }
    if let Value::InlineTable(table) = value {
        for (name, inner) in table.iter() {
            found.extend(safety_keys(&format!("{key}.{name}"), inner));
        }
    }
    found
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
    match comment {
        // A comment can't sit inside an inline table or between the parts of a dotted key.
        Some(comment) if !inline && !table.is_dotted() => {
            let comment = comment.replace(['\n', '\r'], " ");
            let key = Key::new(name).with_leaf_decor(Decor::new(format!("# {comment}\n"), " "));
            table.entry_format(&key).or_insert(item);
        }
        _ => {
            table.insert(name, item);
        }
    }
}

/// The repo root when `file` is a repo's `.argonauts/config.local.toml`.
fn local_config_repo(file: &Path) -> Option<&Path> {
    if file.file_name()? != project_files::CONFIG_LOCAL {
        return None;
    }
    let folder = file.parent()?;
    if folder.file_name()? != ".argonauts" {
        return None;
    }
    folder.parent()
}

/// The file to write. A config that's a link, such as one kept in a dotfiles repo, is written
/// through, so the link stays, even when what it points at doesn't exist yet.
fn real_path(file: &Path) -> Result<PathBuf, SettingsError> {
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
/// never lose a change. A missing folder holds no file to race over.
fn lock_folder(target: &Path) -> Result<Option<fs::File>, SettingsError> {
    let dir = folder_of(target);
    let handle = match fs::File::open(dir) {
        Ok(handle) => handle,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io_error(dir, err)),
    };
    handle.lock().map_err(|err| io_error(dir, err))?;
    Ok(Some(handle))
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
fn write_replacing(target: &Path, text: &str) -> Result<(), SettingsError> {
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

fn io_error(path: &Path, err: io::Error) -> SettingsError {
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
# My settings. Hands off, robots.
preset = \"peer-crew\"   # the default

[ui]
# I like vim.
keys = \"default\"
";
        fs::write(&file, before).unwrap();
        write(&file, "ui.keys", "vim", false).unwrap();
        write(&file, "preset", "\"coordinated-crew\"", false).unwrap();
        write(&file, "ui.theme", "\"light\"", false).unwrap();
        write(&file, "approvals.merge.mode", "\"auto\"", false).unwrap();
        let after = fs::read_to_string(&file).unwrap();
        assert!(
            after.starts_with("# My settings. Hands off, robots.\n"),
            "{after}"
        );
        assert!(
            after.contains("preset = \"coordinated-crew\"   # the default\n"),
            "{after}"
        );
        assert!(after.contains("# I like vim.\nkeys = \"vim\"\n"), "{after}");
        assert!(after.contains("# The palette. Auto follows"), "{after}");
        assert!(
            after.contains("[approvals.merge]\n# The merge checkpoint: supervised waits"),
            "{after}"
        );
        assert!(after.contains("mode = \"auto\"\n"), "{after}");
        assert!(!after.contains("[approvals]\n"), "{after}");
        let settings: Settings = toml::from_str(&after).unwrap();
        assert_eq!(settings.preset, "coordinated-crew");
        assert_eq!(settings.ui.keys, argonauts_core::KeyScheme::Vim);
    }

    #[test]
    fn write_setting_creates_the_file_and_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("home/.argonauts/config.toml");
        write(&file, "agents.codex.model", "gpt-x", false).unwrap();
        write(&file, "roles.reviewer.workers", "[\"codex\"]", false).unwrap();
        let settings: Settings = toml::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(settings.agents["codex"].model.as_deref(), Some("gpt-x"));
        let reviewer = &settings.roles[&argonauts_core::names::RoleName::Reviewer];
        assert_eq!(reviewer.workers.len(), 1);
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
        fs::write(&file, "[roles.owner.native_fanout]\nmax = 3\n").unwrap();
        write(&file, "roles.owner.native_fanout", "off", false).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.contains("native_fanout = \"off\""), "{text}");
        let settings: Settings = toml::from_str(&text).unwrap();
        let owner = &settings.roles[&argonauts_core::names::RoleName::Owner];
        use argonauts_core::settings::sessions::{FanoutSwitch, NativeFanout};
        assert_eq!(
            owner.native_fanout,
            Some(NativeFanout::Switch(FanoutSwitch::Off))
        );
    }

    #[test]
    fn wrong_values_never_reach_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        fs::write(&file, "[budget]\nrun_usd = 20.0\n").unwrap();
        for (key, value) in [
            ("budget.run_usd", "\"lots\""),
            ("budget..run_usd", "5"),
            ("budget", "5"),
            ("budget.run_usd.x", "5"),
            ("budget.run_usd", "5\nsandbox.network = true"),
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
            "[budget]\nrun_usd = 20.0\n"
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
            "[budget]\nrun_usd = 5.0\n[agents.codex]\nbinary = \"/opt/codex\"\n\
             [roles.owner.handoff]\nallow_api_billing = true\n",
        )
        .unwrap();
        write(&file, "budget", "{}", true).unwrap();
        write(&file, "agents.codex", "{ model = \"x\" }", true).unwrap();
        write(&file, "roles.owner.handoff", "{ mode = \"auto\" }", true).unwrap();
        let settings: Settings = toml::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(settings.budget.run_usd, Some(5.0));
        let codex = &settings.agents["codex"];
        assert_eq!(codex.binary.as_deref(), Some(Path::new("/opt/codex")));
        assert_eq!(codex.model.as_deref(), Some("x"));
        let owner = &settings.roles[&argonauts_core::names::RoleName::Owner];
        assert!(owner.handoff.allow_api_billing);
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
            safety_keys_in("gates.run", "[\"cargo test\"]"),
            ["gates.run"]
        );
        assert!(safety_keys_in("ui.keys", "vim").is_empty());
        write(&file, "ui.keys", "vim", true).unwrap();
        write(&file, "sandbox.network", "true", false).unwrap();
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
        write(&shared, "preset", "solo", false).unwrap();
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
# My settings. Hands off, robots.
preset = \"peer-crew\"   # the default

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
            after.starts_with("# My settings. Hands off, robots.\n"),
            "{after}"
        );
        assert!(
            after.contains("preset = \"peer-crew\"   # the default\n"),
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
        assert!(!missing.exists());
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
        let keys: Vec<String> = (0..8).map(|n| format!("ui.keymap.action_{n}")).collect();
        std::thread::scope(|scope| {
            for key in &keys {
                let file = &file;
                scope.spawn(move || write(file, key, "\"ctrl+x\"", false).unwrap());
            }
        });
        let settings: Settings = toml::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(settings.ui.keymap.len(), keys.len(), "a change was lost");
    }
}
