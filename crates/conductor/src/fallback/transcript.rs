//! The old session as Markdown, for the agent a task falls back to and for you. Code writes it
//! from the task's events in the run's log, never from an agent's raw log and never with a model,
//! so it reads the same whichever agent ran the session.
//!
//! A title line and the prompt come first, then the session in order: the agent's messages as
//! text, each tool call as one line with its output in a fenced block cut to [`OUTPUT_LINES`].
//! Past about [`MAX_BYTES`] the oldest turns go first.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use argonauts_core::events::session::{SessionEvent, SessionInput, SessionStarted};
use argonauts_core::events::task::{TaskEvent, TaskSpec};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::layout;
use argonauts_core::limits::TranscriptPaths;
use argonauts_core::names::SessionKey;
use argonauts_core::paths::{create_private_dir_all, exclude_from_git, write_private_file};
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::session_runner::Access;
use argonauts_core::{AgentEvent, ErrorCategory, TaskId, TodoItem, TodoStatus};
use serde_json::Value;

use super::note::Step;
use super::why::{category_words, one_line, stop_line};

/// A tool's output keeps this many lines, half from its start and half from its end.
pub const OUTPUT_LINES: usize = 40;
/// About how big the file may grow before its oldest turns are cut.
pub const MAX_BYTES: usize = 400 * 1024;
pub const FILE_NAME: &str = "transcript.md";
/// The worktree's folder for fallback files, which every CLI reads without extra flags.
pub const WORKTREE_FOLDER: &str = ".argonauts/fallback";
/// The git exclude entry that keeps that folder out of every commit.
pub const EXCLUDE_ENTRY: &str = "/.argonauts/fallback/";

/// One output line longer than this is cut, so a minified file can't fill the limit alone.
const LINE_CHARS: usize = 1_000;
const TITLE_CHARS: usize = 120;
const PATH_CHARS: usize = 200;
const ERROR_CHARS: usize = 400;
/// Room kept for the line that says how many turns were cut.
const CUT_LINE_BYTES: usize = 200;

/// The task's last building session: the one a fallback or a rotation replaces.
pub struct OldSession<'a> {
    pub key: &'a SessionKey,
    pub started: &'a SessionStarted,
    /// Its place among the task's building sessions, from 1. The same stop always gets the same
    /// number, however often its note is built.
    pub number: u32,
    /// The task's worktree, when the log says where it is.
    pub worktree: Option<&'a Path>,
}

/// Reviewers and other read-only sessions on the task don't count; only a session that builds
/// falls back.
pub fn old_session<'a>(task: &TaskId, events: &'a [RunEvent]) -> Option<OldSession<'a>> {
    let mut sessions: Vec<(&SessionKey, &SessionStarted)> = Vec::new();
    let mut worktree = None;
    for event in events {
        if event.task_id.as_ref() != Some(task) {
            continue;
        }
        match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started))
                if matches!(started.access, Access::Build | Access::Experiment) =>
            {
                let Some(key) = &event.session else {
                    continue;
                };
                match sessions.iter_mut().find(|(seen, _)| *seen == key) {
                    Some(seen) => seen.1 = started,
                    None => sessions.push((key, started)),
                }
            }
            RunEventKind::Task(TaskEvent::Worktree { path, .. }) => {
                worktree = Some(path.as_path());
            }
            _ => {}
        }
    }
    let number = u32::try_from(sessions.len()).ok()?;
    let (key, started) = sessions.pop()?;
    Some(OldSession {
        key,
        started,
        number,
        worktree,
    })
}

/// `<task worktree>/.argonauts/fallback/<n>/transcript.md`, the copy the note names.
pub fn worktree_path(worktree: &Path, number: u32) -> PathBuf {
    worktree
        .join(WORKTREE_FOLDER)
        .join(number.to_string())
        .join(FILE_NAME)
}

/// `runs/<run>/tasks/<task>/fallback-<n>/transcript.md`, the run's own, kept for you.
pub fn run_path(run_dir: &Path, task: &TaskId, number: u32) -> PathBuf {
    layout::task_dir(run_dir, task)
        .join(format!("fallback-{number}"))
        .join(FILE_NAME)
}

