//! The checks as they stood before the run changed anything. They run once on the code the run
//! started from, before planning, so the planner, the workers and the reviewers hear what
//! already fails, and a task answers only for what it broke itself. The final check after the
//! merge still runs everything.

use std::collections::BTreeMap;
use std::path::PathBuf;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::events::task::{CheckKind, CheckOutcome};
use argonauts_core::limits::START_CHECKS;
use argonauts_core::settings::protocol::CheckCommand;
use argonauts_core::{Note, NoteLevel, TaskId};

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::state::RunState;
use crate::task::checks;
use crate::task::worktree::TaskTree;

/// What the part that says what already fails is labelled in a prompt.
pub const ALREADY_FAILING: &str = "already failing";
/// The most of a failing command's output a prompt carries, in characters, from its end.
const TAIL_CHARS: usize = 1_500;

/// Runs `checks.run` on the starting code, once in a run. With no commands, or once they have
/// run, it does nothing.
pub async fn ensure(host: &Host) -> EngineResult<()> {
    let commands = &host.settings().checks.run;
    if commands.is_empty() {
        return Ok(());
    }
    let mut unknown = host.shared().start_checks.lock().await;
    if *unknown || host.read(|state| !state.tasks.at_start.is_empty()) {
        return Ok(());
    }
    let Some((scope, copy)) = copy_of_the_start(host).await? else {
        *unknown = true;
        return Ok(());
    };
    for command in commands {
        let request = checks::request(host, &scope, command, &copy, false)?;
        host.check(request).await?;
    }
    if let Some(line) = host.read(said) {
        warn(host, NoteLevel::Info, &line)?;
    }
    Ok(())
}

/// How `command` did on the starting code. One the start didn't run, such as a suite only the
/// final check runs, is run there now, so a failure can be told from one the run caused.
/// `None` when the starting code can't be checked.
pub async fn outcome_of(host: &Host, command: &CheckCommand) -> EngineResult<Option<CheckOutcome>> {
    ensure(host).await?;
    let known = |state: &RunState| {
        let ran = |check: &&CheckOutcome| {
            check.name == command.name && check.command.as_ref() == Some(&command.command)
        };
        state.tasks.at_start.iter().find(ran).cloned()
    };
    let mut unknown = host.shared().start_checks.lock().await;
    if let Some(known) = host.read(known) {
        return Ok(Some(known));
    }
    if *unknown {
        return Ok(None);
    }
    let Some((scope, copy)) = copy_of_the_start(host).await? else {
        *unknown = true;
        return Ok(None);
    };
    let request = checks::request(host, &scope, command, &copy, false)?;
    Ok(Some(host.check(request).await?))
}

/// The code the run started from in a clean worktree, set up for checks, and the name its
/// checks go by. A worktree of its own, so nothing a check leaves behind reaches the run's
/// base. `None` when the run has no starting commit or the setup fails there, which one line
/// says.
async fn copy_of_the_start(host: &Host) -> EngineResult<Option<(TaskId, PathBuf)>> {
    let base = host.read(|state| {
        let started = state.run.started.as_ref();
        started.map(|started| started.base_commit.clone())
    });
    let Some(base) = base else {
        return Ok(None);
    };
    let scope = TaskId::new(START_CHECKS).map_err(|err| EngineError::Refused(err.to_string()))?;
    let tree = TaskTree {
        path: host.repo().worktrees.join(START_CHECKS),
        branch: "the starting code".to_owned(),
        base: base.clone(),
    };
    let copy = checks::clean_copy_at(host, &tree, &base).await?;
    if let Some(output) = checks::setup(host, &scope, &copy, &tree.branch).await? {
        let line = "checks.setup failed on the starting code, so what already fails there isn't \
                    known and every task answers for every check";
        warn(host, NoteLevel::Warn, &format!("{line}: {output}"))?;
        return Ok(None);
    }
    Ok(Some((scope, copy)))
}

