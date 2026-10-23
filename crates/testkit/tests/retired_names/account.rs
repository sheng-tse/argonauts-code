//! What a line keeps of an account: a plan other than the neutral one in any file, and in a
//! recording, any CLI's plan, an id that ties it to the account or the install, and a time
//! that tells the recording machine's time zone. Each check reads the line as JSON, TOML or
//! YAML writes a key and its value.

use super::private::Found;
use super::table::Private;

/// What a file is, for the checks that read only some files.
#[derive(Debug, Clone, Copy)]
pub struct Kind {
    pub recording: bool,
    /// Only YAML writes text without quotes, so only there is a bare word a plan.
    pub yaml: bool,
}

pub fn line<'t>(text: &str, kind: Kind, private: &'t Private, found: &mut Found<'t>) {
    plans(text, kind, private, found);
    if kind.recording {
        plan_labels(text, private, found);
        ids(text, private, found);
        offsets(text, private, found);
    }
}

/// The line a document's finding is on: the first that holds the value found, or none when
/// only the whole document shows it.
pub fn line_of(text: &str, found: &str) -> Option<usize> {
    let value = found.rsplit_once(": ").map_or(found, |(_, value)| value);
    text.lines()
        .position(|line| line.contains(&format!("\"{value}\"")))
        .map(|at| at + 1)
}

/// A value as a line writes it under a key.
enum Field<'l> {
    Quoted(&'l str),
    /// Unquoted: null, a number, or YAML's text.
    Bare(&'l str),
}

/// Each value under `key` in a line: `"key": "value"`, `key = "value"` or `key: value`.
fn fields<'l>(text: &'l str, key: &str) -> Vec<Field<'l>> {
    let mut found = Vec::new();
    for (at, _) in text.match_indices(key) {
        let before = text[..at].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let mut rest = &text[at + key.len()..];
        match before {
            Some(quote @ ('"' | '\'')) => match rest.strip_prefix(quote) {
                Some(after) => rest = after,
                None => continue,
            },
            _ if rest.starts_with(|c: char| c.is_alphanumeric() || c == '_') => continue,
            _ => {}
        }
        let Some(rest) = rest.trim_start().strip_prefix([':', '=']) else {
            continue;
        };
        let rest = rest.trim_start();
        found.push(match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let inner = &rest[1..];
                Field::Quoted(&inner[..inner.find(quote).unwrap_or(inner.len())])
            }
            _ => {
                let end = rest
                    .find(|c: char| !(c.is_alphanumeric() || "_-.<>".contains(c)))
                    .unwrap_or(rest.len());
                Field::Bare(&rest[..end])
            }
        });
    }
    found
}

fn is_placeholder(text: &str) -> bool {
    text.len() > 2 && text.starts_with('<') && text.ends_with('>')
}

/// Codex's plan in any file, and every CLI's plan in a recording, unless it's the neutral one
/// or a placeholder.
fn plans<'t>(text: &str, kind: Kind, private: &'t Private, found: &mut Found<'t>) {
    let everywhere = private.plan_keys.iter().map(|key| (key, false));
    let recorded = private
        .recorded_plan_keys
        .iter()
        .filter(|_| kind.recording)
        .map(|key| (key, true));
    for (key, recorded) in everywhere.chain(recorded) {
        for field in fields(text, key) {
            let plan = match field {
                Field::Quoted(plan) => plan,
                Field::Bare(plan) if kind.yaml => plan,
                Field::Bare(_) => continue,
            };
            let text_not_a_plan = recorded && plan.contains(char::is_whitespace);
            let neutral = plan.is_empty()
                || plan == "null"
                || is_placeholder(plan)
                || plan.eq_ignore_ascii_case(&private.neutral_plan);
            if !neutral && !text_not_a_plan {
                found.push((format!("{key}: {plan}"), private.plan_fix.as_str()));
            }
        }
    }
}

/// A plan in a CLI's own words, after its label, unless it names the neutral plan.
fn plan_labels<'t>(text: &str, private: &'t Private, found: &mut Found<'t>) {
    for label in &private.plan_labels {
        for (at, _) in text.match_indices(label.as_str()) {
            let rest = &text[at + label.len()..];
            let plan = rest[..rest.find(['\n', '"', '│', '|']).unwrap_or(rest.len())].trim();
            let neutral = plan.is_empty()
                || is_placeholder(plan)
                || plan
                    .split(|c: char| !c.is_alphanumeric())
                    .any(|word| word.eq_ignore_ascii_case(&private.neutral_plan));
            if !neutral {
                found.push((format!("{label} {plan}"), private.plan_fix.as_str()));
            }
        }
    }
}

/// An account's, organization's or install's id, unless it is null, a placeholder or zeros.
fn ids<'t>(text: &str, private: &'t Private, found: &mut Found<'t>) {
    for key in &private.ids.keys {
        for field in fields(text, key) {
            let id = match field {
                Field::Quoted(id) => id,
                Field::Bare(id) if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) => id,
                Field::Bare(_) => continue,
            };
            let neutral = id.is_empty()
                || id == "null"
                || is_placeholder(id)
                || id.chars().all(|c| c == '0' || c == '-');
            if !neutral {
                found.push((format!("{key}: {id}"), private.ids.fix.as_str()));
            }
        }
    }
}

/// A time with an offset other than UTC's, as `2026-01-02T03:04:05+05:30`.
fn offsets<'t>(text: &str, private: &'t Private, found: &mut Found<'t>) {
    let bytes = text.as_bytes();
    let digits = |from: usize, count: usize| {
        bytes
            .get(from..from + count)
            .is_some_and(|run| run.iter().all(u8::is_ascii_digit))
    };
    let at_byte = |at: usize, byte: u8| bytes.get(at) == Some(&byte);
    for (t, _) in text.match_indices('T') {
        let Some(date) = t.checked_sub(10) else {
            continue;
        };
        let is_date = digits(date, 4)
            && at_byte(date + 4, b'-')
            && digits(date + 5, 2)
            && at_byte(date + 7, b'-')
            && digits(date + 8, 2);
        if !is_date || !digits(t + 1, 2) || !at_byte(t + 3, b':') || !digits(t + 4, 2) {
            continue;
        }
        let mut end = t + 6;
        if at_byte(end, b':') && digits(end + 1, 2) {
            end += 3;
            if at_byte(end, b'.') {
                end += 1;
                while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                    end += 1;
                }
            }
        }
        if !(at_byte(end, b'+') || at_byte(end, b'-')) || !digits(end + 1, 2) {
            continue;
        }
        let minutes = if at_byte(end + 3, b':') {
            end + 4
        } else {
            end + 3
        };
        if !digits(minutes, 2) {
            continue;
        }
        let offset = format!("{}{}", &text[end + 1..end + 3], &text[minutes..minutes + 2]);
        if offset != "0000" {
            found.push((
                text[date..minutes + 2].to_owned(),
                private.time_fix.as_str(),
            ));
        }
    }
}