/// The Markdown for `session`, from every event its key carries. `why_stopped` is used when
/// the session's end isn't in the log yet.
pub fn render(
    task: &TaskSpec,
    session: &OldSession<'_>,
    events: &[RunEvent],
    why_stopped: &str,
) -> String {
    let mut reader = Reader::default();
    let mut ended = None;
    for event in events {
        if event.session.as_ref() != Some(session.key) {
            continue;
        }
        match &event.kind {
            RunEventKind::Session(SessionEvent::Input(input)) => reader.input(input),
            RunEventKind::Session(SessionEvent::Ended(end)) => {
                ended = Some(stop_line(&end.reason));
            }
            RunEventKind::Agent(agent) => reader.agent(agent),
            _ => {}
        }
    }
    reader.flush_deltas();

    let mut head = String::new();
    let _ = writeln!(
        head,
        "# {} {} · {} · why it stopped: {}\n",
        task.id,
        one_line(&task.title, TITLE_CHARS),
        session.started.worker,
        ended.as_deref().unwrap_or(why_stopped),
    );
    let _ = writeln!(
        head,
        "Written from the run's event log. Each tool's output is cut to {OUTPUT_LINES} lines; \
         the raw log has all of it.\n"
    );
    head.push_str("## Prompt\n\n");
    let prompt = reader.prompt.as_deref().map(clean);
    fence(
        &mut head,
        prompt
            .as_deref()
            .filter(|text| !text.is_empty())
            .unwrap_or("(no prompt was recorded)"),
    );
    head.push_str("\n## Session\n\n");
    let turns = reader
        .turns
        .iter()
        .map(|turn| turn.iter().map(Block::render).collect())
        .collect();
    assemble(head, turns)
}

/// Writes the run's copy and the worktree's, and keeps the worktree's out of commits. The
/// exclude goes in first, so no commit in between can pick the file up.
pub fn write(
    text: &str,
    number: u32,
    worktree: &Path,
    run_dir: &Path,
    task: &TaskId,
) -> io::Result<TranscriptPaths> {
    let paths = TranscriptPaths {
        worktree: worktree_path(worktree, number),
        run: run_path(run_dir, task, number),
    };
    exclude_from_git(worktree, EXCLUDE_ENTRY)?;
    if let Some(dir) = paths.run.parent() {
        create_private_dir_all(dir)?;
    }
    write_private_file(&paths.run, text.as_bytes())?;
    if let Some(dir) = paths.worktree.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&paths.worktree, text)?;
    Ok(paths)
}

/// The session's blocks, in turns. A turn starts at each agent message and at each message sent
/// to the agent, and holds the tool calls that follow.
#[derive(Default)]
struct Reader {
    prompt: Option<String>,
    turns: Vec<Vec<Block>>,
    /// Streamed text that no message has closed yet.
    deltas: String,
    /// Tool calls still waiting for their result: the turn and block each is in.
    open: HashMap<String, (usize, usize)>,
}

impl Reader {
    /// The first input is the prompt, byte for byte; each later one is a message delivered.
    fn input(&mut self, input: &SessionInput) {
        let text: String = input.parts.iter().map(|part| part.text.as_str()).collect();
        if self.prompt.is_none() {
            self.prompt = Some(text);
            return;
        }
        let labels: Vec<&str> = input.parts.iter().map(|part| part.label.as_str()).collect();
        self.flush_deltas();
        self.start_turn();
        self.push(Block::Sent {
            labels: one_line(&labels.join(", "), TITLE_CHARS),
            text,
        });
    }

