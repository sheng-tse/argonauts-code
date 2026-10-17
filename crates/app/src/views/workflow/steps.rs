//! The steps Argonauts Code runs itself at a run's end, as rows of its Merge phase: the final
//! check with its command, bringing your branch in when it moved, a task's merge that git found
//! a conflict in, and the merge. Each is read from the run's events, with the command or `git`
//! where an agent's model would be, so no row says a model did what code did. In a folder
//! without git the last step writes into your folder, and names the history kept for it where
//! git would stand.

use std::collections::BTreeMap;

use argonauts_core::TaskId;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::approval::{AnswerAction, ApprovalEvent};
use argonauts_core::events::run::{Party, RunLifecycle};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::task::{CheckOutcome, MergeEvent, TaskEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::hidden_git::{HISTORY_KEPT, MergedInto};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::views::{CommandRow, RowStatus, Tone, WorkflowRow};
use argonauts_crew::merge::final_check::CHECK_SCOPE;
use time::OffsetDateTime;

/// What a step is named when the run's target isn't known yet.
const YOUR_BRANCH: &str = "your branch";

/// What a step calls the folder a run without git works for.
const YOUR_FOLDER: &str = "your folder";

/// Said under the Merge phase's steps in a folder without git, where the step a model takes
/// in a merge is missing: the hidden history's notes are plain ones.
pub(super) const NO_COMMIT_MESSAGE: &str =
    "no model writes a commit message here: only /rewind reads the history";

/// One step Argonauts Code ran, and when.
pub(super) struct Step {
    pub at: OffsetDateTime,
    /// Whether it is part of checking the whole change, which a workflow with a phase for
    /// that shows there.
    pub checks: bool,
    pub row: WorkflowRow,
    /// What its phase says under its rows because of it.
    pub note: Option<&'static str>,
}

/// A task's merge that git found a conflict in, which waits for the model it went to.
struct Conflicted {
    /// Where its row is among the steps.
    at: usize,
    since: OffsetDateTime,
    said: String,
    settles: ModelRef,
}

/// The run's own steps so far, oldest first.
pub(super) fn of_the_merge(events: &[RunEvent]) -> Vec<Step> {
    let target = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            RunEventKind::Merge(
                MergeEvent::RunMerged { target, .. } | MergeEvent::WaitingToMerge { target, .. },
            ) => Some(target.clone()),
            _ => None,
        })
        .unwrap_or_else(|| YOUR_BRANCH.to_owned());
    // A folder without git has no branch to name, and no commits of yours to count.
    let folder = MergedInto::of(&target) == MergedInto::YourFolder;
    let yours = if folder { YOUR_FOLDER } else { target.as_str() };
    let mut rows: Vec<Step> = Vec::new();
    let mut merged_tasks = 0;
    let mut conflicts = 0;
    // Each task's merge with a conflict, until the task merges.
    let mut unsettled: BTreeMap<TaskId, Conflicted> = BTreeMap::new();
    // When the branch was seen to have moved, and by how many commits.
    let mut moved: Option<(OffsetDateTime, u32)> = None;
    // When you or your rules last decided something, which is when the merge could start.
    let mut decided: Option<OffsetDateTime> = None;
    // Your branch moved under the run, so what merges is more than the tasks as reviewed, and
    // you said yes to it yourself.
    let mut branch_moved = false;
    let mut your_ok = false;
    for event in events {
        let at = event.ts;
        let since = |from: Option<OffsetDateTime>| {
            from.map_or(0, |from| {
                u64::try_from((at - from).whole_milliseconds()).unwrap_or(0)
            })
        };
        match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Decided(answer)) => {
                decided = Some(at);
                if answer.action == AnswerAction::Merge {
                    your_ok = branch_moved && answer.by == Party::User;
                }
            }
            RunEventKind::Check(check)
                if event.task_id.as_ref().map(|task| task.as_str()) == Some(CHECK_SCOPE) =>
            {
                rows.push(Step {
                    at,
                    checks: true,
                    row: final_check(check),
                    note: None,
                });
            }
            // A task's merge that is still to settle when the run ends is left out of it.
            RunEventKind::Run(RunLifecycle::Finished { .. }) => {
                for (_, conflicted) in std::mem::take(&mut unsettled) {
                    let said = format!("{} · left out", conflicted.said);
                    let row = &mut rows[conflicted.at].row;
                    ended(
                        row,
                        RowStatus::Failed,
                        said,
                        Tone::Warn,
                        since(Some(conflicted.since)),
                    );
                }
            }
            RunEventKind::Merge(merge) => match merge {
                MergeEvent::Merged { task, .. } => {
                    merged_tasks += 1;
                    if let Some(conflicted) = unsettled.remove(task) {
                        let by = DisplayNames::default().short(&conflicted.settles);
                        let said = format!("{} · settled by {by}", conflicted.said);
                        let took = since(Some(conflicted.since));
                        let row = &mut rows[conflicted.at].row;
                        ended(row, RowStatus::Done, said, Tone::Plain, took);
                    }
                }
                MergeEvent::Conflict {
                    task,
                    files,
                    assigned_to,
                } => {
                    conflicts += 1;
                    let said = match files.as_slice() {
                        [] => "conflict".to_owned(),
                        [file] => format!("conflict in {file}"),
                        [file, more @ ..] => format!("conflict in {file} and {} more", more.len()),
                    };
                    let label = format!("merge {}", task_name(events, task));
                    let mut row = step(&label, &said, Tone::Warn, 0);
                    if let WorkflowRow::Command(command) = &mut row {
                        command.status = RowStatus::Running;
                    }
                    let conflicted = Conflicted {
                        at: rows.len(),
                        since: at,
                        said,
                        settles: assigned_to.clone(),
                    };
                    // A task tried again keeps one row, its newest.
                    if let Some(before) = unsettled.insert(task.clone(), conflicted) {
                        rows.remove(before.at);
                        for later in unsettled.values_mut() {
                            later.at -= usize::from(later.at > before.at);
                        }
                    }
                    rows.push(Step {
                        at,
                        checks: false,
                        row,
                        note: None,
                    });
                }
                MergeEvent::MainMoved { commits, .. } => {
                    moved = Some((at, *commits));
                    branch_moved = true;
                }
                MergeEvent::MainMergedIn { .. } => {
                    let (since_moved, commits) = moved
                        .take()
                        .map_or((0, 0), |(from, commits)| (since(Some(from)), commits));
                    let result = match commits {
                        _ if folder => format!("{yours} changed · no conflicts"),
                        0 => format!("{yours} moved · no conflicts"),
                        1 => format!("{yours} moved 1 commit · no conflicts"),
                        commits => format!("{yours} moved {commits} commits · no conflicts"),
                    };
                    let label = format!("catch up with {yours}");
                    let row = step(&label, &result, Tone::Plain, since_moved);
                    rows.push(Step {
                        at,
                        checks: true,
                        row,
                        note: None,
                    });
                }
                MergeEvent::RunMerged { target, .. } => {
                    let tasks = match merged_tasks {
                        1 => "1 task".to_owned(),
                        tasks => format!("{tasks} tasks"),
                    };
                    let settled = match conflicts {
                        0 => "no conflicts".to_owned(),
                        1 => "1 conflict settled".to_owned(),
                        conflicts => format!("{conflicts} conflicts settled"),
                    };
                    let into = MergedInto::of(target);
                    let took = since(decided);
                    let note = (into == MergedInto::YourFolder).then_some(NO_COMMIT_MESSAGE);
                    let row = match into {
                        MergedInto::Branch(_) => {
                            let result = if your_ok && conflicts == 0 {
                                format!("{tasks} · after your OK")
                            } else {
                                format!("{tasks} · {settled}")
                            };
                            step(&into.step(), &result, Tone::Plain, took)
                        }
                        // Nothing there is git's to the eye, and with no conflict to settle
                        // the tasks are all there is to say.
                        MergedInto::YourFolder => {
                            let result = match conflicts {
                                0 => tasks,
                                _ => format!("{tasks} · {settled}"),
                            };
                            ran(&into.step(), HISTORY_KEPT, &result, Tone::Plain, took)
                        }
                    };
                    rows.push(Step {
                        at,
                        checks: false,
                        row,
                        note,
                    });
                }
                MergeEvent::PullRequest { branch, url, .. } => {
                    let result = url.clone().unwrap_or_else(|| branch.clone());
                    let took = since(decided);
                    let row = step("open a pull request", &result, Tone::Plain, took);
                    rows.push(Step {
                        at,
                        checks: false,
                        row,
                        note: None,
                    });
                }
                _ => {}
            },
            _ => {}
        }
    }
    rows
}

