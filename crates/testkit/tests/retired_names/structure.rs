//! Checks on how names are tied together, which a word list can't see: a key spelled unlike
//! its field, a slash command unlike its variant, a template without its file, a role outside
//! the one list, a recordings folder named by version.

use std::collections::BTreeSet;

use argonauts_core::names::RoleName;
use regex::Regex;

use super::report::Place;
use super::table::{self, Table};
use super::tree::File;

/// A name without its case, dashes or underscores, so `follow-up` is `FollowUp`.
fn bare(name: &str) -> String {
    name.chars()
        .filter(|c| !"-_".contains(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether a key and its field are one name. A list's field may be the plural of its key
/// (`[[step]]` fills `steps`), and a key no name can spell (`12h`, `note+summary`, `●`) is
/// left to its variant.
fn same_name(key: &str, field: &str) -> bool {
    let spellable = key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c));
    if !spellable {
        return true;
    }
    let (key, field) = (bare(key), bare(field));
    let plural_of_key = [
        format!("{key}s"),
        format!("{key}es"),
        format!("{}ies", key.trim_end_matches('y')),
    ];
    key == field || plural_of_key.contains(&field)
}

/// A serde `rename` that gives a field or variant a key of another name. Code that reads a
/// CLI's or a provider's own JSON is left out, since those keys aren't ours to choose.
pub fn serde_keys(table: &Table, files: &[File]) -> Vec<Place> {
    let rename = Regex::new(r#"\brename\s*=\s*"([^"]+)""#).unwrap();
    let item =
        Regex::new(r"^\s*(?:pub(?:\([a-z]+\))?\s+)?(?:r#)?([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let mut places = Vec::new();
    for file in files {
        let ours =
            file.path.ends_with(".rs") && !table::fits_any(&table.serde_keys.not_ours, &file.path);
        let Some(text) = file.text.as_deref().filter(|_| ours) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        for (at, line) in lines.iter().enumerate() {
            let Some(key) = rename.captures(line).map(|found| found[1].to_owned()) else {
                continue;
            };
            // The item is the first line after the attribute that isn't one itself.
            let attribute_ends = (at..lines.len()).find(|at| lines[*at].contains(")]"));
            let named = attribute_ends.and_then(|end| {
                lines[end + 1..].iter().find(|line| {
                    let line = line.trim_start();
                    !line.is_empty() && !line.starts_with('#') && !line.starts_with("//")
                })
            });
            let Some(name) = named
                .and_then(|line| item.captures(line))
                .map(|found| found[1].to_owned())
            else {
                continue;
            };
            if !same_name(&key, &name) {
                places.push(Place::new(
                    &file.path,
                    Some(at + 1),
                    &format!("{key} on {name}"),
                    "one name for the key and the field",
                ));
            }
        }
    }
    places
}

/// Every slash command is a `SlashCommand` variant of the same name, unless the table says
/// which variant carries it.
pub fn slash_commands(table: &Table, files: &[File]) -> Vec<Place> {
    let path = table.slash_commands.table.as_str();
    let text = text_in(files, path);
    let name = Regex::new(r#"SlashSpec \{ name: "([^"]+)""#).unwrap();
    let variants = variants_of(text, "SlashCommand");
    assert!(
        !variants.is_empty(),
        "{path}: no SlashCommand variants found"
    );
    let bare_variants: BTreeSet<String> = variants.iter().map(|variant| bare(variant)).collect();

    let mut places = Vec::new();
    let mut commands = 0;
    for (number, line) in text.lines().enumerate() {
        let Some(command) = name.captures(line).map(|found| found[1].to_owned()) else {
            continue;
        };
        commands += 1;
        let carried_by = table.slash_commands.variant_of.get(&command);
        let known = match carried_by {
            Some(variant) => variants.contains(variant),
            None => bare_variants.contains(&bare(&command)),
        };
        if !known {
            places.push(Place::new(
                path,
                Some(number + 1),
                &format!("/{command}"),
                "a SlashCommand variant of the same name",
            ));
        }
    }
    assert!(commands > 0, "{path}: no slash commands found");
    for command in table.slash_commands.variant_of.keys() {
        let listed = text.contains(&format!("SlashSpec {{ name: {command:?}"));
        assert!(
            listed,
            "{}: /{command} is no longer a command",
            table::TABLE
        );
    }
    places
}

/// The variant names of one enum, read from its source.
fn variants_of(text: &str, name: &str) -> BTreeSet<String> {
    let Some(start) = text.find(&format!("pub enum {name} {{")) else {
        return BTreeSet::new();
    };
    let variant = Regex::new(r"^([A-Z][A-Za-z0-9]*)\b").unwrap();
    let mut found = BTreeSet::new();
    let mut depth = 0i32;
    for line in text[start..].lines().skip(1) {
        let trimmed = line.trim();
        if depth == 0 {
            if trimmed == "}" {
                break;
            }
            if let Some(name) = variant.captures(trimmed) {
                found.insert(name[1].to_owned());
            }
        }
        for c in trimmed.chars() {
            match c {
                '{' | '(' => depth += 1,
                '}' | ')' => depth -= 1,
                _ => {}
            }
        }
    }
    found
}

/// Every crew prompt template sits at the path its id names, every prompt file has an id in
/// one of the catalogs, and a frame is named after one role, once.
pub fn templates(files: &[File]) -> Vec<Place> {
    const CATALOG: &str = "crates/crew/src/prompts/catalog.rs";
    /// The catalogs of the prompts the app and the judge and narrator add to the crew's.
    const OTHER_CATALOGS: [&str; 2] = [
        "crates/app/src/prompts/catalog.rs",
        "crates/policy/src/templates.rs",
    ];
    const FOLDER: &str = "crates/core/templates/prompts/";
    let text = text_in(files, CATALOG);
    let id = Regex::new(r#""([a-z][a-z-]*/[a-z][a-z-]*)""#).unwrap();
    let roles: BTreeSet<&str> = RoleName::ALL.iter().map(RoleName::as_str).collect();

    let listed = text.split("#[cfg(test)]").next().unwrap_or(text);
    let all_ends = listed.find("];").unwrap_or(listed.len());
    let mut places = Vec::new();
    let mut ids = BTreeSet::new();
    let mut frames = BTreeSet::new();
    for (number, line) in listed.lines().enumerate() {
        for found in id.captures_iter(line) {
            let found = &found[1];
            let in_all = listed[..all_ends].contains(&format!("{found:?}"));
            if let Some(role) = found.strip_prefix("frames/") {
                if !frames.insert(role.to_owned()) {
                    places.push(Place::new(
                        CATALOG,
                        Some(number + 1),
                        found,
                        "one frame for a role",
                    ));
                }
                if !roles.contains(role) {
                    places.push(Place::new(
                        CATALOG,
                        Some(number + 1),
                        found,
                        "a role from the one role list",
                    ));
                }
            }
            if !in_all {
                continue;
            }
            ids.insert(found.to_owned());
            let file = format!("{FOLDER}{found}.md");
            if !files.iter().any(|tracked| tracked.path == file) {
                places.push(Place::new(CATALOG, Some(number + 1), found, &file));
            }
        }
    }
    assert!(!ids.is_empty(), "{CATALOG}: no template ids found");
    for catalog in OTHER_CATALOGS {
        let listed = text_in(files, catalog);
        let listed = listed.split("#[cfg(test)]").next().unwrap_or(listed);
        ids.extend(id.captures_iter(listed).map(|found| found[1].to_owned()));
    }
    for file in files {
        let Some(inside) = file
            .path
            .strip_prefix(FOLDER)
            .and_then(|path| path.strip_suffix(".md"))
        else {
            continue;
        };
        // The judge's default rules are replaced by a file of your own, not by a template,
        // and the README is the folder's own page.
        if inside == "approvals/default-rules" || inside == "README" {
            continue;
        }
        if !ids.contains(inside) {
            places.push(Place::new(
                &file.path,
                None,
                inside,
                "a template id of the same name",
            ));
        }
    }
    places
}

/// No folder under a `fixtures` folder is named after a version, a date, a numbered change or
/// mockup, or a model: a recording is named by what it shows, and its version sits in the
/// `.meta.json` beside it.
pub fn fixture_folders(table: &Table, files: &[File]) -> Vec<Place> {
    let version = Regex::new(r"\d+\.\d+").unwrap();
    let date = Regex::new(r"\d{4}-\d{2}-\d{2}").unwrap();
    let mut places = BTreeSet::new();
    for file in files {
        let Some((before, after)) = file.path.split_once("/fixtures/") else {
            continue;
        };
        let mut folder = format!("{before}/fixtures");
        let mut names: Vec<&str> = after.split('/').collect();
        names.pop();
        for name in names {
            folder = format!("{folder}/{name}");
            let lower = name.to_ascii_lowercase();
            let words = super::words::parts(name);
            let numbered = words.windows(2).any(|pair| {
                table.fixture_folders.numbered.contains(&pair[0]) && pair[1].parse::<u32>().is_ok()
            });
            let model = table
                .fixture_folders
                .models
                .iter()
                .find(|model| words.contains(model));
            let named_after = if version.is_match(&lower) {
                Some("a version")
            } else if date.is_match(&lower) {
                Some("a date")
            } else if numbered {
                Some("a number")
            } else {
                model.map(|_| "a model")
            };
            if let Some(what) = named_after {
                places.insert(Place::new(
                    &folder,
                    None,
                    name,
                    &format!(
                        "a folder named by CLI, not by {what}; the version goes in a .meta.json"
                    ),
                ));
            }
        }
    }
    places.into_iter().collect()
}

/// Every role a workflow file gives a step comes from the one role list: `role = "x"` in a
/// workflow's own file, and `role: x` in a file of phases alone.
pub fn roles(files: &[File]) -> Vec<Place> {
    let in_toml = Regex::new(r#"^\s*role\s*=\s*"([A-Za-z_-]+)"\s*(?:#.*)?$"#).unwrap();
    let in_yaml = Regex::new(r#"^\s*-?\s*role:\s*"?([A-Za-z_-]+)"?\s*(?:#.*)?$"#).unwrap();
    let roles: BTreeSet<&str> = RoleName::ALL.iter().map(RoleName::as_str).collect();
    let mut places = Vec::new();
    for file in files {
        let role = if file.path.ends_with(".yaml") || file.path.ends_with(".yml") {
            &in_yaml
        } else if file.path.contains("workflows/") && file.path.ends_with(".toml") {
            &in_toml
        } else {
            continue;
        };
        let Some(text) = file.text.as_deref() else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            if let Some(found) = role.captures(line)
                && !roles.contains(&found[1])
            {
                places.push(Place::new(
                    &file.path,
                    Some(number + 1),
                    &found[1],
                    "a role from the one role list",
                ));
            }
        }
    }
    places
}

fn text_in<'f>(files: &'f [File], path: &str) -> &'f str {
    files
        .iter()
        .find(|file| file.path == path)
        .and_then(|file| file.text.as_deref())
        .unwrap_or_else(|| panic!("{path} isn't a tracked text file"))
}
