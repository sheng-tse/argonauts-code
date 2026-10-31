//! A reviewer's or a sign-off's finished row. It says what the agent read, the checks' result
//! it was given, the change it judged and its verdict, since a count of tool uses says nothing
//! about whether a change passed:
//!
//! ```text
//! ● review:per-key · Haiku 4.5 on Claude Code
//!   ⎿  Read 3 files · used the checks' result: 12 passed
//!      Accepted. Per-key buckets and the refill clamp are right.
//!
//! ● review:per-key · GPT 6 Luna on Codex
//!   ⎿  Accepted · used the checks' result: 5 passed
//!
//! ● signoff:per-key · Haiku 5.5 on Claude Code
//!   ⎿  Accepted · read in the task's worktree, the change holds 2 files +17 −4: bucket.py,
//!      middleware.py · tests: 5 passed
//! ```
//!
//! A sign-off about no one task works in a folder that holds the change it reads: the task's
//! worktree when one task changed, else one with every task merged in.

use argonauts_core::runs::events::session::{InputPart, InputPartKind, SessionEnd};
use std::collections::BTreeMap;

use argonauts_core::RunId;
use argonauts_core::base::names::RoleName;
use serde_json::Value;

use crate::cells::cell::{CollapsedGroup, Tone, ToolCall, ToolItem, ToolState, ToolTitle};

/// What a judging session was given to judge, from the parts of its prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Given {
    /// The checks' results in a few words, such as `12 passed`.
    pub checks: Option<String>,
    /// The files of the change it read, in the order the diff has them.
    pub change: Vec<String>,
    /// The lines the change adds and takes away.
    pub lines: (u32, u32),
    /// How many tasks' changes it read together, each under its name. 0 for one task's alone.
    pub tasks: usize,
}

impl Given {
    /// Adds what one delivery's parts gave. A session's first delivery holds its task.
    pub fn add(&mut self, parts: &[InputPart]) {
        for part in parts {
            match part.kind {
                InputPartKind::CheckOutput if self.checks.is_none() => {
                    self.checks = checks_result(&part.text);
                }
                InputPartKind::Evidence if self.change.is_empty() => {
                    self.change = changed_files(&part.text);
                    self.lines = changed_lines(&part.text);
                    self.tasks = part
                        .text
                        .lines()
                        .filter(|line| line.starts_with("# "))
                        .count();
                }
                _ => {}
            }
        }
    }
}

/// The row a reviewer or a sign-off ends with, or `None` for any other agent, or one that gave
/// no verdict, which keeps the usual summary.
pub(super) fn row(
    role: Option<RoleName>,
    given: &Given,
    whole: &ToolCall,
    end: &SessionEnd,
) -> Option<ToolCall> {
    let role = role.filter(|role| matches!(role, RoleName::Reviewer | RoleName::Judge))?;
    let (accepted, summary) = verdict(end.structured.as_ref()?)?;
    let word = if accepted { "Accepted" } else { "Sent back" };
    let mut body = Vec::new();
    if role == RoleName::Judge {
        let mut parts = vec![word.to_owned()];
        if !given.change.is_empty() {
            let count = given.change.len();
            let noun = if count == 1 { "file" } else { "files" };
            let (added, removed) = given.lines;
            let place = if given.tasks > 1 {
                "read in a worktree with every task merged in"
            } else {
                "read in the task's worktree"
            };
            parts.push(format!(
                "{place}, the change holds {count} {noun} +{added} \u{2212}{removed}: {}",
                given.change.join(", ")
            ));
        }
        if let Some(checks) = &given.checks {
            parts.push(format!("tests: {checks}"));
        }
        body.push(dim(parts.join(" · ")));
    } else {
        let mut read = vec![tools_used(whole)];
        if let Some(checks) = &given.checks {
            read.push(format!("used the checks' result: {checks}"));
        }
        let read: Vec<String> = read.into_iter().filter(|part| !part.is_empty()).collect();
        match summary.as_deref().map(str::trim) {
            Some(summary) if !summary.is_empty() => {
                if !read.is_empty() {
                    body.push(dim(read.join(" · ")));
                }
                body.push(ToolItem::Text {
                    text: format!("{word}. {summary}"),
                    tone: Tone::Normal,
                });
            }
            // With nothing more to say, the verdict comes first on the line of what it read.
            _ => body.push(dim(std::iter::once(word.to_owned())
                .chain(read)
                .collect::<Vec<_>>()
                .join(" · "))),
        }
    }
    let mut row = whole.summarized(None);
    row.state = ToolState::Done;
    row.body = body;
    Some(row)
}

