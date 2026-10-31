//! A session stopped as stuck may have finished its work first: Qwen on OpenCode passed its
//! tests, then repeated one call until the watchdog stopped it, and its work was committed as
//! every session's is. The checks decide what happens to that work. When they pass, the task
//! goes on as if its worker had reported done; when they fail, the session failed as before.
//! With no command checks set nothing can tell, so you're asked, with keeping the work first.

use argonauts_core::AgentEvent;
use argonauts_core::TaskId;
use argonauts_core::runs::events::session::{EndReason, stuck_words};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use serde_json::Value;

use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::roles;
use crate::state::tasks::TaskEntry;
use crate::task::checks::{self, CheckVerdict};
use crate::task::support::{newest_worker_session, record_of, task_events};

/// What the checks made of a stopped session's work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// They pass, so the work goes on as done.
    Passed,
    /// They fail, or stopped, so the session failed.
    Failed,
    /// No command checks are set, so it's yours to say.
    Unchecked,
}

/// The checks on what a stuck session left on the task's branch, or `None` when it ended any
/// other way or nothing is committed there.
pub(crate) async fn check(
    host: &Host,
    task: &TaskId,
    end: &EndReason,
) -> EngineResult<Option<Verdict>> {
    if !matches!(end, EndReason::Stuck { .. }) || !has_work(&record_of(host, task)?) {
        return Ok(None);
    }
    if host.settings().checks.run.is_empty() {
        return Ok(Some(Verdict::Unchecked));
    }
    Ok(Some(match checks::run(host, task).await? {
        CheckVerdict::Passed => Verdict::Passed,
        CheckVerdict::Failed { .. } | CheckVerdict::Stopped { .. } => Verdict::Failed,
    }))
}

/// Why the task goes on after its checks passed on what its stuck session committed.
pub(crate) fn passed_reason(end: &EndReason) -> String {
    format!(
        "its work passes the checks, though its session was stopped: {}",
        stopped_words(end)
    )
}

/// Why you're asked about what a stuck session committed.
pub(crate) fn unchecked_reason(end: &EndReason) -> String {
    format!(
        "its session was stopped: {}, with its work committed on the task's branch and no \
         command checks set to tell whether it's done",
        stopped_words(end)
    )
}

fn stopped_words(end: &EndReason) -> String {
    match end {
        EndReason::Stuck { watchdog, calls } => {
            format!("it {}", stuck_words(*watchdog, calls.as_ref()))
        }
        _ => "it was stopped".to_owned(),
    }
}

/// What the task's stuck session did, as the stuck question says it first: `Qwen3 Coder 30B
/// (64k) on OpenCode repeated the same execute call 12 times after its tests passed`. `None`
/// unless the task's newest session was stopped as stuck.
pub(crate) fn what_it_did(host: &Host, task: &TaskId) -> Option<String> {
    let (worker, watchdog, calls) = host.read(|state| {
        let session = newest_worker_session(state, task)?;
        match &session.end.as_ref()?.reason {
            EndReason::Stuck { watchdog, calls } => {
                Some((session.started.worker.clone(), *watchdog, calls.clone()))
            }
            _ => None,
        }
    })?;
    let who = host.names().worker(&roles::runs(host, &worker));
    let passed = task_events(host, task)
        .ok()
        .and_then(|events| newest_test_run(&events))
        .is_some_and(|run| run.passed);
    let after = if passed {
        " after its tests passed"
    } else {
        ""
    };
    Some(format!(
        "{who} {}{after}",
        stuck_words(watchdog, calls.as_ref())
    ))
}

/// Some commit on the task's branch holds work.
fn has_work(record: &TaskEntry) -> bool {
    record.commits.iter().any(|commit| commit.sha.is_some())
}

/// What the stuck question's "Keep its work" says, when it offers it: the task's newest session
/// was stopped as stuck with work committed, and no command checks are set to judge it. It
/// names what the work changed and the agent's own last test, when it ran one.
pub(crate) async fn keep_detail(host: &Host, record: &TaskEntry) -> Option<String> {
    let stopped = host.read(|state| {
        newest_worker_session(state, &record.spec.id)
            .and_then(|session| session.end.as_ref())
            .is_some_and(|end| matches!(end.reason, EndReason::Stuck { .. }))
    });
    if !stopped || !has_work(record) || !host.settings().checks.run.is_empty() {
        return None;
    }
    let mut detail = Vec::new();
    detail.extend(changed(host, record).await);
    if let Some(test) = last_test(host, &record.spec.id) {
        detail.push(format!("its last test: {test}"));
    }
    Some(detail.join(" · "))
}

