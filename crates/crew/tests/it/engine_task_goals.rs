//! A task worked as a goal, end to end over fake workers: a Captain run's one task under `/goal`
//! kept by Argonauts Code on Claude Code and by Codex's own goal on Codex, a role that works each
//! of its tasks as a goal, and a task's goal that gets stuck. Argonauts Code runs the check after
//! every try, whoever keeps the goal, and what still fails goes back to the same session.

use crate::support;
use std::sync::Arc;

use serde_json::{Value, json};

use argonauts_core::engine::EngineExit;
use argonauts_core::events::goal::{
    CheckedBy, GoalChecked, GoalEvent, GoalRun, GoalStatus, GoalVerdict,
};
use argonauts_core::events::run::RunMode;
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::task::{CheckKind, CheckOutcome, TaskEvent, TryCause};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::goal::report::LineState;
use argonauts_core::goal::{GoalScope, KeptBy, RoleGoal};
use argonauts_core::names::RoleName;
use argonauts_core::session_runner::{Access, SessionSpec};
use argonauts_core::settings::protocol::CheckCommand;
use argonauts_core::settings::roles::RoleSettings;
use argonauts_core::steps::StepName;
use argonauts_crew::goal::report;
use argonauts_testkit::runner::{FakeRunner, FakeSession};

use support::Crew;

const REQUEST: &str = "Add the overdue command";
/// The goal the tests set: one line, checked by its command.
const UNTIL: &str = "`make check` passes";

fn report_saying(summary: &str) -> Value {
    json!({
        "status": "done",
        "summary": summary,
        "files_changed": [],
        "tests_run": [],
        "concerns": [],
        "questions": [],
        "change_requests": [],
        "memory_proposals": [],
        "answer": "I added the command."
    })
}

/// A Captain crew whose main agent is `worker`, with the goal set for the run.
fn captain(worker: &str, provider: &str) -> anyhow::Result<(Crew, Arc<FakeRunner>)> {
    let mut crew = Crew::new()?;
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.evolution.propose_every = 0;
    crew.settings.goal.until = Some(UNTIL.to_owned());
    let worker = crew.worker(worker, provider);
    Ok((crew, worker))
}

/// A check command's outcome that failed `failed` of 12 tests.
fn failing(failed: u32) -> CheckOutcome {
    CheckOutcome {
        name: "make check".to_owned(),
        kind: CheckKind::Command,
        passed: false,
        command: None,
        exit_code: Some(1),
        duration_ms: 4_100,
        summary: format!("{} passed, {failed} failed", 12 - failed),
        output_tail: "test overdue::skips_tasks_without_a_date ... FAILED\n".to_owned(),
        violations: Vec::new(),
        sandboxed: true,
        skipped: false,
    }
}

fn goal_events(events: &[RunEvent]) -> Vec<&GoalEvent> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Goal(goal) => Some(goal),
            _ => None,
        })
        .collect()
}

/// The goal a task took, as it was first set.
fn task_goal(events: &[RunEvent]) -> GoalRun {
    goal_events(events)
        .into_iter()
        .find_map(|event| match event {
            GoalEvent::Set(goal) if goal.task.is_some() => Some((**goal).clone()),
            _ => None,
        })
        .expect("a task took a goal")
}

fn checks(events: &[RunEvent]) -> Vec<GoalChecked> {
    goal_events(events)
        .into_iter()
        .filter_map(|event| match event {
            GoalEvent::Checked(checked) if checked.task.is_some() => Some((**checked).clone()),
            _ => None,
        })
        .collect()
}

fn statuses(events: &[RunEvent]) -> Vec<(GoalStatus, Option<String>)> {
    goal_events(events)
        .into_iter()
        .filter_map(|event| match event {
            GoalEvent::StatusChanged {
                status,
                why,
                task: Some(_),
                ..
            } => Some((*status, why.clone())),
            _ => None,
        })
        .collect()
}

fn causes(events: &[RunEvent]) -> Vec<TryCause> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::Try { cause, .. }) => Some(*cause),
            _ => None,
        })
        .collect()
}

