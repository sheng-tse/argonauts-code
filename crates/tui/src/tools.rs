//! Turns vendor tool calls into short, vendor-neutral summaries and diff previews.
//! Harnesses name tools and keys differently, so matching ignores case, `_`, and `-`.

use std::path::Path;

use serde_json::{Map, Value};

use crate::sanitize::{clean, clean_line};
use crate::text::truncate;

const DIFF_LINES_KEPT: usize = 12;

/// Longest target text kept, in columns.
const TARGET_MAX: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ToolKind {
    Read,
    List,
    Edit,
    Write,
    Run,
    Search,
    Web,
    Agent,
    Todo,
    Mcp,
    Other,
}

impl ToolKind {
    /// The verb shown for this kind, or `None` when the vendor's own tool name is shown instead.
    fn verb(self) -> Option<&'static str> {
        let verb = match self {
            Self::Read => "Read",
            Self::List => "List",
            Self::Edit => "Edit",
            Self::Write => "Write",
            Self::Run => "Run",
            Self::Search => "Search",
            Self::Web => "Web",
            Self::Agent => "Agent",
            Self::Todo => "Todo",
            Self::Mcp => "MCP",
            Self::Other => return None,
        };
        Some(verb)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffSign {
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffLine {
    pub sign: DiffSign,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolSummary {
    pub kind: ToolKind,
    /// The verb shown, such as `Read`, or the vendor's tool name for unknown tools.
    pub label: String,
    /// What the call acts on, such as a path, a command, or a pattern. May be empty.
    pub target: String,
    /// Lines added and removed, for edits that carry their text.
    pub stat: Option<(u32, u32)>,
    pub diff: Vec<DiffLine>,
    /// Lines not kept in `diff`.
    pub diff_hidden: usize,
}

impl ToolSummary {
    /// `cwd` shortens absolute paths.
    pub(crate) fn from_call(name: &str, input: &Value, cwd: Option<&Path>) -> Self {
        let kind = kind_of(name);
        let label = kind.verb().map_or_else(|| clean_line(name), str::to_owned);
        let target = truncate(&target_of(kind, name, input, cwd), TARGET_MAX);
        let mut summary = Self {
            kind,
            label,
            target,
            stat: None,
            diff: Vec::new(),
            diff_hidden: 0,
        };
        if matches!(kind, ToolKind::Edit | ToolKind::Write) {
            summary.add_changes(input);
        }
        summary
    }

    /// One line such as `Edit src/store.ts  (+18 −4)` or `Run npm test`.
    pub(crate) fn describe(&self) -> String {
        let mut line = self.label.clone();
        if !self.target.is_empty() {
            line.push(' ');
            line.push_str(&self.target);
        }
        if let Some((added, removed)) = self.stat {
            if removed == 0 {
                line.push_str(&format!("  (+{added})"));
            } else {
                line.push_str(&format!("  (+{added} −{removed})"));
            }
        }
        line
    }

    /// Whether consecutive calls like this one read well as a single counted line.
    pub(crate) fn groups(&self) -> bool {
        matches!(
            self.kind,
            ToolKind::Read | ToolKind::List | ToolKind::Search | ToolKind::Web
        )
    }

    fn add_changes(&mut self, input: &Value) {
        let mut removed = Vec::new();
        let mut added = Vec::new();
        let pairs = edit_pairs(input);
        if pairs.is_empty()
            && let Some(content) = string_field(input, CONTENT_KEYS)
        {
            added.extend(content.lines().map(str::to_owned));
        }
        for (old, new) in pairs {
            let (gone, came) = changed_lines(&old, &new);
            removed.extend(gone);
            added.extend(came);
        }
        if removed.is_empty() && added.is_empty() {
            return;
        }
        self.stat = Some((added.len() as u32, removed.len() as u32));
        let lines: Vec<DiffLine> = removed
            .into_iter()
            .map(|text| DiffLine {
                sign: DiffSign::Removed,
                text: clean_line(&text),
            })
            .chain(added.into_iter().map(|text| DiffLine {
                sign: DiffSign::Added,
                text: clean_line(&text),
            }))
            .collect();
        self.diff_hidden = lines.len().saturating_sub(DIFF_LINES_KEPT);
        self.diff = lines.into_iter().take(DIFF_LINES_KEPT).collect();
    }
}

/// A counted label for a run of similar calls, such as `Read 6 files`.
pub(crate) fn group_label(kind: ToolKind, label: &str, n: usize) -> String {
    let noun = |one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    match kind {
        ToolKind::Read | ToolKind::Edit | ToolKind::Write => {
            format!("{label} {}", noun("file", "files"))
        }
        ToolKind::List => format!("List {}", noun("folder", "folders")),
        ToolKind::Run => format!("Run {}", noun("command", "commands")),
        ToolKind::Search => format!("Search {}", noun("pattern", "patterns")),
        ToolKind::Web => format!("Web {}", noun("request", "requests")),
        ToolKind::Agent => format!("Agent {}", noun("task", "tasks")),
        ToolKind::Todo => format!("Todo {}", noun("update", "updates")),
        ToolKind::Mcp => format!("MCP {}", noun("call", "calls")),
        ToolKind::Other => format!("{label} ×{n}"),
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

fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn kind_of(name: &str) -> ToolKind {
    if name.starts_with("mcp__") {
        return ToolKind::Mcp;
    }
    match normalize(name).as_str() {
        "read" | "readfile" | "viewfile" | "view" | "cat" | "notebookread" | "viewcodeitem" => {
            ToolKind::Read
        }
        "ls" | "listdir" | "listdirectory" | "listfiles" => ToolKind::List,
        "edit"
        | "multiedit"
        | "editfile"
        | "replacefilecontent"
        | "strreplace"
        | "strreplacebasededittool"
        | "applypatch"
        | "patch"
        | "filechange"
        | "notebookedit" => ToolKind::Edit,
        "write" | "writefile" | "writetofile" | "createfile" => ToolKind::Write,
        "bash" | "shell" | "commandexecution" | "runcommand" | "execcommand" | "exec"
        | "terminal" | "localshell" => ToolKind::Run,
        "grep" | "glob" | "grepsearch" | "search" | "findbyname" | "codebasesearch" | "find"
        | "rg" | "filesearch" => ToolKind::Search,
        "websearch" | "searchweb" | "webfetch" | "fetch" | "readurlcontent" => ToolKind::Web,
        "task" | "agent" | "invokesubagent" | "collabtoolcall" | "spawnagent" | "subagent" => {
            ToolKind::Agent
        }
        "todowrite" | "todolist" | "updateplan" | "todoread" => ToolKind::Todo,
        "mcptoolcall" => ToolKind::Mcp,
        _ => ToolKind::Other,
    }
}

fn target_of(kind: ToolKind, name: &str, input: &Value, cwd: Option<&Path>) -> String {
    let path = || string_field(input, PATH_KEYS).map(|p| relative(&p, cwd));
    let text = match kind {
        ToolKind::Read | ToolKind::List | ToolKind::Write => path(),
        ToolKind::Edit => path().or_else(|| changed_paths(input, cwd)),
        ToolKind::Run => command_field(input).map(|c| unwrap_shell(&c)),
        ToolKind::Search => string_field(input, PATTERN_KEYS)
            .map(|p| format!("\"{p}\""))
            .or_else(path),
        ToolKind::Web => {
            string_field(input, URL_KEYS).or_else(|| string_field(input, PATTERN_KEYS))
        }
        ToolKind::Agent => string_field(input, TASK_KEYS).map(|t| first_line(&t)),
        ToolKind::Mcp => mcp_target(name, input),
        ToolKind::Todo => None,
        ToolKind::Other => first_string(input).map(|s| first_line(&s)),
    };
    text.map(|t| clean_line(&t)).unwrap_or_default()
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
}

/// The command as one line, whether the vendor sends a string or an argument list.
fn command_field(input: &Value) -> Option<String> {
    let object = input.as_object()?;
    let value = COMMAND_KEYS
        .iter()
        .find_map(|wanted| value_at(object, wanted))?;
    match value {
        Value::String(s) => Some(s.clone()),
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

fn changed_paths(input: &Value, cwd: Option<&Path>) -> Option<String> {
    let changes = input.get("changes")?.as_array()?;
    let paths: Vec<String> = changes
        .iter()
        .filter_map(|c| c.get("path").and_then(Value::as_str))
        .map(|p| relative(p, cwd))
        .collect();
    match paths.as_slice() {
        [] => None,
        [one] => Some(one.clone()),
        many => Some(format!("{} files", many.len())),
    }
}

fn mcp_target(name: &str, input: &Value) -> Option<String> {
    if let Some(rest) = name.strip_prefix("mcp__") {
        return Some(rest.replace("__", " "));
    }
    let server = input.get("server").and_then(Value::as_str);
    let tool = input.get("tool").and_then(Value::as_str);
    match (server, tool) {
        (Some(server), Some(tool)) => Some(format!("{server} {tool}")),
        (None, Some(tool)) => Some(tool.to_owned()),
        _ => None,
    }
}

fn first_string(input: &Value) -> Option<String> {
    match input {
        Value::String(s) => Some(s.clone()),
        Value::Object(map) => map.values().find_map(|v| v.as_str().map(str::to_owned)),
        _ => None,
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_owned()
}

fn relative(path: &str, cwd: Option<&Path>) -> String {
    match cwd.and_then(|cwd| Path::new(path).strip_prefix(cwd).ok()) {
        Some(rest) if !rest.as_os_str().is_empty() => rest.display().to_string(),
        _ => path.to_owned(),
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

/// Lines removed and added between `old` and `new`, ignoring shared leading and trailing lines.
fn changed_lines(old: &str, new: &str) -> (Vec<String>, Vec<String>) {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let old_rest = &old[prefix..];
    let new_rest = &new[prefix..];
    let suffix = old_rest
        .iter()
        .rev()
        .zip(new_rest.iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let gone = old_rest[..old_rest.len() - suffix]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let came = new_rest[..new_rest.len() - suffix]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    (gone, came)
}

/// The last `max` lines of `text`, cleaned, with how many earlier lines were left out.
pub(crate) fn tail_lines(text: &str, max: usize) -> (Vec<String>, usize) {
    let cleaned = clean(text);
    let lines: Vec<&str> = cleaned.trim_end().lines().collect();
    let skip = lines.len().saturating_sub(max);
    (
        lines[skip..].iter().map(|l| (*l).to_owned()).collect(),
        skip,
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn describe(name: &str, input: Value) -> String {
        ToolSummary::from_call(name, &input, Some(Path::new("/wt/card-001"))).describe()
    }

    #[test]
    fn claude_code_tools() {
        assert_eq!(
            describe("Read", json!({"file_path": "/wt/card-001/README.md"})),
            "Read README.md"
        );
        assert_eq!(
            describe(
                "Edit",
                json!({"file_path": "/wt/card-001/README.md", "old_string": "npm install acme-api", "new_string": "npm install @acme/api"})
            ),
            "Edit README.md  (+1 −1)"
        );
        assert_eq!(
            describe(
                "Write",
                json!({"file_path": "hello.txt", "content": "hi\n"})
            ),
            "Write hello.txt  (+1)"
        );
        assert_eq!(
            describe(
                "Bash",
                json!({"command": "npm test -- store", "description": "tests"})
            ),
            "Run npm test -- store"
        );
        assert_eq!(
            describe("Grep", json!({"pattern": "npm install", "path": "."})),
            "Search \"npm install\""
        );
        assert_eq!(
            describe(
                "Task",
                json!({"description": "find where auth runs", "prompt": "..."})
            ),
            "Agent find where auth runs"
        );
        assert_eq!(
            describe("mcp__github__create_issue", json!({})),
            "MCP github create_issue"
        );
    }

    #[test]
    fn codex_items() {
        assert_eq!(
            describe(
                "command_execution",
                json!({"command": "/bin/zsh -lc 'ls -la'", "status": "in_progress"})
            ),
            "Run ls -la"
        );
        assert_eq!(
            describe(
                "file_change",
                json!({"changes": [{"path": "/wt/card-001/hello.txt", "kind": "add"}]})
            ),
            "Edit hello.txt"
        );
        assert_eq!(
            describe(
                "file_change",
                json!({"changes": [{"path": "a", "kind": "add"}, {"path": "b", "kind": "update"}]})
            ),
            "Edit 2 files"
        );
        assert_eq!(
            describe("mcp_tool_call", json!({"server": "docs", "tool": "search"})),
            "MCP docs search"
        );
        assert_eq!(
            describe(
                "collab_tool_call",
                json!({"tool": "spawn_agent", "prompt": "check the tests\nmore"})
            ),
            "Agent check the tests"
        );
    }

    #[test]
    fn antigravity_style_keys() {
        assert_eq!(
            describe(
                "run_command",
                json!({"CommandLine": "cargo test", "Cwd": "/wt"})
            ),
            "Run cargo test"
        );
        assert_eq!(
            describe(
                "view_file",
                json!({"AbsolutePath": "/wt/card-001/src/lib.rs"})
            ),
            "Read src/lib.rs"
        );
        let summary = ToolSummary::from_call(
            "replace_file_content",
            &json!({"TargetFile": "a.rs", "ReplacementChunks": [
                {"TargetContent": "x", "ReplacementContent": "y\nz"}
            ]}),
            None,
        );
        assert_eq!(summary.describe(), "Edit a.rs  (+2 −1)");
    }

    #[test]
    fn unknown_tools_use_their_name() {
        assert_eq!(
            describe("frobnicate", json!({"level": "max"})),
            "frobnicate max"
        );
        assert_eq!(describe("frobnicate", json!(null)), "frobnicate");
    }

    #[test]
    fn diff_preview_skips_shared_lines() {
        let summary = ToolSummary::from_call(
            "Edit",
            &json!({
                "file_path": "s.ts",
                "old_string": "a\nconst count = memory.get(key) ?? 0\nz",
                "new_string": "a\nconst bucket = await redis.eval()\nz"
            }),
            None,
        );
        assert_eq!(summary.stat, Some((1, 1)));
        assert_eq!(
            summary.diff,
            vec![
                DiffLine {
                    sign: DiffSign::Removed,
                    text: "const count = memory.get(key) ?? 0".into()
                },
                DiffLine {
                    sign: DiffSign::Added,
                    text: "const bucket = await redis.eval()".into()
                },
            ]
        );
    }

    #[test]
    fn multi_edit_sums_changes_and_caps_preview() {
        let edits: Vec<Value> = (0..10)
            .map(|i| json!({"old_string": format!("old {i}"), "new_string": format!("new {i}")}))
            .collect();
        let summary = ToolSummary::from_call(
            "MultiEdit",
            &json!({"file_path": "f", "edits": edits}),
            None,
        );
        assert_eq!(summary.stat, Some((10, 10)));
        assert_eq!(summary.diff.len(), DIFF_LINES_KEPT);
        assert_eq!(summary.diff_hidden, 8);
    }

    #[test]
    fn targets_are_sanitized() {
        assert_eq!(
            describe(
                "Bash",
                json!({"command": "echo \u{1b}[31mred\u{1b}[0m\nrm -rf x"})
            ),
            "Run echo red rm -rf x"
        );
    }

    #[test]
    fn shell_unwrapping_is_conservative() {
        assert_eq!(unwrap_shell("bash -lc \"npm test\""), "npm test");
        assert_eq!(unwrap_shell("python -c 'x'"), "python -c 'x'");
        assert_eq!(unwrap_shell("ls"), "ls");
    }

    #[test]
    fn group_labels() {
        assert_eq!(group_label(ToolKind::Read, "Read", 6), "Read 6 files");
        assert_eq!(group_label(ToolKind::Read, "Read", 1), "Read 1 file");
        assert_eq!(group_label(ToolKind::Run, "Run", 2), "Run 2 commands");
        assert_eq!(group_label(ToolKind::Other, "frob", 3), "frob ×3");
    }

    #[test]
    fn tail_keeps_the_end() {
        let (lines, skipped) = tail_lines("1\n2\n3\n4\n", 2);
        assert_eq!(lines, vec!["3", "4"]);
        assert_eq!(skipped, 2);
        let (lines, skipped) = tail_lines("\x1b[32mok\x1b[0m", 5);
        assert_eq!(lines, vec!["ok"]);
        assert_eq!(skipped, 0);
    }
}
