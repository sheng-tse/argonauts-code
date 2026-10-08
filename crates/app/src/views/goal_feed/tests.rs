use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::goal::{
    CheckRun, CheckedBy, CliGoalSaid, Criterion, CriterionCheck, CriterionState, CriterionVerdict,
    GoalChecked, GoalEvent, GoalRun, GoalVerdict,
};
use argonauts_core::events::run::Party;
use argonauts_core::goal::{CheckWith, GoalFrom, GoalScope, GoalSpec};
use argonauts_core::names::{RoleName, SessionKey};

use super::*;
use crate::client::test_support::{event, run_id, session_started};
use crate::views::workflow_view;

fn task() -> CardId {
    CardId::new("task-002").unwrap()
}

fn goal(seq: u64, goal: GoalEvent) -> RunEvent {
    let mut event = event(seq, RunEventKind::Goal(goal));
    event.card_id = Some(task());
    event
}

/// The task's three done-when lines: one for the judge, two with a command each.
fn set(kept_by: KeptBy) -> GoalEvent {
    let command = |id: &str, text: &str, command: &str| {
        let mut line = Criterion::new(id, text);
        line.check = CriterionCheck::Command(command.to_owned());
        line
    };
    let spec = GoalSpec::from_lines(
        "add the overdue command",
        vec![
            Criterion::new("G1", "tasks overdue lists tasks due before today"),
            command(
                "G2",
                "tasks add --due rejects a date that isn't ISO",
                "cargo test due::parse",
            ),
            command(
                "G3",
                "every test in tests/overdue.rs passes",
                "cargo test overdue",
            ),
        ],
        CheckWith::Auto,
        GoalFrom::Role,
    );
    let mut goal = GoalRun::new(&spec, GoalScope::Task, kept_by);
    goal.task = Some(task());
    GoalEvent::Set(Box::new(goal))
}

fn started(n: u32) -> GoalEvent {
    GoalEvent::AttemptStarted {
        n,
        title: format!("try {n}"),
        phase: 0,
        task: Some(task()),
    }
}

/// The check after try `n`: the judge's line holds from try 2, the parse line always, and
/// `failed` of the twelve overdue tests still fail.
fn checked(n: u32, failed: u32) -> GoalEvent {
    let verdict = |id: &str, met: bool| CriterionVerdict {
        id: id.to_owned(),
        verdict: if met {
            CriterionState::Met
        } else {
            CriterionState::NotYet
        },
        evidence: String::new(),
        missing: String::new(),
    };
    let command = |name: &str, line: &str, failed: u32, total: u32, ms: u64| CheckRun {
        name: name.to_owned(),
        by: CheckedBy::Command,
        lines: vec![line.to_owned()],
        passed: failed == 0,
        summary: String::new(),
        failed: Some(failed),
        total: Some(total),
        ms,
        output: if failed == 0 {
            String::new()
        } else {
            "test overdue::skips_tasks_without_a_date ... FAILED\nexpected 2 tasks, got 3\n"
                .to_owned()
        },
        agent: None,
    };
    let judge = CheckRun {
        name: "the judge".to_owned(),
        by: CheckedBy::Judge,
        lines: vec!["G1".to_owned()],
        passed: n >= 2,
        summary: "1 of 1 line met".to_owned(),
        failed: None,
        total: None,
        ms: 9_000,
        output: String::new(),
        agent: Some("claude-code/astra".parse().unwrap()),
    };
    GoalEvent::Checked(Box::new(GoalChecked {
        attempt: n,
        verdicts: vec![
            verdict("G1", n >= 2),
            verdict("G2", true),
            verdict("G3", failed == 0),
        ],
        overall: GoalVerdict::NotYet,
        reason: String::new(),
        task: Some(task()),
        checks: vec![
            command("cargo test overdue", "G3", failed, 12, 4_100),
            command("cargo test due::parse", "G2", 0, 6, 0),
            judge,
        ],
    }))
}

fn said(note: &str) -> GoalEvent {
    GoalEvent::CliSaid {
        task: Some(task()),
        said: CliGoalSaid {
            cli: "codex".to_owned(),
            status: "active".to_owned(),
            note: note.to_owned(),
            turns: None,
        },
    }
}