fn builds(worker: &FakeRunner) -> Vec<SessionSpec> {
    worker
        .specs()
        .into_iter()
        .filter(|spec| spec.access == Access::Build)
        .collect()
}

/// Everything a session was sent, as one text.
fn sent(spec: &SessionSpec) -> String {
    spec.prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// On Claude Code, which has no goal of its own, Argonauts Code keeps a Captain run's goal: after
/// each try it runs the check and sends back what still fails, to the same session.
#[tokio::test]
async fn in_captain_on_claude_code_argonauts_keeps_the_goal() -> anyhow::Result<()> {
    let (crew, worker) = captain("claude-code/opus", "anthropic")?;
    worker.push(FakeSession::structured(report_saying(
        "Wrote the command and its tests.",
    )));
    worker.push(FakeSession::structured(report_saying(
        "Fixed the date comparison.",
    )));
    // The goal's own command fails after the first try and passes after the second.
    crew.services.checks.push(failing(2));
    let outcome = crew.start(RunMode::Captain, REQUEST).await?;
    assert!(
        !matches!(outcome.exit, EngineExit::Crashed(_)),
        "{:?}",
        outcome.exit
    );
    let events = &outcome.events;

    let goal = task_goal(events);
    assert_eq!(goal.scope, GoalScope::Captain);
    assert_eq!(goal.objective, UNTIL);
    assert_eq!(goal.kept_by, KeptBy::Argonauts);
    assert_eq!(
        goal.kept_note.as_deref(),
        Some("Claude Code has no goal of its own")
    );
    assert_eq!(goal.max_rounds, None, "no fixed number of tries");

    let checked = checks(events);
    assert_eq!(
        checked
            .iter()
            .map(|checked| (checked.try_number, checked.overall))
            .collect::<Vec<_>>(),
        [(1, GoalVerdict::NotYet), (2, GoalVerdict::Met)]
    );
    // The command decided it by its exit code, and no judge was asked.
    let [run] = checked[0].checks.as_slice() else {
        panic!("one command ran: {:?}", checked[0].checks);
    };
    assert_eq!(
        (run.name.as_str(), run.by, run.passed),
        ("make check", CheckedBy::Command, false)
    );
    assert_eq!((run.failed, run.total), (Some(2), Some(12)));
    assert_eq!(statuses(events), [(GoalStatus::Met, None)]);

    // The same session got what still failed, and it's a try of the goal, not a check retry.
    let sessions = builds(&worker);
    assert_eq!(sessions.len(), 2, "two tries, two sessions");
    let first = sent(&sessions[0]);
    assert!(first.contains("This task is worked as a goal"), "{first}");
    assert!(
        first.contains("`make check` passes (checked by make check)"),
        "{first}"
    );
    assert!(
        sessions[1].resume.is_some(),
        "the second try resumes the first"
    );
    let second = sent(&sessions[1]);
    assert!(second.contains("The goal doesn't hold yet"), "{second}");
    assert!(second.contains("make check: 2 of 12 fail"), "{second}");
    assert!(
        second.contains("overdue::skips_tasks_without_a_date"),
        "{second}"
    );
    assert_eq!(causes(events), [TryCause::First, TryCause::GoalRetry]);

    // What the screens are told, from the journal alone.
    let reports = report::reports(events);
    let [told] = reports.as_slice() else {
        panic!("one goal, the task's: {reports:?}");
    };
    assert_eq!(told.row_words(), "goal met in 2 tries");
    assert_eq!(told.kept_words(), "kept by Argonauts Code");
    assert_eq!(told.lines[0].state, LineState::Met);
    assert_eq!(told.lines[0].checked_by, "make check");
    assert_eq!(told.lines[0].met_on_try, Some(2));
    assert_eq!(told.tries[0].what, "Wrote the command and its tests");
    assert_eq!(told.tries[1].what, "Fixed the date comparison");
    // Its agent's row is keyed by its session, which is the last one that built.
    let last_build = events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Session(SessionEvent::Started(started))
            if started.access == Access::Build =>
        {
            event.session.clone()
        }
        _ => None,
    });
    assert!(last_build.is_some());
    assert_eq!(told.session, last_build);
    assert_eq!(
        told.worker.as_ref().map(|model| model.agent.as_str()),
        Some("claude-code")
    );
    Ok(())
}

