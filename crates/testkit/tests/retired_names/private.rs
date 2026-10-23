//! Real home folders, email addresses, scratch and temp folders, plans and ids: none belongs
//! in a tracked file, recordings included. Sample text uses a neutral folder, a reserved mail
//! domain and the neutral plan. A recording also keeps none of the recording machine's own
//! setup.

use std::borrow::Cow;

use serde_json::Value;

use super::account::{self, Kind};
use super::report::Place;
use super::table::{self, Private, Setup, Table};
use super::tree::File;

/// What a check found on a line, with its fix.
pub type Found<'t> = Vec<(String, &'t str)>;

pub fn places(table: &Table, files: &[File]) -> Vec<Place> {
    let private = &table.private;
    let banned: Vec<(usize, u64)> = private
        .banned
        .iter()
        .map(|text| (text.length, parsed_hash(&text.hash, &text.what)))
        .collect();
    let contact = (
        private.contact.length,
        parsed_hash(&private.contact.hash, "the contact address"),
    );

    let mut places = Vec::new();
    for file in files {
        let Some(text) = file.text.as_deref() else {
            continue;
        };
        let addresses_are_authors = table::fits_any(&private.authors_addresses_in, &file.path);
        let gives_contact = table::fits_any(&private.contact.given_in, &file.path);
        let kind = Kind {
            recording: table.is_recording(&file.path),
            yaml: file.path.ends_with(".yaml") || file.path.ends_with(".yml"),
        };
        let mut seen = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let mut found = Vec::new();
            for reading in readings(line) {
                homes(&reading, private, &mut found);
                if !addresses_are_authors {
                    let contact = gives_contact.then_some(contact);
                    mail(&reading, private, contact, &mut found);
                }
                scratch_folders(&reading, private, &mut found);
                temp_folders(&reading, private, &mut found);
                banned_texts(&reading, &banned, private, &mut found);
                account::line(&reading, kind, private, &mut found);
            }
            for (text, fix) in once_each(found) {
                places.push(Place::new(&file.path, Some(number + 1), &text, fix));
                seen.push(text);
            }
        }
        // A JSON document over many lines may put a key and its value on lines of their own.
        if text.lines().nth(1).is_some()
            && let Ok(value) = serde_json::from_str::<Value>(text)
        {
            let compact = value.to_string();
            let mut found = Vec::new();
            for reading in readings(&compact) {
                account::line(&reading, kind, private, &mut found);
            }
            for (found, fix) in once_each(found) {
                if !seen.contains(&found) {
                    let line = account::line_of(text, &found);
                    places.push(Place::new(&file.path, line, &found, fix));
                }
            }
        }
    }
    places
}

/// The first of each, in the order found.
fn once_each(found: Found<'_>) -> Found<'_> {
    let mut kept: Found<'_> = Vec::new();
    for item in found {
        if !kept.contains(&item) {
            kept.push(item);
        }
    }
    kept
}

/// A line as written, then as it reads with each layer of escapes undone: JSON's (`\/`, `\\`,
/// `\"`, `\u0040`, `\n`), however deeply one string holds another, and a URL's percent signs
/// before a path's or an address's characters. Every layer is read, since undoing one can
/// also turn a Windows path's `\n` into a new line.
pub fn readings(line: &str) -> Vec<Cow<'_, str>> {
    let mut readings = vec![Cow::Borrowed(line)];
    while readings.len() < 6 {
        let last = &readings[readings.len() - 1];
        if !last.contains(['\\', '%']) {
            break;
        }
        let next = percent_decoded(&unescaped(last));
        if next == **last {
            break;
        }
        readings.push(Cow::Owned(next));
    }
    readings
}

/// One layer of a JSON string's escapes undone. A backslash that starts no escape stays, as
/// in `C:\Users`.
fn unescaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('\\') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let code = after
            .strip_prefix('u')
            .and_then(|hex| hex.get(..4))
            .filter(|hex| hex.chars().all(|c| c.is_ascii_hexdigit()))
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .and_then(char::from_u32);
        let (replaced, used) = match (after.chars().next(), code) {
            (Some('u'), Some(code)) => (code, 5),
            (Some(c @ ('\\' | '"' | '/')), _) => (c, 1),
            (Some('n' | 'r'), _) => ('\n', 1),
            (Some('t'), _) => (' ', 1),
            _ => ('\\', 0),
        };
        out.push(replaced);
        rest = &after[used..];
    }
    out.push_str(rest);
    out
}

