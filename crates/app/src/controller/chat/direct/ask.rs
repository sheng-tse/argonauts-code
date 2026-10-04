//! What a direct turn's CLI asks you, as Claude Code asks it: a card per edit, command, or other
//! tool, answered Yes, Yes and don't ask again, or No. What you already allowed is answered
//! here without a card.

use argonauts_core::asking::AskReply;
use argonauts_core::events::checkpoint::AnswerOption;
use serde_json::Value;

use crate::choices::{choose, option};

/// Claude Code's tools that change a file.
const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Claude Code's shell tool.
const COMMAND_TOOL: &str = "Bash";

/// A folder outside your checkout, as OpenCode asks for it (`external_directory`). Never
/// allowed without asking, and never for good.
pub(crate) const OUTSIDE_TOOL: &str = "ExternalDirectory";

/// Body rows shown at most; the card says how many more there are.
const MAX_ROWS: usize = 12;

/// What "don't ask again" would cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Grant {
    /// Every edit, for the rest of the conversation.
    Edits,
    /// Commands that start with this, saved to `approvals.allow_commands`.
    Commands(String),
    /// Every call of this tool, saved to `approvals.allow_commands` by its name.
    Tool(String),
    /// Nothing: this question is asked every time.
    Never,
}

/// One question, ready for a card.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Question {
    pub title: String,
    pub body: Vec<String>,
    pub grant: Grant,
    /// What it asks about, as the closed card says it: "the edit to notes.txt".
    pub about: String,
}

impl Question {
    pub(crate) fn of(tool: &str, input: &Value) -> Self {
        if EDIT_TOOLS.contains(&tool) {
            let file = text(input, &["file_path", "notebook_path", "path"]);
            let file = if file.is_empty() { "a file" } else { &file };
            return Self {
                title: format!("Make this edit to {file}?"),
                body: edit_rows(input),
                grant: Grant::Edits,
                about: format!("the edit to {file}"),
            };
        }
        if tool == COMMAND_TOOL {
            let command = text(input, &["command"]);
            let mut body: Vec<String> = command.lines().map(str::to_owned).collect();
            let description = text(input, &["description"]);
            if !description.is_empty() {
                body.push(description);
            }
            let first = command.lines().next().unwrap_or_default().trim();
            return Self {
                title: "Run this command?".to_owned(),
                body: capped(body),
                grant: Grant::Commands(command_prefix(&command)),
                about: crate::controller::recap::title_from_request(first),
            };
        }
        if tool == OUTSIDE_TOOL {
            let folder = text(input, &["path"]);
            return Self {
                title: format!("Work outside your folder, in {folder}?"),
                about: format!("working in {folder}"),
                body: vec![folder],
                grant: Grant::Never,
            };
        }
        let shown = serde_json::to_string_pretty(input).unwrap_or_default();
        Self {
            title: String::new(),
            body: capped(shown.lines().map(str::to_owned).collect()),
            grant: Grant::Tool(tool.to_owned()),
            about: tool.to_owned(),
        }
    }

    /// The card's title, naming the CLI where the tool is its own.
    pub(crate) fn title(&self, cli: &str, tool: &str) -> String {
        if self.title.is_empty() {
            format!("Let {cli} use {tool}?")
        } else {
            self.title.clone()
        }
    }

    /// Yes `y`, Yes and don't ask again `a`, No `n`, as Claude Code words them.
    pub(crate) fn options(&self, cli: &str) -> Vec<AnswerOption> {
        let again = match &self.grant {
            Grant::Edits => {
                Some("Yes, and allow edits for the rest of this conversation".to_owned())
            }
            Grant::Commands(prefix) => Some(format!(
                "Yes, and don't ask again for {prefix} in this project"
            )),
            Grant::Tool(tool) => Some(format!(
                "Yes, and don't ask again for {tool} in this project"
            )),
            Grant::Never => None,
        };
        let mut options = vec![option('y', "Yes", choose("yes"), true)];
        options.extend(again.map(|again| option('a', &again, choose("always"), false)));
        options.push(option(
            'n',
            &format!("No, and tell {cli} what to do instead (esc)"),
            choose("no"),
            false,
        ));
        options
    }
}

/// Whether your earlier answers already allow this call.
pub(crate) fn allowed(tool: &str, input: &Value, edits_allowed: bool, patterns: &[String]) -> bool {
    if tool == OUTSIDE_TOOL {
        return false;
    }
    if EDIT_TOOLS.contains(&tool) {
        return edits_allowed;
    }
    if tool == COMMAND_TOOL {
        let command = text(input, &["command"]);
        return patterns
            .iter()
            .any(|pattern| command_matches(pattern, &command));
    }
    patterns.iter().any(|pattern| pattern == tool)
}

/// The pattern "don't ask again" saves for a command: its prefix, then ` *`.
pub(crate) fn command_pattern(prefix: &str) -> String {
    format!("{prefix} *")
}

/// A pattern ending in ` *` covers the command it names and any that go on from it; any other
/// pattern covers itself. A command that chains, pipes, or redirects is never covered, since
/// its prefix says nothing of the rest.
pub(crate) fn command_matches(pattern: &str, command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() || has_shell_operators(command) {
        return false;
    }
    match pattern.strip_suffix(" *") {
        Some(base) => command == base || command.starts_with(&format!("{base} ")),
        None => command == pattern,
    }
}

/// The words a command starts with that say what it runs, such as `python -m pytest` or
/// `cargo test`: up to three, stopping at a flag (but for `-m`), a path, or anything the shell
/// reads.
pub(crate) fn command_prefix(command: &str) -> String {
    let mut words = Vec::new();
    let mut takes_module = false;
    for word in command.split_whitespace() {
        if words.len() == 3 || has_shell_operators(word) {
            break;
        }
        if takes_module {
            words.push(word);
            takes_module = false;
            continue;
        }
        if word == "-m" {
            words.push(word);
            takes_module = true;
            continue;
        }
        if word.starts_with('-') || word.contains('/') || word.contains('=') {
            break;
        }
        words.push(word);
    }
    if words.is_empty() {
        command.trim().to_owned()
    } else {
        words.join(" ")
    }
}