/// Verdicts given so far under each label by each agent, so a later round of the same review
/// reads as `review:per-key · round 2`, the round after the label, while two reviewers of one
/// round, who share the label, each read as themselves.
#[derive(Debug, Default)]
pub(super) struct Rounds {
    seen: BTreeMap<(RunId, String, String), u32>,
}

impl Rounds {
    /// Counts this verdict's round and names it on its row from the second on. A label the run
    /// made unique with `-2` for a later round counts under the first.
    pub fn count(&mut self, run: &RunId, row: &mut ToolCall) {
        let ToolTitle::Agent {
            name, role, round, ..
        } = &mut row.title
        else {
            return;
        };
        let key = |label: &str| (run.clone(), label.to_owned(), role.clone());
        let base = match name.rsplit_once('-') {
            Some((stem, n)) if n.parse::<u32>().is_ok() && self.seen.contains_key(&key(stem)) => {
                stem.to_owned()
            }
            _ => name.clone(),
        };
        let seen = self.seen.entry(key(&base)).or_default();
        *seen += 1;
        if *seen > 1 {
            *name = base;
            *round = u16::try_from(*seen).unwrap_or(u16::MAX);
        }
    }
}

fn dim(text: String) -> ToolItem {
    ToolItem::Text {
        text,
        tone: Tone::Dim,
    }
}

/// `Read 3 files, searched for 1 pattern`: the calls it made that ran. A call its CLI refused
/// ran nothing, so it isn't counted.
fn tools_used(whole: &ToolCall) -> String {
    let mut group = CollapsedGroup::new();
    for item in &whole.body {
        let calls: &[ToolCall] = match item {
            ToolItem::Call(call) => std::slice::from_ref(call),
            ToolItem::Group(group) => &group.calls,
            _ => continue,
        };
        for call in calls.iter().filter(|call| call.state == ToolState::Done) {
            group.push(call.clone());
        }
    }
    group.summary()
}

/// Accepted or not, and its summary, from either answer shape a review comes in: `accept` and
/// `summary`, or a review step's `verdict`.
fn verdict(answer: &Value) -> Option<(bool, Option<String>)> {
    let accepted = match answer.get("accept").and_then(Value::as_bool) {
        Some(accepted) => accepted,
        None => answer.get("verdict")?.as_str()? == "approve",
    };
    let summary = answer
        .get("summary")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some((accepted, summary))
}

/// `12 passed` from the checks' lines, such as `tests (python3 -m unittest -q) passed: 12
/// passed in 0.03s`: each check's counts without its time, joined. A check that gave no
/// counts reads as its result, `passed` or `failed`.
fn checks_result(text: &str) -> Option<String> {
    let results: Vec<String> = text
        .lines()
        .filter_map(|line| {
            let line = line.trim_end();
            for bare in ["passed", "failed"] {
                if line.ends_with(&format!(") {bare}")) {
                    return Some(bare.to_owned());
                }
            }
            let (_, result) = line
                .split_once(") passed: ")
                .or_else(|| line.split_once(") failed: "))
                .or_else(|| line.split_once(" passed: "))
                .or_else(|| line.split_once(" failed: "))?;
            let result = result.split(" in ").next().unwrap_or(result).trim();
            (!result.is_empty()).then(|| result.to_owned())
        })
        .collect();
    (!results.is_empty()).then(|| results.join(", "))
}