/// What the run's rows call `task`: the label of its worker's newest session, such as
/// `build:add-tests-for`, else its title.
fn task_name(events: &[RunEvent], task: &TaskId) -> String {
    let labelled = events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Session(SessionEvent::Started(started))
            if event.task_id.as_ref() == Some(task) && started.role == RoleName::Worker =>
        {
            started
                .label
                .as_deref()
                .map(str::trim)
                .filter(|label| !label.is_empty())
        }
        _ => None,
    });
    let titled = || {
        events.iter().find_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::Created(spec)) if &spec.id == task => {
                Some(spec.title.trim()).filter(|title| !title.is_empty())
            }
            _ => None,
        })
    };
    labelled
        .or_else(titled)
        .map_or_else(|| task.to_string(), str::to_owned)
}

/// A step that was at work, as it ended.
fn ended(row: &mut WorkflowRow, status: RowStatus, said: String, tone: Tone, took: u64) {
    if let WorkflowRow::Command(command) = row {
        command.status = status;
        command.result = Some(said);
        command.tone = tone;
        command.elapsed_ms = took;
    }
}

/// One of the final check's commands: what ran, and what came of it in the check's own words.
fn final_check(check: &CheckOutcome) -> WorkflowRow {
    let command = match &check.command {
        Some(argv) if !argv.is_empty() => argv.join(" "),
        _ => check.name.clone(),
    };
    let summary = check.summary.trim();
    WorkflowRow::Command(CommandRow {
        label: "final check".to_owned(),
        command,
        result: (!summary.is_empty()).then(|| summary.to_owned()),
        tone: if check.passed { Tone::Plain } else { Tone::Bad },
        status: if check.passed {
            RowStatus::Done
        } else {
            RowStatus::Failed
        },
        elapsed_ms: check.duration_ms,
    })
}