    fn agent(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::TextDelta { text, parent: None } => self.deltas.push_str(text),
            AgentEvent::Message { text, parent: None } => {
                // The message is the whole text its deltas streamed.
                self.deltas.clear();
                self.text(text);
            }
            AgentEvent::ToolStarted {
                id,
                name,
                input,
                parent,
            } => {
                self.flush_deltas();
                let at = self.push(Block::Tool(Tool {
                    did: Did::of(name, input),
                    subagent: parent.is_some(),
                    result: None,
                }));
                self.open.insert(id.clone(), at);
            }
            AgentEvent::ToolFinished { id, ok, output, .. } => {
                if let Some((turn, block)) = self.open.remove(id)
                    && let Some(Block::Tool(tool)) = self.turns[turn].get_mut(block)
                {
                    tool.result = Some((*ok, output.clone()));
                }
            }
            AgentEvent::FileChanged { path, kind } => self.file_changed(path, kind),
            AgentEvent::Todo { items } => {
                self.flush_deltas();
                self.push(Block::Todo(items.clone()));
            }
            // A stall is a warning the agent may yet answer, not why it ended.
            AgentEvent::Error {
                category: ErrorCategory::Stalled,
                ..
            } => {}
            AgentEvent::Error {
                category, message, ..
            } => {
                self.flush_deltas();
                self.push(Block::Line(format!(
                    "Error, {}: {}",
                    category_words(*category),
                    one_line(message, ERROR_CHARS)
                )));
            }
            AgentEvent::Result {
                text: Some(text),
                is_error,
                ..
            } => {
                self.flush_deltas();
                if *is_error {
                    self.push(Block::Line(format!(
                        "It ended with an error: {}",
                        one_line(text, ERROR_CHARS)
                    )));
                } else if !self.said_last(text) {
                    self.text(text);
                }
            }
            _ => {}
        }
    }

    /// A CLI that reports a change after its edit tool names the same file: the change
    /// only tells whether the tool created or deleted it.
    fn file_changed(&mut self, path: &str, kind: &str) {
        self.flush_deltas();
        let path = one_line(path, PATH_CHARS);
        let named = self.turns.last_mut().and_then(|turn| {
            turn.iter_mut().rev().find_map(|block| match block {
                Block::Tool(tool) if tool.names(&path) => Some(tool),
                _ => None,
            })
        });
        match named {
            Some(tool) => tool.changed(kind),
            None => {
                self.push(Block::Tool(Tool {
                    did: Did::Changed(vec![(path, kind.to_owned())]),
                    subagent: false,
                    result: None,
                }));
            }
        }
    }

    fn text(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        self.start_turn();
        self.push(Block::Text(text.to_owned()));
    }

    fn flush_deltas(&mut self) {
        if !self.deltas.is_empty() {
            let text = std::mem::take(&mut self.deltas);
            self.text(&text);
        }
    }

    fn said_last(&self, text: &str) -> bool {
        self.turns
            .last()
            .and_then(|turn| turn.last())
            .is_some_and(|block| matches!(block, Block::Text(said) if said.trim() == text.trim()))
    }

    fn start_turn(&mut self) {
        if self.turns.last().is_none_or(|turn| !turn.is_empty()) {
            self.turns.push(Vec::new());
        }
    }

    /// Where the block went: its turn and its place in it.
    fn push(&mut self, block: Block) -> (usize, usize) {
        match self.turns.last_mut() {
            Some(turn) => turn.push(block),
            None => self.turns.push(vec![block]),
        }
        let turn = self.turns.len() - 1;
        (turn, self.turns[turn].len() - 1)
    }
}

enum Block {
    Text(String),
    Sent { labels: String, text: String },
    Tool(Tool),
    Line(String),
    Todo(Vec<TodoItem>),
}

impl Block {
    /// The block and the blank line after it.
    fn render(&self) -> String {
        let mut out = String::new();
        match self {
            Self::Text(text) => {
                out.push_str(&clean(text));
                out.push('\n');
            }
            Self::Sent { labels, text } => {
                let _ = writeln!(out, "Sent to the agent ({labels}):\n");
                fence(&mut out, &clean(text));
            }
            Self::Tool(tool) => tool.render(&mut out),
            Self::Line(line) => {
                out.push_str(line);
                out.push('\n');
            }
            Self::Todo(items) => {
                out.push_str("To-do list:\n\n");
                for item in items {
                    let mark = match item.status {
                        TodoStatus::Completed => "[x]",
                        TodoStatus::InProgress => "[~]",
                        TodoStatus::Pending => "[ ]",
                    };
                    let _ = writeln!(out, "- {mark} {}", one_line(&item.text, PATH_CHARS));
                }
            }
        }
        out.push('\n');
        out
    }
}

struct Tool {
    did: Did,
    /// Called by the agent's subagent rather than the agent itself.
    subagent: bool,
    /// Whether it worked, and its output. `None` while it never finished.
    result: Option<(bool, Option<String>)>,
}

enum Did {
    Ran(String),
    Read(String),
    Edited {
        path: String,
        verb: &'static str,
        /// Lines added and removed, when the tool's input shows them.
        lines: Option<(usize, usize)>,
    },
    /// Files a CLI changed in one step, each with its kind, such as `add` or `delete`.
    Changed(Vec<(String, String)>),
    Used(String),
}

impl Did {
    fn of(name: &str, input: &Value) -> Self {
        let changes: Vec<(String, String)> = input
            .get("changes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|change| {
                let path = change.get("path").and_then(Value::as_str)?;
                let kind = change.get("kind").and_then(Value::as_str).unwrap_or("");
                Some((one_line(path, PATH_CHARS), kind.to_owned()))
            })
            .collect();
        if !changes.is_empty() {
            return Self::Changed(changes);
        }
        match Step::of(name, input) {
            Step::Command(command) => Self::Ran(command),
            Step::Read(path) => Self::Read(path),
            Step::Edit(path) => Self::Edited {
                path,
                verb: "Edited",
                lines: edit_lines(input),
            },
            Step::Other(what) => Self::Used(what),
        }
    }
}