/// On Codex the goal is Codex's own: the task's lines are set as its goal, it's resumed while
/// it says the goal is still active, and Argonauts Code runs the check once it says it's done.
#[tokio::test]
async fn in_captain_on_codex_the_goal_is_codexs_own_and_argonauts_still_checks()
-> anyhow::Result<()> {
    let (crew, worker) = captain("codex/gpt-x", "openai")?;
    worker.push(FakeSession::structured(report_saying(
        "Wrote the command and its tests.\ngoal: active · one test left: tasks without a date",
    )));
    worker.push(FakeSession::structured(report_saying(
        "Handled tasks with no due date.\ngoal: complete · every test passes",
    )));
    let outcome = crew.start(RunMode::Captain, REQUEST).await?;
    assert!(
        !matches!(outcome.exit, EngineExit::Crashed(_)),
        "{:?}",
        outcome.exit
    );
    let events = &outcome.events;

    let goal = task_goal(events);
    assert_eq!(goal.kept_by, KeptBy::Cli);
    assert_eq!(goal.kept_note, None);
    let sessions = builds(&worker);
    assert_eq!(sessions.len(), 2);
    let first = sent(&sessions[0]);
    assert!(first.contains("with your own goal tool"), "{first}");
    assert!(first.contains("goal: <its status>"), "{first}");
    // Its goal was still active, so it carries on without a check in between.
    let second = sent(&sessions[1]);
    assert!(
        second.starts_with("Continue working toward your goal"),
        "{second}"
    );
    assert!(sessions[1].resume.is_some());

    // One try, one check: Codex saying "complete" wasn't enough by itself, and wasn't needed.
    let checked = checks(events);
    assert_eq!(checked.len(), 1, "{checked:?}");
    assert_eq!(checked[0].overall, GoalVerdict::Met);
    assert_eq!(causes(events), [TryCause::First, TryCause::Resumed]);

    let said: Vec<(String, String)> = goal_events(events)
        .into_iter()
        .filter_map(|event| match event {
            GoalEvent::CliSaid { said, .. } => Some((said.status.clone(), said.note.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        said,
        [
            (
                "active".to_owned(),
                "one test left: tasks without a date".to_owned()
            ),
            ("complete".to_owned(), "every test passes".to_owned()),
        ]
    );
    let reports = report::reports(events);
    let told = &reports[0];
    assert_eq!(
        told.kept_words(),
        "kept by Codex's own goal · checked by Argonauts Code"
    );
    assert_eq!(told.kept.cli.as_deref(), Some("Codex"));
    assert_eq!(
        told.role(),
        Some("main"),
        "a Captain run's goal is its main agent's"
    );
    assert_eq!(told.row_words(), "goal met in 1 try");
    let says = told.cli_says.as_ref().expect("what Codex says");
    assert_eq!(
        (says.cli.as_str(), says.status.as_str(), says.turns),
        ("Codex", "complete", Some(2))
    );
    Ok(())
}

/// When Codex says "complete" and the check disagrees, the failure goes back as the next try.
#[tokio::test]
async fn codex_saying_done_is_not_enough() -> anyhow::Result<()> {
    let (crew, worker) = captain("codex/gpt-x", "openai")?;
    for summary in [
        "Wrote the command.\ngoal: complete · all done",
        "Fixed the last test.\ngoal: complete · every test passes",
    ] {
        worker.push(FakeSession::structured(report_saying(summary)));
    }
    crew.services.checks.push(failing(1));
    let outcome = crew.start(RunMode::Captain, REQUEST).await?;
    let events = &outcome.events;
    assert_eq!(
        checks(events)
            .iter()
            .map(|checked| checked.overall)
            .collect::<Vec<_>>(),
        [GoalVerdict::NotYet, GoalVerdict::Met]
    );
    let sessions = builds(&worker);
    let second = sent(&sessions[1]);
    assert!(second.contains("make check: 1 of 12 fails"), "{second}");
    assert!(
        second.contains("Your goal isn't achieved yet, whatever your goal tool says"),
        "{second}"
    );
    assert_eq!(causes(events), [TryCause::First, TryCause::GoalRetry]);
    Ok(())
}

/// The same failure `stuck_after` tries running changes approach once, then the goal stops
/// being sent round and says why. The task goes on as its own checks say.
#[tokio::test]
async fn a_tasks_goal_changes_approach_once_then_stops_and_says_why() -> anyhow::Result<()> {
    let (mut crew, worker) = captain("claude-code/opus", "anthropic")?;
    crew.settings.goal.stuck_after = 2;
    for _ in 0..4 {
        worker.push(FakeSession::structured(report_saying("Tried a fix.")));
    }
    for _ in 0..4 {
        crew.services.checks.push(failing(3));
    }
    let outcome = crew.start(RunMode::Captain, REQUEST).await?;
    assert!(
        !matches!(outcome.exit, EngineExit::Crashed(_)),
        "{:?}",
        outcome.exit
    );
    let events = &outcome.events;

    // Three tries and three checks, never a fourth.
    assert_eq!(checks(events).len(), 3);
    let sessions = builds(&worker);
    assert_eq!(sessions.len(), 3);
    assert!(!sent(&sessions[1]).contains("different approach"));
    let third = sent(&sessions[2]);
    assert!(
        third.contains("This is the same failure 2 rounds running. Take a different approach"),
        "{third}"
    );
    let changed: Vec<u32> = goal_events(events)
        .into_iter()
        .filter_map(|event| match event {
            GoalEvent::ApproachChanged { after, .. } => Some(*after),
            _ => None,
        })
        .collect();
    assert_eq!(changed, [2]);
    assert_eq!(
        statuses(events),
        [(
            GoalStatus::Stuck,
            Some("the same failure 2 rounds running, after a change of approach".to_owned())
        )]
    );
    let told = &report::reports(events)[0];
    assert_eq!(told.status, GoalStatus::Stuck);
    assert!(told.tries[2].changed_approach);
    assert_eq!(told.row_words(), "goal stuck · 0 of 1 line met");
    Ok(())
}

/// A crew member that plans one task, writes a file when it builds, and otherwise gives the
/// smallest valid answer.
fn member(crew: &mut Crew, worker: &str, provider: &str) {
    let fake = FakeRunner::cli(worker, provider).respond_with(|spec| match spec.step {
        Some(StepName::PlanToTasks) => FakeSession::structured(json!({
            "tasks": [{
                "key": "overdue",
                "title": "Add the overdue command",
                "task": "Add a command that lists what's overdue.",
                "kind": "build",
                "owned_paths": ["overdue.txt"],
                "read_only_paths": [],
                "depends_on": [],
                "criteria": [],
            }]
        })),
        Some(StepName::Build) => {
            std::fs::write(spec.workdir.join("overdue.txt"), "overdue\n")
                .expect("the build session writes its file");
            smallest(spec)
        }
        _ => smallest(spec),
    });
    crew.services.workers.add(fake);
    let reference = worker.parse().expect("a worker reference");
    if crew.settings.crew.is_empty() {
        crew.settings.crew.main = Clone::clone(&reference);
    }
    crew.settings.crew.add(reference);
}

fn smallest(spec: &SessionSpec) -> FakeSession {
    match &spec.json_schema {
        Some(schema) => FakeSession::structured(support::fill(schema)),
        None => FakeSession::completed("done"),
    }
}

/// `[roles.worker] goal = "argonauts"`: every task the workers take is worked as a goal, with
/// the task's own done-when lines. A task that names none is done when its own tests pass.
#[tokio::test]
async fn a_role_works_each_task_as_a_goal() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.divide.contracts_first = false;
    crew.settings.checks.run = vec![CheckCommand {
        name: "tests".to_owned(),
        command: vec!["true".to_owned()],
    }];
    crew.settings.roles.insert(
        RoleName::Worker,
        RoleSettings {
            goal: Some(RoleGoal::Argonauts),
            ..RoleSettings::default()
        },
    );
    member(&mut crew, "claude-code/opus", "anthropic");
    member(&mut crew, "codex/gpt-x", "openai");
    // The task's own tests fail after its first try.
    crew.services.checks.fail_next("11 passed, 1 failed");
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 8).await?;
    let events = &outcomes.last().expect("an outcome").events;

    let goal = task_goal(events);
    assert_eq!(goal.scope, GoalScope::Task);
    assert_eq!(goal.kept_by, KeptBy::Argonauts);
    assert_eq!(goal.kept_note, None, "it was asked of Argonauts Code");
    assert_eq!(goal.criteria.len(), 1);
    assert_eq!(goal.criteria[0].text, "the task's own tests pass");
    let checked = checks(events);
    assert_eq!(
        checked
            .iter()
            .map(|checked| checked.overall)
            .collect::<Vec<_>>(),
        [GoalVerdict::NotYet, GoalVerdict::Met],
        "{checked:?}"
    );
    // The failing tests are a failed try of the goal: what failed goes back as the next try,
    // with no fixed number of tries, where a task without a goal gets its checks back once.
    assert!(
        checked[0].verdicts[0].missing.contains("1 of 12 fails"),
        "{:?}",
        checked[0].verdicts
    );
    let causes = causes(events);
    assert!(causes.contains(&TryCause::GoalRetry), "{causes:?}");
    assert!(!causes.contains(&TryCause::CheckRetry), "{causes:?}");
    // The run itself took no goal, since none was set: only its tasks did.
    let reports = report::reports(events);
    let [told] = reports.as_slice() else {
        panic!("one goal, the task's: {reports:?}");
    };
    assert!(told.task.is_some());
    assert_eq!(told.row_words(), "goal met in 2 tries");
    assert_eq!(told.lines[0].checked_by, "your checks: tests");
    Ok(())
}

/// A task of the plan, owned by `worker`.
fn task_of(worker: &str) -> argonauts_core::events::task::TaskSpec {
    argonauts_core::events::task::TaskSpec {
        id: argonauts_core::TaskId::new("task-001").expect("an id"),
        title: "Add the overdue command".to_owned(),
        task: "Add a command that lists what's overdue.".to_owned(),
        kind: argonauts_core::events::task::TaskKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        worker: Some(worker.parse().expect("a model")),
        reviewers: Vec::new(),
        limit: argonauts_core::events::task::TaskLimit::default(),
        predictions: None,
        item: None,
    }
}

/// A step's `until`, with its `kept_by`, is the workflow runner's to hand over: the task is
/// worked as that goal whatever its role's setting says, kept by Codex's own goal on Codex and
/// by Argonauts Code on a CLI that has none.
#[tokio::test]
async fn a_steps_until_is_the_goal_of_its_task() -> anyhow::Result<()> {
    use argonauts_core::goal::{CheckMeans, CheckWith, GoalFrom, GoalSpec};
    use argonauts_crew::goal::task;

    let until = GoalSpec::from_words(
        "its own tests pass",
        CheckWith::Auto,
        CheckMeans {
            project_checks: true,
        },
        GoalFrom::Step,
    )
    .map_err(anyhow::Error::msg)?;
    for (worker, kept_by, note) in [
        ("codex/gpt-x", KeptBy::Cli, None),
        (
            "claude-code/opus",
            KeptBy::Argonauts,
            Some("Claude Code has no goal of its own"),
        ),
    ] {
        let crew = Crew::new()?;
        let started = crew.run_started(RunMode::Crew, REQUEST)?;
        let spec = task_of(worker);
        let id = spec.id.clone();
        let created = RunEvent {
            seq: 1,
            task_id: Some(id.clone()),
            kind: RunEventKind::Task(TaskEvent::Created(Box::new(spec))),
            ..started.clone()
        };
        let (host, sink) = crew.host_over(vec![started, created])?;
        task::work_as(&host, &id, &until, RoleGoal::Cli)?;
        // Handing it over twice changes nothing, and `off` hands nothing over.
        task::work_as(&host, &id, &until, RoleGoal::Cli)?;
        let events = sink.events();
        let goals: Vec<GoalRun> = goal_events(&events)
            .into_iter()
            .filter_map(|event| match event {
                GoalEvent::Set(goal) => Some((**goal).clone()),
                _ => None,
            })
            .collect();
        let [goal] = goals.as_slice() else {
            panic!("{worker}: one goal: {goals:?}");
        };
        assert_eq!(goal.task.as_ref(), Some(&id), "{worker}");
        assert_eq!(goal.scope, GoalScope::Step, "{worker}");
        assert_eq!(goal.kept_by, kept_by, "{worker}");
        assert_eq!(goal.kept_note.as_deref(), note, "{worker}");
        assert_eq!(goal.objective, "its own tests pass", "{worker}");
        assert!(host.read(|state| task::pursued(state, &id)), "{worker}");
    }
    Ok(())
}

/// A run with one task under way in a worktree of its own, worked as a goal with one line a
/// command checks and one the judge reads. The judge, from another provider than the work,
/// finds its line met each time it reads.
fn host_with_a_tasks_goal(
    crew: &mut Crew,
) -> anyhow::Result<(
    argonauts_crew::engine::host::Host,
    Arc<argonauts_testkit::MemorySink>,
    argonauts_core::TaskId,
    Arc<FakeRunner>,
)> {
    use argonauts_core::goal::{CheckMeans, CheckWith, GoalFrom, GoalSpec};

    crew.worker("codex/gpt-x", "openai");
    let judge = crew.worker("claude-code/opus", "anthropic");
    judge.respond(|_| {
        FakeSession::structured(json!({
            "verdicts": [{
                "id": "G2",
                "verdict": "met",
                "evidence": "README.md names the command",
                "missing": "",
            }],
            "overall": "met",
            "reason": "",
        }))
    });
    let started = crew.run_started(RunMode::Crew, REQUEST)?;
    let spec = task_of("codex/gpt-x");
    let id = spec.id.clone();
    let of_task = |seq: u64, event: TaskEvent| RunEvent {
        seq,
        task_id: Some(id.clone()),
        kind: RunEventKind::Task(event),
        ..started.clone()
    };
    let history = vec![
        started.clone(),
        of_task(1, TaskEvent::Created(Box::new(spec))),
        of_task(
            2,
            TaskEvent::Worktree {
                path: crew.repo.root().to_path_buf(),
                branch: "argonauts/run-01/task-001".to_owned(),
                base: crew.repo.head()?,
            },
        ),
    ];
    let (host, sink) = crew.host_over(history)?;
    let until = GoalSpec::from_words(
        "`make check` passes and the README names the command",
        CheckWith::Auto,
        CheckMeans::default(),
        GoalFrom::Step,
    )
    .map_err(anyhow::Error::msg)?;
    argonauts_crew::goal::task::work_as(&host, &id, &until, RoleGoal::Argonauts)?;
    Ok((host, sink, id, judge))
}

/// What `c` on a goal sends the engine.
async fn check_now(
    host: &argonauts_crew::engine::host::Host,
    task: Option<&argonauts_core::TaskId>,
) -> Result<(), String> {
    let action = argonauts_core::events::goal::GoalAction::CheckNow {
        task: task.cloned(),
    };
    argonauts_crew::goal::pursue::act(host, action).await
}

/// `c` on an agent's goal: its checks run now in the task's worktree, beside the try under
/// way. What they find shows as the last check, and the try stays open: only its own check
/// sends the work round again or ends the goal.
#[tokio::test]
async fn the_checks_run_now_when_you_ask_and_decide_nothing() -> anyhow::Result<()> {
    use argonauts_core::goal::report::{Failing, FailingWhat};
    use argonauts_crew::goal::task;

    let mut crew = Crew::new()?;
    let (host, sink, id, judge) = host_with_a_tasks_goal(&mut crew)?;

    crew.services.checks.push(failing(1));
    check_now(&host, Some(&id))
        .await
        .map_err(anyhow::Error::msg)?;
    // Asked again while they run, the same commands don't run beside the first.
    assert_eq!(
        check_now(&host, Some(&id)).await,
        Err("its checks are already running".to_owned())
    );
    // While they run they count as work under way, which is what holds the run from waiting.
    let checking = || host.shared().checking_goals.lock().expect("the lock").len();
    assert_eq!(checking(), 1);
    host.shared().tasks.wait().await;
    assert_eq!(checking(), 0);

    let events = sink.events();
    let asked: Vec<&GoalEvent> = goal_events(&events)
        .into_iter()
        .filter(|event| {
            matches!(
                event,
                GoalEvent::CheckingNow { .. } | GoalEvent::CheckedNow(_)
            )
        })
        .collect();
    let [
        GoalEvent::CheckingNow { try_number: 1, .. },
        GoalEvent::CheckedNow(found),
    ] = asked.as_slice()
    else {
        panic!("asked, then answered: {asked:?}");
    };
    assert_eq!(found.overall, GoalVerdict::NotYet);
    assert_eq!(
        found
            .checks
            .iter()
            .map(|run| (run.name.as_str(), run.passed))
            .collect::<Vec<_>>(),
        [("make check", false), ("the judge", true)]
    );
    // It decides nothing: no check of the try is recorded, and the goal is still pursued.
    assert!(checks(&events).is_empty());
    assert!(host.read(|state| state.goal.checks.is_empty()));
    assert!(host.read(|state| task::pursued(state, &id)));
    let said = events.iter().any(|event| match &event.kind {
        RunEventKind::Workflow(argonauts_core::events::workflow::WorkflowEvent::Note(note)) => {
            note.text.contains("checked now: 1 of 2 lines met")
                && note.text.contains("make check: 1 of 12 fails")
        }
        _ => false,
    });
    assert!(said, "{events:?}");

    let reports = report::reports(&events);
    let [told] = reports.as_slice() else {
        panic!("one goal, the task's: {reports:?}");
    };
    assert!(!told.checking);
    assert_eq!(told.row_words(), "try 1 · 1 of 2 lines met");
    assert!(told.tries[0].running);
    assert_eq!(
        told.tries[0].still_failing,
        Some(Failing {
            count: 1,
            what: FailingWhat::Tests,
        })
    );

    // Asked again, everything runs again: the judge reads afresh and never answers with what
    // it found before.
    check_now(&host, Some(&id))
        .await
        .map_err(anyhow::Error::msg)?;
    host.shared().tasks.wait().await;
    let events = sink.events();
    let holds = goal_events(&events)
        .into_iter()
        .rev()
        .find_map(|event| match event {
            GoalEvent::CheckedNow(found) => Some(found.overall),
            _ => None,
        });
    assert_eq!(holds, Some(GoalVerdict::Met));
    assert_eq!(judge.specs().len(), 2, "a fresh judge each time");
    assert!(host.read(|state| task::pursued(state, &id)));
    Ok(())
}

/// There's nothing to run for a task that isn't worked as a goal, and a workflow's own goal
/// is checked when its round ends.
#[tokio::test]
async fn the_checks_cant_run_where_theres_no_goal_of_one_task() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let (host, sink, _, _) = host_with_a_tasks_goal(&mut crew)?;
    let other = argonauts_core::TaskId::new("task-009").map_err(anyhow::Error::msg)?;
    assert_eq!(
        check_now(&host, Some(&other)).await,
        Err("that task isn't worked as a goal".to_owned())
    );
    let whole = check_now(&host, None).await.expect_err("no task named");
    assert!(whole.contains("checked when its round ends"), "{whole}");
    assert!(
        !goal_events(&sink.events())
            .iter()
            .any(|event| matches!(event, GoalEvent::CheckingNow { .. }))
    );
    Ok(())
}