fn has_shell_operators(text: &str) -> bool {
    ["&&", "||", ";", "|", ">", "<", "`", "$(", "\n", "&"]
        .iter()
        .any(|operator| text.contains(operator))
}

/// What No says to the CLI, which then stops, as Claude Code does when you answer no.
pub(crate) fn no() -> AskReply {
    AskReply::deny("The user said no to this. Stop, and wait for what they say next.")
}

/// An edit as `-` and `+` rows, or a new file's first lines.
fn edit_rows(input: &Value) -> Vec<String> {
    let mut rows = Vec::new();
    let mut pair = |old: &str, new: &str| {
        rows.extend(old.lines().map(|line| format!("- {line}")));
        rows.extend(new.lines().map(|line| format!("+ {line}")));
    };
    // An ACP agent's edit comes as a unified diff.
    if let Some(diff) = input.get("diff").and_then(Value::as_str) {
        let changed = diff
            .lines()
            .filter(|line| !line.starts_with("+++") && !line.starts_with("---"))
            .filter_map(|line| match line.split_at_checked(1) {
                Some(("+", rest)) => Some(format!("+ {rest}")),
                Some(("-", rest)) => Some(format!("- {rest}")),
                _ => None,
            });
        return capped(changed.collect());
    }
    if let Some(edits) = input.get("edits").and_then(Value::as_array) {
        for edit in edits {
            pair(&text(edit, &["old_string"]), &text(edit, &["new_string"]));
        }
    } else if input.get("old_string").is_some() {
        pair(&text(input, &["old_string"]), &text(input, &["new_string"]));
    } else {
        pair("", &text(input, &["content", "new_source"]));
    }
    capped(rows)
}

fn capped(mut rows: Vec<String>) -> Vec<String> {
    if rows.len() > MAX_ROWS {
        let hidden = rows.len() - (MAX_ROWS - 1);
        rows.truncate(MAX_ROWS - 1);
        rows.push(format!("… +{hidden} lines"));
    }
    rows
}

fn text(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// OpenCode asking to reach a folder outside yours (live, 1.18.34) gets a card of its own,
    /// answered Yes or No, and nothing you allowed before covers it.
    #[test]
    fn a_folder_outside_yours_is_asked_about_every_time() {
        let input = json!({"path": "/"});
        let question = Question::of(OUTSIDE_TOOL, &input);
        assert_eq!(
            question.title("OpenCode", OUTSIDE_TOOL),
            "Work outside your folder, in /?"
        );
        let keys: Vec<char> = question
            .options("OpenCode")
            .iter()
            .map(|option| option.key)
            .collect();
        assert_eq!(keys, ['y', 'n']);
        let everything = [OUTSIDE_TOOL.to_owned(), "* *".to_owned()];
        assert!(!allowed(OUTSIDE_TOOL, &input, true, &everything));
    }

    #[test]
    fn each_tool_gets_claude_codes_question() {
        let edit = Question::of(
            "Edit",
            &json!({"file_path": "invoice/money.py", "old_string": "sign = 1", "new_string": "negative = True"}),
        );
        assert_eq!(
            edit.title("Claude Code", "Edit"),
            "Make this edit to invoice/money.py?"
        );
        assert_eq!(edit.body, ["- sign = 1", "+ negative = True"]);
        let labels: Vec<_> = edit
            .options("Claude Code")
            .into_iter()
            .map(|option| option.label)
            .collect();
        assert_eq!(
            labels,
            [
                "Yes",
                "Yes, and allow edits for the rest of this conversation",
                "No, and tell Claude Code what to do instead (esc)"
            ]
        );

        let bash = Question::of("Bash", &json!({"command": "python -m pytest -q"}));
        assert_eq!(bash.title("Claude Code", "Bash"), "Run this command?");
        assert_eq!(bash.grant, Grant::Commands("python -m pytest".into()));

        let fetch = Question::of("WebFetch", &json!({"url": "https://example.com"}));
        assert_eq!(
            fetch.title("Claude Code", "WebFetch"),
            "Let Claude Code use WebFetch?"
        );
        assert_eq!(fetch.grant, Grant::Tool("WebFetch".into()));
    }

    #[test]
    fn a_saved_pattern_covers_its_commands_and_nothing_chained() {
        let pattern = command_pattern(&command_prefix("cargo test --workspace"));
        assert_eq!(pattern, "cargo test *");
        assert!(command_matches(&pattern, "cargo test"));
        assert!(command_matches(&pattern, "cargo test -p app"));
        assert!(!command_matches(&pattern, "cargo testify"));
        assert!(!command_matches(&pattern, "cargo test && rm -rf x"));
        assert!(!command_matches(&pattern, "cargo test > out.txt"));
        assert_eq!(command_prefix("echo hi > shell.txt"), "echo hi");
        assert_eq!(command_prefix("./run.sh"), "./run.sh");

        let patterns = vec![pattern, "WebFetch".to_owned()];
        assert!(allowed(
            "Bash",
            &json!({"command": "cargo test"}),
            false,
            &patterns
        ));
        assert!(!allowed(
            "Bash",
            &json!({"command": "ls"}),
            false,
            &patterns
        ));
        assert!(allowed("WebFetch", &json!({}), false, &patterns));
        assert!(!allowed("Edit", &json!({}), false, &patterns));
        assert!(allowed("Write", &json!({}), true, &patterns));
    }
}