impl Tool {
    fn names(&self, path: &str) -> bool {
        match &self.did {
            Did::Edited { path: own, .. } => same_file(own, path),
            Did::Changed(changes) => changes.iter().any(|(own, _)| same_file(own, path)),
            _ => false,
        }
    }

    fn changed(&mut self, kind: &str) {
        if let Did::Edited { verb, .. } = &mut self.did
            && let Some(said) = change_verb(kind)
        {
            *verb = said;
        }
    }

    fn render(&self, out: &mut String) {
        let (ok, output) = match &self.result {
            Some((ok, output)) => (Some(*ok), output.as_deref().map(clean)),
            None => (None, None),
        };
        let mut output = output.filter(|text| !text.is_empty());
        let mut failed = ok == Some(false);
        let line = match &self.did {
            Did::Ran(command) => {
                let exit = output
                    .as_deref()
                    .and_then(exit_line)
                    .map(|(code, rest)| (code, rest.to_owned()));
                let code = exit.as_ref().map(|(code, _)| *code);
                if let Some((_, rest)) = exit {
                    output = Some(clean(&rest)).filter(|rest| !rest.is_empty());
                }
                let result = match (ok, code) {
                    (_, Some(code)) => format!("exit {code}"),
                    (Some(true), None) => "exit 0".to_owned(),
                    (Some(false), None) => "failed".to_owned(),
                    (None, None) => "no result before it stopped".to_owned(),
                };
                // The exit code already says it failed.
                failed = false;
                format!("Ran {} → {result}", code_span(command))
            }
            Did::Read(path) => format!("Read {path}"),
            Did::Edited { path, verb, lines } => match lines {
                Some((added, removed)) => format!("{verb} {path} (+{added} −{removed})"),
                None => format!("{verb} {path}"),
            },
            Did::Changed(changes) => changes
                .iter()
                .map(|(path, kind)| format!("{} {path}", change_verb(kind).unwrap_or("Edited")))
                .collect::<Vec<_>>()
                .join(", "),
            Did::Used(what) => format!("Used {what}"),
        };
        let main_agent = if self.subagent { "↳ " } else { "" };
        let tail = if failed { " → failed" } else { "" };
        let _ = writeln!(out, "{main_agent}{line}{tail}");
        if let Some(output) = output {
            out.push('\n');
            fence(out, &cut_output(&output));
        }
    }
}

fn change_verb(kind: &str) -> Option<&'static str> {
    match kind.to_ascii_lowercase().as_str() {
        "add" | "added" | "create" | "created" => Some("Created"),
        "delete" | "deleted" | "remove" | "removed" => Some("Deleted"),
        "update" | "updated" | "modify" | "modified" | "edit" => Some("Edited"),
        _ => None,
    }
}

/// One CLI gives a path from the workdir and another the absolute one.
fn same_file(a: &str, b: &str) -> bool {
    a == b || a.ends_with(&format!("/{b}")) || b.ends_with(&format!("/{a}"))
}

/// Lines added and removed, read from the edit's own input: one replaced text, several, a whole
/// file written, or a patch.
fn edit_lines(input: &Value) -> Option<(usize, usize)> {
    if let Some(edits) = input.get("edits").and_then(Value::as_array) {
        let mut total = None;
        for (added, removed) in edits.iter().filter_map(replaced) {
            let (all_added, all_removed) = total.unwrap_or((0, 0));
            total = Some((all_added + added, all_removed + removed));
        }
        return total;
    }
    if let Some(lines) = replaced(input) {
        return Some(lines);
    }
    let text = |key: &str| input.get(key).and_then(Value::as_str);
    if let Some(content) = text("content") {
        return Some((content.lines().count(), 0));
    }
    let patch = text("patch").or_else(|| text("diff"))?;
    let (mut added, mut removed) = (0, 0);
    for line in patch.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    Some((added, removed))
}

fn replaced(edit: &Value) -> Option<(usize, usize)> {
    let text = |keys: [&str; 2]| {
        keys.into_iter()
            .find_map(|key| edit.get(key).and_then(Value::as_str))
    };
    let old = text(["old_string", "old_str"])?;
    let new = text(["new_string", "new_str"])?;
    Some(changed_lines(old, new))
}