/// A URL's percent signs undone for the characters of a path or an address.
fn percent_decoded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        let code = rest
            .get(at + 1..at + 3)
            .filter(|hex| hex.chars().all(|c| c.is_ascii_hexdigit()))
            .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            .map(char::from)
            .filter(|c| "/\\@:~. ".contains(*c));
        match code {
            Some(c) => {
                out.push(c);
                rest = &rest[at + 3..];
            }
            None => {
                out.push('%');
                rest = &rest[at + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A home folder that isn't one of the neutral names: a path on macOS, Linux or Windows in any
/// case, one under a Mac's other volumes or Linux's other homes, `~name`, or the way Claude
/// Code names a project's folder, with a dash for each slash.
fn homes<'t>(text: &str, private: &'t Private, found: &mut Found<'t>) {
    let neutral = |name: &str| {
        let name = name.to_lowercase();
        private.neutral_homes.contains(&name)
    };
    let lower = text.to_ascii_lowercase();
    for (root, dashed) in [
        ("/users/", false),
        ("/home/", false),
        ("\\users\\", false),
        ("-users-", true),
        ("-home-", true),
    ] {
        for (at, _) in lower.match_indices(root) {
            let before = &text[..at];
            let starts = if dashed {
                starts_a_dashed_name(before)
            } else {
                starts_a_path(before.trim_end_matches('/')) || under_another_root(before)
            };
            if !starts {
                continue;
            }
            let name: String = text[at + root.len()..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || (!dashed && "-.".contains(*c)))
                .collect();
            let is_a_name = name.chars().next().is_some_and(char::is_alphanumeric);
            if is_a_name && !neutral(&name) {
                let root = &text[at..at + root.len()];
                found.push((format!("{root}{name}"), private.home_fix.as_str()));
            }
        }
    }
    // `~name` is that user's home, in a path or alone.
    for (at, _) in text.match_indices('~') {
        let opens = text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || "\"'(=:,[".contains(c));
        let rest = &text[at + 1..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || "_.-".contains(*c))
            .collect();
        let closes = rest[name.len()..]
            .chars()
            .next()
            .is_none_or(|c| "/\"')".contains(c));
        let is_a_name = name.chars().next().is_some_and(char::is_alphabetic);
        if opens && closes && is_a_name && !neutral(&name) {
            found.push((format!("~{name}"), private.home_fix.as_str()));
        }
    }
}

/// Part of a longer path (`{dir}/home/skills`) is a folder that happens to be called home, not
/// anyone's. Slashes before the root still start a path, as in `file:///Users/you` or a
/// `Read(//home/you/**)` rule.
fn starts_a_path(before: &str) -> bool {
    before
        .chars()
        .next_back()
        .is_none_or(|c| !(c.is_alphanumeric() || "_.~}".contains(c)))
}

/// The folders a home sits under besides the root: `/var/home` (Fedora's atomic desktops),
/// `/usr/home` (FreeBSD), a Mac's data volume and other disks under `/Volumes`, and a Windows
/// drive under WSL's `/mnt`.
fn under_another_root(before: &str) -> bool {
    let before = before.to_ascii_lowercase();
    let ends_with_root =
        |text: &str, root: &str| text.strip_suffix(root).is_some_and(starts_a_path);
    let parent = before.rsplit_once('/').map_or("", |(parent, _)| parent);
    ["/var", "/usr", "/system/volumes/data"]
        .iter()
        .any(|root| ends_with_root(&before, root))
        || ["/volumes", "/mnt"]
            .iter()
            .any(|root| ends_with_root(parent, root))
}

/// A dashed name starts after anything but a name's own characters, or after a Windows drive's
/// letter, as `C--Users-you-repo` names `C:\Users\you\repo`.
fn starts_a_dashed_name(before: &str) -> bool {
    match before.strip_suffix('-') {
        Some(drive) => {
            let mut back = drive.chars().rev();
            back.next().is_some_and(|c| c.is_ascii_alphabetic())
                && back.next().is_none_or(|c| !c.is_alphanumeric())
        }
        None => starts_a_path(before),
    }
}

/// An address whose domain isn't a reserved or listed one, unless its whole name is one that
/// takes no mail. GitHub's address for a hidden login passes only for a neutral login, and the
/// maintainer's address (`contact`, as length and hash) only in a file that gives it.
/// `name@1.2.3` is a package and `@notes.txt` a file, so neither counts.
fn mail<'t>(
    text: &str,
    private: &'t Private,
    contact: Option<(usize, u64)>,
    found: &mut Found<'t>,
) {
    let local = |c: char| c.is_ascii_alphanumeric() || "._%+-".contains(c);
    let domain = |c: char| c.is_ascii_alphanumeric() || ".-".contains(c);
    for (at, _) in text.match_indices('@') {
        let before: String = text[..at].chars().rev().take_while(|c| local(*c)).collect();
        let before: String = before.chars().rev().collect();
        let after: String = text[at + 1..].chars().take_while(|c| domain(*c)).collect();
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
        let name = before.to_ascii_lowercase();
        let takes_no_mail = private.no_reply.contains(&name);
        let login = name.rsplit_once('+').map_or(name.as_str(), |(id, login)| {
            if id.chars().all(|c| c.is_ascii_digit()) {
                login
            } else {
                ""
            }
        });
        let neutral_login =
            lower == private.github_no_reply && private.neutral_homes.iter().any(|n| n == login);
        let address = format!("{before}@{after}");
        let is_contact = contact.is_some_and(|(length, hash)| {
            address.len() == length && hash_of(address.as_bytes()) == hash
        });
        if is_address && !neutral && !takes_no_mail && !neutral_login && !is_contact {
            found.push((address, private.mail_fix.as_str()));
        }
    }
}

