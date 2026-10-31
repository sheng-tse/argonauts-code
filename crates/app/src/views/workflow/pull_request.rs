//! The "Pull request" phase: the follow-up of a pull request the run opened, a round of fixes at
//! a time, its agents as rows. Each round's fixer (`fix:…`) is the task's builder resumed with
//! what to fix, and its reviewer (`review:…`) looks again before the fix is pushed. Plain steps,
//! such as reading the checks or pushing, are no rows: only agents are.

use argonauts_core::app::views::{
    AgentRow, PhaseCount, PhaseRow, RowNote, RowStatus, StatusKind, StatusLine, Tone, WorkflowRow,
    WorkflowView,
};
use argonauts_core::base::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::runs::events::session::{EndReason, SessionEvent};
use argonauts_core::runs::events::task::MergeEvent;
use argonauts_core::runs::events::{RunEvent, RunEventKind, RunLifecycle};
use argonauts_core::workflows::workflow::StandardPhase;
use argonauts_core::{Spend, TaskId};
use argonauts_crew::follow::Followed;
use std::path::Path;

/// The phase's name, as the Phases list shows it.
pub(super) const NAME: &str = "Pull request";

/// While the pull request the run opened is followed, the line under the run's header says
/// where it stands, as the newest look at GitHub put it, in place of the line that says it's
/// open.
pub(crate) fn followed_heading(view: &mut WorkflowView, state_dir: &Path) {
    let open = view.status_lines.first().is_some_and(|line| {
        line.kind == StatusKind::Ended && line.text.starts_with(super::PULL_REQUEST_OPEN)
    });
    if !open {
        return;
    }
    let Some(heading) =
        Followed::of_run(state_dir, &view.run).and_then(|followed| followed.heading)
    else {
        return;
    };
    let text = match heading.then.trim() {
        "" => heading.said,
        then => format!("{} · {then}", heading.said),
    };
    view.status_lines = vec![StatusLine::new(StatusKind::PullRequest, text)];
}

/// The labels of a round's fixer and its reviewer start so.
const FIX: &str = "fix:";
const REVIEW: &str = "review:";

/// The phases the follow-up of the run's pull request adds after its workflow's own. A run that
/// opened none, or whose pull request no round has worked on yet, has none.
pub(super) fn phases(events: &[RunEvent]) -> Vec<PhaseRow> {
    let Some(opened) = events.iter().rposition(|event| {
        matches!(
            event.kind,
            RunEventKind::Merge(MergeEvent::PullRequest { url: Some(_), .. })
        )
    }) else {
        return Vec::new();
    };
    let rows = agents_after(events, opened);
    if rows.is_empty() {
        return Vec::new();
    }
    let done = rows
        .iter()
        .filter(|row| row.status == RowStatus::Done)
        .count();
    let status = if rows.iter().any(|row| row.status == RowStatus::Failed) {
        RowStatus::Failed
    } else if done == rows.len() {
        RowStatus::Done
    } else {
        RowStatus::Running
    };
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    vec![PhaseRow {
        name: NAME.to_owned(),
        detail: String::new(),
        standard: StandardPhase::Check,
        status,
        try_number: None,
        count: PhaseCount::of(count(done), count(rows.len())),
        rows: rows.into_iter().map(WorkflowRow::Agent).collect(),
        note: None,
    }]
}

/// The follow-up's agents, in the order they started, each as its session stands. A round's
/// reviewer is a queued row under its fixer until it starts.
fn agents_after(events: &[RunEvent], opened: usize) -> Vec<AgentRow> {
    let mut rows: Vec<Agent> = Vec::new();
    for (at, event) in events.iter().enumerate().skip(opened + 1) {
        let (Some(session), RunEventKind::Session(happened)) = (&event.session, &event.kind) else {
            continue;
        };
        match happened {
            SessionEvent::Started(started) => {
                let Some(label) = started
                    .label
                    .as_deref()
                    .filter(|label| label.starts_with(FIX) || label.starts_with(REVIEW))
                else {
                    continue;
                };
                let mut row = row_of(session.clone(), label, started.worker.clone());
                row.status = RowStatus::Running;
                rows.push(Agent {
                    row,
                    task: event.task_id.clone(),
                    at,
                });
            }
            SessionEvent::Ended(end) => {
                let Some(agent) = rows.iter_mut().find(|agent| agent.row.session == *session)
                else {
                    continue;
                };
                agent.row.status = if end.reason == EndReason::Completed {
                    RowStatus::Done
                } else {
                    RowStatus::Failed
                };
                agent.row.spent = end.spend;
                agent.row.elapsed_ms = end.duration_ms;
            }
            _ => {}
        }
    }
    let mut shown = Vec::with_capacity(rows.len() + 1);
    for Agent { mut row, task, at } in rows {
        if row.status == RowStatus::Running {
            row.note = at_work(&row.label).map(|note| RowNote::new(note, Tone::Plain));
        }
        let queued = reviewer_to_come(events, &row, task.as_ref(), at);
        shown.push(row);
        shown.extend(queued);
    }
    shown
}

