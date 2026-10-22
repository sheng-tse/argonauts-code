//! Labels each action by rule from the tool call the adapters parsed: its name, its input,
//! whether it worked, and for an edit the patch the CLI gave. No model is asked.

mod files;
mod shell;
mod test_edits;

use argonauts_core::behaviors::{Behavior, Label};
use argonauts_core::patch_files::patched_files;
use serde_json::Value;

pub(crate) use files::{FileRole, path_of};
pub(crate) use shell::unwrapped as script_of;

/// Raised whenever a rule changes what an action is labelled, so labels kept from before are
/// worked out again.
pub(crate) const RULES_VERSION: u32 = 2;

/// One tool call as a run's log has it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ToolCall<'a> {
    /// As the CLI names it, such as `Bash`, `command_execution` or `view_file`.
    pub name: &'a str,
    pub input: &'a Value,
    /// `None` while it runs, or when its end was never logged.
    pub ok: Option<bool>,
    /// The hunks of its file patch, when the CLI gave one.
    pub patch: Option<&'a str>,
    /// The project's folder, where it's known, which a file's role is judged below.
    pub root: Option<&'a str>,
}

/// What kind of tool a CLI's tool is, whichever CLI named it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Read,
    Search,
    List,
    Shell,
    Edit,
    Write,
    /// Codex's `file_change`: a list of files, each added, updated or deleted.
    FileChange,
    /// Codex's and OpenCode's patch text, `*** Update File: …`.
    Patch,
    Answer,
    Workflow,
    AskAgent,
    RunStatus,
    Subagent,
    AskUser,
    Todo,
    Plan,
    DeferredTools,
    WebFetch,
    WebSearch,
    OtherMcp,
}

fn tool_of(name: &str) -> Option<Tool> {
    let lower = name.to_ascii_lowercase();
    // MCP tools come as Claude Code's `mcp__server__tool` and Codex's `mcp:server/tool`;
    // Argonauts' own are known by their names wherever they come from.
    if lower.contains("argonauts_report") {
        return Some(Tool::Answer);
    }
    if lower.contains("argonauts_ask_") {
        return Some(Tool::AskAgent);
    }
    const STATUS: [&str; 5] = [
        "argonauts_status",
        "argonauts_run_status",
        "argonauts_list_runs",
        "argonauts_run_events",
        "argonauts_lookup",
    ];
    if STATUS.iter().any(|tool| lower.ends_with(tool)) {
        return Some(Tool::RunStatus);
    }
    if lower.starts_with("mcp__") || lower.starts_with("mcp:") || lower.starts_with("argonauts_") {
        return Some(Tool::OtherMcp);
    }
    let tool = match lower.as_str() {
        "read" | "view_file" | "read_file" | "view_code_item" | "notebookread" => Tool::Read,
        "grep" | "grep_search" | "codebase_search" | "search" => Tool::Search,
        "glob" | "ls" | "list" | "list_dir" | "find_by_name" => Tool::List,
        "bash" | "command_execution" | "run_command" | "shell" | "exec_command" | "local_shell" => {
            Tool::Shell
        }
        "edit"
        | "multiedit"
        | "replace_file_content"
        | "multi_replace_file_content"
        | "sed_file"
        | "notebookedit"
        | "notebook_edit" => Tool::Edit,
        "write" | "write_to_file" | "create_file" => Tool::Write,
        "file_change" => Tool::FileChange,
        "patch" | "apply_patch" => Tool::Patch,
        "structuredoutput" | "structured_output" | "finish" => Tool::Answer,
        "workflow" => Tool::Workflow,
        "task" | "agent" | "invoke_subagent" | "spawn_agent" | "manage_subagents" => Tool::Subagent,
        "askuserquestion" | "ask_user" | "question" => Tool::AskUser,
        "todowrite" | "todoread" | "todo" | "update_plan" | "manage_task" => Tool::Todo,
        "exitplanmode" => Tool::Plan,
        "toolsearch" => Tool::DeferredTools,
        "webfetch" | "read_url_content" | "fetch" => Tool::WebFetch,
        "websearch" | "web_search" | "search_web" => Tool::WebSearch,
        _ => return None,
    };
    Some(tool)
}