fn warn(host: &Host, level: NoteLevel, text: &str) -> EngineResult<()> {
    let note = Note {
        level,
        text: text.to_owned(),
    };
    host.record(None, None, RunEventKind::Note(note))?;
    Ok(())
}

/// The commands that failed on the starting code, by name. The first run under a name is the
/// one a task's checks go by, since `checks.run` ran before anything else did.
pub fn failing(state: &RunState) -> BTreeMap<&str, &CheckOutcome> {
    let mut by_name = BTreeMap::new();
    let commands = state
        .tasks
        .at_start
        .iter()
        .filter(|check| check.kind == CheckKind::Command && check.command.is_some());
    for check in commands {
        by_name.entry(check.name.as_str()).or_insert(check);
    }
    by_name.retain(|_, check| !check.passed);
    by_name
}

/// `tests (npm test): 15 passed, 1 failed`, each failing command in one line.
fn lines(state: &RunState) -> Vec<String> {
    failing(state)
        .values()
        .map(|check| {
            let ran = check
                .command
                .as_ref()
                .map(|command| format!(" ({})", command.join(" ")))
                .unwrap_or_default();
            format!("{}{ran}: {}", check.name, check.summary.trim())
        })
        .collect()
}

/// The transcript's line once the starting code was checked and something failed.
fn said(state: &RunState) -> Option<String> {
    let lines = lines(state);
    (!lines.is_empty()).then(|| {
        format!(
            "Already failing before the run changed anything: {}. Each task answers only for \
             what it breaks, and the final check still runs everything.",
            lines.join("; ")
        )
    })
}

/// What the planner and the workers read about the starting code when a check already failed
/// there: each command with its result and the end of its output, and what that means for them.
pub fn note(state: &RunState) -> Option<String> {
    let failing = failing(state);
    if failing.is_empty() {
        return None;
    }
    let mut text = String::from(
        "Already failing on the code this run started from, before anything changed:\n",
    );
    for (line, check) in lines(state).iter().zip(failing.values()) {
        text.push_str(&format!("- {line}\n"));
        let tail = check.output_tail.trim();
        if !tail.is_empty() {
            let from = tail
                .char_indices()
                .rev()
                .nth(TAIL_CHARS.saturating_sub(1))
                .map_or(0, |(at, _)| at);
            text.push_str(&format!("```\n{}\n```\n", &tail[from..]));
        }
    }
    text.push_str(
        "Nothing in this run broke these. When the request needs one fixed, the plan gives that \
         fix to one task and never rules it out. No other task is failed for them, and none \
         should work around them.",
    );
    Some(text)
}

/// [`note`] as a part of a prompt that writes a plan or its tasks, with the starting code
/// checked first when it hasn't been, so no step plans without knowing what already fails.
pub async fn part(host: &Host) -> EngineResult<Option<InputPart>> {
    ensure(host).await?;
    let part = |text| crate::task::support::part(InputPartKind::Notes, ALREADY_FAILING, text, None);
    Ok(host.read(note).map(part))
}

/// The same for a reviewer, as lines beside the change's own check results.
pub fn review_lines(state: &RunState) -> Vec<String> {
    lines(state)
        .into_iter()
        .map(|line| {
            format!(
                "at the start, before this run: {line}. It failed already, so the change under \
                 review isn't answerable for it"
            )
        })
        .collect()
}

/// A command that failed on the starting code too counts as passed for a task that broke
/// nothing more, with why in its summary: no more tests fail than did, or, where the command
/// counts nothing, it prints exactly what it printed. The exit code and output stay as they
/// ran. A task that makes more fail, or fails in a way that can't be compared, still fails.
pub fn judge(mut outcome: CheckOutcome, at_start: Option<&CheckOutcome>) -> CheckOutcome {
    let Some(at_start) = at_start.filter(|at_start| !at_start.passed && !outcome.passed) else {
        return outcome;
    };
    let same = match (failures(&at_start.summary), failures(&outcome.summary)) {
        (Some(before), Some(now)) => now <= before,
        // With no counts, only the very same output says it's the very same failure.
        (None, None) => {
            let said = outcome.output_tail.trim();
            !said.is_empty()
                && said == at_start.output_tail.trim()
                && at_start.summary.trim() == outcome.summary.trim()
        }
        _ => false,
    };
    if same {
        outcome.passed = true;
        outcome.summary = format!("{}, as at the start", outcome.summary.trim());
    } else {
        outcome.summary = format!(
            "{} (at the start: {})",
            outcome.summary.trim(),
            at_start.summary.trim()
        );
    }
    outcome
}

