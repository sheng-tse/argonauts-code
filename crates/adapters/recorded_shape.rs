//! Shared by each CLI adapter's recording tests. A CLI update moves no recording: a fresh
//! run is worth keeping only when its shape changed, the kinds of line and their fields, and
//! not when the model merely worded its answer another way. The recorder says which it is
//! as it writes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

/// Every kind of line a recording holds, with the fields seen on lines of that kind. Values
/// are left out, so two runs with nothing but their wording apart have one shape.
pub type Shape = BTreeMap<String, BTreeSet<String>>;

pub fn shape(recording: &str) -> Shape {
    let mut shape = Shape::new();
    for line in recording.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str::<Value>(line) {
            Ok(value) => fields(&value, "", shape.entry(kind(&value)).or_default()),
            // A banner or a warning the CLI printed among its events.
            Err(_) => {
                shape.entry("plain text".to_owned()).or_default();
            }
        }
    }
    shape
}

/// A line's kind: its `type`, with its `subtype` when it has one.
fn kind(line: &Value) -> String {
    let named = |key: &str| line.get(key).and_then(Value::as_str);
    match (named("type"), named("subtype")) {
        (Some(kind), Some(subtype)) => format!("{kind}/{subtype}"),
        (Some(kind), None) => kind.to_owned(),
        _ => "untyped".to_owned(),
    }
}

/// Adds the path of every field under `value`. An object inside a line that names its own
/// type is one of several forms, each with fields of its own, so the type joins the path.
fn fields(value: &Value, path: &str, found: &mut BTreeSet<String>) {
    match value {
        Value::Object(inside) => {
            let form = inside
                .get("type")
                .and_then(Value::as_str)
                .filter(|_| !path.is_empty());
            let path = match form {
                Some(form) => format!("{path}<{form}>"),
                None => path.to_owned(),
            };
            if form.is_some() {
                found.insert(path.clone());
            }
            let dot = if path.is_empty() { "" } else { "." };
            for (key, inner) in inside {
                if form.is_some() && key == "type" {
                    continue;
                }
                let field = format!("{path}{dot}{key}");
                fields(inner, &field, found);
                found.insert(field);
            }
        }
        // A list is a field whether or not this run put anything in it.
        Value::Array(items) => {
            for item in items {
                fields(item, &format!("{path}[]"), found);
            }
        }
        _ => {}
    }
}

/// What a fresh shape has that the stored one hadn't, and what it lacks, one line for each.
pub fn changes(stored: &Shape, fresh: &Shape) -> Vec<String> {
    let mut changes = Vec::new();
    for (kind, fields) in fresh {
        match stored.get(kind) {
            None => changes.push(format!("new kind of line: {kind}")),
            Some(before) => changes.extend(
                fields
                    .difference(before)
                    .map(|field| format!("{kind} gained {field}")),
            ),
        }
    }
    for (kind, fields) in stored {
        match fresh.get(kind) {
            None => changes.push(format!("no {kind} line any more")),
            Some(now) => changes.extend(
                fields
                    .difference(now)
                    .map(|field| format!("{kind} lost {field}")),
            ),
        }
    }
    changes
}

/// Says how a fresh run compares with the recording stored at `stored`, before it is written
/// over. One of the same shape can be put back from git, with the new version added to the
/// `also_matches` of its `.meta.json`.
pub fn report(stored: &Path, fresh: &str) {
    let name = stored.file_name().unwrap_or_default().to_string_lossy();
    let Ok(before) = std::fs::read_to_string(stored) else {
        println!("{name}: recorded for the first time");
        return;
    };
    let changes = changes(&shape(&before), &shape(fresh));
    if changes.is_empty() {
        println!("{name}: the same shape as the stored recording, only the wording is new");
        return;
    }
    println!("{name}: the shape changed");
    for change in changes {
        println!("  {change}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STORED: &str = r#"{"type":"system","subtype":"init","session_id":"s-1","tools":["Read"]}
{"type":"assistant","message":{"content":[{"type":"text","text":"Two files."}]}}
{"type":"result","subtype":"success","usage":{"input_tokens":12}}
"#;

    #[test]
    fn other_wording_is_the_same_shape() {
        let fresh = r#"{"type":"system","subtype":"init","session_id":"s-2","tools":["Read","Grep"]}
{"type":"assistant","message":{"content":[{"type":"text","text":"I found"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":" two files."}]}}
{"type":"result","subtype":"success","usage":{"input_tokens":40}}
"#;
        assert_eq!(changes(&shape(STORED), &shape(fresh)), Vec::<String>::new());
    }

    #[test]
    fn a_new_field_a_new_form_and_a_new_kind_of_line_change_the_shape() {
        let fresh = r#"{"type":"system","subtype":"init","session_id":"s-2","tools":[],"plugins":[]}
{"type":"system","subtype":"status","status":"compacting"}
{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"Hm."}]}}
{"type":"result","subtype":"success","usage":{"input_tokens":40}}
"#;
        assert_eq!(
            changes(&shape(STORED), &shape(fresh)),
            [
                "assistant gained message.content[]<thinking>",
                "assistant gained message.content[]<thinking>.thinking",
                "system/init gained plugins",
                "new kind of line: system/status",
                "assistant lost message.content[]<text>",
                "assistant lost message.content[]<text>.text",
            ]
        );
    }

    #[test]
    fn a_missing_kind_of_line_and_a_line_that_is_not_json_are_told() {
        let fresh = "Warning: an update is ready\n{\"type\":\"result\",\"subtype\":\"success\",\"usage\":{\"input_tokens\":1}}\n";
        assert_eq!(
            changes(&shape(STORED), &shape(fresh)),
            [
                "new kind of line: plain text",
                "no assistant line any more",
                "no system/init line any more",
            ]
        );
    }
}
