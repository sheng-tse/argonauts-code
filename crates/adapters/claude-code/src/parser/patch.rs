//! An edit's `structuredPatch`, which Claude Code sends in the result's `tool_use_result`: the
//! hunks it draws under the call, with the file's own line numbers and three lines of context
//! around each change.

use std::fmt::Write as _;

use argonauts_core::AgentEvent;
use serde_json::Value;

use crate::parser::json::{array_at, str_at, u64_at};

/// The tools whose result carries a patch. A new file's patch is empty, and its content is
/// already in the call.
const PATCHING_TOOLS: &[&str] = &["Edit", "MultiEdit", "Write"];

/// The patch a finished file tool reports, as unified hunks. `None` for any other tool, or
/// when the result has no hunks.
pub(crate) fn file_patch(
    id: &str,
    tool: &str,
    input: &Value,
    structured: Option<&Value>,
    parent: Option<String>,
) -> Option<AgentEvent> {
    if !PATCHING_TOOLS.contains(&tool) {
        return None;
    }
    let structured = structured?;
    let path = str_at(structured, "filePath").or_else(|| str_at(input, "file_path"))?;
    let hunks = unified(array_at(structured, "structuredPatch"));
    if hunks.is_empty() {
        return None;
    }
    Some(AgentEvent::FilePatch {
        id: id.to_owned(),
        path: path.to_owned(),
        hunks,
        parent,
    })
}

/// `@@ -a,b +c,d @@` and the hunk's lines, which already start with their marker.
fn unified(hunks: &[Value]) -> String {
    let mut out = String::new();
    for hunk in hunks {
        let lines: Vec<&str> = array_at(hunk, "lines")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        if lines.is_empty() {
            continue;
        }
        let _ = writeln!(
            out,
            "@@ -{},{} +{},{} @@",
            u64_at(hunk, "oldStart"),
            u64_at(hunk, "oldLines"),
            u64_at(hunk, "newStart"),
            u64_at(hunk, "newLines"),
        );
        for line in lines {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn an_edit_reports_its_hunks_with_the_file_numbers() {
        let result = json!({
            "filePath": "/w/src/lib.rs",
            "structuredPatch": [
                {"oldStart": 8, "oldLines": 4, "newStart": 8, "newLines": 5,
                 "lines": [" fn a() {}", "-fn b() {}", "+fn b() -> u8 {", "+    1", " ", " fn c() {}"]},
                {"oldStart": 40, "oldLines": 1, "newStart": 41, "newLines": 1,
                 "lines": ["-x", "+y", "\\ No newline at end of file"]}
            ]
        });
        let event = file_patch("t1", "Edit", &json!({}), Some(&result), None);
        assert_eq!(
            event,
            Some(AgentEvent::FilePatch {
                id: "t1".into(),
                path: "/w/src/lib.rs".into(),
                hunks: "@@ -8,4 +8,5 @@\n fn a() {}\n-fn b() {}\n+fn b() -> u8 {\n+    1\n \n \
                        fn c() {}\n@@ -40,1 +41,1 @@\n-x\n+y\n\\ No newline at end of file\n"
                    .into(),
                parent: None,
            })
        );
    }

    #[test]
    fn a_new_file_or_another_tool_reports_none() {
        let created = json!({"type": "create", "filePath": "/w/a.txt", "structuredPatch": []});
        assert_eq!(
            file_patch("t1", "Write", &json!({}), Some(&created), None),
            None
        );
        let read = json!({"filePath": "/w/a.txt", "structuredPatch": [
            {"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1, "lines": ["-a", "+b"]}]});
        assert_eq!(
            file_patch("t1", "Read", &json!({}), Some(&read), None),
            None
        );
        assert_eq!(file_patch("t1", "Edit", &json!({}), None, None), None);
    }
}
