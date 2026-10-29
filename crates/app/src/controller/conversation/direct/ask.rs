//! What a direct turn's CLI asks you, as Claude Code asks it: a question per edit, command, or
//! other tool, answered Yes, Yes and don't ask again, or No. What you already allowed is answered
//! here without one.

use std::path::Path;

use argonauts_core::asking::AskReply;
use argonauts_core::events::approval::AnswerOption;
use argonauts_core::outside::forms;
use serde_json::Value;

use crate::choices::{choose, option};

/// Claude Code's tools that change a file.
const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Claude Code's shell tool.
const COMMAND_TOOL: &str = "Bash";

/// A folder outside your checkout, as OpenCode asks for it (`external_directory`). Never
/// allowed without asking, and never for good.
pub(crate) const OUTSIDE_TOOL: &str = "ExternalDirectory";

/// Body rows shown at most; the question says how many more there are.
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

/// One question, ready to show.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Question {
    pub title: String,
    pub body: Vec<String>,
    pub grant: Grant,
    /// What it asks about, as the closed question says it: "the edit to notes.txt".
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
            let mut body = capped(body);
            // Claude Code asks about these every time, so nothing can be allowed for good.
            let grant = match super::rm::always_asked(&command) {
                Some(why) => {
                    body.push(format!("Claude Code always asks about this one: {why}."));
                    Grant::Never
                }
                None => Grant::Commands(command_prefix(&command)),
            };
            return Self {
                title: "Run this command?".to_owned(),
                body,
                grant,
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

    /// The question for a call from a turn that works in `folder`. Where the turn may write
    /// outside it, an edit there, or a command that names a path there, is asked about every
    /// time: Claude Code's own grants keep to its working folder, and so do the ones answered
    /// here.
    pub(crate) fn in_folder(tool: &str, input: &Value, folder: &Folder<'_>) -> Self {
        let mut question = Self::of(tool, input);
        let outside = match &question.grant {
            Grant::Edits => folder.edit_outside(&edited_file(input)),
            Grant::Commands(_) => names_outside(folder, &text(input, &["command"])),
            Grant::Tool(_) | Grant::Never => false,
        };
        if outside {
            question.grant = Grant::Never;
        }
        question
    }

    /// The question's title, naming the CLI where the tool is its own.
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

/// The folder a turn works in, and whether its sandbox lets it write outside: only then must
/// what you allowed keep to the folder, since elsewhere the sandbox keeps every write in it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Folder<'a> {
    pub(crate) path: &'a Path,
    pub(crate) writes_outside: bool,
    /// Your home, which `~` and `$HOME` name.
    pub(crate) home: Option<&'a Path>,
}

impl Folder<'_> {
    fn edit_outside(&self, file: &str) -> bool {
        self.writes_outside && outside_folder(self.path, file)
    }
}

