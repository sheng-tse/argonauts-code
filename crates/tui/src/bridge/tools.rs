//! One table for every vendor's tool names, so a read looks the same from Claude Code, Codex,
//! agy, or an ACP agent, and each call says what it did (`Read 120 lines`, not `Read 1 file`).
//! Names and input keys differ by vendor, so matching ignores case, `_`, and `-`.

use argonauts_core::artifacts;
use argonauts_core::steps::summary_line;
use serde_json::{Map, Value};

use crate::cell::{DiffView, Todo, TodoState, Tone, ToolCall, ToolItem, ToolKind, ToolTitle};

/// Lines of a written file shown under its call.
const WRITE_PREVIEW: usize = 10;

/// Longest args kept in a call's title, in characters.
const ARGS_MAX: usize = 160;

/// What a call does, whatever the vendor calls it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Read,
    List,
    Search,
    Shell,
    Update,
    Write,
    Delete,
    Fetch,
    WebSearch,
    Todos,
    Task,
    Mcp,
    Report,
    /// Claude Code's Artifact tool, which publishes a page on claude.ai.
    Artifact,
    Other,
}

impl Tool {
    fn named(name: &str) -> Self {
        let normal = normalize(name);
        // However the vendor prefixes its server, as `mcp__argonauts__argonauts_report` or
        // OpenCode's `argonauts_argonauts_report`, it's the report.
        if normal.ends_with("argonautsreport") {
            return Tool::Report;
        }
        if name.starts_with("mcp__") || name.starts_with("mcp:") {
            return Tool::Mcp;
        }
        match normal.as_str() {
            "read" | "readfile" | "viewfile" | "view" | "cat" | "notebookread" | "viewcodeitem" => {
                Tool::Read
            }
            "ls" | "list" | "listdir" | "listdirectory" | "listfiles" => Tool::List,
            "grep" | "grepsearch" | "search" | "codebasesearch" | "codesearch" | "rg"
            | "filesearch" | "glob" | "findbyname" | "find" => Tool::Search,
            "bash" | "shell" | "commandexecution" | "runcommand" | "execcommand" | "exec"
            | "execute" | "terminal" | "localshell" => Tool::Shell,
            "edit"
            | "multiedit"
            | "editfile"
            | "replacefilecontent"
            | "multireplacefilecontent"
            | "strreplace"
            | "strreplacebasededittool"
            | "applypatch"
            | "patch"
            | "filechange"
            | "notebookedit"
            | "sedfile"
            | "move" => Tool::Update,
            "write" | "writefile" | "writetofile" | "createfile" => Tool::Write,
            "delete" | "deletefile" => Tool::Delete,
            "webfetch" | "fetch" | "readurlcontent" => Tool::Fetch,
            "websearch" | "searchweb" => Tool::WebSearch,
            "todowrite" | "todoread" | "todolist" | "updateplan" => Tool::Todos,
            "task" | "agent" | "invokesubagent" | "spawnagent" | "subagent" | "collabtoolcall" => {
                Tool::Task
            }
            "structuredoutput" | "finish" => Tool::Report,
            "artifact" => Tool::Artifact,
            _ => Tool::Other,
        }
    }

    /// The shared name in a call's title, as Claude Code names its own tools.
    fn label(self) -> Option<&'static str> {
        Some(match self {
            Tool::Read => "Read",
            Tool::List => "List",
            Tool::Search => "Search",
            Tool::Shell => "Bash",
            Tool::Update => "Update",
            Tool::Write => "Write",
            Tool::Delete => "Delete",
            Tool::Fetch => "Fetch",
            Tool::WebSearch => "Web Search",
            Tool::Todos => "Update Todos",
            Tool::Task => "Task",
            Tool::Report => "Report",
            Tool::Artifact => argonauts_core::artifacts::TOOL,
            Tool::Mcp | Tool::Other => return None,
        })
    }

    /// Back from a title's name, for [`finish_call`].
    fn labelled(label: &str, kind: ToolKind) -> Self {
        const ALL: [Tool; 13] = [
            Tool::Artifact,
            Tool::Read,
            Tool::List,
            Tool::Search,
            Tool::Shell,
            Tool::Update,
            Tool::Write,
            Tool::Delete,
            Tool::Fetch,
            Tool::WebSearch,
            Tool::Todos,
            Tool::Task,
            Tool::Report,
        ];
        if kind == ToolKind::Mcp {
            return Tool::Mcp;
        }
        ALL.into_iter()
            .find(|tool| tool.label() == Some(label) && tool.kind() == kind)
            .unwrap_or(Tool::Other)
    }

    fn kind(self) -> ToolKind {
        match self {
            Tool::Read => ToolKind::Read,
            Tool::List => ToolKind::List,
            Tool::Search => ToolKind::Search,
            Tool::Shell => ToolKind::Shell,
            Tool::Update | Tool::Delete => ToolKind::Edit,
            Tool::Write => ToolKind::Write,
            Tool::Fetch | Tool::WebSearch => ToolKind::Fetch,
            Tool::Mcp => ToolKind::Mcp,
            Tool::Task => ToolKind::Agent,
            Tool::Todos | Tool::Report | Tool::Artifact | Tool::Other => ToolKind::Other,
        }
    }
}

/// A running call's cell from its vendor, name, and input. `vendor` is the adapter id.
pub fn start_call(vendor: &str, name: &str, input: &Value) -> ToolCall {
    let _ = vendor;
    let mut tool = Tool::named(name);
    if normalize(name) == "filechange" {
        tool = file_change_tool(input);
    }
    let label = match tool.label() {
        Some(label) => label.to_owned(),
        None if tool == Tool::Mcp => mcp_label(name),
        None => name.trim().to_owned(),
    };
    let mut call = ToolCall::call(tool.kind(), label, cut(&args_of(tool, input)));
    match tool {
        Tool::Update => call.body.extend(edit_preview(input)),
        Tool::Write => call.body.extend(write_preview(input)),
        Tool::Todos => call.body.extend(todo_list(input).map(ToolItem::Todos)),
        // Own calls never print, so this is the JSON the Ctrl+O detail shows.
        Tool::Report => call.body.push(json_block(input)),
        _ => {}
    }
    call
}

/// The one line the transcript shows for a report call: the report's summary.
pub fn report_summary(name: &str, input: &Value) -> Option<String> {
    (Tool::named(name) == Tool::Report)
        .then(|| summary_line(input))
        .flatten()
}