/// Claude Code's scratch folder, named for the user's id: `claude-` and three digits or more,
/// in a path or in a folder name made from one. Model ids never have a number that long there.
fn scratch_folders<'t>(line: &str, private: &'t Private, found: &mut Found<'t>) {
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

/// macOS's temp folder for a user: `/var/folders/`, then two folders named from the user's
/// id, two characters and then twenty or more, as a path or in Claude Code's dashed name for a
/// folder, where a dash stands for an underscore too. Both made of x's are a placeholder, and
/// a shorter made-up name (`/var/folders/x/T`) is no one's.
fn temp_folders<'t>(text: &str, private: &'t Private, found: &mut Found<'t>) {
    for (root, gap) in [("/var/folders/", '/'), ("-var-folders-", '-')] {
        for (at, _) in text.match_indices(root) {
            let Some((group, rest)) = text[at + root.len()..].split_once(gap) else {
                continue;
            };
            let name = if gap == '/' {
                rest.split('/').next().unwrap_or_default()
            } else {
                temp_name_in_dashes(rest)
            };
            let named = |folder: &str| {
                folder
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || (gap == '-' && c == '-'))
            };
            let shaped = group.len() == 2 && name.len() >= 20 && named(group) && named(name);
            let placeholder = group.chars().chain(name.chars()).all(|c| c == 'x');
            if shaped && !placeholder {
                found.push((
                    format!("{root}{group}{gap}{name}"),
                    private.temp_fix.as_str(),
                ));
            }
        }
    }
}

/// The user's folder in a dashed name, which ends where the folder inside it starts: `T` for
/// temp files, `C` for caches, `0` for the rest.
fn temp_name_in_dashes(rest: &str) -> &str {
    let run = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || "-_".contains(c)))
        .map_or(rest, |end| &rest[..end]);
    let end = ["-T", "-C", "-0"]
        .iter()
        .flat_map(|inner| run.match_indices(inner))
        .filter(|(at, inner)| {
            run[at + inner.len()..]
                .chars()
                .next()
                .is_none_or(|c| c == '-')
        })
        .map(|(at, _)| at)
        .min()
        .unwrap_or(run.len());
    &run[..end]
}