/// A check you asked for is dropped when the run pauses under it, and says so, so a pause
/// never waits on it and the screen never shows it running for ever.
#[tokio::test]
async fn a_pause_drops_the_checks_you_asked_for() -> anyhow::Result<()> {
    use argonauts_crew::engine::shared::Admission;

    let mut crew = Crew::new()?;
    let (host, sink, id, _) = host_with_a_tasks_goal(&mut crew)?;
    check_now(&host, Some(&id))
        .await
        .map_err(anyhow::Error::msg)?;
    host.shared().admission.send_replace(Admission::Paused);
    host.shared().tasks.wait().await;

    let events = sink.events();
    let dropped = goal_events(&events)
        .into_iter()
        .find_map(|event| match event {
            GoalEvent::CheckNowDropped { why, .. } => Some(why.clone()),
            _ => None,
        });
    assert_eq!(dropped.as_deref(), Some("the run paused or stopped"));
    assert!(
        !goal_events(&events)
            .iter()
            .any(|event| matches!(event, GoalEvent::CheckedNow(_)))
    );
    assert!(!report::reports(&events)[0].checking);
    // Nothing new starts while it's paused.
    let refused = check_now(&host, Some(&id)).await.expect_err("paused");
    assert!(refused.contains("pausing or stopping"), "{refused}");
    Ok(())
}