/// A row of the follow-up, as it stands when its session starts.
struct Agent {
    row: AgentRow,
    task: Option<TaskId>,
    /// Where its start is in the run's events.
    at: usize,
}

/// The reviewer a round's fixer hands its fix to, while that reviewer hasn't started: the
/// newest that reviewed the fixer's task, as the round asks. Once the fixer failed, or the
/// round ended with the run's next finish, none is to come; and a task none reviewed has its
/// reviewer chosen by the round, which shows only once it starts.
fn reviewer_to_come(
    events: &[RunEvent],
    fixer: &AgentRow,
    task: Option<&TaskId>,
    at: usize,
) -> Option<AgentRow> {
    let round = fixer.label.strip_prefix(FIX)?;
    let label = format!("{REVIEW}{round}");
    let came_or_ended = events[at + 1..].iter().any(|event| match &event.kind {
        RunEventKind::Session(SessionEvent::Started(started)) => {
            started.label.as_deref() == Some(label.as_str())
        }
        RunEventKind::Run(RunLifecycle::Finished { .. }) => true,
        _ => false,
    });
    if fixer.status == RowStatus::Failed || came_or_ended {
        return None;
    }
    let task = task?;
    let (session, worker) = events[..at].iter().rev().find_map(|event| {
        let (Some(session), RunEventKind::Session(SessionEvent::Started(started))) =
            (&event.session, &event.kind)
        else {
            return None;
        };
        (event.task_id.as_ref() == Some(task) && started.role == RoleName::Reviewer)
            .then(|| (session.clone(), started.worker.clone()))
    })?;
    Some(row_of(session, &label, worker))
}

/// An agent's row with nothing done yet: queued, until its session starts.
fn row_of(session: SessionKey, label: &str, worker: ModelRef) -> AgentRow {
    AgentRow {
        session,
        label: label.to_owned(),
        worker,
        status: RowStatus::Queued,
        tokens: 0,
        spent: Spend::default(),
        elapsed_ms: 0,
        subagents: 0,
        context: None,
        fallback_for: None,
        answered: false,
        tries: Vec::new(),
        note: None,
        inner: None,
        left_out: false,
        not_needed: false,
        fallback_why: None,
    }
}

