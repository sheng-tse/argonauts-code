//! One table for every agent's tool names, so a read looks the same from Claude Code, Codex,
//! agy, or an ACP agent, and each call says what it did (`Read 120 lines`, not `Read 1 file`).
//! Names and input keys vary by agent, so matching ignores case, `_`, and `-`.

use argonauts_core::app::artifacts;
use argonauts_core::workflows::steps::summary_line;
use serde_json::{Map, Value};

use crate::cells::cell::{
    DiffView, Held, Todo, TodoState, Tone, ToolCall, ToolItem, ToolKind, ToolTitle,
};
use crate::frame::state::Places;

/// Lines of a written file shown under its call.
const WRITE_PREVIEW: usize = 10;

/// Longest args kept in a call's title, in characters.
const ARGS_MAX: usize = 160;

/// What a call does, whatever the CLI calls it.
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
        // However the agent prefixes its server, as `mcp__argonauts__argonauts_report` or
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
            Tool::Artifact => argonauts_core::app::artifacts::TOOL,
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

/// A running call's cell from its agent, name, and input. `agent` is the adapter id.
pub fn start_call(agent_id: &str, name: &str, input: &Value) -> ToolCall {
    start_call_in(agent_id, name, input, &Places::here())
}

/// [`start_call`] for a call made in `places.folder`, whose paths show relative to it.
pub fn start_call_in(agent_id: &str, name: &str, input: &Value, places: &Places) -> ToolCall {
    let _ = agent_id;
    let tool = tool_of(name, input);
    let args = cut(&args_of(tool, input, places));
    let mut call = ToolCall::call(tool.kind(), label_of(tool, name), args);
    match tool {
        // The preview waits for the end, where the patch the result reports takes its place.
        Tool::Update => {
            call.held = held(
                "Updating…",
                edit_preview(input, places).into_iter().collect(),
            );
        }
        Tool::Write => call.held = held("Writing…", write_preview(input, places)),
        Tool::Todos => call.body.extend(todo_list(input).map(ToolItem::Todos)),
        // Own calls never print, so this is the JSON the Ctrl+O detail shows.
        Tool::Report => call.body.push(json_block(input)),
        _ => {}
    }
    call
}

fn held(waiting: &str, items: Vec<ToolItem>) -> Option<Box<Held>> {
    Some(Box::new(Held {
        waiting: waiting.to_owned(),
        items,
    }))
}

/// The row of a call the model is still writing, by the name it will be shown with.
pub fn writing_call(agent_id: &str, name: &str) -> ToolCall {
    let _ = agent_id;
    let tool = Tool::named(name);
    ToolCall::writing(tool.kind(), label_of(tool, name))
}

fn label_of(tool: Tool, name: &str) -> String {
    match tool.label() {
        Some(label) => label.to_owned(),
        None if tool == Tool::Mcp => mcp_label(name),
        None => name.trim().to_owned(),
    }
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

/// Where a JSON object that may still end the message starts, at or after byte `from`: a line
/// that opens with `{` or a ```` ```json ```` fence, whose object is still open or closes the
/// message. A screen that streams the message holds back what follows it, so a result the agent
/// writes out at the end never shows; once words follow the object, it is part of the answer.
/// `text.len()` when nothing may.
pub fn json_tail_from(text: &str, from: usize) -> usize {
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        if start < from {
            continue;
        }
        let opening = line.trim_end();
        let fence = "```json".starts_with(opening) && !opening.is_empty() && !line.ends_with('\n');
        let opens = opening.starts_with('{') || opening == "```json" || opening == "```" || fence;
        if opens && may_end_with_object(&text[start..]) {
            return start;
        }
    }
    text.len()
}

