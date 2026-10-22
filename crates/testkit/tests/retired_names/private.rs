//! Real home folders, email addresses and scratch folders: none belongs in a tracked file,
//! recordings included. Sample text uses a neutral folder and a reserved mail domain. A
//! recording also keeps none of the recording machine's own setup.

use serde_json::Value;

use super::report::Place;
use super::table::{self, Private, Setup, Table};
use super::tree::File;

pub fn places(table: &Table, files: &[File]) -> Vec<Place> {
    let private = &table.private;
    let banned: Vec<(usize, u64)> = private
        .banned
        .iter()
        .map(|text| {
            let hash = u64::from_str_radix(text.hash.trim_start_matches("0x"), 16)
                .unwrap_or_else(|e| panic!("{}: {e}", text.what));
            (text.length, hash)
        })
        .collect();

    let mut places = Vec::new();
    for file in files {
        let Some(text) = file.text.as_deref() else {
            continue;
        };
        let addresses_are_authors = table::fits_any(&private.authors_addresses_in, &file.path);
        for (number, line) in text.lines().enumerate() {
            let mut found = Vec::new();
            homes(line, private, &mut found);
            if !addresses_are_authors {
                mail(line, private, &mut found);
            }
            scratch_folders(line, private, &mut found);
            banned_texts(line, &banned, private, &mut found);
            for (text, fix) in found {
                places.push(Place::new(&file.path, Some(number + 1), &text, fix));
            }
        }
    }
    places
}

/// A home folder that isn't one of the neutral names, spelled as a path or the way Claude
/// Code names a project's folder, with a dash for each slash.
fn homes<'t>(line: &str, private: &'t Private, found: &mut Vec<(String, &'t str)>) {
    for (root, ends_at_dash) in [
        ("/Users/", false),
        ("/home/", false),
        ("\\Users\\", false),
        ("-Users-", true),
    ] {
        for (at, _) in line.match_indices(root) {
            // Part of a longer path (`{dir}/home/skills`) is a folder that happens to be
            // called home, not anyone's.
            let inside_a_path = line[..at]
                .chars()
                .next_back()
                .is_some_and(|before| before.is_alphanumeric() || "_.~}/".contains(before));
            if inside_a_path {
                continue;
            }
            let rest = &line[at + root.len()..];
            let name: String = rest
                .chars()
                .take_while(|c| {
                    c.is_ascii_alphanumeric() || *c == '_' || (!ends_at_dash && "-.".contains(*c))
                })
                .collect();
            let is_a_name = name.chars().next().is_some_and(char::is_alphanumeric);
            if is_a_name && !private.neutral_homes.contains(&name) {
                found.push((format!("{root}{name}"), private.home_fix.as_str()));
            }
        }
    }
}

/// An address whose domain isn't a reserved or listed one. `name@1.2.3` is a package and
/// `@notes.txt` a file, so neither counts.
fn mail<'t>(line: &str, private: &'t Private, found: &mut Vec<(String, &'t str)>) {
    let local = |c: char| c.is_ascii_alphanumeric() || "._%+-".contains(c);
    let domain = |c: char| c.is_ascii_alphanumeric() || ".-".contains(c);
    for (at, _) in line.match_indices('@') {
        let before: String = line[..at].chars().rev().take_while(|c| local(*c)).collect();
        let before: String = before.chars().rev().collect();
        let after: String = line[at + 1..].chars().take_while(|c| domain(*c)).collect();
        let after = after.trim_end_matches(['.', '-']);
        let Some((_, last)) = after.rsplit_once('.') else {
            continue;
        };
        let is_address = before.chars().any(|c| c.is_ascii_alphabetic())
            && last.len() >= 2
            && last.chars().all(|c| c.is_ascii_alphabetic())
            && !private.file_endings.iter().any(|ending| ending == last);
        let lower = after.to_ascii_lowercase();
        let neutral = private
            .neutral_mail_endings
            .iter()
            .any(|ending| lower == *ending || lower.ends_with(&format!(".{ending}")));
        if is_address && !neutral {
            found.push((format!("{before}@{after}"), private.mail_fix.as_str()));
        }
    }
}

/// Claude Code's scratch folder, named for the user's id: `claude-` and three digits or more,
/// in a path or in a folder name made from one. Model ids never have a number that long there.
fn scratch_folders<'t>(line: &str, private: &'t Private, found: &mut Vec<(String, &'t str)>) {
    let bytes = line.as_bytes();
    for (at, word) in line.match_indices("claude-") {
        if at > 0 && bytes[at - 1].is_ascii_alphanumeric() {
            continue;
        }
        let digits = line[at + word.len()..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        let end = at + word.len() + digits;
        let ends_the_name = bytes
            .get(end)
            .is_none_or(|after| !after.is_ascii_alphanumeric());
        if digits >= 3 && ends_the_name {
            found.push((line[at..end].to_owned(), private.scratch_fix.as_str()));
        }
    }
}

/// What a recording keeps of the recording machine's own setup: a synced skill, an installed
/// plugin or a claude.ai connector not named by its placeholder, and the account's plan or
/// organization. Only recordings are read, since the code's own examples are made up.
pub fn setup_places(table: &Table, files: &[File]) -> Vec<Place> {
    let setup = &table.private.setup;
    let mut places = Vec::new();
    for file in files.iter().filter(|file| table.is_recording(&file.path)) {
        let Some(text) = file.text.as_deref() else {
            continue;
        };
        let mut add = |line: Option<usize>, found: String| {
            places.push(Place::new(&file.path, line, &found, &setup.fix));
        };
        for (number, line) in text.lines().enumerate() {
            for found in setup_words(line, setup) {
                add(Some(number + 1), found);
            }
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                for found in setup_fields(&value, setup) {
                    add(Some(number + 1), found);
                }
            }
        }
        // A meta file is one JSON document over many lines.
        if let Ok(value) = serde_json::from_str::<Value>(text)
            && text.lines().nth(1).is_some()
        {
            for found in setup_fields(&value, setup) {
                add(None, found);
            }
        }
    }
    places
}