/// What a round's builder works from, said beside its row while it works.
fn at_work(label: &str) -> Option<&'static str> {
    if label.starts_with("fix:ci-") {
        Some("the builder, resumed, with the log")
    } else if label.starts_with("fix:comments-") {
        Some("the builder, resumed, with the comments")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::runs::events::session::SessionStarted;
    use time::macros::datetime;

    use super::*;

    fn event(session: Option<u32>, kind: RunEventKind) -> RunEvent {
        RunEvent {
            seq: 0,
            ts: datetime!(2026-10-20 10:00 UTC),
            run_id: argonauts_core::RunId::new("run-01").unwrap(),
            task_id: None,
            session: session.map(SessionKey::numbered),
            source: "conductor".to_owned(),
            kind,
        }
    }

    fn opened() -> RunEvent {
        event(
            None,
            RunEventKind::Merge(MergeEvent::PullRequest {
                branch: "issue-42-per-key-limiter".to_owned(),
                url: Some("https://github.com/test-user/limiter/pull/57".to_owned()),
                command: None,
            }),
        )
    }

    fn started(session: u32, label: &str, worker: &str) -> RunEvent {
        let worker: ModelRef = worker.parse().unwrap();
        let mut started: SessionStarted = serde_json::from_value(serde_json::json!({
            "worker": worker,
            "agent_id": worker.agent,
            "provider": "anthropic",
            "kind": "cli",
            "role": "worker",
            "access": "build",
            "lifetime": "task",
        }))
        .expect("a session start");
        started.label = Some(label.to_owned());
        event(
            Some(session),
            RunEventKind::Session(SessionEvent::Started(Box::new(started))),
        )
    }

    /// `started` as a session of `task`, in `role`.
    fn on_task(mut event: RunEvent, task: &str, role: RoleName) -> RunEvent {
        event.task_id = Some(TaskId::new(task).expect("a task id"));
        if let RunEventKind::Session(SessionEvent::Started(started)) = &mut event.kind {
            started.role = role;
        }
        event
    }

    fn finished() -> RunEvent {
        event(
            None,
            RunEventKind::Run(RunLifecycle::Finished {
                outcome: argonauts_core::runs::events::run::RunOutcome::PullRequest,
                detail: None,
            }),
        )
    }

    /// The council's build of `per-key`, reviewed by GPT 6 Luna, then its pull request opened
    /// and round 1's fixer at work.
    fn fixing() -> Vec<RunEvent> {
        vec![
            on_task(
                started(2, "build:per-key", "claude-code/claude-haiku-5-5"),
                "per-key",
                RoleName::Worker,
            ),
            on_task(
                started(5, "review:per-key", "codex/gpt-6-luna"),
                "per-key",
                RoleName::Reviewer,
            ),
            opened(),
            finished(),
            on_task(
                started(12, "fix:ci-round-1", "claude-code/claude-haiku-5-5"),
                "per-key",
                RoleName::Worker,
            ),
        ]
    }

    fn labels_and_statuses(phase: &PhaseRow) -> Vec<(&str, RowStatus)> {
        phase
            .rows
            .iter()
            .filter_map(|row| match row {
                WorkflowRow::Agent(agent) => Some((agent.label.as_str(), agent.status)),
                _ => None,
            })
            .collect()
    }

    fn ended(session: u32, reason: EndReason) -> RunEvent {
        let end = argonauts_testkit::engine::runner::session_end(reason, None, None);
        event(
            Some(session),
            RunEventKind::Session(SessionEvent::Ended(Box::new(end))),
        )
    }

    /// While the pull request is followed, the line under the run's header says where it
    /// stands, in place of the line that says it's open. With nothing followed that line
    /// stays.
    #[test]
    fn a_followed_pull_request_heads_its_run() {
        use argonauts_conductor::gh::RepoName;
        use argonauts_core::app::views::WorkflowState;
        use argonauts_core::base::names::WorkflowId;
        use argonauts_crew::follow::Heading;

        let state = tempfile::tempdir().unwrap();
        let run = argonauts_core::RunId::new("run-01").unwrap();
        let open = || WorkflowView {
            run: run.clone(),
            id: WorkflowId::numbered(1),
            name: "Per key limiter".to_owned(),
            description: "Council".to_owned(),
            style: None,
            status_lines: vec![
                StatusLine::new(StatusKind::Ended, super::super::PULL_REQUEST_OPEN)
                    .toned(Tone::Good),
            ],
            agents_done: 11,
            agents_total: 13,
            elapsed_ms: 0,
            spent: Spend::default(),
            phases: Vec::new(),
            notes: Vec::new(),
            required: Vec::new(),
            goals: Vec::new(),
            worker: None,
            state: WorkflowState::Done,
        };
        let mut view = open();
        followed_heading(&mut view, state.path());
        assert_eq!(view.status_lines, open().status_lines, "nothing followed");

        let mut followed = Followed::new(
            run.clone(),
            RepoName::parse("team/limiter").unwrap(),
            57,
            "https://github.com/team/limiter/pull/57".to_owned(),
            "issue-42-per-key".to_owned(),
        );
        followed.heading = Some(Heading {
            said: "#57 · test (ubuntu) failed".to_owned(),
            then: "back to its builder, round 1 of 3 · the fix is pushed once reviewed".to_owned(),
        });
        followed.save(state.path()).unwrap();
        let mut view = open();
        followed_heading(&mut view, state.path());
        assert_eq!(
            view.status_lines,
            [StatusLine::new(
                StatusKind::PullRequest,
                "#57 · test (ubuntu) failed · back to its builder, round 1 of 3 · the fix is \
                 pushed once reviewed"
            )]
        );
    }

    #[test]
    fn a_round_of_fixes_is_a_phase_of_its_agents() {
        let events = vec![
            started(2, "build:per-key", "claude-code/claude-haiku-5-5"),
            opened(),
            started(12, "fix:ci-round-1", "claude-code/claude-haiku-5-5"),
            started(13, "review:ci-round-1", "codex/gpt-6-luna"),
            ended(12, EndReason::Completed),
        ];
        let [phase] = <[PhaseRow; 1]>::try_from(phases(&events)).expect("one phase");
        assert_eq!(phase.name, NAME);
        assert_eq!(phase.status, RowStatus::Running);
        assert_eq!(phase.count, PhaseCount::of(1, 2));
        let labels: Vec<&str> = phase
            .rows
            .iter()
            .map(|row| match row {
                WorkflowRow::Agent(agent) => agent.label.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(labels, ["fix:ci-round-1", "review:ci-round-1"]);
        let notes: Vec<Option<&str>> = phase
            .rows
            .iter()
            .map(|row| match row {
                WorkflowRow::Agent(agent) => agent.note.as_ref().map(|note| note.text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(notes, [None, None], "done and reviewing say nothing more");
    }

    #[test]
    fn a_builder_at_work_says_what_it_works_from() {
        let events = vec![
            opened(),
            started(12, "fix:ci-round-1", "claude-code/claude-haiku-5-5"),
        ];
        let WorkflowRow::Agent(fixer) = &phases(&events)[0].rows[0] else {
            panic!("an agent row");
        };
        assert_eq!(
            fixer.note.as_ref().map(|note| note.text.as_str()),
            Some("the builder, resumed, with the log")
        );
    }

    #[test]
    fn a_rounds_reviewer_is_queued_under_its_fixer_until_it_starts() {
        let events = fixing();
        let [phase] = <[PhaseRow; 1]>::try_from(phases(&events)).expect("one phase");
        assert_eq!(
            labels_and_statuses(&phase),
            [
                ("fix:ci-round-1", RowStatus::Running),
                ("review:ci-round-1", RowStatus::Queued),
            ]
        );
        assert_eq!(phase.count, PhaseCount::of(0, 2));
        let WorkflowRow::Agent(queued) = &phase.rows[1] else {
            panic!("an agent row");
        };
        assert_eq!(queued.worker.to_string(), "codex/gpt-6-luna");
        assert_eq!(
            queued.session,
            SessionKey::numbered(5),
            "the reviewer it carries on"
        );

        let mut done = events.clone();
        done.push(ended(12, EndReason::Completed));
        let [phase] = <[PhaseRow; 1]>::try_from(phases(&done)).expect("one phase");
        assert_eq!(
            labels_and_statuses(&phase),
            [
                ("fix:ci-round-1", RowStatus::Done),
                ("review:ci-round-1", RowStatus::Queued),
            ]
        );

        done.push(on_task(
            started(13, "review:ci-round-1", "codex/gpt-6-luna"),
            "per-key",
            RoleName::Reviewer,
        ));
        let [phase] = <[PhaseRow; 1]>::try_from(phases(&done)).expect("one phase");
        assert_eq!(
            labels_and_statuses(&phase),
            [
                ("fix:ci-round-1", RowStatus::Done),
                ("review:ci-round-1", RowStatus::Running),
            ]
        );
    }

    #[test]
    fn no_reviewer_is_queued_once_none_is_to_come() {
        // The fixer found nothing to change, and the round ended.
        let mut ended_round = fixing();
        ended_round.push(ended(12, EndReason::Completed));
        ended_round.push(finished());
        // The fixer failed.
        let mut failed = fixing();
        failed.push(ended(12, EndReason::Stopped));
        // No one reviewed the task, as in a Captain run, so the round picks its reviewer.
        let unreviewed: Vec<RunEvent> = fixing()
            .into_iter()
            .filter(|event| event.session != Some(SessionKey::numbered(5)))
            .collect();
        for events in [ended_round, failed, unreviewed] {
            let [phase] = <[PhaseRow; 1]>::try_from(phases(&events)).expect("one phase");
            assert_eq!(phase.rows.len(), 1, "only the fixer: {phase:?}");
        }
    }

    #[test]
    fn no_pull_request_or_no_round_yet_adds_nothing() {
        assert!(phases(&[started(2, "fix:ci-round-1", "codex/gpt-6-luna")]).is_empty());
        assert!(phases(&[opened()]).is_empty());
    }

    #[test]
    fn a_failed_fixer_fails_the_phase() {
        let events = vec![
            opened(),
            started(12, "fix:ci-round-1", "claude-code/claude-haiku-5-5"),
            ended(12, EndReason::Stopped),
        ];
        assert_eq!(phases(&events)[0].status, RowStatus::Failed);
    }

    #[test]
    fn a_run_without_a_later_step_has_no_later_phase() {
        for appended in super::super::APPENDED {
            assert!(appended(&[]).is_empty());
        }
    }
}