/// The behavior of one tool call.
pub(crate) fn label(call: &ToolCall<'_>) -> Behavior {
    let Some(tool) = tool_of(call.name) else {
        return Behavior::Unlabelled;
    };
    let failed = call.ok == Some(false);
    let path = files::path_of(call.input);
    let root = call.root;
    let one = |label: Label| Behavior::One(label);
    match tool {
        Tool::Read if failed => one(Label::ReadFailed),
        Tool::Read => path.map_or(Behavior::Unlabelled, |path| one(files::read(path, root))),
        Tool::Search | Tool::List if path.is_some_and(files::is_records) => one(Label::ListRecords),
        Tool::Search => one(Label::SearchCode),
        Tool::List => one(Label::ListFiles),
        Tool::Shell => shell(call.input, failed, root),
        Tool::Edit | Tool::Write if failed => one(Label::EditFailed),
        Tool::Edit => path.map_or(Behavior::Unlabelled, |path| one(edited(path, call))),
        Tool::Write => path.map_or(Behavior::Unlabelled, |path| one(written(path, call))),
        Tool::FileChange if failed => one(Label::EditFailed),
        Tool::FileChange => file_change(call),
        Tool::Patch if failed => one(Label::EditFailed),
        Tool::Patch => patch(call),
        Tool::Answer if failed => one(Label::AnswerFailed),
        Tool::Answer => one(Label::Answer),
        Tool::Workflow => one(Label::Workflow),
        Tool::AskAgent => one(Label::AskReview),
        Tool::RunStatus => one(Label::RunStatus),
        Tool::Subagent => one(Label::Subagent),
        Tool::AskUser => one(Label::AskUser),
        Tool::Todo => one(Label::Todo),
        Tool::Plan => one(Label::PresentPlan),
        Tool::DeferredTools => one(Label::ToolSearch),
        Tool::WebFetch => one(Label::WebFetch),
        Tool::WebSearch => one(Label::WebSearch),
        Tool::OtherMcp => one(Label::McpTool),
    }
}

/// Its first two kinds; a failed run of the tests says so, since tests that fail are a
/// behavior of their own.
fn shell(input: &Value, failed: bool, root: Option<&str>) -> Behavior {
    let command = command_of(input);
    let mut kinds = shell::kinds(&command, root);
    if failed {
        for kind in &mut kinds {
            if *kind == Label::RunTests {
                *kind = Label::RunTestsFailed;
            }
        }
    }
    match Behavior::of(kinds) {
        Behavior::Unlabelled => Behavior::One(Label::Shell),
        behavior => behavior,
    }
}

/// The command line: Claude Code's and Codex's `command` (a string, or Codex's argument
/// list), agy's `CommandLine`, or `cmd`.
pub(crate) fn command_of(input: &Value) -> String {
    let value = ["command", "CommandLine", "cmd"]
        .iter()
        .find_map(|key| input.get(*key));
    match value {
        Some(Value::String(command)) => command.clone(),
        Some(Value::Array(words)) => {
            let words: Vec<&str> = words.iter().filter_map(Value::as_str).collect();
            match words.as_slice() {
                [_, flag, script] if flag.starts_with('-') && flag.ends_with('c') => {
                    (*script).to_owned()
                }
                _ => words.join(" "),
            }
        }
        _ => String::new(),
    }
}

fn edited(path: &str, call: &ToolCall<'_>) -> Label {
    match files::project_role(path, call.root) {
        FileRole::Test => {
            let (removed, added) = match call.patch {
                Some(hunks) => test_edits::patch_lines(hunks),
                None => test_edits::input_lines(call.input),
            };
            test_edits::label(&removed, &added)
        }
        FileRole::Config => Label::EditConfig,
        FileRole::Doc => Label::EditDoc,
        FileRole::Source | FileRole::Records => Label::EditSource,
    }
}

/// A file written whole. A test file written over is read by what its patch did to its
/// tests, as an edit is; a new one has no patch.
fn written(path: &str, call: &ToolCall<'_>) -> Label {
    let test = files::project_role(path, call.root) == FileRole::Test;
    match call.patch {
        Some(_) if test => edited(path, call),
        _ => files::wrote(path, call.root),
    }
}

/// Codex's `file_change`, by its first file: `{"changes": [{"path", "kind"}]}`.
fn file_change(call: &ToolCall<'_>) -> Behavior {
    let first = call
        .input
        .get("changes")
        .and_then(Value::as_array)
        .and_then(|changes| changes.first());
    let Some(change) = first else {
        return Behavior::Unlabelled;
    };
    let path = change.get("path").and_then(Value::as_str).unwrap_or("");
    let kind = change
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("update");
    Behavior::One(changed(path, kind, call))
}

/// Patch text, by its first file; an update to a test file is read from the patch's own
/// lines.
fn patch(call: &ToolCall<'_>) -> Behavior {
    let text = ["patchText", "patch", "input"]
        .iter()
        .find_map(|key| call.input.get(*key)?.as_str())
        .or_else(|| call.input.as_str())
        .unwrap_or("");
    let Some((path, kind)) = patched_files(text).into_iter().next() else {
        return Behavior::Unlabelled;
    };
    let with_lines = ToolCall {
        patch: Some(call.patch.unwrap_or(text)),
        ..*call
    };
    Behavior::One(changed(&path, kind, &with_lines))
}

/// A test file deleted removes its tests.
fn changed(path: &str, kind: &str, call: &ToolCall<'_>) -> Label {
    match kind {
        "add" => files::wrote(path, call.root),
        "delete" if files::holds_tests(path, call.root) => Label::EditTestRemoved,
        "delete" => Label::DeleteFile,
        _ => edited(path, call),
    }
}

#[cfg(test)]
mod tests;