/// The run's own records, which never count as the task's work.
const OWN_RECORDS: &str = ".argonauts/";

/// The files named past this many are counted instead.
const FILES_NAMED: usize = 3;

/// What the task's branch changed, such as `slug.py +13 −1`.
async fn changed(host: &Host, record: &TaskEntry) -> Option<String> {
    let (base, branch) = (record.base.as_ref()?, record.branch.as_ref()?);
    let root = host.repo().root.clone();
    let files: Vec<(String, u32, u32)> = host
        .git()
        .diff_files(&root, base, branch)
        .await
        .ok()?
        .into_iter()
        .filter(|(path, _, _)| !path.starts_with(OWN_RECORDS))
        .collect();
    stat_words(&files)
}

fn stat_words(files: &[(String, u32, u32)]) -> Option<String> {
    match files.len() {
        0 => None,
        n if n <= FILES_NAMED => Some(
            files
                .iter()
                .map(|(path, added, removed)| format!("{path} +{added} −{removed}"))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        n => {
            let added: u32 = files.iter().map(|(_, added, _)| added).sum();
            let removed: u32 = files.iter().map(|(_, _, removed)| removed).sum();
            Some(format!("{n} files +{added} −{removed}"))
        }
    }
}

/// The newest test command the task's agents ran themselves, with how it ended, from the run's
/// journal: `PYTHONPATH=. python3 tests/test_slug.py (OK)`. A journal that can't be read has
/// none to name.
fn last_test(host: &Host, task: &TaskId) -> Option<String> {
    newest_test(&task_events(host, task).ok()?)
}

/// Commands longer than this are cut, since the line sits beside an option.
const COMMAND_CHARS: usize = 80;

fn newest_test(events: &[RunEvent]) -> Option<String> {
    newest_test_run(events).map(|run| run.words)
}

/// A test command an agent ran, with how it ended.
struct TestRun {
    /// `PYTHONPATH=. python3 tests/test_slug.py (OK)`.
    words: String,
    /// Its last line says it passed, and nothing failed.
    passed: bool,
}

fn newest_test_run(events: &[RunEvent]) -> Option<TestRun> {
    let mut tests: Vec<(String, String)> = Vec::new();
    let mut newest = None;
    for event in events {
        match &event.kind {
            RunEventKind::Agent(AgentEvent::ToolStarted { id, input, .. }) => {
                if let Some(command) = command_of(input).filter(|command| runs_tests(command)) {
                    tests.push((id.clone(), command));
                }
            }
            RunEventKind::Agent(AgentEvent::ToolFinished { id, ok, output, .. }) => {
                let Some(at) = tests.iter().position(|(started, _)| started == id) else {
                    continue;
                };
                let (_, command) = tests.remove(at);
                let last_line = output
                    .as_deref()
                    .and_then(|text| text.lines().rev().find(|line| !line.trim().is_empty()))
                    .map(str::trim);
                let ended = match (ok, last_line) {
                    (true, Some(line)) => line.to_owned(),
                    (true, None) => "ok".to_owned(),
                    (false, Some(line)) => format!("failed: {line}"),
                    (false, None) => "failed".to_owned(),
                };
                newest = Some(TestRun {
                    words: format!("{command} ({})", cut(&ended, COMMAND_CHARS)),
                    passed: *ok && last_line.is_some_and(reads_as_passed),
                });
            }
            _ => {}
        }
    }
    newest
}

/// A shell command as the agent ran it, without the `cd <its folder> &&` it starts with.
fn command_of(input: &Value) -> Option<String> {
    let command = match input.get("command").or_else(|| input.get("cmd"))? {
        Value::String(command) => command.clone(),
        Value::Array(words) => words
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    let mut command = command.trim();
    while let Some(rest) = command
        .strip_prefix("cd ")
        .and_then(|rest| rest.split_once("&&"))
        .map(|(_, rest)| rest.trim())
    {
        command = rest;
    }
    Some(cut(command, COMMAND_CHARS))
}

/// A test run's last line that says it passed and nothing failed: unittest's `OK`, pytest's
/// `3 passed in 0.12s`, cargo's `test result: ok.`.
fn reads_as_passed(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    let mut words = line.split(|c: char| !c.is_ascii_alphanumeric());
    !line.contains("fail")
        && !line.contains("error")
        && words.any(|word| word == "ok" || word == "passed")
}

fn runs_tests(command: &str) -> bool {
    command.to_ascii_lowercase().contains("test")
}

fn cut(text: &str, chars: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= chars {
        return line;
    }
    let kept: String = line.chars().take(chars - 1).collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use argonauts_core::runs::events::session::{CallCount, Watchdog};
    use serde_json::json;

    use super::*;

    fn agent(event: AgentEvent) -> RunEvent {
        crate::testing::run_event(0, Some("task-001"), None, RunEventKind::Agent(event))
    }

    fn ran(id: &str, command: &str) -> RunEvent {
        agent(AgentEvent::ToolStarted {
            id: id.into(),
            name: "shell".into(),
            input: json!({"command": command}),
            parent: None,
        })
    }

    fn ended(id: &str, ok: bool, output: &str) -> RunEvent {
        agent(AgentEvent::ToolFinished {
            id: id.into(),
            ok,
            output: Some(output.into()),
            parent: None,
        })
    }

    #[test]
    fn the_newest_test_the_agent_ran_is_named_with_how_it_ended() {
        let events = [
            ran(
                "c1",
                "cd /work/task-001 && PYTHONPATH=. python3 tests/test_slug.py",
            ),
            ended("c1", true, "..F\nFAILED (failures=2)\n"),
            ran("c2", "cat slug.py"),
            ended("c2", true, "def slugify(title):"),
            ran(
                "c3",
                "cd /work/task-001 && PYTHONPATH=. python3 tests/test_slug.py",
            ),
            ended("c3", true, "...\n\nOK\n"),
            ran("c4", "find . -name '*.py'"),
            ended("c4", true, "./slug.py"),
        ];
        assert_eq!(
            newest_test(&events).as_deref(),
            Some("PYTHONPATH=. python3 tests/test_slug.py (OK)")
        );
        let failing = [ran("c1", "cargo test -p slug"), ended("c1", false, "")];
        assert_eq!(
            newest_test(&failing).as_deref(),
            Some("cargo test -p slug (failed)")
        );
        assert_eq!(newest_test(&[ran("c1", "pytest")]), None);
    }

    #[test]
    fn what_changed_names_a_few_files_and_counts_more() {
        assert_eq!(
            stat_words(&[("slug.py".into(), 13, 1)]).as_deref(),
            Some("slug.py +13 −1")
        );
        let many: Vec<(String, u32, u32)> = (1..=4).map(|n| (format!("f{n}.rs"), n, 1)).collect();
        assert_eq!(stat_words(&many).as_deref(), Some("4 files +10 −4"));
        assert_eq!(stat_words(&[]), None);
    }

    #[test]
    fn the_reasons_say_how_the_session_was_stopped() {
        let stuck = EndReason::Stuck {
            watchdog: Watchdog::Repeat,
            calls: Some(CallCount {
                name: "execute".into(),
                times: 12,
            }),
        };
        assert_eq!(
            passed_reason(&stuck),
            "its work passes the checks, though its session was stopped: it repeated the same \
             execute call 12 times"
        );
        assert!(unchecked_reason(&stuck).starts_with(
            "its session was stopped: it repeated the same execute call 12 times, with its work"
        ));
    }

    #[test]
    fn a_last_line_reads_as_passed_only_when_nothing_failed() {
        for passed in [
            "OK",
            "=== 3 passed in 0.12s ===",
            "test result: ok. 4 passed",
        ] {
            assert!(reads_as_passed(passed), "{passed}");
        }
        for not in [
            "FAILED (failures=2)",
            "1 failed, 2 passed",
            "Error: no tests",
            "done",
        ] {
            assert!(!reads_as_passed(not), "{not}");
        }
        let events = [
            ran("c1", "python3 tests/test_slug.py"),
            ended("c1", true, "...\n\nOK\n"),
        ];
        assert!(newest_test_run(&events).unwrap().passed);
        let events = [
            ran("c1", "python3 tests/test_slug.py"),
            ended("c1", true, "..F\nFAILED (failures=2)\n"),
        ];
        assert!(!newest_test_run(&events).unwrap().passed);
    }
}