/// A message that ends with a JSON object, fenced or not, as an agent that writes its report or
/// its step's result out: the words before the JSON, and the report's summary when it has one.
/// `None` when no JSON object ends the message.
pub fn written_report(text: &str) -> Option<(String, Option<String>)> {
    let (words, report) = written_object(text)?;
    Some((words, summary_line(&report)))
}

/// The words before the JSON object that ends a message, and the object itself, for a view
/// that draws the result as what it means.
pub fn written_object(text: &str) -> Option<(String, Value)> {
    let body = text.trim_end();
    let unfenced = body.strip_suffix("```").map_or(body, str::trim_end);
    if !unfenced.ends_with('}') {
        return None;
    }
    let (at, report) = object_lines(unfenced).find_map(|at| {
        let value = serde_json::from_str::<Value>(&unfenced[at..]).ok()?;
        value.is_object().then_some((at, value))
    })?;
    let before = unfenced[..at].trim_end();
    let words = before
        .strip_suffix("```json")
        .or_else(|| before.strip_suffix("```"))
        .unwrap_or(before)
        .trim();
    Some((words.to_owned(), report))
}

/// Where each line that opens with `{` opens it. A written report starts its own line, and
/// trying only those keeps a long message cheap.
fn object_lines(text: &str) -> impl Iterator<Item = usize> + '_ {
    let mut at = 0;
    text.split_inclusive('\n').filter_map(move |line| {
        let indent = line.len() - line.trim_start().len();
        let start = at + indent;
        at += line.len();
        line.trim_start().starts_with('{').then_some(start)
    })
}

fn json_block(value: &Value) -> ToolItem {
    let json = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    ToolItem::Markdown(format!("```json\n{json}\n```"))
}

/// Adds what the call did under it and marks it done or failed.
pub fn finish_call(call: &mut ToolCall, ok: bool, output: Option<&str>) {
    let output = output.filter(|text| !text.trim().is_empty());
    // Send now moved a running command to the background: say so as Claude Code's screen does.
    if ok && output.is_some_and(|text| text.starts_with(MOVED_TO_BACKGROUND)) {
        call.push(ToolItem::Text {
            text: MOVED_NOTE.to_owned(),
            tone: Tone::Dim,
        });
        call.finish(ok);
        return;
    }
    let (label, target) = match &call.title {
        ToolTitle::Call { name, args } => (name.clone(), args.clone()),
        other => (other.plain(), String::new()),
    };
    let tool = Tool::labelled(&label, call.kind);
    if tool == Tool::Shell {
        shell_result(call, ok, output);
    } else if ok {
        let items = result_items(tool, call, &target, output);
        call.body.extend(items);
    } else {
        // A preview of what it meant to change would read as done.
        call.body
            .retain(|item| matches!(item, ToolItem::Call(_) | ToolItem::Group(_)));
        call.push(ToolItem::Text {
            text: first_line(output.unwrap_or("Failed")),
            tone: Tone::Error,
        });
    }
    call.finish(ok);
}

/// How Claude Code 2.1.289 starts a command's result when send now moved it to the background.
const MOVED_TO_BACKGROUND: &str = "Command was moved to the background";
const MOVED_NOTE: &str =
    "Moved to the background so your message could reach Claude. It's still running.";

/// Argonauts' own tools: the report, however the vendor sends it, and `argonauts_status`.
/// They're how a session talks to Argonauts rather than work it does, so the transcript leaves
/// them out and only the Ctrl+O detail shows them.
pub fn is_own(name: &str) -> bool {
    argonauts_core::steps::is_own_tool(name)
}

/// The normalized kind, which decides folding into collapsed groups.
pub fn kind_of(vendor: &str, name: &str) -> ToolKind {
    let _ = vendor;
    Tool::named(name).kind()
}

fn result_items(tool: Tool, call: &ToolCall, target: &str, output: Option<&str>) -> Vec<ToolItem> {
    let has_diff = call
        .body
        .iter()
        .any(|item| matches!(item, ToolItem::Diff(_)));
    let text = match tool {
        Tool::Read => format!("Read {}", count(read_lines(output), "line", "lines")),
        Tool::List => format!("Listed {}", count(lines(output).len(), "path", "paths")),
        Tool::Search => search_result(output),
        Tool::Update | Tool::Write if has_diff => return Vec::new(),
        Tool::Update => format!("Updated {target}"),
        Tool::Write => format!("Wrote {target}"),
        Tool::Delete => format!("Deleted {target}"),
        Tool::Fetch => match output {
            Some(text) => format!("Received {}", size(text.len())),
            None => "Fetched".to_owned(),
        },
        Tool::WebSearch => "Did 1 search".to_owned(),
        Tool::Todos if call.body.is_empty() => "Updated the to-do list".to_owned(),
        Tool::Todos => return Vec::new(),
        Tool::Task => {
            let uses = nested_calls(call);
            if uses == 0 {
                "Done".to_owned()
            } else {
                format!("Done ({})", count(uses, "tool use", "tool uses"))
            }
        }
        Tool::Report => "Sent the report".to_owned(),
        // `Published <link>`, dim, as Claude Code's own row under the call. Anything else the
        // tool did, such as a read, shows what it said.
        Tool::Artifact => {
            let made = output.and_then(|text| artifacts::published(&Value::Null, text));
            return vec![match (made, output) {
                (Some(page), _) => ToolItem::Text {
                    text: format!("{} {}", page.verb(), page.url),
                    tone: Tone::Dim,
                },
                (None, Some(text)) => output_item(text),
                (None, None) => ToolItem::text("(No output)"),
            }];
        }
        Tool::Shell | Tool::Mcp | Tool::Other => {
            return vec![match output {
                Some(text) => output_item(text),
                None => ToolItem::text("(No output)"),
            }];
        }
    };
    vec![ToolItem::text(text)]
}