/// Whether `tail`, fenced or not, is a JSON object still being written, or one with nothing
/// after it but its closing fence.
fn may_end_with_object(tail: &str) -> bool {
    let body = tail
        .strip_prefix("```json")
        .or_else(|| tail.strip_prefix("```"))
        .unwrap_or(tail)
        .trim_start();
    // A fence still being typed, or one whose object hasn't started.
    if body.is_empty() || "```json".starts_with(tail.trim_end()) {
        return true;
    }
    if !body.starts_with('{') {
        return false;
    }
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for (i, c) in body.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let after = body[i + 1..].trim();
                    return after.is_empty() || "```".starts_with(after);
                }
            }
            _ => {}
        }
    }
    true
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
    let has_diff = call
        .body
        .iter()
        .any(|item| matches!(item, ToolItem::Diff(_)));
    if let Some(held) = call.held.take()
        && ok
        && !has_diff
    {
        call.body.splice(0..0, held.items);
    }
    if !ok && output.is_some_and(super::refusal::is_refusal) {
        // Nothing ran, so nothing failed: the call says it wasn't allowed, dim.
        call.body
            .retain(|item| matches!(item, ToolItem::Call(_) | ToolItem::Group(_)));
        call.push(ToolItem::Text {
            text: super::refusal::NOT_ALLOWED.to_owned(),
            tone: Tone::Dim,
        });
        call.refuse();
        return;
    }
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

/// Argonauts Code's own tools: the report, however the agent sends it, and `argonauts_status`.
/// They're how a session talks to Argonauts Code rather than work it does, so the transcript leaves
/// them out and only the Ctrl+O detail shows them.
pub fn is_own(name: &str) -> bool {
    argonauts_core::workflows::steps::is_own_tool(name)
}

/// A lookup of the tools a CLI holds back until asked for, such as Claude Code's ToolSearch.
/// Claude Code never shows one, and neither does Argonauts Code.
pub fn is_tool_lookup(name: &str) -> bool {
    normalize(name) == "toolsearch"
}

/// The normalized kind, which decides folding into collapsed groups, from the call's name and
/// its input, since Codex names a written file and an edited one alike.
pub fn kind_of(agent_id: &str, name: &str, input: &Value) -> ToolKind {
    let _ = agent_id;
    tool_of(name, input).kind()
}