/// The lines a unified diff adds and takes away, its file headers left out.
fn changed_lines(diff: &str) -> (u32, u32) {
    let mut lines = (0, 0);
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            lines.0 += 1;
        } else if line.starts_with('-') {
            lines.1 += 1;
        }
    }
    lines
}

/// The paths a unified diff changes, each once.
fn changed_files(diff: &str) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    for line in diff.lines() {
        let Some(rest) = line.strip_prefix("diff --git a/") else {
            continue;
        };
        let path = rest.split_once(" b/").map_or(rest, |(_, after)| after);
        if !files.iter().any(|file| file == path) {
            files.push(path.to_owned());
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checks_result_reads_as_its_counts() {
        let text = "Argonauts Code ran these after the build, in the task's worktree:\n\
                    tests (python3 -m unittest -q) passed: 12 passed in 0.03s";
        assert_eq!(checks_result(text).as_deref(), Some("12 passed"));
        assert_eq!(checks_result("nothing here"), None);
        let bare = "tests (node --test) passed";
        assert_eq!(checks_result(bare).as_deref(), Some("passed"));
    }

    #[test]
    fn a_second_round_reads_as_one() {
        let run = RunId::new("run-01").unwrap();
        let row = |label: &str| ToolCall::agent(label, None, "", "");
        let mut rounds = Rounds::default();
        let mut counted = |mut row: ToolCall| {
            rounds.count(&run, &mut row);
            row.title.plain()
        };
        assert_eq!(counted(row("review:per-key")), "review:per-key");
        assert_eq!(counted(row("review:per-key")), "review:per-key · round 2");
        assert_eq!(counted(row("review:per-key-2")), "review:per-key · round 3");
        assert_eq!(
            counted(row("review:fix-2")),
            "review:fix-2",
            "a name of its own"
        );
        let beside = ToolCall::agent("review:per-key", None, "GPT 5.6 Luna on Codex", "");
        assert_eq!(
            counted(beside),
            "review:per-key · GPT 5.6 Luna on Codex",
            "a second reviewer of the same task reads as its own first round"
        );
    }

    /// The round reads after the label and its task, outside the bold label.
    #[test]
    fn the_round_reads_after_the_label() {
        use crate::base::draw::render::{Clock, RenderCtx};
        use crate::base::theme::{Glyphs, Role, Theme};
        use crate::cells::cell::{HistoryCell as _, ModelShade};
        use ratatui::style::Modifier;

        let run = RunId::new("run-01").unwrap();
        let shade = ModelShade {
            cli: Role::CliCodex,
            place: 0,
            logo: argonauts_core::look::logos::logo_for("codex", Some("gpt-6-luna")),
            model: Some("GPT 6 Luna".to_owned()),
            on: "Codex".to_owned(),
        };
        let mut rounds = Rounds::default();
        let mut rows = Vec::new();
        for _ in 0..2 {
            let mut row = ToolCall::agent("review:due-flag", None, "", "review the flag")
                .with_model(shade.clone());
            row.state = ToolState::Done;
            rounds.count(&run, &mut row);
            rows.push(row);
        }
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(100, &theme, Clock::still(std::time::Instant::now()));
        let header = rows[1].lines(&ctx).remove(0);
        assert_eq!(
            header.to_string(),
            "● review:due-flag(review the flag) · round 2 · GPT 6 Luna on Codex"
        );
        let span = |text: &str| {
            header
                .spans
                .iter()
                .find(|span| span.content == text)
                .unwrap_or_else(|| panic!("no {text:?} in {header:?}"))
        };
        assert!(
            span("review:due-flag")
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        let round = span(" · round 2");
        assert!(!round.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(round.style.fg, theme.style(Role::Dim).fg);
    }

    #[test]
    fn the_change_names_each_file_once() {
        let diff = "diff --git a/ratelimit/bucket.py b/ratelimit/bucket.py\n+x\n\
                    diff --git a/tests/test_middleware.py b/tests/test_middleware.py\n";
        assert_eq!(
            changed_files(diff),
            ["ratelimit/bucket.py", "tests/test_middleware.py"]
        );
    }
}