/// A run waiting on your approval still takes `c`: its engine wakes, runs the checks, and
/// waits again only once they answered.
#[tokio::test]
async fn a_waiting_run_runs_the_checks_before_it_waits_again() -> anyhow::Result<()> {
    use argonauts_core::engine::EngineCommand;
    use argonauts_core::events::goal::GoalAction;
    use argonauts_core::events::run::RunLifecycle;

    let mut crew = Crew::new()?;
    crew.settings.divide.contracts_first = false;
    crew.settings.checks.run = vec![CheckCommand {
        name: "tests".to_owned(),
        command: vec!["true".to_owned()],
    }];
    crew.settings.roles.insert(
        RoleName::Worker,
        RoleSettings {
            goal: Some(RoleGoal::Argonauts),
            ..RoleSettings::default()
        },
    );
    member(&mut crew, "claude-code/opus", "anthropic");
    member(&mut crew, "codex/gpt-x", "openai");
    // Up to the stretch that waits with the task's goal set, and no further: a run that ends
    // takes its worktrees with it.
    let works_a_task_as_a_goal = |outcome: &support::Outcome| {
        matches!(outcome.exit, EngineExit::Waiting(_))
            && goal_events(&outcome.events)
                .iter()
                .any(|event| matches!(event, GoalEvent::Set(goal) if goal.task.is_some()))
    };
    let mut waiting = crew.start(RunMode::Crew, REQUEST).await?;
    for _ in 0..8 {
        if works_a_task_as_a_goal(&waiting) || !matches!(waiting.exit, EngineExit::Waiting(_)) {
            break;
        }
        let (command, _reply) =
            support::recommended(&waiting.events).expect("a question to answer");
        waiting = crew.resume(waiting.events.clone(), vec![command]).await?;
    }
    assert!(
        works_a_task_as_a_goal(&waiting),
        "the run waited with a task worked as a goal"
    );
    let task = task_goal(&waiting.events).task.expect("a task's goal");

    let (reply, replied) = tokio::sync::oneshot::channel();
    let command = EngineCommand::Goal {
        action: GoalAction::CheckNow {
            task: Some(task.clone()),
        },
        reply,
    };
    let again = crew.resume(waiting.events.clone(), vec![command]).await?;
    assert_eq!(replied.await?, Ok(()));
    assert!(
        matches!(again.exit, EngineExit::Waiting(_)),
        "{:?}",
        again.exit
    );
    let new = &again.events[waiting.events.len()..];
    let order: Vec<&str> = new
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Goal(GoalEvent::CheckingNow { .. }) => Some("checking"),
            RunEventKind::Goal(GoalEvent::CheckedNow(_)) => Some("checked"),
            RunEventKind::Goal(GoalEvent::CheckNowDropped { .. }) => Some("dropped"),
            RunEventKind::Run(RunLifecycle::Waiting { .. }) => Some("waiting"),
            _ => None,
        })
        .collect();
    assert_eq!(order, ["checking", "checked", "waiting"]);
    let told = report::reports(&again.events);
    assert!(told.iter().all(|goal| !goal.checking));
    // The try's check that met the goal still stands; the one you asked for added none.
    assert_eq!(checks(&again.events).len(), checks(&waiting.events).len());
    Ok(())
}