/// Names that read the same in any text, a settings file held in a string included.
fn setup_words(line: &str, setup: &Setup) -> Vec<String> {
    let name_char = |c: char| c.is_ascii_alphanumeric() || "-_".contains(c);
    let mut found = Vec::new();
    let mut after = |prefix: &str, placeholder: &str| {
        let wanted = placeholder.strip_prefix(prefix).unwrap_or(placeholder);
        for (at, _) in line.match_indices(prefix) {
            let rest = &line[at + prefix.len()..];
            let name: String = rest.chars().take_while(|c| name_char(*c)).collect();
            // A connector's tool follows its name after `__`.
            let name = name.split("__").next().unwrap_or_default();
            if !name.is_empty() && !rest.starts_with(wanted) {
                found.push(format!("{prefix}{name}"));
            }
        }
    };
    after("anthropic-skills:", &setup.synced_skill);
    after("mcp__claude_ai_", &setup.connector_tool);
    // A plugin from the official marketplace is one the recording machine installed, since a
    // recording's own plugins load from a folder.
    for (at, _) in line.match_indices("@claude-plugins-official") {
        let before: String = line[..at]
            .chars()
            .rev()
            .take_while(|c| name_char(*c))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if !before.is_empty() && !before.starts_with(&setup.plugin) {
            found.push(before);
        }
    }
    found
}

/// The fields a CLI fills from the recording machine: an init message's plugins and claude.ai
/// connectors, and an account's plan and organization, anywhere in a recorded value.
fn setup_fields(value: &Value, setup: &Setup) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        match value {
            Value::Array(items) => stack.extend(items),
            Value::Object(map) => {
                stack.extend(map.values());
                if map.get("subtype").and_then(Value::as_str) == Some("init") {
                    found.extend(init_setup(map, setup));
                }
                if let Some(Value::Object(account)) = map.get("account") {
                    for (key, placeholder) in [
                        ("subscriptionType", &setup.plan),
                        ("organization", &setup.organization),
                    ] {
                        if let Some(text) = account.get(key).and_then(Value::as_str)
                            && text != placeholder
                        {
                            found.push(format!("{key}: {text}"));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    found
}

fn init_setup(init: &serde_json::Map<String, Value>, setup: &Setup) -> Vec<String> {
    let text = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let mut found = Vec::new();
    for plugin in init
        .get("plugins")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (name, path, source) = (
            text(plugin, "name"),
            text(plugin, "path"),
            text(plugin, "source"),
        );
        let own = path == "builtin" || source.ends_with("@builtin") || source.ends_with("@inline");
        if !name.is_empty() && !own && !name.starts_with(&setup.plugin) {
            found.push(format!("plugin {name}"));
        }
    }
    // A name the CLI and the recording's repo don't account for is a skill, command or agent
    // of the recording machine's own.
    for key in ["skills", "slash_commands", "agents"] {
        for name in init
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let known = setup.known.iter().any(|known| known == name)
                || name.starts_with("anthropic-skills:");
            if !known {
                found.push(format!("{key} {name}"));
            }
        }
    }
    for server in init
        .get("mcp_servers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = text(server, "name");
        if text(server, "source") == "claudeai" && !name.starts_with(&setup.connector) {
            found.push(format!("connector {name}"));
        }
    }
    found
}

/// A banned text anywhere it stands as a word of its own, whatever its case.
fn banned_texts<'t>(
    line: &str,
    banned: &[(usize, u64)],
    private: &'t Private,
    found: &mut Vec<(String, &'t str)>,
) {
    let bytes = line.as_bytes();
    let starts_a_word = |at: usize| {
        bytes[at].is_ascii_alphanumeric() && (at == 0 || !bytes[at - 1].is_ascii_alphanumeric())
    };
    for at in (0..bytes.len()).filter(|at| starts_a_word(*at)) {
        for (which, (length, hash)) in banned.iter().enumerate() {
            let end = at + length;
            let ends_a_word =
                end == bytes.len() || (end < bytes.len() && !bytes[end].is_ascii_alphanumeric());
            if ends_a_word && hash_of(&bytes[at..end]) == *hash {
                let fix = private.banned[which].fix.as_str();
                found.push((String::from_utf8_lossy(&bytes[at..end]).into_owned(), fix));
            }
        }
    }
}

/// FNV-1a over the lowercased bytes. It only has to tell a banned text from other words.
pub fn hash_of(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte.to_ascii_lowercase())).wrapping_mul(0x0100_0000_01b3)
    })
}
