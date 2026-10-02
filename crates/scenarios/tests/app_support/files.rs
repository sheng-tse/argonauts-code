//! Reading settings files back. The scenarios crate has no TOML parser, and the checks need
//! more than one anyway: which keys a file sets, and whether each carries a comment.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TomlEntry {
    /// Dotted, with the table's name in front.
    pub key: String,
    /// TOML text, joined onto one line when it spans several.
    pub value: String,
    /// A comment sits right above the key, or above its table's header.
    pub commented: bool,
}

/// Every `key = value` in a settings file, in file order. Handles tables, arrays of tables,
/// dotted and quoted keys, and arrays over several lines, which is what settings files use.
pub fn toml_entries(text: &str) -> Vec<TomlEntry> {
    let mut entries = Vec::new();
    let mut table = String::new();
    let mut table_commented = false;
    let mut comment_above = false;
    let mut open: Option<TomlEntry> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(entry) = open.as_mut() {
            entry.value.push(' ');
            entry.value.push_str(strip_comment(line));
            if brackets_closed(&entry.value) {
                entries.extend(open.take());
            }
            continue;
        }
        if line.starts_with('#') {
            comment_above = true;
            continue;
        }
        if line.is_empty() {
            comment_above = false;
            continue;
        }
        if let Some(name) = header(line) {
            table = name;
            table_commented = comment_above;
            comment_above = false;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = unquote_key(key);
        let entry = TomlEntry {
            key: if table.is_empty() {
                key
            } else {
                format!("{table}.{key}")
            },
            value: strip_comment(value.trim()).to_owned(),
            commented: comment_above || table_commented,
        };
        comment_above = false;
        if brackets_closed(&entry.value) {
            entries.push(entry);
        } else {
            open = Some(entry);
        }
    }
    entries
}

/// The last value `key` gets in the file.
pub fn toml_value(text: &str, key: &str) -> Option<String> {
    toml_entries(text)
        .into_iter()
        .rev()
        .find(|entry| entry.key == key)
        .map(|entry| entry.value)
}

/// Two TOML values written with different spacing are the same.
pub fn same_toml(a: &str, b: &str) -> bool {
    squeeze(a) == squeeze(b)
}

/// A dotted key's value in a JSON document, such as a run's `settings.json`.
pub fn json_at<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.').try_fold(value, |node, part| node.get(part))
}

fn header(line: &str) -> Option<String> {
    let inner = line
        .strip_prefix("[[")
        .and_then(|rest| rest.strip_suffix("]]"))
        .or_else(|| {
            line.strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
        })?;
    Some(unquote_key(inner))
}

fn unquote_key(key: &str) -> String {
    key.trim()
        .split('.')
        .map(|part| part.trim().trim_matches('"'))
        .collect::<Vec<_>>()
        .join(".")
}

/// Drops a trailing `# comment` that isn't inside a string.
fn strip_comment(value: &str) -> &str {
    let mut quote: Option<char> = None;
    for (at, c) in value.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, '#') => return value[..at].trim_end(),
            _ => {}
        }
    }
    value
}

fn brackets_closed(value: &str) -> bool {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for c in value.chars() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, '[' | '{') => depth += 1,
            (None, ']' | '}') => depth -= 1,
            _ => {}
        }
    }
    depth <= 0
}

fn squeeze(value: &str) -> String {
    let mut out = String::new();
    let mut quote: Option<char> = None;
    for c in value.chars() {
        match (quote, c) {
            (None, '"' | '\'') => {
                quote = Some(c);
                out.push('"');
            }
            (Some(q), _) if c == q => {
                quote = None;
                out.push('"');
            }
            (None, c) if c.is_whitespace() => {}
            _ => out.push(c),
        }
    }
    out.replace(",]", "]").replace(",}", "}")
}
