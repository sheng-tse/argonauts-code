//! A settings file serde or the TOML parser refused, said in plain words: what the key takes,
//! what the file has instead, and for a shape that's easy to get wrong, the shape to write.

use argonauts_core::sanitize::escape_controls;
use argonauts_core::settings::protocol::CheckCommand;
use toml::{Table, Value};

use crate::catalog::setting_meta;

/// The keys that take a list of named commands, each written as a `[[key]]` table.
const COMMAND_LISTS: [&str; 3] = ["checks.run", "checks.full_suite", "checks.setup"];

/// The shape shown when the file gives nothing to build one from.
const EXAMPLE_COMMAND: &str = "make test";

/// What serde said about `key`, reworded. `written` is the value the file gave the key, when
/// it can be found, which says more than serde's description of it.
pub(crate) fn type_message(key: &str, serde_message: &str, written: Option<&Value>) -> String {
    let problem = Problem::read(serde_message);
    if COMMAND_LISTS.contains(&key)
        && let Some(message) = command_list(key, &problem, written)
    {
        return message;
    }
    match problem {
        Problem::Type { found, expected } => {
            let found = Found::from_written(written, &found);
            // A list whose entries are the wrong kind: serde names the entry, not the list.
            let entry = matches!(written, Some(Value::Array(_))) && expected != "a sequence";
            let takes = takes(key, &expected, &found, entry);
            let sentence = if entry {
                format!("{key} can't hold {}; each entry {takes}", found.shown)
            } else {
                format!("{key} can't be {}; it {takes}", found.shown)
            };
            match unquoted(key, &expected, &found) {
                Some(fix) => format!("{sentence}. Write it without quotes: {fix}"),
                None => sentence,
            }
        }
        Problem::Variant { found, expected } => {
            let allowed = allowed_values(key).unwrap_or_else(|| quoted_list(&expected));
            format!("{key} can't be \"{found}\"; it takes {allowed}")
        }
        Problem::Missing { field } => match written {
            Some(Value::Array(_)) => format!("each [[{key}]] table needs a {field}"),
            _ => format!("[{key}] needs a {field}"),
        },
        Problem::UnknownField { field, expected } => format!(
            "{key} has no setting named {field}; it takes {}",
            quoted_list(&expected).replace('"', "")
        ),
        Problem::Other(message) => format!("{key}: {message}"),
    }
}

/// A file the TOML parser refused, by the line it stopped at. The line is quoted with its
/// control characters written out, since a file you haven't trusted could hold escapes for
/// your terminal, and this message can print before the screen opens.
pub(crate) fn syntax_message(text: &str, err: &toml::de::Error) -> String {
    let message = escape_controls(err.message().trim());
    let Some(span) = err.span() else {
        return format!("it isn't valid TOML: {message}");
    };
    let start = span.start.min(text.len());
    let number = text[..start].matches('\n').count() + 1;
    let line = escape_controls(text.lines().nth(number - 1).unwrap_or_default().trim());
    let line = line.as_str();
    if message == "duplicate key" {
        let name = line.split('=').next().unwrap_or(line).trim();
        return format!("line {number} sets {name} a second time; each key goes in a table once");
    }
    format!("line {number} isn't valid TOML ({message}): {line}")
}

/// Where the parser stopped, as a line and a column counted from 1.
pub(crate) fn syntax_place(text: &str, err: &toml::de::Error) -> Option<(usize, usize)> {
    let start = err.span()?.start.min(text.len());
    let before = &text[..start];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |at| at + 1);
    Some((line, before[line_start..].chars().count() + 1))
}

/// The value at a dotted key, through tables only. A key inside a list of tables isn't found.
pub(crate) fn written_at<'a>(table: &'a Table, key: &str) -> Option<&'a Value> {
    let mut segments = key.split('.');
    let mut value = table.get(segments.next()?)?;
    for segment in segments {
        value = value.as_table()?.get(segment)?;
    }
    Some(value)
}