/// `Error: Exit code 1` when the command failed with a known code, then the first output lines
/// and `… +10 lines (ctrl+o to expand)`, as Claude Code shows a failed command. A failed call
/// draws its output in the error color.
fn shell_result(call: &mut ToolCall, ok: bool, output: Option<&str>) {
    let (code, rest) = match output.map(exit_code) {
        Some((code, rest)) => (code, rest),
        None => (None, None),
    };
    match code.filter(|code| *code != 0) {
        Some(code) => call.push(ToolItem::Text {
            text: format!("Error: Exit code {code}"),
            tone: Tone::Error,
        }),
        None if !ok && rest.is_none() => call.push(ToolItem::Text {
            text: "Failed".to_owned(),
            tone: Tone::Error,
        }),
        None => {}
    }
    match rest {
        Some(text) => call.push(output_item(text)),
        None if ok => call.push(ToolItem::text("(No output)")),
        None => {}
    }
}

fn exit_code(output: &str) -> (Option<i32>, Option<&str>) {
    let (first, rest) = output.split_once('\n').unwrap_or((output, ""));
    let code = first
        .trim()
        .trim_start_matches("Error: ")
        .strip_prefix("Exit code ")
        .and_then(|code| code.trim().parse().ok());
    match code {
        Some(code) => (
            Some(code),
            Some(rest).filter(|rest| !rest.trim().is_empty()),
        ),
        None => (None, Some(output)),
    }
}

/// Output as a shell command's, without the command, which the title already shows.
fn output_item(text: &str) -> ToolItem {
    ToolItem::Command {
        command: String::new(),
        output: vec![text.trim_end().to_owned()],
        hidden: 0,
    }
}

fn search_result(output: Option<&str>) -> String {
    let found = lines(output);
    match found.first() {
        None => "No matches".to_owned(),
        // Claude Code's Grep counts its files itself, as `Found 3 files`.
        Some(first) if first.starts_with("Found ") || first.starts_with("No ") => {
            (*first).to_owned()
        }
        Some(_) => {
            let (one, many) = noun_for(&found);
            format!("Found {}", count(found.len(), one, many))
        }
    }
}

/// Grep's content lines read `path:12:text`; anything else is a list of files.
fn noun_for(found: &[&str]) -> (&'static str, &'static str) {
    let is_match = |line: &&str| {
        line.split(':')
            .nth(1)
            .is_some_and(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
    };
    if found.iter().any(is_match) {
        ("match", "matches")
    } else {
        ("file", "files")
    }
}

/// Claude Code numbers what it reads as `    12→text`; other harnesses send the text as is.
fn read_lines(output: Option<&str>) -> usize {
    let body = output
        .map(|text| text.split("<system-reminder>").next().unwrap_or(text))
        .unwrap_or("");
    let numbered = body
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            let digits = line.chars().take_while(char::is_ascii_digit).count();
            digits > 0 && matches!(line[digits..].chars().next(), Some('→' | '\t'))
        })
        .count();
    if numbered > 0 {
        numbered
    } else {
        body.trim_end().lines().count()
    }
}