/// Whether your earlier answers already allow this call from a turn that works in `folder`.
pub(crate) fn allowed(
    tool: &str,
    input: &Value,
    edits_allowed: bool,
    patterns: &[String],
    folder: &Folder<'_>,
) -> bool {
    if tool == OUTSIDE_TOOL {
        return false;
    }
    if EDIT_TOOLS.contains(&tool) {
        // Argonauts Code's own files change through a setup you pick a place for, never through an
        // edit you allowed for the code, so such an edit always asks, as one outside the folder
        // does.
        let file = edited_file(input);
        return edits_allowed && !in_setup_folder(&file) && !folder.edit_outside(&file);
    }
    if tool == COMMAND_TOOL {
        // A command that reaches into Argonauts Code's own files asks each time, as their edits
        // do, and so does one that names a path outside the folder.
        let command = text(input, &["command"]);
        return !names_setup_folder(&command)
            && !names_outside(folder, &command)
            && patterns
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

/// Whether autopilot says yes with nobody there. A turn started on autopilot never asks, so
/// whatever "don't ask again" could cover is a yes. What is asked about every time isn't: a
/// removal Claude Code always asks about, a folder outside yours, and Argonauts Code's own
/// files.
pub(crate) fn autopilot_allows(tool: &str, input: &Value) -> bool {
    if Question::of(tool, input).grant == Grant::Never {
        return false;
    }
    if EDIT_TOOLS.contains(&tool) {
        return !in_setup_folder(&text(input, &["file_path", "notebook_path", "path"]));
    }
    tool != COMMAND_TOOL || !names_setup_folder(&text(input, &["command"]))
}

/// What autopilot's no says to the CLI, which skips the call and carries on.
pub(crate) fn not_on_autopilot() -> AskReply {
    AskReply::deny(
        "Nobody is at the terminal: the user is on autopilot, and this is asked about every \
         time. Carry on without it, and say in your answer what you left undone.",
    )
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

/// Whether a shell command names a `.argonauts` folder anywhere in it.
pub(crate) fn names_setup_folder(command: &str) -> bool {
    command.contains(".argonauts")
}

/// A path in a `.argonauts` folder, the project's or yours.
pub(crate) fn in_setup_folder(path: &str) -> bool {
    std::path::Path::new(path)
        .components()
        .any(|part| part.as_os_str() == ".argonauts")
}

/// Whether a command may write outside `folder`, where the turn may: it names a path that
/// lands there, by where its links point, or runs code given on its command line, whose paths
/// no word names.
fn names_outside(folder: &Folder<'_>, command: &str) -> bool {
    if !folder.writes_outside {
        return false;
    }
    let words: Vec<&str> = command
        .split_whitespace()
        .map(|word| word.trim_matches(['"', '\'']))
        .filter(|word| !word.is_empty())
        .collect();
    runs_inline_code(&words)
        || words
            .iter()
            .filter_map(|word| path_in(word))
            .any(|path| lands_outside(folder, path))
}

/// Interpreters that take a program on their command line.
const INTERPRETERS: &[&str] = &[
    "python",
    "python2",
    "python3",
    "node",
    "nodejs",
    "deno",
    "bun",
    "ruby",
    "perl",
    "php",
    "sh",
    "bash",
    "zsh",
    "dash",
    "fish",
    "lua",
    "osascript",
    "pwsh",
];

/// Commands that run the one after them, by their words, such as `nice -n 10 python3`.
const WRAPPERS: &[&[&str]] = &[
    &["env"],
    &["time"],
    &["nice"],
    &["nohup"],
    &["xargs"],
    &["command"],
    &["exec"],
    &["npx"],
    &["bunx"],
    &["uv", "run"],
    &["poetry", "run"],
    &["pipenv", "run"],
    &["pnpm", "exec"],
];

/// The short flags that give an interpreter its program inline, alone or in a group, as
/// `python3 -c`, `bash -lc` and `perl -pe` do.
const INLINE_FLAGS: &[char] = &['c', 'e', 'E', 'p', 'r'];

/// Whether the command runs an interpreter on code given inline, past assignments and
/// wrappers such as `env`, `nice` and `uv run`.
fn runs_inline_code(words: &[&str]) -> bool {
    let mut rest = words;
    let mut wrapped = false;
    loop {
        while rest
            .first()
            .is_some_and(|word| word.contains('=') || word.starts_with('-'))
        {
            rest = &rest[1..];
        }
        match WRAPPERS.iter().find(|wrapper| rest.starts_with(wrapper)) {
            Some(wrapper) => {
                rest = &rest[wrapper.len()..];
                wrapped = true;
            }
            None => break,
        }
    }
    // A wrapper's own options may take a value, as `nice -n 10` does, so past one any word
    // may be the program.
    let programs = if wrapped {
        rest.len()
    } else {
        rest.len().min(1)
    };
    (0..programs).any(|at| {
        is_interpreter(rest[at]) && rest[at + 1..].iter().any(|word| gives_inline_code(word))
    })
}

fn is_interpreter(program: &str) -> bool {
    let name = program.rsplit('/').next().unwrap_or(program);
    INTERPRETERS.contains(&name)
        || name
            .strip_prefix("python3.")
            .is_some_and(|minor| minor.chars().all(|ch| ch.is_ascii_digit()))
}

/// Whether a word gives a program inline: `--eval` or `--print`, with its value after `=` or
/// not, or a group of short flags with one of `INLINE_FLAGS` in it, such as `-Ic`.
fn gives_inline_code(word: &str) -> bool {
    if let Some(long) = word.strip_prefix("--") {
        let name = long.split_once('=').map_or(long, |(name, _)| name);
        return ["eval", "print"].contains(&name);
    }
    word.strip_prefix('-').is_some_and(|group| {
        !group.is_empty()
            && group.chars().all(|ch| ch.is_ascii_alphanumeric())
            && group.contains(INLINE_FLAGS)
    })
}

/// The path a word could name: an option's value after `=`, one glued to a short option such as
/// `-C/tmp`, or the word itself. A flag alone names none.
fn path_in(word: &str) -> Option<&str> {
    if let Some(long) = word.strip_prefix("--") {
        return long.split_once('=').map(|(_, value)| value);
    }
    if let Some(short) = word.strip_prefix('-') {
        let glued = short.get(1..).filter(|rest| !rest.is_empty())?;
        return Some(glued.strip_prefix('=').unwrap_or(glued));
    }
    Some(word.split_once('=').map_or(word, |(_, value)| value)).filter(|path| !path.is_empty())
}

/// Whether `path`, as a command names it, lands outside `folder`: `~` and `$HOME` are your
/// home, another variable before a `/` could be anywhere, and a relative path is the folder's,
/// followed through its links.
fn lands_outside(folder: &Folder<'_>, path: &str) -> bool {
    let in_home = ["~/", "$HOME/", "${HOME}/"]
        .iter()
        .find_map(|prefix| path.strip_prefix(prefix))
        .or_else(|| ["~", "$HOME", "${HOME}"].contains(&path).then_some(""));
    if let Some(rest) = in_home {
        return folder
            .home
            .is_none_or(|home| outside_folder(folder.path, &home.join(rest).to_string_lossy()));
    }
    if path.starts_with('~') || (path.starts_with('$') && path.contains('/')) {
        return true;
    }
    outside_folder(folder.path, path)
}

fn edited_file(input: &Value) -> String {
    text(input, &["file_path", "notebook_path", "path"])
}

/// Whether `file`, as the CLI names it, lands outside `folder`, by where its links point: a
/// link in the folder to a file outside counts as outside. A relative path is the folder's own.
fn outside_folder(folder: &Path, file: &str) -> bool {
    if file.is_empty() {
        return false;
    }
    let real = |path: &Path| forms(path).pop().unwrap_or_else(|| path.to_path_buf());
    !real(&folder.join(file)).starts_with(real(folder))
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

    /// OpenCode asking to reach a folder outside yours (live, 1.18.34) gets a question of its own,
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
        let repo = Folder {
            path: Path::new("/repo"),
            writes_outside: true,
            home: None,
        };
        assert!(!allowed(OUTSIDE_TOOL, &input, true, &everything, &repo));
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

        // A flagged rm says why it's asked, and offers no "don't ask again".
        let rm = Question::of("Bash", &json!({"command": r#"rm -rf "$BUILD_DIR/tmp""#}));
        assert_eq!(rm.grant, Grant::Never);
        assert_eq!(
            rm.body,
            [
                r#"rm -rf "$BUILD_DIR/tmp""#,
                "Claude Code always asks about this one: if BUILD_DIR were empty, it would \
                 remove /tmp."
            ]
        );
        let labels: Vec<_> = rm
            .options("Claude Code")
            .into_iter()
            .map(|option| option.label)
            .collect();
        assert_eq!(
            labels,
            ["Yes", "No, and tell Claude Code what to do instead (esc)"]
        );

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
        let repo = &Folder {
            path: Path::new("/repo"),
            writes_outside: true,
            home: None,
        };
        assert!(allowed(
            "Bash",
            &json!({"command": "cargo test"}),
            false,
            &patterns,
            repo
        ));
        assert!(!allowed(
            "Bash",
            &json!({"command": "ls"}),
            false,
            &patterns,
            repo
        ));
        // A command into Argonauts Code's own files asks each time, whatever was allowed before.
        let into_settings = json!({"command": "cargo test --manifest-path .argonauts/x.toml"});
        assert!(!allowed("Bash", &into_settings, true, &patterns, repo));
        assert!(allowed("WebFetch", &json!({}), false, &patterns, repo));
        assert!(!allowed("Edit", &json!({}), false, &patterns, repo));
        assert!(allowed("Write", &json!({}), true, &patterns, repo));
        // Edits allowed for the code never reach Argonauts Code's own files.
        let settings = json!({"file_path": "/repo/.argonauts/config.toml"});
        assert!(!allowed("Write", &settings, true, &patterns, repo));
        let role = json!({"file_path": ".argonauts/roles/reviewer.md"});
        assert!(!allowed("Edit", &role, true, &patterns, repo));
        assert!(allowed(
            "Edit",
            &json!({"file_path": "src/app.rs"}),
            true,
            &patterns,
            repo
        ));
    }

    /// Edits allowed for the rest of the conversation keep to the turn's folder, as Claude
    /// Code's own do: one outside it, however its path is written, asks every time.
    #[test]
    fn an_edit_outside_the_folder_asks_every_time() {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("app");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::create_dir_all(home.path().join("Downloads")).unwrap();
        std::os::unix::fs::symlink(home.path().join("Downloads"), repo.join("out")).unwrap();
        let edit = |file: &str| json!({ "file_path": file, "content": "notes" });
        let notes = home.path().join("Downloads/y.md");
        let open = Folder {
            path: &repo,
            writes_outside: true,
            home: Some(home.path()),
        };
        for file in [notes.to_str().unwrap(), "../Downloads/y.md", "out/y.md"] {
            assert!(!allowed("Write", &edit(file), true, &[], &open), "{file}");
            let question = Question::in_folder("Write", &edit(file), &open);
            assert_eq!(question.grant, Grant::Never, "{file}");
            let keys: Vec<char> = question
                .options("Claude Code")
                .iter()
                .map(|option| option.key)
                .collect();
            assert_eq!(keys, ['y', 'n'], "{file}");
        }
        let inside = repo.join("src/a.rs");
        for file in ["src/a.rs", inside.to_str().unwrap()] {
            assert!(allowed("Write", &edit(file), true, &[], &open), "{file}");
            let question = Question::in_folder("Write", &edit(file), &open);
            assert_eq!(question.grant, Grant::Edits, "{file}");
        }
        // Where the sandbox keeps every write in the folder, as with `[outside] ask` emptied,
        // the edits you allowed cover one outside as before: the sandbox refuses it.
        let kept_in = Folder {
            writes_outside: false,
            ..open
        };
        assert!(allowed("Write", &edit("out/y.md"), true, &[], &kept_in));
        let question = Question::in_folder("Write", &edit("out/y.md"), &kept_in);
        assert_eq!(question.grant, Grant::Edits);
        // Autopilot coming on mid-turn works outside without asking, as autopilot does.
        assert!(autopilot_allows("Write", &edit(notes.to_str().unwrap())));
    }

    /// A command you said not to ask about again is still asked about, where the turn may write
    /// outside the folder, when it names a path there however it's written, or runs code given
    /// on its command line, behind a flag group or a wrapper too. A word that only holds a `$`
    /// or a pattern isn't a path, and an interpreter's name as a command's argument runs none.
    #[test]
    fn a_command_that_names_a_path_outside_the_folder_asks_every_time() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().canonicalize().unwrap();
        let repo = home.join("app");
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        std::fs::create_dir_all(home.join("Downloads")).unwrap();
        std::os::unix::fs::symlink(home.join("Downloads"), repo.join("out")).unwrap();
        let open = Folder {
            path: &repo,
            writes_outside: true,
            home: Some(&home),
        };
        let patterns = [
            "cp", "tar", "python3", "node", "grep", "awk", "ls", "bash", "sh", "perl", "uv",
            "poetry", "pipenv", "pnpm", "nice", "time", "nohup", "xargs", "command", "exec", "npx",
            "bunx",
        ]
        .map(command_pattern);
        let command = |line: &str| json!({ "command": line });
        let downloads = home.join("Downloads/y.md");
        let absolute = format!("cp notes.md {}", downloads.display());
        for line in [
            absolute.as_str(),
            "cp notes.md ../y.md",
            "cp notes.md ~/Downloads/y.md",
            "cp notes.md $HOME/y.md",
            "cp notes.md ${TMPDIR}/y.md",
            "cp --target-directory=/tmp notes.md",
            "cp notes.md out/y.md",
            "tar -C/tmp -xf notes.tar",
            "python3 -c \"open('/tmp/y.md','w')\"",
            "node -e \"require('fs').writeFileSync('/tmp/y.md','')\"",
            "ls ~",
            "bash -lc \"echo x > /tmp/y\"",
            "python3 -Ic \"open('/tmp/y','w')\"",
            "sh -ec 'touch $OUT'",
            "perl -pi -e 's/a/b/' $FILE",
            "node --eval=\"require('fs').rmSync(process.env.OUT)\"",
            "uv run python -c \"open('/tmp/y','w')\"",
            "poetry run python -c \"open('/tmp/y','w')\"",
            "pipenv run python3 -Ic \"open('/tmp/y','w')\"",
            "pnpm exec node -e \"require('fs').writeFileSync(process.env.OUT,'')\"",
            "nice -n 10 python3 -c \"open('/tmp/y','w')\"",
            "time bash -c 'touch $OUT'",
            "nohup sh -c 'touch $OUT'",
            "xargs -I{} sh -c 'touch {}'",
            "command python3 -c \"open('/tmp/y','w')\"",
            "exec python3 -c \"open('/tmp/y','w')\"",
            "npx -p node@20 node -e \"require('fs').writeFileSync(process.env.OUT,'')\"",
            "bunx bun -e \"Bun.write(process.env.OUT,'')\"",
        ] {
            assert!(
                !allowed("Bash", &command(line), false, &patterns, &open),
                "{line}"
            );
            let question = Question::in_folder("Bash", &command(line), &open);
            assert_eq!(question.grant, Grant::Never, "{line}");
        }
        let inside = format!("cp notes.md {}/docs/", repo.display());
        for line in [
            "cp notes.md docs/notes.md",
            inside.as_str(),
            "cp notes.md ~/app/docs/",
            "grep -n 'main()$' docs",
            "awk '{print $1}' docs/notes.md",
            "python3 tools/notes.py",
            "bash scripts/build.sh",
            "grep -rc python docs",
            "uv run pytest -q",
            "nice -n 10 python3 tools/notes.py",
            "npx prettier --check docs",
        ] {
            assert!(
                allowed("Bash", &command(line), false, &patterns, &open),
                "{line}"
            );
            let question = Question::in_folder("Bash", &command(line), &open);
            assert!(matches!(question.grant, Grant::Commands(_)), "{line}");
        }
        // Where the sandbox keeps every write in the folder, a saved pattern covers what it
        // did before `[outside] ask` existed.
        let kept_in = Folder {
            writes_outside: false,
            ..open
        };
        for line in [
            "cp notes.md ~/Downloads/y.md",
            "ls ~",
            "tar -C/tmp -xf notes.tar",
        ] {
            assert!(
                allowed("Bash", &command(line), false, &patterns, &kept_in),
                "{line}"
            );
        }
    }
}