/// build:cli as a run records it: Codex on its third try, two of three lines met.
fn build_cli(session: &SessionKey) -> Vec<RunEvent> {
    vec![
        goal(1, set(KeptBy::Cli)),
        session_started(2, session, Some(&task()), RoleName::Worker, "codex"),
        goal(3, started(1)),
        goal(4, checked(1, 4)),
        goal(5, started(2)),
        goal(6, checked(2, 2)),
        goal(7, started(3)),
        goal(
            8,
            said("One test left: tasks without a date are still listed."),
        ),
        goal(
            9,
            GoalEvent::CheckingNow {
                task: Some(task()),
                attempt: 3,
            },
        ),
        goal(
            10,
            match checked(3, 1) {
                GoalEvent::Checked(found) => GoalEvent::CheckedNow(found),
                other => other,
            },
        ),
    ]
}

/// A task's goal becomes its agent's: who keeps it, each line with what checks it, each try
/// with what still failed, the last check, and what the CLI says.
#[test]
fn a_tasks_goal_is_its_agents_with_the_four_parts_the_panels_draw() {
    let session = SessionKey::numbered(7);
    let events = build_cli(&session);
    let goals = goals_at(&events, events[0].ts + time::Duration::minutes(6));
    let [goal] = goals.as_slice() else {
        panic!("one agent works toward a goal: {goals:?}");
    };
    assert_eq!(goal.session, session);
    assert_eq!(goal.task, Some(task()));
    assert_eq!(goal.role.as_deref(), Some("workers"));
    assert_eq!(
        goal.kept_by,
        GoalKeeper::Cli {
            name: "Codex".to_owned()
        }
    );
    assert_eq!(goal.kept_by.words(), "Codex's own goal");
    assert_eq!(goal.checked_by, "Argonauts");
    assert_eq!(goal.state, GoalState::Running);
    assert_eq!(goal.set_ms_ago, Some(360_000));
    assert_eq!((goal.lines_met(), goal.current_try()), (2, 3));

    let lines: Vec<_> = goal
        .lines
        .iter()
        .map(|line| {
            (
                line.met,
                line.checked_by.as_str(),
                line.met_on_try,
                line.failing.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        lines,
        [
            (Some(true), "the judge", Some(2), None),
            (Some(true), "cargo test due::parse", Some(1), None),
            (
                Some(false),
                "cargo test overdue",
                None,
                Some("1 of 12 fails")
            ),
        ]
    );

    assert_eq!(goal.counting, "failing tests");
    assert_eq!(goal.max_tries, None);
    let tries: Vec<_> = goal
        .tries
        .iter()
        .map(|a_try| (a_try.number, a_try.failing, a_try.running))
        .collect();
    assert_eq!(
        tries,
        [(1, Some(4), false), (2, Some(2), false), (3, Some(1), true)]
    );

    let checks: Vec<_> = goal
        .checks
        .iter()
        .map(|check| (check.name.as_str(), check.passed, check.result.as_str()))
        .collect();
    assert_eq!(
        checks,
        [
            (
                "cargo test overdue",
                Some(false),
                "1 failed · 11 passed · 4.1s"
            ),
            ("cargo test due::parse", Some(true), "passed · 6 tests"),
            ("the judge", Some(true), "1 of 1 line met · Astra"),
        ]
    );
    assert_eq!(
        goal.checks[0].detail,
        [
            "test overdue::skips_tasks_without_a_date ... FAILED",
            "expected 2 tasks, got 3"
        ]
    );
    assert!(!goal.checking, "the check you asked for has answered");

    let says = goal.cli_says.as_ref().unwrap();
    assert_eq!(
        (says.cli.as_str(), says.state.as_str()),
        ("Codex", "active")
    );
    assert_eq!(
        says.note.as_deref(),
        Some("One test left: tasks without a date are still listed.")
    );
}

/// The checks you asked for show as running until they answer, a goal Argonauts keeps says
/// why, and one that stopped short says why too.
#[test]
fn a_goal_says_when_its_checks_run_who_keeps_it_and_why_it_stopped() {
    let session = SessionKey::numbered(7);
    let mut events = vec![
        goal(1, set(KeptBy::Argonauts)),
        session_started(2, &session, Some(&task()), RoleName::Worker, "claude-code"),
        goal(3, started(1)),
        goal(
            4,
            GoalEvent::CheckingNow {
                task: Some(task()),
                attempt: 1,
            },
        ),
    ];
    let goal_now = |events: &[RunEvent]| agent_goals(events, &[]).remove(0);
    let checking = goal_now(&events);
    assert!(checking.checking);
    assert_eq!(checking.kept_by, GoalKeeper::Argonauts);
    assert_eq!(checking.lines[0].met, None);
    assert_eq!(checking.cli_says, None);
    assert_eq!(checking.counting, "");

    events.push(goal(
        5,
        GoalEvent::StatusChanged {
            status: GoalStatus::Stuck,
            by: Party::Conductor,
            task: Some(task()),
            why: Some("the same failure 3 rounds running".to_owned()),
        },
    ));
    let stuck = goal_now(&events);
    assert_eq!(stuck.state, GoalState::Stuck);
    assert_eq!(
        stuck.stopped_because.as_deref(),
        Some("the same failure 3 rounds running")
    );
}

/// The run's own goal goes round the whole workflow, so it is no one agent's, and a task's
/// goal has no row to sit on before its agent's first session.
#[test]
fn a_goal_with_no_agent_of_its_own_is_no_agents_goal() {
    let spec = GoalSpec::from_lines(
        "the export tests pass",
        vec![Criterion::new("G1", "the export tests pass")],
        CheckWith::Auto,
        GoalFrom::Command,
    );
    let runs = GoalRun::new(&spec, GoalScope::Workflow, KeptBy::Argonauts);
    let events = vec![
        event(1, RunEventKind::Goal(GoalEvent::Set(Box::new(runs)))),
        goal(2, set(KeptBy::Cli)),
    ];
    assert_eq!(argonauts_crew::goal::report::reports(&events).len(), 2);
    assert!(agent_goals(&events, &[]).is_empty());
}

/// The workflow view carries each agent's goal under the label its row has there, and the
/// opened agent carries its own the same way.
#[test]
fn the_workflow_view_and_the_opened_agent_carry_the_goal() {
    use argonauts_core::events::session::SessionEvent;
    use argonauts_core::events::workflow::{WorkflowEvent, WorkflowStarted};
    use argonauts_core::names::WorkflowId;
    use argonauts_core::workflow::{StepRef, Workflow};

    let session = SessionKey::numbered(7);
    let workflow: Workflow = serde_json::from_value(serde_json::json!({
        "name": "add-due-dates",
        "phases": [{"name": "Build", "standard": "do", "steps": [{"kind": "build"}]}]
    }))
    .unwrap();
    let mut events = vec![event(
        0,
        RunEventKind::Workflow(WorkflowEvent::Started(Box::new(WorkflowStarted {
            id: WorkflowId::numbered(1),
            from: None,
            version: "v1".to_owned(),
            parent: None,
            workflow,
        }))),
    )];
    events.extend(build_cli(&session));
    // The agent's row in the Build phase, under the label its session was given.
    let seat = events
        .iter_mut()
        .find_map(|event| match &mut event.kind {
            RunEventKind::Session(SessionEvent::Started(started)) => Some(started),
            _ => None,
        })
        .unwrap();
    seat.label = Some("build:cli".to_owned());
    seat.workflow_step = Some(StepRef {
        workflow: WorkflowId::numbered(1),
        phase: 0,
        path: vec![0],
    });

    let view = workflow_view(&events, &run_id(), None).unwrap();
    let [goal] = view.goals.as_slice() else {
        panic!("the view carries the one goal: {:?}", view.goals);
    };
    assert_eq!(goal.session, session);
    let WorkflowRow::Agent(row) = &view.phases[0].rows[0] else {
        panic!("the Build phase holds the agent");
    };
    assert_eq!(
        (row.label.as_str(), goal.label.as_str()),
        ("build:cli", "build:cli")
    );
    assert!(view.goal_of(row).is_some());

    let agent = crate::views::agent_view(&events, &run_id(), &session).unwrap();
    let opened = agent.goal.expect("the opened agent carries its goal");
    assert_eq!((&opened.session, &opened.task), (&session, &goal.task));
    assert_eq!(opened.label, agent.row.label);
}
