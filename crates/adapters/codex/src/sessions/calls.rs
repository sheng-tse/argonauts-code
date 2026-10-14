//! A rollout's tool calls, named and given as `codex exec`'s live stream names and gives them,
//! so a call reprinted from a session draws the row it drew live.

use std::fmt::Write as _;

use argonauts_core::cli_sessions::{CliCall, CliPatch};
use serde_json::{Value, json};

/// The names Codex has given its shell tool.
const SHELL_TOOLS: &[&str] = &[
    "shell",
    "shell_command",
    "exec_command",
    "local_shell",
    "container.exec",
];

const PATCH_TOOL: &str = "apply_patch";

/// The call `name` made with `given`: a function's arguments as JSON text, or a custom tool's
/// input as plain text.
pub(super) fn call(id: &str, name: &str, given: Option<&str>) -> CliCall {
    let given = given.unwrap_or_default();
    let arguments = serde_json::from_str::<Value>(given).ok();
    if SHELL_TOOLS.contains(&name) {
        let command = arguments.as_ref().map(command_line).unwrap_or_default();
        return CliCall::new(id, "command_execution", json!({ "command": command }));
    }
    if name == PATCH_TOOL {
        // As a function it takes `{"input": "<patch>"}`, and as a custom tool the patch itself.
        let patch = arguments
            .as_ref()
            .and_then(|arguments| arguments.get("input"))
            .and_then(Value::as_str)
            .unwrap_or(given);
        return patch_call(id, patch);
    }
    let input = arguments.unwrap_or_else(|| Value::String(given.to_owned()));
    CliCall::new(id, name, input)
}

/// `{"command": ["rg", "read_key"]}`, or `{"cmd": "rg read_key"}`, as one line.
fn command_line(arguments: &Value) -> String {
    let command = arguments.get("command").or_else(|| arguments.get("cmd"));
    match command {
        Some(Value::String(line)) => line.clone(),
        Some(Value::Array(words)) => words
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// How a call ended and what it printed. Codex has written a result as plain text, as
/// `{"output", "metadata": {"exit_code"}}` in JSON text, and as `{"content", "success"}`.
pub(super) fn result(output: Option<&Value>) -> (bool, Option<String>) {
    let text = match output {
        Some(Value::String(text)) => text.as_str(),
        Some(Value::Object(fields)) => {
            let ok = fields
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let text = fields.get("content").and_then(Value::as_str);
            return (ok, non_empty(text.unwrap_or_default()));
        }
        _ => return (true, None),
    };
    if let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(text) {
        let code = fields
            .get("metadata")
            .and_then(|metadata| metadata.get("exit_code"))
            .and_then(Value::as_i64);
        let printed = fields.get("output").and_then(Value::as_str);
        return (
            code.unwrap_or(0) == 0,
            non_empty(printed.unwrap_or_default()),
        );
    }
    // `Exit code: 1`, a line of timing, then `Output:` and what it printed.
    if let Some(rest) = text.strip_prefix("Exit code: ") {
        let code = rest.lines().next().unwrap_or_default().trim();
        let printed = rest
            .split_once("Output:\n")
            .map_or("", |(_, printed)| printed);
        return (code == "0", non_empty(printed));
    }
    (true, non_empty(text))
}

fn non_empty(text: &str) -> Option<String> {
    (!text.trim().is_empty()).then(|| text.to_owned())
}

/// What reasoning's summary says, its parts a paragraph each.
pub(super) fn summary_text(summary: &[Value]) -> String {
    summary
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// An `apply_patch` call as the live stream's `file_change`, with each file's hunks.
fn patch_call(id: &str, patch: &str) -> CliCall {
    let files = files(patch);
    let changes: Vec<Value> = files
        .iter()
        .map(|file| json!({ "path": file.path, "kind": file.kind }))
        .collect();
    let mut call = CliCall::new(id, "file_change", json!({ "changes": changes }));
    call.patches = files
        .into_iter()
        .filter_map(|file| {
            let hunks = file.hunks();
            (!hunks.is_empty()).then_some(CliPatch {
                path: file.path,
                hunks,
                numbered: false,
            })
        })
        .collect();
    call
}

/// One file of a patch: `*** Update File: src/a.rs` and the lines under it.
struct PatchedFile {
    path: String,
    /// `add`, `update` or `delete`, as the live stream says it.
    kind: &'static str,
    lines: Vec<String>,
}

fn files(patch: &str) -> Vec<PatchedFile> {
    let mut files: Vec<PatchedFile> = Vec::new();
    for line in patch.lines() {
        let started = [
            ("*** Add File: ", "add"),
            ("*** Update File: ", "update"),
            ("*** Delete File: ", "delete"),
        ]
        .into_iter()
        .find_map(|(marker, kind)| Some((line.strip_prefix(marker)?, kind)));
        if let Some((path, kind)) = started {
            files.push(PatchedFile {
                path: path.trim().to_owned(),
                kind,
                lines: Vec::new(),
            });
        } else if let Some(file) = files.last_mut()
            // `*** Move to:`, `*** End of File` and `*** End Patch` change no line.
            && !line.starts_with("*** ")
        {
            file.lines.push(line.to_owned());
        }
    }
    files
}

impl PatchedFile {
    /// Its changes as unified hunks. A patch names no line numbers, so each hunk counts from
    /// one and only its lengths are true.
    fn hunks(&self) -> String {
        let mut out = String::new();
        let mut chunk: Vec<&str> = Vec::new();
        let mut flush = |chunk: &mut Vec<&str>| {
            let old = chunk.iter().filter(|line| !line.starts_with('+')).count();
            let new = chunk.iter().filter(|line| !line.starts_with('-')).count();
            if chunk.iter().any(|line| !line.starts_with(' ')) {
                let _ = writeln!(out, "@@ -1,{old} +1,{new} @@");
                for line in chunk.iter() {
                    out.push_str(line);
                    out.push('\n');
                }
            }
            chunk.clear();
        };
        for line in &self.lines {
            if line.starts_with("@@") {
                flush(&mut chunk);
            } else if line.starts_with([' ', '+', '-']) {
                chunk.push(line);
            } else if line.is_empty() {
                // An empty context line, whose leading space an editor trimmed.
                chunk.push(" ");
            }
        }
        flush(&mut chunk);
        out
    }
}