/// Each check named by its words before any flag, so `cargo clippy -- -D warnings` is `clippy`,
/// and a second `test` is spelled out as `cargo-test`. A command is split on spaces, never run
/// through a shell.
pub fn check_commands(lines: &[String]) -> Vec<CheckCommand> {
    let mut checks: Vec<CheckCommand> = Vec::new();
    for line in lines {
        let command: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        let words: Vec<&str> = command
            .iter()
            .map(String::as_str)
            .take_while(|word| !word.starts_with('-'))
            .collect();
        let Some(last) = words.last() else { continue };
        let name = if checks.iter().any(|check| check.name == *last) {
            words.join("-")
        } else {
            (*last).to_owned()
        };
        checks.push(CheckCommand { name, command });
    }
    checks
}

/// `checks.run` and its kin take `[[key]]` tables. The usual slip is a command written as a
/// string, so the fix is built from the strings the file has.
fn command_list(key: &str, problem: &Problem, written: Option<&Value>) -> Option<String> {
    let shape = format!("a list of checks, each a [[{key}]] table with a name and a command");
    let (found, commands) = match (problem, written) {
        (Problem::Missing { .. }, _) => {
            let fix = example(key, &[EXAMPLE_COMMAND.to_owned()]);
            return Some(format!(
                "each [[{key}]] table needs both a name and a command. Write it like this:\n{fix}"
            ));
        }
        (_, Some(Value::Array(items))) => {
            let strings: Vec<String> = items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect();
            if strings.len() != items.len() || strings.is_empty() {
                return None;
            }
            let found = if strings.len() == 1 {
                "a list of strings".to_owned()
            } else {
                format!("a list of {} strings", strings.len())
            };
            (found, strings)
        }
        (_, Some(Value::String(command))) => (
            Value::String(command.clone()).to_string(),
            vec![command.clone()],
        ),
        // One check written as `[key]` or `key = { ... }`, where each takes `[[key]]`.
        (_, Some(Value::Table(table))) => {
            let fix = list_entry(key, table);
            return Some(format!(
                "{key} is written as a single table, but it takes a list of checks, each a \
                 [[{key}]] table with two brackets. Write it like this:\n{fix}"
            ));
        }
        _ => return None,
    };
    // Strings of flags alone name no command, so the shape comes from the example instead.
    let fix = Some(example(key, &commands))
        .filter(|fix| !fix.is_empty())
        .unwrap_or_else(|| example(key, &[EXAMPLE_COMMAND.to_owned()]));
    Some(format!(
        "{key} can't be {found}; it takes {shape}. Write it like this:\n{fix}"
    ))
}