/// How many tests a summary such as `15 passed, 1 failed` says failed or errored. `None`
/// when it gives no count.
pub(crate) fn failures(summary: &str) -> Option<u64> {
    let mut found = None;
    for piece in summary.split([',', ';']) {
        let mut words = piece.split_whitespace();
        let (Some(count), Some(what)) = (words.next(), words.next()) else {
            continue;
        };
        let Ok(count) = count.parse::<u64>() else {
            continue;
        };
        let what = what.to_lowercase();
        if what.starts_with("fail") || what.starts_with("error") {
            found = Some(found.unwrap_or(0) + count);
        } else if what.starts_with("pass") && found.is_none() {
            // A summary that counts what passed and names no failure has none.
            found = Some(0);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(passed: bool, summary: &str) -> CheckOutcome {
        CheckOutcome {
            name: "tests".to_owned(),
            kind: CheckKind::Command,
            passed,
            command: Some(vec!["npm".to_owned(), "test".to_owned()]),
            exit_code: Some(i32::from(!passed)),
            duration_ms: 5,
            summary: summary.to_owned(),
            output_tail: String::new(),
            violations: Vec::new(),
            sandboxed: false,
            skipped: false,
        }
    }

    #[test]
    fn a_summary_says_how_many_failed() {
        assert_eq!(failures("15 passed, 1 failed"), Some(1));
        assert_eq!(failures("41 passed; 2 failed; 1 ignored"), Some(2));
        assert_eq!(failures("3 failed, 1 errors, 9 passed"), Some(4));
        assert_eq!(failures("9 passed"), Some(0));
        assert_eq!(failures("failed with exit code 1"), None);
        assert_eq!(failures("timed out after 500ms"), None);
    }

    #[test]
    fn a_task_answers_only_for_what_it_broke() {
        let at_start = outcome(false, "15 passed, 1 failed");
        // The same failure as before the run isn't the task's.
        let same = judge(outcome(false, "16 passed, 1 failed"), Some(&at_start));
        assert!(same.passed);
        assert_eq!(same.summary, "16 passed, 1 failed, as at the start");
        assert_eq!(same.exit_code, Some(1));
        // One more failing is.
        let broke = judge(outcome(false, "14 passed, 2 failed"), Some(&at_start));
        assert!(!broke.passed);
        assert_eq!(
            broke.summary,
            "14 passed, 2 failed (at the start: 15 passed, 1 failed)"
        );
        // So is a failure that gives nothing to compare, such as a build that no longer runs.
        let unknown = judge(outcome(false, "failed with exit code 1"), Some(&at_start));
        assert!(!unknown.passed);
        // A command that passed at the start, or passes now, is judged as it ran.
        let passing = outcome(true, "16 passed");
        assert!(!judge(outcome(false, "15 passed, 1 failed"), Some(&passing)).passed);
        assert!(!judge(outcome(false, "15 passed, 1 failed"), None).passed);
        assert!(judge(outcome(true, "17 passed"), Some(&at_start)).passed);
        // Two failures with no counts are the same only when they print the same.
        let said = |text: &str| CheckOutcome {
            output_tail: text.to_owned(),
            ..outcome(false, "failed with exit code 1")
        };
        let plain = said("lint: 2 problems in docs/");
        assert!(judge(said("lint: 2 problems in docs/"), Some(&plain)).passed);
        assert!(!judge(said("lint: 3 problems in docs/"), Some(&plain)).passed);
        assert!(!judge(said(""), Some(&said(""))).passed);
    }
}