/// The tool a call is, by its name and, for Codex's `file_change`, what it changed.
fn tool_of(name: &str, input: &Value) -> Tool {
    if normalize(name) == "filechange" {
        return file_change_tool(input);
    }
    Tool::named(name)
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

/// Claude Code numbers what it reads as `    12→text`; other CLIs send the text as is.
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

fn args_of(tool: Tool, input: &Value, places: &Places) -> String {
    let path = || string_field(input, PATH_KEYS).map(|path| places.show(&path));
    let text = match tool {
        Tool::Read => path(),
        Tool::List => path().or_else(|| Some(".".to_owned())),
        Tool::Update | Tool::Write | Tool::Delete => {
            path().or_else(|| changed_paths(input, places))
        }
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
        Tool::Artifact => Some(artifact_args(input, places)),
        Tool::Mcp | Tool::Other => Some(call_args(input)),
    };
    text.unwrap_or_default()
}

/// A publish shows the file in quotes, as Claude Code's own call line does. Any other action
/// of the tool says what it is and which artifact.
fn artifact_args(input: &Value, places: &Places) -> String {
    let file = string_field(input, &["filepath"]).map(|path| places.show(&path));
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

fn changed_paths(input: &Value, places: &Places) -> Option<String> {
    let paths: Vec<String> = changes(input)
        .iter()
        .filter_map(|change| change.get("path").and_then(Value::as_str))
        .map(|path| places.show(path))
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

/// A path as a run's agent shows it: inside its worktree of the repo, or from your home.
pub(crate) fn short_path(path: &str) -> String {
    Places::here().show(path)
}

/// Whether a path as a CLI named it, often absolute, and one relative to the repo name the same
/// file.
pub(crate) fn same_file(named: &str, relative: &str) -> bool {
    let named = short_path(named);
    let relative = relative.trim_start_matches("./");
    !relative.is_empty()
        && (named == relative
            || named
                .strip_suffix(relative)
                .is_some_and(|folder| folder.ends_with('/')))
}

fn edit_preview(input: &Value, places: &Places) -> Option<ToolItem> {
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
        .map(|path| places.show(&path))
        .unwrap_or_default();
    Some(ToolItem::Diff(view.updated(path, added, removed)))
}

/// Swaps the preview a call drew from its input for the patch its result reported, which has
/// the file's line numbers and the lines around each change. An empty or unreadable patch
/// leaves the preview.
pub fn take_patch(call: &mut ToolCall, path: &str, hunks: &str) {
    take_patch_in(call, path, hunks, &Places::here());
}

/// [`take_patch`] for a call [`start_call_in`] drew from the same places.
pub fn take_patch_in(call: &mut ToolCall, path: &str, hunks: &str, places: &Places) {
    if let Ok(view) = DiffView::from_unified(hunks) {
        take_view(call, path, view, places);
    }
}

/// [`take_patch_in`] for a patch that names no line numbers, which draws without them.
pub fn take_unnumbered_patch_in(call: &mut ToolCall, path: &str, hunks: &str, places: &Places) {
    if let Ok(view) = DiffView::from_unified(hunks) {
        take_view(call, path, view.without_numbers(), places);
    }
}

fn take_view(call: &mut ToolCall, path: &str, view: DiffView, places: &Places) {
    if view.is_empty() {
        return;
    }
    let (added, removed) = view.stats();
    let path = places.show(path);
    // The preview's rows and its `… +N lines` go, and the patch takes their place. The patch
    // of another file the same call changed stays.
    let preview = |item: &ToolItem| match item {
        ToolItem::Diff(diff) => diff.file().is_none_or(|file| file == path),
        ToolItem::Text { .. } => true,
        _ => false,
    };
    let at = call.body.iter().position(preview);
    call.body.retain(|item| !preview(item));
    let view = view.updated(path, added, removed);
    let at = at.unwrap_or(call.body.len()).min(call.body.len());
    call.body.insert(at, ToolItem::Diff(view));
}

fn write_preview(input: &Value, places: &Places) -> Vec<ToolItem> {
    let Some(content) = string_field(input, CONTENT_KEYS) else {
        return Vec::new();
    };
    let path = string_field(input, PATH_KEYS)
        .map(|path| places.show(&path))
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

/// The lines an edit or a write adds and removes, read off its input as its preview is. A
/// written file counts every line as added, since the file it replaced isn't known.
pub fn changed_lines(name: &str, input: &Value) -> (usize, usize) {
    match tool_of(name, input) {
        Tool::Update => edit_pairs(input)
            .iter()
            .map(|(old, new)| DiffView::between(old, new, 0).stats())
            .fold((0, 0), |(added, removed), (more, fewer)| {
                (added + more, removed + fewer)
            }),
        Tool::Write => (
            string_field(input, CONTENT_KEYS).map_or(0, |content| content.lines().count()),
            0,
        ),
        _ => (0, 0),
    }
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

/// The command as one line, whether the agent sends a string or an argument list.
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

/// Whether a shell call only looks: every command in it searches, reads or lists, as
/// Claude Code 2.1.295 decides which commands fold with the reads, or asks git what is there.
/// Anything else, such as a test run, keeps its own row, and a call whose command isn't known
/// yet does too.
pub fn only_looks(call: &ToolCall) -> bool {
    const SEARCH: &[&str] = &[
        "find", "grep", "rg", "ag", "ack", "locate", "which", "whereis",
    ];
    const READ: &[&str] = &[
        "cat", "head", "tail", "less", "more", "wc", "stat", "file", "strings", "jq", "awk", "cut",
        "sort", "uniq", "tr",
    ];
    const LIST: &[&str] = &["ls", "tree", "du"];
    const SAYS: &[&str] = &["echo", "printf", "true", "false", ":"];
    // The git subcommands that change nothing whatever follows them.
    const GIT_READS: &[&str] = &[
        "log",
        "status",
        "diff",
        "show",
        "blame",
        "shortlog",
        "describe",
        "rev-parse",
        "rev-list",
        "merge-base",
        "ls-files",
        "ls-tree",
        "cat-file",
    ];
    let ToolTitle::Call { args, .. } = &call.title else {
        return false;
    };
    let mut looked = false;
    for part in args.split(['|', '&', ';', '\n']) {
        let mut words = part.split_whitespace();
        let Some(word) = words.next() else {
            continue;
        };
        if SAYS.contains(&word) {
            continue;
        }
        let asks_git = word == "git" && words.next().is_some_and(|sub| GIT_READS.contains(&sub));
        if !(asks_git || SEARCH.contains(&word) || READ.contains(&word) || LIST.contains(&word)) {
            return false;
        }
        looked = true;
    }
    looked
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
    use crate::base::draw::render::{Clock, RenderCtx};
    use crate::base::text::plain_text;
    use crate::base::theme::{Glyphs, Role, Theme};
    use crate::cells::cell::{CollapsedGroup, HistoryCell, ToolState};

    #[test]
    fn a_json_object_that_may_end_the_message_is_held_back() {
        let words = "The limiter now keys on X-Api-Key.\n\n";
        // Fenced, while it streams and once it closes.
        let open = format!("{words}```json\n{{\"answer_markdown\": \"The lim");
        assert_eq!(json_tail_from(&open, 0), words.len());
        let closed = format!("{words}```json\n{{\"answer_markdown\": \"a }} b\"}}\n```\n");
        assert_eq!(json_tail_from(&closed, 0), words.len());
        // Bare, and a fence still being typed.
        let bare = format!("{words}{{\"answer_markdown\": \"x\"}}");
        assert_eq!(json_tail_from(&bare, 0), words.len());
        let typing = format!("{words}``");
        assert_eq!(json_tail_from(&typing, 0), words.len());
        // Words after the object make it part of the answer, and plain words hold nothing.
        let example = format!("{words}```json\n{{\"a\": 1}}\n```\n\nThat is the shape.");
        assert_eq!(json_tail_from(&example, 0), example.len());
        assert_eq!(json_tail_from(words, 0), words.len());
        // Nothing before `from` is held, since it has shown already.
        assert_eq!(json_tail_from(&bare, bare.len()), bare.len());
    }

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
        let bare = r#"{"plan": [{"task": "a"}]}"#;
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

    fn done(agent_id: &str, name: &str, input: Value, output: Option<&str>) -> ToolCall {
        let mut call = start_call(agent_id, name, &input);
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
    fn tool_names_every_agent() {
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
        for (agent_id, name, input, title, kind) in cases {
            let call = start_call(agent_id, name, &input);
            assert_eq!(call.title.plain(), title, "{agent_id} {name}");
            assert_eq!(call.kind, kind, "{agent_id} {name}");
            assert_eq!(kind_of(agent_id, name, &input), kind, "{agent_id} {name}");
            assert_eq!(call.state, ToolState::Running);
        }
    }

    #[test]
    fn argonauts_own_tools_are_known_by_every_agent_name() {
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

    /// A command its CLI turned down ran nothing: it reads dim and says so, and never counts
    /// as failed.
    #[test]
    fn a_refused_command_reads_not_allowed() {
        let mut call = start_call(
            "claude-code",
            "Bash",
            &json!({"command": "python3 -B -m unittest -q && echo ok"}),
        );
        finish_call(
            &mut call,
            false,
            Some(
                "Permission to use Bash has been denied because Claude Code is running in \
                 don't ask mode. IMPORTANT: You *may* try to accomplish this action",
            ),
        );
        assert_eq!(call.state, ToolState::Refused);
        assert_eq!(
            rows(&call, 80),
            "● Bash(python3 -B -m unittest -q && echo ok)\n  ⎿  Not allowed"
        );
        let theme = theme();
        let lines = call.lines(&RenderCtx::new(80, &theme, Clock::still(Instant::now())));
        let dim = theme.style(crate::base::theme::Role::Dim);
        assert_eq!(lines[0].spans[0].style.fg, dim.fg, "the bullet is dim");
        assert!(
            lines[1]
                .spans
                .iter()
                .filter(|span| !span.content.trim().is_empty())
                .all(|span| span.style.fg == dim.fg),
            "{:?}",
            lines[1]
        );
    }

    #[test]
    fn an_edit_takes_the_patch_its_result_reported() {
        let mut call = start_call(
            "claude-code",
            "Edit",
            &json!({"file_path": "/w/src/a.rs", "old_string": "x", "new_string": "y"}),
        );
        // While it runs the edit says so, and its diff waits for the end, so it keeps one shape.
        assert_eq!(rows(&call, 40), "● Update(/w/src/a.rs)\n  ⎿  Updating…");
        take_patch(
            &mut call,
            "/w/src/a.rs",
            "@@ -6,3 +6,3 @@\n a\n-x\n+y\n b\n",
        );
        finish_call(&mut call, true, Some("The file has been updated."));
        assert_eq!(
            rows(&call, 40),
            "● Update(/w/src/a.rs)\n  ⎿  Added 1 line, removed 1 line\n      6  a\n      7 -x\n      7 +y\n      8  b"
        );
        // A write over a file swaps the whole new text for what changed.
        let mut call = start_call(
            "claude-code",
            "Write",
            &json!({"file_path": "/w/b.txt", "content": "one\ntwo\n"}),
        );
        take_patch(&mut call, "/w/b.txt", "@@ -1,2 +1,2 @@\n one\n-2\n+two\n");
        assert_eq!(
            rows(&call, 40),
            "● Write(/w/b.txt)\n  ⎿  Added 1 line, removed 1 line\n      1  one\n      2 -2\n      2 +two"
        );
        // A patch that can't be read leaves the preview.
        let before = rows(&call, 40);
        take_patch(&mut call, "/w/b.txt", "nonsense");
        assert_eq!(rows(&call, 40), before);
    }

    #[test]
    fn a_codex_edit_of_two_files_keeps_both_patches() {
        let changes = json!({"changes": [
            {"path": "/w/a.txt", "kind": "update"},
            {"path": "/w/b.txt", "kind": "update"},
        ]});
        let mut call = start_call("codex", "file_change", &changes);
        take_patch(&mut call, "/w/a.txt", "@@ -1 +1 @@\n-1\n+one\n");
        take_patch(&mut call, "/w/b.txt", "@@ -4 +4 @@\n-4\n+four\n");
        finish_call(&mut call, true, None);
        assert_eq!(
            rows(&call, 40),
            "● Update(/w/a.txt, /w/b.txt)\n  ⎿  Added 1 line, removed 1 line\n      1 -1\n      1 +one\n     Added 1 line, removed 1 line\n      4 -4\n      4 +four"
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
        let error = theme.style(Role::Danger).fg;
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
    fn a_shell_call_that_only_asks_git_what_is_there_folds_with_the_reads() {
        let looks = |command: &str| only_looks(&ToolCall::call(ToolKind::Shell, "Bash", command));
        assert!(looks("git log --oneline -3 main && git status --short"));
        assert!(looks("git diff HEAD~1 | head -40"));
        assert!(looks("ls src; git show HEAD:Cargo.toml"));
        // Anything git may change keeps its own row, and so does git with nothing after it.
        assert!(!looks("git commit -m wip"));
        assert!(!looks("git log && git push"));
        assert!(!looks("git branch feature"));
        assert!(!looks("git"));
        assert!(!looks("cargo test"));
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
        for (agent_id, name, input) in &calls {
            let call = done(agent_id, name, input.clone(), Some("ok"));
            assert!(call.kind.is_quiet(), "{agent_id} {name}");
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