/// A step git ran for Argonauts Code.
fn step(label: &str, result: &str, tone: Tone, elapsed_ms: u64) -> WorkflowRow {
    ran(label, "git", result, tone, elapsed_ms)
}

/// A step Argonauts Code ran, with what ran it where an agent's model would be.
fn ran(label: &str, command: &str, result: &str, tone: Tone, elapsed_ms: u64) -> WorkflowRow {
    WorkflowRow::Command(CommandRow {
        label: label.to_owned(),
        command: command.to_owned(),
        result: Some(result.to_owned()),
        tone,
        status: RowStatus::Done,
        elapsed_ms,
    })
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::approval::ApprovalDecided;
    use argonauts_core::names::ApprovalId;

    use super::*;
    use crate::client::test_support::event;

    fn merge(seq: u64, merge: MergeEvent) -> RunEvent {
        event(seq, RunEventKind::Merge(merge))
    }

    fn merge_answered(seq: u64, by: Party) -> RunEvent {
        event(
            seq,
            RunEventKind::Approval(ApprovalEvent::Decided(ApprovalDecided {
                id: ApprovalId::numbered(1),
                action: AnswerAction::Merge,
                text: None,
                by,
                evidence_sha256: String::new(),
                auto_preview: None,
                tasks: Vec::new(),
            })),
        )
    }

    /// What each step says where a model's name would be, and after it.
    fn said(events: &[RunEvent]) -> Vec<(String, String)> {
        of_the_merge(events)
            .into_iter()
            .filter_map(|step| match step.row {
                WorkflowRow::Command(row) => Some((
                    row.label,
                    format!("{} · {}", row.command, row.result.unwrap_or_default()),
                )),
                _ => None,
            })
            .collect()
    }

    fn two_tasks_then(rest: Vec<RunEvent>) -> Vec<RunEvent> {
        let task = |n: u32| argonauts_core::TaskId::new(format!("task-{n:03}")).unwrap();
        let merged = |seq: u64, n: u32| {
            merge(
                seq,
                MergeEvent::Merged {
                    task: task(n),
                    sha: "abc".to_owned(),
                },
            )
        };
        let mut events = vec![merged(1, 1), merged(2, 2)];
        events.extend(rest);
        events
    }

    fn run_merged(seq: u64) -> RunEvent {
        merge(
            seq,
            MergeEvent::RunMerged {
                target: "main".to_owned(),
                sha: "def".to_owned(),
                fast_forward: true,
            },
        )
    }

    /// Your branch moved while the run worked, and you said yes to the merge yourself: the
    /// merge's row says so, where a merge nothing changed under says there were no conflicts.
    #[test]
    fn a_merge_you_approved_after_your_branch_moved_says_so() {
        let moved = vec![
            merge(
                3,
                MergeEvent::MainMoved {
                    from: "aaa".to_owned(),
                    to: "bbb".to_owned(),
                    commits: 2,
                },
            ),
            merge(
                4,
                MergeEvent::MainMergedIn {
                    sha: "ccc".to_owned(),
                },
            ),
            merge_answered(5, Party::User),
            run_merged(6),
        ];
        assert_eq!(
            said(&two_tasks_then(moved)),
            [
                (
                    "catch up with main".to_owned(),
                    "git · main moved 2 commits · no conflicts".to_owned()
                ),
                (
                    "merge onto main".to_owned(),
                    "git · 2 tasks · after your OK".to_owned()
                ),
            ]
        );

        let still = vec![merge_answered(3, Party::User), run_merged(4)];
        assert_eq!(
            said(&two_tasks_then(still)),
            [(
                "merge onto main".to_owned(),
                "git · 2 tasks · no conflicts".to_owned()
            )]
        );
    }

    fn conflict(seq: u64, n: u32) -> RunEvent {
        merge(
            seq,
            MergeEvent::Conflict {
                task: argonauts_core::TaskId::new(format!("task-{n:03}")).unwrap(),
                files: vec!["src/parse.rs".to_owned()],
                assigned_to: "claude-code/claude-haiku-5-5".parse().unwrap(),
            },
        )
    }

    /// The worker's session of task `n`, labelled as the run's rows label it.
    fn worker_of(seq: u64, n: u32, label: &str) -> RunEvent {
        use crate::client::test_support::session_started;
        let task = argonauts_core::TaskId::new(format!("task-{n:03}")).unwrap();
        let key = argonauts_core::names::SessionKey::numbered(n);
        let mut started = session_started(seq, &key, Some(&task), RoleName::Worker, "codex");
        if let RunEventKind::Session(SessionEvent::Started(facts)) = &mut started.kind {
            facts.label = Some(label.to_owned());
        }
        started
    }

    fn statuses(events: &[RunEvent]) -> Vec<(String, RowStatus, String)> {
        of_the_merge(events)
            .into_iter()
            .filter_map(|step| match step.row {
                WorkflowRow::Command(row) => {
                    Some((row.label, row.status, row.result.unwrap_or_default()))
                }
                _ => None,
            })
            .collect()
    }

    /// A conflict in a task's merge is that task's row, at work until the task merges, when
    /// it says which model settled it.
    #[test]
    fn a_conflicted_merge_names_its_task_and_waits_until_it_merges() {
        let mut events = vec![worker_of(1, 2, "build:add-tests-for"), conflict(2, 2)];
        let waiting = statuses(&events);
        assert_eq!(
            waiting,
            [(
                "merge build:add-tests-for".to_owned(),
                RowStatus::Running,
                "conflict in src/parse.rs".to_owned()
            )]
        );
        events.push(merge(
            3,
            MergeEvent::Merged {
                task: argonauts_core::TaskId::new("task-002").unwrap(),
                sha: "abc".to_owned(),
            },
        ));
        let settled = statuses(&events);
        assert_eq!(settled[0].1, RowStatus::Done);
        assert_eq!(
            settled[0].2,
            "conflict in src/parse.rs · settled by Haiku 5.5"
        );
    }

    /// A run that ends with a task's merge still unsettled leaves that task out of it, and a
    /// task's merge tried again is one row.
    #[test]
    fn a_merge_still_unsettled_when_the_run_ends_is_left_out() {
        let finished = event(
            9,
            RunEventKind::Run(RunLifecycle::Finished {
                outcome: argonauts_core::events::run::RunOutcome::Merged,
                detail: None,
            }),
        );
        let events = vec![conflict(2, 3), conflict(4, 3), finished];
        assert_eq!(
            statuses(&events),
            [(
                "merge task-003".to_owned(),
                RowStatus::Failed,
                "conflict in src/parse.rs · left out".to_owned()
            )]
        );
    }

    /// Your rules said yes: nobody's OK is claimed.
    #[test]
    fn a_merge_your_rules_approved_claims_no_ok_of_yours() {
        let moved = vec![
            merge(
                3,
                MergeEvent::MainMoved {
                    from: "aaa".to_owned(),
                    to: "bbb".to_owned(),
                    commits: 1,
                },
            ),
            merge(
                4,
                MergeEvent::MainMergedIn {
                    sha: "ccc".to_owned(),
                },
            ),
            merge_answered(5, Party::Rules),
            run_merged(6),
        ];
        let rows = said(&two_tasks_then(moved));
        assert_eq!(rows[1].1, "git · 2 tasks · no conflicts");
    }
}