/// Lines added and removed once the lines both sides share at the start and at the end are
/// set aside.
fn changed_lines(old: &str, new: &str) -> (usize, usize) {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let start = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let end = old[start..]
        .iter()
        .rev()
        .zip(new[start..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (new.len() - start - end, old.len() - start - end)
}

/// `Exit code 1` on the first line, as Claude Code reports a failed command, and the rest.
fn exit_line(output: &str) -> Option<(i32, &str)> {
    let (first, rest) = output.split_once('\n').unwrap_or((output, ""));
    let code = first
        .trim()
        .trim_start_matches("Error: ")
        .strip_prefix("Exit code ")?
        .trim()
        .parse()
        .ok()?;
    Some((code, rest))
}

/// Half the lines from the start and half from the end, which is where a failure shows.
fn cut_output(text: &str) -> String {
    let lines: Vec<String> = text.lines().map(cut_line_chars).collect();
    if lines.len() <= OUTPUT_LINES {
        return lines.join("\n");
    }
    let head = OUTPUT_LINES / 2;
    let tail = OUTPUT_LINES - head;
    let cut = lines.len() - OUTPUT_LINES;
    let mut kept = lines[..head].to_vec();
    kept.push(format!(
        "… {} cut (full output in the raw log)",
        count(cut, "line")
    ));
    kept.extend_from_slice(&lines[lines.len() - tail..]);
    kept.join("\n")
}

fn cut_line_chars(line: &str) -> String {
    if line.chars().count() <= LINE_CHARS {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(LINE_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// The head, then the turns that fit, newest kept. When even the newest turn is too big alone,
/// it keeps only its latest steps.
fn assemble(mut out: String, mut turns: Vec<Vec<String>>) -> String {
    let bytes = |blocks: &[String]| blocks.iter().map(String::len).sum::<usize>();
    let total = out.len() + turns.iter().map(|turn| bytes(turn)).sum::<usize>();
    if total > MAX_BYTES && !turns.is_empty() {
        let mut room = MAX_BYTES.saturating_sub(out.len() + CUT_LINE_BYTES);
        let mut kept = 0;
        for turn in turns.iter().rev() {
            let size = bytes(turn);
            if size > room {
                break;
            }
            room -= size;
            kept += 1;
        }
        let mut steps = 0;
        if kept == 0
            && let Some(last) = turns.last_mut()
        {
            let mut fit = 0;
            let mut used = 0;
            for block in last.iter().rev() {
                if used + block.len() > room {
                    break;
                }
                used += block.len();
                fit += 1;
            }
            steps = last.len() - fit.max(1);
            last.drain(..steps);
            kept = 1;
        }
        let cut = turns.len() - kept;
        turns.drain(..cut);
        let _ = writeln!(out, "{}\n", cut_note(cut, steps));
    }
    for block in turns.into_iter().flatten() {
        out.push_str(&block);
    }
    out
}

fn cut_note(turns: usize, steps: usize) -> String {
    let what = match (turns, steps) {
        (_, 0) => format!("{} earlier {}", turns, plural(turns, "turn")),
        (0, _) => format!("the first {} of this turn", count(steps, "step")),
        _ => format!(
            "{} earlier {} and the first {} of the next",
            turns,
            plural(turns, "turn"),
            count(steps, "step")
        ),
    };
    format!(
        "… {what} cut, to keep this file under about {} KB (the raw log has everything)",
        MAX_BYTES / 1024
    )
}

fn count(n: usize, noun: &str) -> String {
    format!("{n} {}", plural(n, noun))
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        noun.to_owned()
    } else {
        format!("{noun}s")
    }
}

/// CLI text without terminal escapes, and without blank lines around it.
fn clean(text: &str) -> String {
    sanitize_for_terminal(text)
        .trim_end()
        .trim_start_matches('\n')
        .to_owned()
}

/// A fence longer than any run of backticks inside, so the text can't close it early.
fn fence(out: &mut String, text: &str) {
    let ticks = "`".repeat(longest_backticks(text).max(2) + 1);
    let _ = writeln!(out, "{ticks}\n{text}\n{ticks}");
}

fn code_span(text: &str) -> String {
    let ticks = "`".repeat(longest_backticks(text) + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{text}{pad}{ticks}")
}

fn longest_backticks(text: &str) -> usize {
    let (mut longest, mut run) = (0, 0);
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

#[cfg(test)]
mod tests;