/// One `[[key]]` table for each command.
fn example(key: &str, commands: &[String]) -> String {
    check_commands(commands)
        .iter()
        .map(|check| entry(key, &check.name, &check.command))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The `[[key]]` table for a check written as a single table, keeping its name and command.
fn list_entry(key: &str, table: &Table) -> String {
    let mut command: Vec<String> = match table.get("command") {
        Some(Value::Array(words)) => words
            .iter()
            .filter_map(|word| word.as_str().map(str::to_owned))
            .collect(),
        Some(Value::String(line)) => line.split_whitespace().map(str::to_owned).collect(),
        _ => Vec::new(),
    };
    if command.is_empty() {
        command = EXAMPLE_COMMAND.split(' ').map(str::to_owned).collect();
    }
    let name = match table.get("name").and_then(Value::as_str) {
        Some(name) => name.to_owned(),
        None => check_commands(&[command.join(" ")])
            .into_iter()
            .next()
            .map_or_else(|| "check".to_owned(), |check| check.name),
    };
    entry(key, &name, &command)
}

fn entry(key: &str, name: &str, command: &[String]) -> String {
    let words: Vec<String> = command
        .iter()
        .map(|word| Value::String(word.clone()).to_string())
        .collect();
    format!(
        "[[{key}]]\nname = {}\ncommand = [{}]",
        Value::String(name.to_owned()),
        words.join(", ")
    )
}

/// What serde's message says went wrong, parsed from its fixed wording.
#[derive(Debug, PartialEq)]
enum Problem {
    /// "invalid type: string "1", expected f64", and "invalid value: integer `-1`, expected
    /// u32" for a value of the right type out of range.
    Type { found: String, expected: String },
    /// "unknown variant `emacs`, expected `default` or `vim`".
    Variant { found: String, expected: String },
    /// "missing field `command`".
    Missing { field: String },
    /// "unknown field `x`, expected `a` or `b`".
    UnknownField { field: String, expected: String },
    /// A message of the type's own, already in words.
    Other(String),
}

impl Problem {
    fn read(message: &str) -> Self {
        let message = message.trim();
        let typed = message
            .strip_prefix("invalid type: ")
            .or_else(|| message.strip_prefix("invalid value: "));
        if let Some((found, expected)) = typed.and_then(|rest| rest.split_once(", expected ")) {
            return Self::Type {
                found: found.to_owned(),
                expected: expected.to_owned(),
            };
        }
        if let Some((found, expected)) = message
            .strip_prefix("unknown variant ")
            .and_then(|rest| rest.split_once(", expected "))
        {
            return Self::Variant {
                found: found.trim_matches('`').to_owned(),
                expected: expected.to_owned(),
            };
        }
        if let Some((field, expected)) = message
            .strip_prefix("unknown field ")
            .and_then(|rest| rest.split_once(", expected "))
        {
            return Self::UnknownField {
                field: field.trim_matches('`').to_owned(),
                expected: expected.to_owned(),
            };
        }
        if let Some(field) = message.strip_prefix("missing field ") {
            return Self::Missing {
                field: field.trim_matches('`').to_owned(),
            };
        }
        Self::Other(message.to_owned())
    }
}

/// What the file has, as TOML would write it.
struct Found {
    shown: String,
    /// The text inside the quotes, when the file has a string.
    text: Option<String>,
}

impl Found {
    /// The value the file wrote, or else serde's description of it, such as `string "1"`.
    fn from_written(written: Option<&Value>, serde_found: &str) -> Self {
        // A list is described by its entry, which serde names; the whole list says less.
        if let Some(value) = written.filter(|value| !value.is_array()) {
            return Self {
                shown: value.to_string(),
                text: value.as_str().map(str::to_owned),
            };
        }
        if let Some(text) = serde_found
            .strip_prefix("string ")
            .and_then(|quoted| quoted.strip_prefix('"')?.strip_suffix('"'))
        {
            return Self {
                shown: format!("\"{text}\""),
                text: Some(text.to_owned()),
            };
        }
        let shown = match serde_found.split_once(' ') {
            Some((_, value)) if value.starts_with('`') => value.trim_matches('`').to_owned(),
            _ => match serde_found {
                "sequence" => "a list".to_owned(),
                "map" => "a table".to_owned(),
                other => other.to_owned(),
            },
        };
        Self { shown, text: None }
    }
}

/// "takes a number", "is a table", or the values the key allows.
fn takes(key: &str, expected: &str, found: &Found, entry: bool) -> String {
    let table = expected.starts_with("struct ") || expected == "a map";
    if let Some(allowed) = allowed_values(key).filter(|_| !table && !entry) {
        return format!("takes {allowed}");
    }
    if table {
        let header = if entry {
            format!("[[{key}]]")
        } else {
            format!("[{key}]")
        };
        return format!("is a table, written {header} with its settings under it");
    }
    let words = match expected {
        "a string" | "string only" | "a borrowed string" => "text in quotes",
        "f64" | "f32" | "a float" => "a number",
        "u8" | "u16" | "u32" | "u64" | "usize" => "a whole number, 0 or more",
        "i8" | "i16" | "i32" | "i64" | "isize" => "a whole number",
        "a boolean" => "true or false",
        "a sequence" => match &found.text {
            Some(text) => return format!("takes a list, such as {}", as_list(key, text)),
            None => "a list",
        },
        other => other,
    };
    format!("takes {words}")
}

/// A string where a list goes, as the list it likely meant. A command's words are split;
/// anything else becomes a list of one.
fn as_list(key: &str, text: &str) -> String {
    let items: Vec<String> = if key.ends_with("command") || key.ends_with("args") {
        text.split_whitespace()
            .map(|word| Value::String(word.to_owned()).to_string())
            .collect()
    } else {
        vec![Value::String(text.to_owned()).to_string()]
    };
    format!("[{}]", items.join(", "))
}

/// `limits.spend_per_run = "1"` meant the number, so the fix drops the quotes.
fn unquoted(key: &str, expected: &str, found: &Found) -> Option<String> {
    let text = found.text.as_deref()?.trim();
    let fits = match expected {
        "f64" | "f32" | "a float" => text.parse::<f64>().is_ok_and(f64::is_finite),
        "u8" | "u16" | "u32" | "u64" | "usize" => text.parse::<u64>().is_ok(),
        "i8" | "i16" | "i32" | "i64" | "isize" => text.parse::<i64>().is_ok(),
        "a boolean" => matches!(text, "true" | "false"),
        _ => false,
    };
    let name = key.rsplit('.').next().unwrap_or(key);
    fits.then(|| format!("{name} = {text}"))
}

/// The catalog's values for a key that takes a word or a boolean, such as `"default" or "vim"`.
fn allowed_values(key: &str) -> Option<String> {
    let allowed = setting_meta(key)?.allowed;
    (!allowed.is_empty()).then(|| one_of(&allowed))
}

/// Serde's "`a` or `b`" and "one of `a`, `b`, `c`" as quoted words.
fn quoted_list(expected: &str) -> String {
    let words: Vec<String> = expected
        .trim_start_matches("one of ")
        .split(['`'])
        .map(str::trim)
        .filter(|word| !word.is_empty() && *word != "," && *word != "or")
        .map(|word| format!("\"{word}\""))
        .collect();
    one_of(&words)
}

/// `"a", "b", or "c"`.
pub(crate) fn one_of(values: &[String]) -> String {
    match values {
        [] => String::new(),
        [only] => only.clone(),
        [first, second] => format!("{first} or {second}"),
        [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kinds_settings_never_reach_today_still_read_plainly() {
        assert_eq!(
            type_message(
                "agents.codex",
                "unknown field `modle`, expected `model` or `effort`",
                None
            ),
            "agents.codex has no setting named modle; it takes model or effort"
        );
        assert_eq!(
            type_message("approvals.merge", "missing field `mode`", None),
            "[approvals.merge] needs a mode"
        );
        assert_eq!(
            type_message(
                "steps.review.when",
                "unknown variant `later`, expected one of `always`, `never`, `ask`",
                None
            ),
            "steps.review.when can't be \"later\"; it takes \"always\", \"never\", or \"ask\""
        );
        assert_eq!(
            type_message("crew.main", "invalid model \"a b\"", None),
            "crew.main: invalid model \"a b\""
        );
    }

    #[test]
    fn a_line_quoted_back_shows_its_escapes_without_acting_on_them() {
        let comment = "[ui]\n# \u{1b}]52;c;cHduZWQ=\u{7} \u{1b}]0;owned\u{7}\nbell = true\n";
        let string = "[ui]\ntheme = \"\u{1b}]0;owned\u{7}\"\n";
        for text in [comment, string] {
            let err = text.parse::<Table>().unwrap_err();
            let message = syntax_message(text, &err);
            assert!(!message.contains(['\u{1b}', '\u{7}']), "{message:?}");
            assert!(message.starts_with("line 2 isn't valid TOML"), "{message}");
            assert!(message.contains("\\u{1b}]0;owned\\u{7}"), "{message}");
        }
    }

    #[test]
    fn a_check_of_flags_alone_still_shows_the_shape() {
        let written = Value::Array(vec![Value::String("--all".to_owned())]);
        let message = type_message(
            "checks.run",
            "invalid type: string \"--all\", expected struct CheckCommand",
            Some(&written),
        );
        assert!(
            message.ends_with("[[checks.run]]\nname = \"test\"\ncommand = [\"make\", \"test\"]"),
            "{message}"
        );
    }
}