fn lines(output: Option<&str>) -> Vec<&str> {
    output
        .map(|text| {
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn nested_calls(call: &ToolCall) -> usize {
    call.body
        .iter()
        .map(|item| match item {
            ToolItem::Call(child) => 1 + nested_calls(child),
            ToolItem::Group(group) => group.calls.iter().map(|c| 1 + nested_calls(c)).sum(),
            _ => 0,
        })
        .sum()
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn size(bytes: usize) -> String {
    if bytes < 1024 {
        count(bytes, "byte", "bytes")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

const PATH_KEYS: &[&str] = &[
    "filepath",
    "path",
    "file",
    "targetfile",
    "absolutepath",
    "notebookpath",
    "directorypath",
    "searchpath",
    "searchdirectory",
    "uri",
];
const COMMAND_KEYS: &[&str] = &["command", "commandline", "cmd"];
const PATTERN_KEYS: &[&str] = &["pattern", "query", "regex", "searchterm", "glob"];
const URL_KEYS: &[&str] = &["url"];
const TASK_KEYS: &[&str] = &["description", "prompt", "task"];
const OLD_KEYS: &[&str] = &["oldstring", "oldstr", "targetcontent"];
const NEW_KEYS: &[&str] = &["newstring", "newstr", "replacementcontent"];
const CONTENT_KEYS: &[&str] = &["content", "codecontent", "filetext", "contents"];
const EDIT_LIST_KEYS: &[&str] = &["edits", "replacementchunks", "chunks"];

fn args_of(tool: Tool, input: &Value) -> String {
    let path = || string_field(input, PATH_KEYS).map(|path| short_path(&path));
    let text = match tool {
        Tool::Read => path(),
        Tool::List => path().or_else(|| Some(".".to_owned())),
        Tool::Update | Tool::Write | Tool::Delete => path().or_else(|| changed_paths(input)),
        Tool::Shell => command_field(input).map(|command| first_line(&unwrap_shell(&command))),
        Tool::Search => string_field(input, PATTERN_KEYS).map(|pattern| {
            let mut args = format!("pattern: \"{}\"", first_line(&pattern));
            if let Some(path) = path() {
                args.push_str(&format!(", path: \"{path}\""));
            }
            args
        }),
        Tool::Fetch => string_field(input, URL_KEYS),
        Tool::WebSearch => {
            string_field(input, PATTERN_KEYS).map(|q| format!("\"{}\"", first_line(&q)))
        }
        Tool::Task => string_field(input, TASK_KEYS).map(|task| first_line(&task)),
        Tool::Report => string_field(input, &["status"]),
        Tool::Todos => None,
        Tool::Artifact => Some(artifact_args(input)),
        Tool::Mcp | Tool::Other => Some(call_args(input)),
    };
    text.unwrap_or_default()
}

/// A publish shows the file in quotes, as Claude Code's own call line does. Any other action
/// of the tool says what it is and which artifact.
fn artifact_args(input: &Value) -> String {
    let file = string_field(input, &["filepath"]).map(|path| short_path(&path));
    if artifacts::publishes(artifacts::TOOL, input) {
        return match (file, string_field(input, &["title"])) {
            (Some(file), _) => format!("\"{file}\""),
            (None, Some(title)) => format!("\"{}\"", first_line(&title)),
            (None, None) => String::new(),
        };
    }
    let action = string_field(input, &["action"]).unwrap_or_else(|| "upload".to_owned());
    match string_field(input, URL_KEYS) {
        Some(url) => format!("{action} {url}"),
        None => action,
    }
}

/// Codex reports added, changed, and deleted files as one call, named for what it did.
fn file_change_tool(input: &Value) -> Tool {
    let kinds: Vec<&str> = changes(input)
        .iter()
        .filter_map(|change| change.get("kind").and_then(Value::as_str))
        .collect();
    if !kinds.is_empty() && kinds.iter().all(|kind| *kind == "add") {
        Tool::Write
    } else if !kinds.is_empty() && kinds.iter().all(|kind| *kind == "delete") {
        Tool::Delete
    } else {
        Tool::Update
    }
}

fn changes(input: &Value) -> Vec<&Value> {
    input
        .get("changes")
        .and_then(Value::as_array)
        .map(|changes| changes.iter().collect())
        .unwrap_or_default()
}

fn changed_paths(input: &Value) -> Option<String> {
    let paths: Vec<String> = changes(input)
        .iter()
        .filter_map(|change| change.get("path").and_then(Value::as_str))
        .map(short_path)
        .collect();
    (!paths.is_empty()).then(|| paths.join(", "))
}

/// `slack - post_message (MCP)`, as Claude Code names MCP calls, from `mcp__slack__post_message`
/// (Claude Code) or `mcp:slack/post_message` (Codex).
fn mcp_label(name: &str) -> String {
    let (server, tool) = match name.strip_prefix("mcp__") {
        Some(rest) => rest.split_once("__").unwrap_or((rest, "")),
        None => {
            let rest = name.strip_prefix("mcp:").unwrap_or(name);
            rest.split_once('/').unwrap_or((rest, ""))
        }
    };
    if tool.is_empty() {
        format!("{server} (MCP)")
    } else {
        format!("{server} - {tool} (MCP)")
    }
}

/// `query: "x", limit: 5`
fn call_args(input: &Value) -> String {
    let show = |value: &Value| match value {
        Value::String(text) => format!("\"{}\"", first_line(text)),
        other => other.to_string(),
    };
    match input {
        Value::Null => String::new(),
        Value::Object(map) => map
            .iter()
            .filter(|(_, value)| !value.is_null())
            .map(|(key, value)| format!("{key}: {}", show(value)))
            .collect::<Vec<_>>()
            .join(", "),
        other => show(other),
    }
}

/// Worktrees sit at `<home>/worktrees/<repo>/<run>/<card>/`, and the block's header already
/// names the card, so a path inside one starts after it.
fn short_path(path: &str) -> String {
    let path = path.strip_prefix("file://").unwrap_or(path);
    if let Some(at) = path.find("/worktrees/") {
        let mut parts = path[at + "/worktrees/".len()..].splitn(4, '/');
        if let (Some(_), Some(_), Some(_), Some(inner)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
            && !inner.is_empty()
        {
            return inner.to_owned();
        }
    }
    path.to_owned()
}

fn edit_preview(input: &Value) -> Option<ToolItem> {
    let pairs = edit_pairs(input);
    if pairs.is_empty() {
        return None;
    }
    let mut view = DiffView::default();
    for (old, new) in &pairs {
        view.hunks
            .extend(DiffView::between(old, new, 3).without_numbers().hunks);
    }
    let (added, removed) = view.stats();
    let path = string_field(input, PATH_KEYS)
        .map(|path| short_path(&path))
        .unwrap_or_default();
    Some(ToolItem::Diff(view.updated(path, added, removed)))
}

fn write_preview(input: &Value) -> Vec<ToolItem> {
    let Some(content) = string_field(input, CONTENT_KEYS) else {
        return Vec::new();
    };
    let path = string_field(input, PATH_KEYS)
        .map(|path| short_path(&path))
        .unwrap_or_default();
    let all: Vec<&str> = content.lines().collect();
    let shown: String = all
        .iter()
        .take(WRITE_PREVIEW)
        .map(|line| format!("{line}\n"))
        .collect();
    let view = DiffView::between("", &shown, 0).wrote(path, all.len());
    let mut items = vec![ToolItem::Diff(view)];
    let more = all.len().saturating_sub(WRITE_PREVIEW);
    if more > 0 {
        items.push(ToolItem::text(format!(
            "… +{}",
            count(more, "line", "lines")
        )));
    }
    items
}

fn edit_pairs(input: &Value) -> Vec<(String, String)> {
    let pair = |value: &Value| {
        let old = string_field(value, OLD_KEYS)?;
        let new = string_field(value, NEW_KEYS)?;
        Some((old, new))
    };
    if let Some(one) = pair(input) {
        return vec![one];
    }
    let Some(object) = input.as_object() else {
        return Vec::new();
    };
    object
        .iter()
        .filter(|(key, _)| EDIT_LIST_KEYS.contains(&normalize(key).as_str()))
        .filter_map(|(_, value)| value.as_array())
        .flatten()
        .filter_map(pair)
        .collect()
}

/// Claude Code's and opencode's `{content, status}` items.
fn todo_list(input: &Value) -> Option<Vec<Todo>> {
    let items = input.get("todos")?.as_array()?;
    let todos: Vec<Todo> = items
        .iter()
        .filter_map(|item| {
            let text = string_field(item, &["content", "text"])?;
            let state = match item.get("status").and_then(Value::as_str) {
                Some("completed") => TodoState::Done,
                Some("in_progress") => TodoState::InProgress,
                _ => TodoState::Pending,
            };
            Some(Todo { text, state })
        })
        .collect();
    (!todos.is_empty()).then_some(todos)
}

fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn value_at<'a>(object: &'a Map<String, Value>, wanted: &str) -> Option<&'a Value> {
    object
        .iter()
        .find(|(key, _)| normalize(key) == wanted)
        .map(|(_, value)| value)
}

fn string_field(input: &Value, keys: &[&str]) -> Option<String> {
    let object = input.as_object()?;
    keys.iter()
        .find_map(|wanted| value_at(object, wanted)?.as_str().map(str::to_owned))
        .filter(|text| !text.trim().is_empty())
}

/// The command as one line, whether the vendor sends a string or an argument list.
fn command_field(input: &Value) -> Option<String> {
    let object = input.as_object()?;
    let value = COMMAND_KEYS
        .iter()
        .find_map(|wanted| value_at(object, wanted))?;
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

/// Shows `ls -la` for `/bin/zsh -lc 'ls -la'`, since the shell wrapper is noise.
fn unwrap_shell(command: &str) -> String {
    let trimmed = command.trim();
    let mut parts = trimmed.splitn(3, ' ');
    let (Some(shell), Some(flag), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
        return trimmed.to_owned();
    };
    let shell_name = shell.rsplit('/').next().unwrap_or(shell);
    let is_shell = matches!(shell_name, "sh" | "bash" | "zsh" | "dash");
    let is_flag = matches!(flag, "-c" | "-lc" | "-ic");
    if !(is_shell && is_flag) {
        return trimmed.to_owned();
    }
    let rest = rest.trim();
    for quote in ['\'', '"'] {
        if rest.len() >= 2 && rest.starts_with(quote) && rest.ends_with(quote) {
            return rest[1..rest.len() - 1].to_owned();
        }
    }
    rest.to_owned()
}

/// The first line with text, marked `…` when more follows.
fn first_line(text: &str) -> String {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or("").to_owned();
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first
    }
}

fn cut(text: &str) -> String {
    if text.chars().count() <= ARGS_MAX {
        return text.to_owned();
    }
    let mut kept: String = text.chars().take(ARGS_MAX - 1).collect();
    kept.push('…');
    kept
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use serde_json::json;

    use super::*;
    use crate::cell::{CollapsedGroup, HistoryCell, ToolState};
    use crate::render::{Clock, RenderCtx};
    use crate::text::plain_text;
    use crate::theme::{Glyphs, Role, Theme};

    /// Claude Code wrote its report out as fenced JSON before it called the report tool.
    #[test]
    fn a_written_report_reads_as_its_words_and_summary() {
        let fenced = r#"Done

```json
{
  "status": "done",
  "summary": "Created hello.txt.\nNothing else.",
  "files_changed": ["hello.txt"]
}
```"#;
        assert_eq!(
            written_report(fenced),
            Some(("Done".to_owned(), Some("Created hello.txt.".to_owned())))
        );
        let bare = r#"{"plan": [{"card": "a"}]}"#;
        assert_eq!(written_report(bare), Some((String::new(), None)));
        let indented = "Here it is:\n  {\"summary\": \"ok\"}";
        assert_eq!(
            written_report(indented),
            Some(("Here it is:".to_owned(), Some("ok".to_owned())))
        );
        for plain in [
            "Set `{a: 1}` in the config.",
            "Use {braces} like this {}",
            "The map is {\"a\": 1} and that's all.",
        ] {
            assert_eq!(written_report(plain), None, "{plain}");
        }
    }

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn rows(call: &ToolCall, width: u16) -> String {
        let theme = theme();
        plain_text(&call.lines(&RenderCtx::new(width, &theme, Clock::still(Instant::now()))))
    }

    fn done(vendor: &str, name: &str, input: Value, output: Option<&str>) -> ToolCall {
        let mut call = start_call(vendor, name, &input);
        finish_call(&mut call, true, output);
        call
    }

    /// An artifact reads as Claude Code draws it: the published file in quotes on the call's
    /// line, and `Published <link>` under it. The link stays whole on a row of
    /// its own where the row is too narrow for both, so it can still be clicked.
    #[test]
    fn an_artifact_reads_as_claude_code_draws_it() {
        let url = "https://claude.ai/code/artifact/0b1e7c9a-4f1d-4c57-9a55-2f6f0d3a8e11";
        let input = json!({"file_path": "out/weekly-usage-chart.html", "icon": "chart",
                           "description": "This week's spending"});
        let output = format!(
            "Published out/weekly-usage-chart.html at {url} (Version 1) Icon: chart. Not \
             previewed this session."
        );
        let call = done("claude-code", "Artifact", input.clone(), Some(&output));
        assert_eq!(call.kind, ToolKind::Other);
        assert_eq!(
            rows(&call, 100),
            format!("● Artifact(\"out/weekly-usage-chart.html\")\n  ⎿  Published {url}")
        );
        assert_eq!(
            rows(&call, 80),
            format!("● Artifact(\"out/weekly-usage-chart.html\")\n  ⎿  Published\n     {url}")
        );
        let theme = theme();
        let lines = call.lines(&RenderCtx::new(100, &theme, Clock::still(Instant::now())));
        let link = lines[1]
            .spans
            .iter()
            .find(|span| span.content.contains("Published"))
            .expect("the row under the call");
        assert_eq!(link.style, theme.style(Role::Dim));

        // One made from a type is updated in place, and a long name is cut like any call's.
        let updated = done(
            "claude-code",
            "Artifact",
            json!({"title": "This week", "url": url}),
            Some(&format!("Updated the Artifact at {url} with (unknown).")),
        );
        assert_eq!(
            rows(&updated, 100),
            format!("● Artifact(\"This week\")\n  ⎿  Updated {url}")
        );

        // A publish that was refused shows what the tool said, and the tool's other actions
        // say which one they are.
        let mut refused = start_call("claude-code", "Artifact", &input);
        finish_call(
            &mut refused,
            false,
            Some("content must first be written to a file"),
        );
        assert!(rows(&refused, 100).ends_with("⎿  content must first be written to a file"));
        let listed = done(
            "claude-code",
            "Artifact",
            json!({"action": "list"}),
            Some("2 artifacts (newest first)"),
        );
        assert_eq!(
            rows(&listed, 100),
            "● Artifact(list)\n  ⎿  2 artifacts (newest first)"
        );
    }

    #[test]
    fn tool_names_every_vendor() {
        let worktree = "/home/me/.argonauts/worktrees/repo-1/run-01/task-003";
        let cases = [
            // Claude Code
            (
                "claude-code",
                "Read",
                json!({"file_path": format!("{worktree}/src/app.rs")}),
                "Read(src/app.rs)",
                ToolKind::Read,
            ),
            (
                "claude-code",
                "Edit",
                json!({"file_path": "src/app.rs", "old_string": "a", "new_string": "b"}),
                "Update(src/app.rs)",
                ToolKind::Edit,
            ),
            (
                "claude-code",
                "MultiEdit",
                json!({"file_path": "src/app.rs", "edits": []}),
                "Update(src/app.rs)",
                ToolKind::Edit,
            ),
            (
                "claude-code",
                "Write",
                json!({"file_path": "hello.txt", "content": "hi"}),
                "Write(hello.txt)",
                ToolKind::Write,
            ),
            (
                "claude-code",
                "Bash",
                json!({"command": "cargo test", "description": "tests"}),
                "Bash(cargo test)",
                ToolKind::Shell,
            ),
            (
                "claude-code",
                "Grep",
                json!({"pattern": "fn main", "path": "src"}),
                "Search(pattern: \"fn main\", path: \"src\")",
                ToolKind::Search,
            ),
            (
                "claude-code",
                "Glob",
                json!({"pattern": "**/*.rs"}),
                "Search(pattern: \"**/*.rs\")",
                ToolKind::Search,
            ),
            (
                "claude-code",
                "WebFetch",
                json!({"url": "https://docs.rs", "prompt": "x"}),
                "Fetch(https://docs.rs)",
                ToolKind::Fetch,
            ),
            (
                "claude-code",
                "TodoWrite",
                json!({"todos": []}),
                "Update Todos",
                ToolKind::Other,
            ),
            (
                "claude-code",
                "Task",
                json!({"description": "find auth", "prompt": "…"}),
                "Task(find auth)",
                ToolKind::Agent,
            ),
            (
                "claude-code",
                "Agent",
                json!({"description": "find auth"}),
                "Task(find auth)",
                ToolKind::Agent,
            ),
            (
                "claude-code",
                "mcp__github__create_issue",
                json!({"title": "x"}),
                "github - create_issue (MCP)(title: \"x\")",
                ToolKind::Mcp,
            ),
            (
                "claude-code",
                "StructuredOutput",
                json!({"status": "done"}),
                "Report(done)",
                ToolKind::Other,
            ),
            // Codex
            (
                "codex",
                "command_execution",
                json!({"command": "/bin/zsh -lc 'ls -la'"}),
                "Bash(ls -la)",
                ToolKind::Shell,
            ),
            (
                "codex",
                "structured_output",
                json!({"status": "done", "summary": "ok"}),
                "Report(done)",
                ToolKind::Other,
            ),
            (
                "codex",
                "file_change",
                json!({"changes": [{"path": format!("{worktree}/hello.txt"), "kind": "add"}]}),
                "Write(hello.txt)",
                ToolKind::Write,
            ),
            (
                "codex",
                "file_change",
                json!({"changes": [{"path": "src/a.rs", "kind": "update"}]}),
                "Update(src/a.rs)",
                ToolKind::Edit,
            ),
            (
                "codex",
                "file_change",
                json!({"changes": [{"path": "a.rs", "kind": "add"}, {"path": "b.rs", "kind": "update"}]}),
                "Update(a.rs, b.rs)",
                ToolKind::Edit,
            ),
            (
                "codex",
                "file_change",
                json!({"changes": [{"path": "old.txt", "kind": "delete"}]}),
                "Delete(old.txt)",
                ToolKind::Edit,
            ),
            (
                "codex",
                "mcp:docs/search",
                json!({"q": "x"}),
                "docs - search (MCP)(q: \"x\")",
                ToolKind::Mcp,
            ),
            (
                "codex",
                "web_search",
                json!({"query": "rust", "action": {"type": "search"}}),
                "Web Search(\"rust\")",
                ToolKind::Fetch,
            ),
            (
                "codex",
                "todo_list",
                json!({}),
                "Update Todos",
                ToolKind::Other,
            ),
            (
                "codex",
                "spawn_agent",
                json!({"prompt": "look around\nmore"}),
                "Task(look around …)",
                ToolKind::Agent,
            ),
            (
                "codex",
                "collab_tool_call",
                json!({"prompt": "check"}),
                "Task(check)",
                ToolKind::Agent,
            ),
            // agy, with the keys its fixtures show
            (
                "antigravity-cli",
                "run_command",
                json!({"CommandLine": "ls -1 | wc -l"}),
                "Bash(ls -1 | wc -l)",
                ToolKind::Shell,
            ),
            (
                "antigravity-cli",
                "view_file",
                json!({"AbsolutePath": format!("{worktree}/src/lib.rs")}),
                "Read(src/lib.rs)",
                ToolKind::Read,
            ),
            (
                "antigravity-cli",
                "write_to_file",
                json!({"TargetFile": "<workdir>/hello.txt"}),
                "Write(<workdir>/hello.txt)",
                ToolKind::Write,
            ),
            (
                "antigravity-cli",
                "replace_file_content",
                json!({"TargetFile": "a.rs", "ReplacementChunks": []}),
                "Update(a.rs)",
                ToolKind::Edit,
            ),
            (
                "antigravity-cli",
                "multi_replace_file_content",
                json!({"TargetFile": "a.rs"}),
                "Update(a.rs)",
                ToolKind::Edit,
            ),
            (
                "antigravity-cli",
                "grep_search",
                json!({"Query": "todo", "SearchPath": "src"}),
                "Search(pattern: \"todo\", path: \"src\")",
                ToolKind::Search,
            ),
            (
                "antigravity-cli",
                "find_by_name",
                json!({"Pattern": "*.rs", "SearchDirectory": "src"}),
                "Search(pattern: \"*.rs\", path: \"src\")",
                ToolKind::Search,
            ),
            (
                "antigravity-cli",
                "list_dir",
                json!({"DirectoryPath": "src"}),
                "List(src)",
                ToolKind::List,
            ),
            (
                "antigravity-cli",
                "read_url_content",
                json!({"Url": "https://x.dev"}),
                "Fetch(https://x.dev)",
                ToolKind::Fetch,
            ),
            (
                "antigravity-cli",
                "search_web",
                json!({"query": "tokio"}),
                "Web Search(\"tokio\")",
                ToolKind::Fetch,
            ),
            (
                "antigravity-cli",
                "invoke_subagent",
                json!({"Task": "research"}),
                "Task(research)",
                ToolKind::Agent,
            ),
            (
                "antigravity-cli",
                "finish",
                json!({"status": "done", "summary": "ok"}),
                "Report(done)",
                ToolKind::Other,
            ),
            // ACP tool kinds
            (
                "acp",
                "read",
                json!({"path": "a.rs"}),
                "Read(a.rs)",
                ToolKind::Read,
            ),
            (
                "acp",
                "edit",
                json!({"path": "a.rs"}),
                "Update(a.rs)",
                ToolKind::Edit,
            ),
            (
                "acp",
                "delete",
                json!({"path": "a.rs"}),
                "Delete(a.rs)",
                ToolKind::Edit,
            ),
            (
                "acp",
                "move",
                json!({"path": "a.rs"}),
                "Update(a.rs)",
                ToolKind::Edit,
            ),
            (
                "acp",
                "search",
                json!({"query": "x"}),
                "Search(pattern: \"x\")",
                ToolKind::Search,
            ),
            (
                "acp",
                "execute",
                json!({"command": ["cargo", "build"]}),
                "Bash(cargo build)",
                ToolKind::Shell,
            ),
            (
                "acp",
                "fetch",
                json!({"url": "https://x.dev"}),
                "Fetch(https://x.dev)",
                ToolKind::Fetch,
            ),
            ("acp", "think", json!(null), "think", ToolKind::Other),
            // opencode
            (
                "opencode",
                "read",
                json!({"filePath": "a.rs"}),
                "Read(a.rs)",
                ToolKind::Read,
            ),
            (
                "opencode",
                "list",
                json!({"path": "src"}),
                "List(src)",
                ToolKind::List,
            ),
            (
                "opencode",
                "glob",
                json!({"pattern": "*.md"}),
                "Search(pattern: \"*.md\")",
                ToolKind::Search,
            ),
            (
                "opencode",
                "bash",
                json!({"command": "ls"}),
                "Bash(ls)",
                ToolKind::Shell,
            ),
            (
                "opencode",
                "patch",
                json!({"filePath": "a.rs"}),
                "Update(a.rs)",
                ToolKind::Edit,
            ),
            (
                "opencode",
                "webfetch",
                json!({"url": "https://x.dev"}),
                "Fetch(https://x.dev)",
                ToolKind::Fetch,
            ),
            (
                "opencode",
                "todowrite",
                json!({"todos": []}),
                "Update Todos",
                ToolKind::Other,
            ),
            (
                "opencode",
                "task",
                json!({"description": "explore"}),
                "Task(explore)",
                ToolKind::Agent,
            ),
            // Anything else keeps its own name.
            (
                "codex",
                "frobnicate",
                json!({"level": "max"}),
                "frobnicate(level: \"max\")",
                ToolKind::Other,
            ),
        ];
        for (vendor, name, input, title, kind) in cases {
            let call = start_call(vendor, name, &input);
            assert_eq!(call.title.plain(), title, "{vendor} {name}");
            assert_eq!(call.kind, kind, "{vendor} {name}");
            if name != "file_change" {
                assert_eq!(kind_of(vendor, name), kind, "{vendor} {name}");
            }
            assert_eq!(call.state, ToolState::Running);
        }
    }

    #[test]
    fn argonauts_own_tools_are_known_by_every_vendor_name() {
        let own = [
            "StructuredOutput",
            "structured_output",
            "finish",
            "argonauts_report",
            "argonauts_argonauts_report",
            "mcp__argonauts__argonauts_report",
            "argonauts_status",
            "argonauts_argonauts_status",
            "mcp__argonauts_status__argonauts_status",
            "mcp:argonauts_status/argonauts_status",
        ];
        for name in own {
            assert!(is_own(name), "{name}");
        }
        let theirs = [
            "Read",
            "Bash",
            "mcp__github__create_issue",
            "mcp:docs/search",
            "status",
            "report_bug",
        ];
        for name in theirs {
            assert!(!is_own(name), "{name}");
        }
        // The report reads the same in the Ctrl+O detail, whichever way it came, its JSON
        // under it.
        for name in ["argonauts_report", "mcp__argonauts__argonauts_report"] {
            let call = done("acp", name, json!({"status": "done"}), None);
            assert_eq!(
                rows(&call, 80),
                "● Report(done)\n  ⎿  {\n       \"status\": \"done\"\n     }\n     Sent the report",
                "{name}"
            );
        }
    }

    #[test]
    fn results_say_what_happened() {
        let numbered: String = (1..=120).map(|n| format!("{n:>6}→line {n}\n")).collect();
        let read = done(
            "claude-code",
            "Read",
            json!({"file_path": "src/app.rs"}),
            Some(&numbered),
        );
        assert_eq!(rows(&read, 80), "● Read(src/app.rs)\n  ⎿  Read 120 lines");

        let edit = done(
            "claude-code",
            "Edit",
            json!({"file_path": "src/app.rs", "old_string": "a\nb\n", "new_string": "a\nc\nd\n"}),
            Some("The file src/app.rs has been updated."),
        );
        let edit = rows(&edit, 80);
        assert!(
            edit.starts_with("● Update(src/app.rs)\n  ⎿  Added 2 lines, removed 1 line\n"),
            "{edit}"
        );
        assert!(
            edit.ends_with("       a\n        -b\n        +c\n        +d"),
            "{edit}"
        );

        let write = done(
            "claude-code",
            "Write",
            json!({"file_path": "hello.txt", "content": "hi"}),
            None,
        );
        assert_eq!(
            rows(&write, 80),
            "● Write(hello.txt)\n  ⎿  Wrote 1 line to hello.txt\n      1 +hi"
        );

        let grep = done(
            "claude-code",
            "Grep",
            json!({"pattern": "fn main"}),
            Some("src/main.rs:3:fn main() {"),
        );
        assert_eq!(
            rows(&grep, 80),
            "● Search(pattern: \"fn main\")\n  ⎿  Found 1 match"
        );
        let files = done(
            "claude-code",
            "Grep",
            json!({"pattern": "x"}),
            Some("Found 2 files\na.rs\nb.rs"),
        );
        assert!(rows(&files, 80).ends_with("⎿  Found 2 files"));
        let glob = done(
            "claude-code",
            "Glob",
            json!({"pattern": "*.rs"}),
            Some("a.rs\nb.rs\nc.rs"),
        );
        assert!(rows(&glob, 80).ends_with("⎿  Found 3 files"));

        let added = done(
            "codex",
            "file_change",
            json!({"changes": [{"path": "hello.txt", "kind": "add"}]}),
            None,
        );
        assert!(rows(&added, 80).ends_with("⎿  Wrote hello.txt"));
        let gone = done(
            "codex",
            "file_change",
            json!({"changes": [{"path": "old.txt", "kind": "delete"}]}),
            None,
        );
        assert!(rows(&gone, 80).ends_with("⎿  Deleted old.txt"));
        let fetch = done(
            "claude-code",
            "WebFetch",
            json!({"url": "https://x.dev"}),
            Some(&"x".repeat(2048)),
        );
        assert!(rows(&fetch, 80).ends_with("⎿  Received 2.0KB"));
        let task = done(
            "claude-code",
            "Task",
            json!({"description": "explore"}),
            Some("found it"),
        );
        assert!(rows(&task, 80).ends_with("⎿  Done"));
    }

    #[test]
    fn a_failed_edit_drops_its_preview() {
        let mut call = start_call(
            "claude-code",
            "Edit",
            &json!({"file_path": "a.rs", "old_string": "x", "new_string": "y"}),
        );
        finish_call(
            &mut call,
            false,
            Some("String to replace not found in file.\nmore"),
        );
        assert_eq!(call.state, ToolState::Failed);
        assert_eq!(
            rows(&call, 80),
            "● Update(a.rs)\n  ⎿  String to replace not found in file. …"
        );
    }

    #[test]
    fn bash_result_cuts_and_shows_exit_code() {
        let output: String = (1..=15)
            .map(|n| format!("test case_{n} ... ok\n"))
            .collect();
        let failed = {
            let mut call = start_call("claude-code", "Bash", &json!({"command": "cargo test"}));
            finish_call(&mut call, false, Some(&format!("Exit code 101\n{output}")));
            call
        };
        assert_eq!(
            rows(&failed, 80),
            "● Bash(cargo test)\n  ⎿  Error: Exit code 101\n     test case_1 ... ok\n     test case_2 ... ok\n     test case_3 ... ok\n     test case_4 ... ok\n     test case_5 ... ok\n     … +10 lines (ctrl+o to expand)"
        );
        let theme = theme();
        let lines = failed.lines(&RenderCtx::new(80, &theme, Clock::still(Instant::now())));
        let error = theme.style(Role::Error).fg;
        for row in &lines[1..7] {
            let text = row.spans.last().expect("text");
            assert_eq!(text.style.fg, error, "{row:?}");
        }
        assert_ne!(lines[7].spans.last().expect("more").style.fg, error);
        let mut missing = start_call(
            "claude-code",
            "Bash",
            &json!({"command": "cat missing.txt"}),
        );
        finish_call(
            &mut missing,
            false,
            Some("Error: Exit code 1\ncat: missing.txt: No such file or directory"),
        );
        assert_eq!(
            rows(&missing, 80),
            "● Bash(cat missing.txt)\n  ⎿  Error: Exit code 1\n     cat: missing.txt: No such file or directory"
        );
        let passed = done(
            "claude-code",
            "Bash",
            json!({"command": "ls"}),
            Some("a\nb"),
        );
        assert_eq!(rows(&passed, 80), "● Bash(ls)\n  ⎿  a\n     b");
        let silent = done(
            "codex",
            "command_execution",
            json!({"command": "true"}),
            None,
        );
        assert_eq!(rows(&silent, 80), "● Bash(true)\n  ⎿  (No output)");
        let mut quiet_failure =
            start_call("codex", "command_execution", &json!({"command": "false"}));
        finish_call(&mut quiet_failure, false, None);
        assert_eq!(rows(&quiet_failure, 80), "● Bash(false)\n  ⎿  Failed");
        let mut zero = start_call("claude-code", "Bash", &json!({"command": "x"}));
        finish_call(&mut zero, true, Some("Exit code 0\nfine"));
        assert_eq!(rows(&zero, 80), "● Bash(x)\n  ⎿  fine");
        // Send now moved it to the background (Claude Code 2.1.289's words, cut).
        let mut moved = start_call("claude-code", "Bash", &json!({"command": "./slow-test.sh"}));
        let output = "Command was moved to the background (ID: bmvwwjv0b) so that a message \
                      that arrived while it was running can reach you; it was not interrupted.";
        finish_call(&mut moved, true, Some(output));
        assert_eq!(
            rows(&moved, 100),
            "● Bash(./slow-test.sh)\n  ⎿  Moved to the background so your message could reach \
             Claude. It's still running."
        );
    }

    #[test]
    fn collapsed_group_phrases() {
        let calls = [
            ("claude-code", "Read", json!({"file_path": "a.rs"})),
            ("codex", "command_execution", json!({"command": "ls"})),
            (
                "antigravity-cli",
                "view_file",
                json!({"AbsolutePath": "b.rs"}),
            ),
            (
                "antigravity-cli",
                "run_command",
                json!({"CommandLine": "pwd"}),
            ),
            ("antigravity-cli", "grep_search", json!({"Query": "x"})),
            ("acp", "execute", json!({"command": "make"})),
            ("antigravity-cli", "list_dir", json!({"DirectoryPath": "."})),
            ("claude-code", "WebFetch", json!({"url": "https://x.dev"})),
        ];
        let mut group = CollapsedGroup::new();
        for (vendor, name, input) in &calls {
            let call = done(vendor, name, input.clone(), Some("ok"));
            assert!(call.kind.is_quiet(), "{vendor} {name}");
            group.push(call);
        }
        assert_eq!(
            group.summary(),
            "Read 2 files, ran 3 shell commands, searched for 1 pattern, listed 1 directory, fetched 1 page"
        );
        group.push(start_call("claude-code", "Grep", &json!({"pattern": "y"})));
        assert!(
            group
                .summary()
                .starts_with("Reading 2 files, running 3 shell commands")
        );

        // Edits and writes stay out of groups, with their diffs.
        let mut session = ToolCall::agent("Codex", None, "worker", "");
        session.push_call(done("claude-code", "Read", json!({"file_path": "a"}), None));
        session.push_call(done(
            "codex",
            "file_change",
            json!({"changes": [{"path": "a", "kind": "update"}]}),
            None,
        ));
        session.push_call(done("claude-code", "Glob", json!({"pattern": "*"}), None));
        assert!(matches!(
            session.body.as_slice(),
            [ToolItem::Group(_), ToolItem::Call(_), ToolItem::Group(_)]
        ));
    }

    #[test]
    fn todo_writes_show_their_list() {
        let call = done(
            "claude-code",
            "TodoWrite",
            json!({"todos": [
                {"content": "wire middleware", "status": "completed"},
                {"content": "tests", "status": "in_progress"},
                {"content": "docs", "status": "pending"},
            ]}),
            Some("Todos have been modified successfully."),
        );
        assert_eq!(
            rows(&call, 80),
            "● Update Todos\n  ⎿  ☒ wire middleware\n     ◧ tests\n     ☐ docs"
        );
    }
}