/// What a recording keeps of the recording machine's own setup: a synced skill, an installed
/// plugin or a claude.ai connector not named by its placeholder, the account's organization,
/// and the machine's name. Only recordings are read, since the code's own examples are made up.
pub fn setup_places(table: &Table, files: &[File]) -> Vec<Place> {
    let setup = &table.private.setup;
    let mut places = Vec::new();
    for file in files.iter().filter(|file| table.is_recording(&file.path)) {
        let Some(text) = file.text.as_deref() else {
            continue;
        };
        let mut add = |line: Option<usize>, (found, fix): (String, &str)| {
            places.push(Place::new(&file.path, line, &found, fix));
        };
        for (number, line) in text.lines().enumerate() {
            for found in setup_words(line, setup) {
                add(Some(number + 1), (found, &setup.fix));
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
    // A connector's own tool names tell which connector it is, so they take placeholders too.
    for (at, _) in line.match_indices(setup.connector_tool.as_str()) {
        let name: String = line[at..].chars().take_while(|c| name_char(*c)).collect();
        let tool = name[setup.connector_tool.len()..]
            .split_once("__")
            .map_or("", |(_, tool)| tool);
        if !tool.is_empty() && !tool.starts_with(&setup.connector_tool_name) {
            found.push(name);
        }
    }
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
/// connectors, a synced skill's command and its context usage entry, the account's
/// organization, and the name Codex gives the machine, anywhere in a recorded value and in the
/// JSON its strings hold.
fn setup_fields<'t>(value: &Value, setup: &'t Setup) -> Vec<(String, &'t str)> {
    let mut found = Vec::new();
    match value {
        Value::Array(items) => {
            for item in items {
                found.extend(setup_fields(item, setup));
            }
        }
        Value::Object(map) => {
            if map.get("subtype").and_then(Value::as_str) == Some("init") {
                found.extend(
                    init_setup(map, setup)
                        .into_iter()
                        .map(|name| (name, setup.fix.as_str())),
                );
            }
            // `*` is every organization, as an OpenAI-style server's model permissions write it.
            for key in &setup.organization_keys {
                if let Some(text) = map.get(key).and_then(Value::as_str)
                    && text != setup.organization
                    && text != "*"
                {
                    found.push((format!("{key}: {text}"), setup.fix.as_str()));
                }
            }
            found.extend(
                synced_entry(map, setup)
                    .into_iter()
                    .map(|name| (name, setup.fix.as_str())),
            );
            if map.contains_key(&setup.machine_name_beside)
                && let Some(name) = map.get(&setup.machine_name_key).and_then(Value::as_str)
                && !setup.machine_names.iter().any(|neutral| neutral == name)
            {
                let key = &setup.machine_name_key;
                found.push((format!("{key}: {name}"), setup.machine_name_fix.as_str()));
            }
            for item in map.values() {
                found.extend(setup_fields(item, setup));
            }
        }
        Value::String(text) if text.trim_start().starts_with(['{', '[']) => {
            if let Ok(held) = serde_json::from_str::<Value>(text) {
                found.extend(setup_fields(&held, setup));
            }
        }
        _ => {}
    }
    found
}

/// A skill the account synced, as an initialize reply's commands list it (its alias is under
/// `anthropic-skills:`) and as a context usage's skills do (its source is `syncedSkills`): its
/// name is the placeholder's, and a command's description the one the table gives.
fn synced_entry(entry: &serde_json::Map<String, Value>, setup: &Setup) -> Vec<String> {
    let text = |key: &str| entry.get(key).and_then(Value::as_str);
    let aliased = entry
        .get("aliases")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|alias| alias.starts_with("anthropic-skills:"));
    let Some(name) = text("name").filter(|_| aliased || text("source") == Some("syncedSkills"))
    else {
        return Vec::new();
    };
    let wanted = setup
        .synced_skill
        .strip_prefix("anthropic-skills:")
        .unwrap_or(&setup.synced_skill);
    let mut found = Vec::new();
    if !name.starts_with(wanted) {
        found.push(format!("synced skill {name}"));
    }
    if let Some(description) = text("description")
        && description != setup.synced_skill_description
    {
        let short: String = description.chars().take(40).collect();
        found.push(format!("synced skill {name}: {short}"));
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
    found: &mut Found<'t>,
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

/// A table's hash, written as hex with or without `0x`.
fn parsed_hash(hash: &str, what: &str) -> u64 {
    u64::from_str_radix(hash.trim_start_matches("0x"), 16).unwrap_or_else(|e| panic!("{what}: {e}"))
}

/// FNV-1a over the lowercased bytes. It only has to tell a banned text from other words.
pub fn hash_of(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte.to_ascii_lowercase())).wrapping_mul(0x0100_0000_01b3)
    })
}
