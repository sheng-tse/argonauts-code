//! Some models write a tool call with many parameters as one string that runs on past its own
//! field: the answer ends with `</answer>`, then `<files_changed>[…]</files_changed>` or
//! `<parameter name="files_changed">[…]` carry the parameters that never became fields. Others
//! add a field of their own that the schema forbids. The report is mended back into the fields
//! it meant before it is checked.

use serde_json::{Map, Value};

/// `report` with the parameters that leaked into one of its strings made fields again, each
/// string cut at its own closing tag, and the fields the schema doesn't know dropped when it
/// allows no others. A field the call did send keeps its own value. `None` when nothing needed
/// mending.
pub fn mended(report: &Value, schema: &Value) -> Option<Value> {
    let fields = report.as_object()?;
    let known = schema.get("properties")?.as_object()?;
    let closed = schema.get("additionalProperties") == Some(&Value::Bool(false));
    let mut mended = fields.clone();
    let mut leaked = Map::new();
    for (key, value) in fields {
        let Some(text) = value.as_str() else {
            continue;
        };
        let Some((own, rest)) = text.split_once(&format!("</{key}>")) else {
            continue;
        };
        mended.insert(key.clone(), Value::String(own.trim_end().to_owned()));
        for (name, value) in parameters(rest) {
            if known.contains_key(&name) {
                leaked.entry(name).or_insert(value);
            }
        }
    }
    for (name, value) in leaked {
        mended.entry(name).or_insert(value);
    }
    if closed {
        mended.retain(|name, _| known.contains_key(name));
    }
    (mended != *fields).then_some(Value::Object(mended))
}

/// Each `<name>value</name>` or `<parameter name="name">value</parameter>` in `text`, in order.
/// A value runs to its closing tag, else to the next parameter or the end. One that reads as
/// JSON is that JSON; any other is its text.
fn parameters(text: &str) -> Vec<(String, Value)> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some((name, _, start)) = next_opening(rest) {
        let after = &rest[start..];
        let closing = [format!("</{name}>"), "</parameter>".to_owned()];
        let end = closing
            .iter()
            .filter_map(|tag| after.find(tag.as_str()).map(|at| (at, tag.len())))
            .min();
        let next = next_opening(after).map(|(_, open, _)| open);
        let (body, skip) = match (end, next) {
            (Some((at, _)), Some(open)) if open < at => (&after[..open], open),
            (Some((at, len)), _) => (&after[..at], at + len),
            (None, Some(open)) => (&after[..open], open),
            (None, None) => (after, after.len()),
        };
        let body = body.trim();
        let value = serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.to_owned()));
        found.push((name, value));
        rest = &after[skip..];
    }
    found
}

/// The next opening tag in `text`: its parameter's name, where the tag starts, and where the
/// text after it starts. Closing tags and anything that isn't a plain name are passed over.
fn next_opening(text: &str) -> Option<(String, usize, usize)> {
    let mut from = 0;
    while let Some(at) = text[from..].find('<').map(|at| from + at) {
        let tail = &text[at + 1..];
        let close = tail.find('>')?;
        let tag = &tail[..close];
        let name = match tag.strip_prefix("parameter name=") {
            Some(quoted) => quoted.trim_matches('"'),
            None => tag,
        };
        let plain = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if plain {
            return Some((name.to_owned(), at, at + close + 2));
        }
        from = at + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": {"type": "string"},
                "answer": {"type": "string"},
                "files_changed": {"type": "array", "items": {"type": "string"}},
                "tests_run": {"type": "array", "items": {"type": "string"}},
                "questions": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["status", "answer", "files_changed", "tests_run", "questions"],
            "additionalProperties": false
        })
    }

    #[test]
    fn parameters_written_as_tags_become_fields_again() {
        let report = json!({
            "status": "done",
            "answer": "All 10 tests pass.</answer>\n<files_changed>[\"invoice/money.py\", \
                       \"tests/test_totals.py\"]</files_changed>\n<tests_run>[\"python3 -m \
                       unittest: Ran 10 tests, OK\"]</tests_run>\n<questions>[]</questions>\n"
        });
        assert_eq!(
            mended(&report, &schema()),
            Some(json!({
                "status": "done",
                "answer": "All 10 tests pass.",
                "files_changed": ["invoice/money.py", "tests/test_totals.py"],
                "tests_run": ["python3 -m unittest: Ran 10 tests, OK"],
                "questions": []
            }))
        );
    }

    #[test]
    fn a_parameter_tag_with_no_end_runs_to_the_end_and_unknown_fields_go() {
        let report = json!({
            "status": "done",
            "answer": "Done.</answer>\n<parameter name=\"files_changed\">[\"a.py\"]",
            "tests_run": [],
            "questions": [],
            "concerns_placeholder": "unused"
        });
        assert_eq!(
            mended(&report, &schema()),
            Some(json!({
                "status": "done",
                "answer": "Done.",
                "files_changed": ["a.py"],
                "tests_run": [],
                "questions": []
            }))
        );
    }

    #[test]
    fn a_field_the_call_sent_keeps_its_value() {
        let report = json!({
            "status": "done",
            "answer": "Done.</answer><files_changed>[\"b.py\"]</files_changed><questions>[\"Which?\"]",
            "files_changed": ["a.py"]
        });
        let mended = mended(&report, &schema()).unwrap();
        assert_eq!(mended["files_changed"], json!(["a.py"]));
        assert_eq!(mended["questions"], json!(["Which?"]));
    }

    #[test]
    fn a_field_the_schema_forbids_goes() {
        let report = json!({
            "status": "done",
            "answer": "Done.",
            "files_changed": [],
            "tests_run": [],
            "questions": [],
            "concerns_extra": "none"
        });
        let mended = mended(&report, &schema()).unwrap();
        assert!(mended.get("concerns_extra").is_none(), "{mended}");
        assert_eq!(mended["answer"], json!("Done."));
    }

    #[test]
    fn a_string_that_only_mentions_tags_is_left_alone() {
        let report = json!({
            "status": "done",
            "answer": "Wrapped the list in <ul> and closed it with </ul>.",
            "files_changed": [],
            "tests_run": [],
            "questions": []
        });
        assert_eq!(mended(&report, &schema()), None);
    }
}
