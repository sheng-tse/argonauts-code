//! A condition a workflow's own steps keep, asked about the way its runner asks: the `goal` at
//! the top of a file goes round the whole workflow, and a step's `until` is the goal of the
//! task its writer works. The runner makes the tries; each answer says whether the condition
//! holds, what still fails, and whether to stop.

use crate::support;
use std::sync::Arc;

use argonauts_core::TaskId;
use argonauts_core::events::goal::{GoalEvent, GoalStatus};
use argonauts_core::events::run::RunMode;
use argonauts_core::events::task::{CheckKind, CheckOutcome};
use argonauts_core::events::task::{TaskEvent, TaskKind, TaskLimit, TaskSpec};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::goal::{GoalScope, KeptBy};
use argonauts_core::names::WorkflowId;
use argonauts_core::workflow::StepRef;
use argonauts_crew::engine::host::Host;
use argonauts_crew::goal::report;
use argonauts_crew::goal::until::{self, Asked, Held};
use argonauts_crew::workflow::StepAt;
use argonauts_testkit::MemorySink;

use support::Crew;

const REQUEST: &str = "Add the overdue command";
const CONDITION: &str = "`make check` passes";

/// The workflow's one step, where its runner asks from.
fn at() -> StepAt {
    StepAt {
        step: StepRef {
            workflow: WorkflowId::numbered(1),
            phase: 0,
            path: vec![0],
        },
        round: 1,
    }
}

/// The file's goal, asked about after round `round`.
fn the_files_goal(round: u32) -> Asked<'static> {
    Asked {
        condition: CONDITION,
        commands: &[],
        step: None,
        task: None,
        kept_by: None,
        try_number: None,
        round: Some(round),
        max_rounds: None,
    }
}

/// `make check` failing `failed` of its 12 tests.
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

fn statuses(events: &[RunEvent]) -> Vec<(GoalStatus, Option<String>)> {
    goal_events(events)
        .into_iter()
        .filter_map(|event| match event {
            GoalEvent::StatusChanged { status, why, .. } => Some((*status, why.clone())),
            _ => None,
        })
        .collect()
}

/// A crew run that has only started, over a host a test asks as the runner would.
fn crew_run(crew: &mut Crew) -> anyhow::Result<(Host, Arc<MemorySink>)> {
    crew.worker("codex/gpt-x", "openai");
    crew.worker("claude-code/opus", "anthropic");
    let started = crew.run_started(RunMode::Crew, REQUEST)?;
    crew.host_over(vec![started])
}

/// `goal =` at the top of a file: the run takes it before round 1, each round is a try, and
/// what still fails goes back for the next round until it holds.
#[tokio::test]
async fn a_files_goal_goes_round_until_it_holds() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let (host, sink) = crew_run(&mut crew)?;
    let workdir = crew.repo.root().to_path_buf();

    until::around(&host, &at(), &the_files_goal(1))?;
    let goal = host
        .read(|state| state.goal.pursuit.clone())
        .expect("the run took the file's goal");
    assert_eq!(goal.objective, CONDITION);
    assert_eq!(goal.scope, GoalScope::Workflow);
    assert_eq!(goal.kept_by, KeptBy::Argonauts);
    assert_eq!(goal.max_rounds, None, "no fixed number of rounds");

    crew.services.checks.push(failing(2));
    let first = until::held(&host, &at(), &the_files_goal(1), &workdir).await?;
    assert!(!first.holds && !first.stop, "{first:?}");
    assert!(
        first.failed.contains("make check: 2 of 12 fail"),
        "{first:?}"
    );
    assert!(
        first.failed.contains("overdue::skips_tasks_without_a_date"),
        "{first:?}"
    );

    // Asked about round 1 again, as when the run picks its script up: the same answer, and
    // nothing is checked or recorded twice.
    let before = sink.events().len();
    let again = until::held(&host, &at(), &the_files_goal(1), &workdir).await?;
    assert_eq!(again, first);
    assert_eq!(sink.events().len(), before);
    assert_eq!(crew.services.checks.requests().len(), 1);

    let second = until::held(&host, &at(), &the_files_goal(2), &workdir).await?;
    assert_eq!(
        second,
        Held {
            holds: true,
            failed: String::new(),
            stop: false
        }
    );
    let events = sink.events();
    assert_eq!(statuses(&events), [(GoalStatus::Met, None)]);
    let reports = report::reports(&events);
    let [told] = reports.as_slice() else {
        panic!("one goal, the run's: {reports:?}");
    };
    assert_eq!(told.task, None);
    assert_eq!(told.row_words(), "goal met in 2 tries");
    assert_eq!(told.tries[1].what, "Round 2");
    Ok(())
}

/// A file's goal that fails the same way round after round changes approach once, then stops
/// and says why. No fixed number of rounds ends it.
#[tokio::test]
async fn a_files_goal_that_is_stuck_changes_approach_once_then_stops() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.goal.stuck_after = 2;
    let (host, sink) = crew_run(&mut crew)?;
    let workdir = crew.repo.root().to_path_buf();

    let mut answers = Vec::new();
    for round in 1..=3 {
        crew.services.checks.push(failing(2));
        answers.push(until::held(&host, &at(), &the_files_goal(round), &workdir).await?);
    }
    let [first, second, third] = answers.as_slice() else {
        unreachable!()
    };
    assert!(!first.stop && !first.failed.contains("different approach"));
    assert!(!second.stop, "{second:?}");
    assert!(
        second.failed.contains("Take a different approach"),
        "{second:?}"
    );
    assert_eq!(
        *third,
        Held {
            holds: false,
            failed: "the same failure 2 rounds running, after a change of approach".to_owned(),
            stop: true,
        }
    );
    let events = sink.events();
    assert_eq!(
        statuses(&events),
        [(
            GoalStatus::Stuck,
            Some("the same failure 2 rounds running, after a change of approach".to_owned())
        )]
    );
    // Asked once more, it stays stopped and nothing more is checked.
    let after = until::held(&host, &at(), &the_files_goal(4), &workdir).await?;
    assert!(after.stop && !after.holds, "{after:?}");
    assert_eq!(crew.services.checks.requests().len(), 3);
    Ok(())
}

/// A file's goal you pause: the round under way is checked, and no other follows.
#[tokio::test]
async fn a_files_goal_you_pause_gets_no_further_round() -> anyhow::Result<()> {
    use argonauts_core::events::goal::GoalAction;
    use argonauts_crew::goal::pursue;

    let mut crew = Crew::new()?;
    let (host, sink) = crew_run(&mut crew)?;
    let workdir = crew.repo.root().to_path_buf();
    until::around(&host, &at(), &the_files_goal(1))?;
    pursue::act(&host, GoalAction::Pause { task: None })
        .await
        .map_err(anyhow::Error::msg)?;

    crew.services.checks.push(failing(1));
    let paused = until::held(&host, &at(), &the_files_goal(1), &workdir).await?;
    assert_eq!(
        paused,
        Held {
            holds: false,
            failed: pursue::YOU_PAUSED.to_owned(),
            stop: true,
        }
    );
    // The round was still checked, so the panels show where it stands.
    let told = &report::reports(&sink.events())[0];
    assert_eq!(told.status, GoalStatus::Paused);
    assert_eq!(told.lines[0].still_fails, "1 of 12 fails");
    Ok(())
}

/// A task of the plan, owned by `worker`, with a worktree of its own.
fn task_events(crew: &Crew, started: &RunEvent, worker: &str) -> anyhow::Result<Vec<RunEvent>> {
    let id = TaskId::numbered(1);
    let spec = TaskSpec {
        id: id.clone(),
        title: "Add the overdue command".to_owned(),
        task: "Add a command that lists what's overdue.".to_owned(),
        kind: TaskKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        worker: Some(worker.parse().expect("a model")),
        reviewers: Vec::new(),
        limit: TaskLimit::default(),
        predictions: None,
        item: None,
        way: None,
    };
    let of_task = |seq: u64, event: TaskEvent| RunEvent {
        seq,
        task_id: Some(id.clone()),
        kind: RunEventKind::Task(event),
        ..started.clone()
    };
    Ok(vec![
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
    ])
}

/// A step's `until` is the goal of the task its writer works, kept as the step's `kept_by`
/// says: handed over before the writer's first turn, and checked after a try by the same
/// code that checks any task's goal.
#[tokio::test]
async fn a_steps_until_is_asked_about_as_its_tasks_goal() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.worker("codex/gpt-x", "openai");
    crew.worker("claude-code/opus", "anthropic");
    let started = crew.run_started(RunMode::Crew, REQUEST)?;
    let (host, sink) = crew.host_over(task_events(&crew, &started, "codex/gpt-x")?)?;
    let workdir = crew.repo.root().to_path_buf();
    let task = TaskId::numbered(1);
    let asked = |try_number: u32| Asked {
        condition: CONDITION,
        commands: &[],
        step: Some("Build"),
        task: Some(&task),
        kept_by: Some("cli"),
        try_number: Some(try_number),
        round: None,
        max_rounds: None,
    };

    until::work_as(&host, &asked(1))?;
    let goal = host
        .read(|state| state.goal.goal_of(Some(&task)).cloned())
        .expect("the task took the step's until as its goal");
    assert_eq!(goal.scope, GoalScope::Step);
    assert_eq!(goal.kept_by, KeptBy::Cli, "Codex has a goal of its own");
    assert_eq!(goal.objective, CONDITION);
    // The panels can say whose goal keeps it before its agent's first session.
    let set = &report::reports(&sink.events())[0];
    assert_eq!(set.kept.cli.as_deref(), Some("Codex"));
    assert_eq!(
        set.kept_words(),
        "kept by Codex's own goal · checked by Argonauts Code"
    );
    assert_eq!(set.role(), Some("worker"));

    crew.services.checks.push(failing(1));
    let first = until::held(&host, &at(), &asked(1), &workdir).await?;
    assert!(!first.holds && !first.stop, "{first:?}");
    assert!(
        first.failed.contains("make check: 1 of 12 fails"),
        "{first:?}"
    );
    let second = until::held(&host, &at(), &asked(2), &workdir).await?;
    assert!(second.holds, "{second:?}");
    // Once it holds, asking again checks nothing more.
    let again = until::held(&host, &at(), &asked(3), &workdir).await?;
    assert!(again.holds);
    assert_eq!(crew.services.checks.requests().len(), 2);

    let events = sink.events();
    assert_eq!(statuses(&events), [(GoalStatus::Met, None)]);
    let told = &report::reports(&events)[0];
    assert_eq!(told.task.as_ref(), Some(&task));
    assert_eq!(told.row_words(), "goal met in 2 tries");

    // A script that asks about the task's work by itself gets its own answer, not the goal's.
    crew.services.checks.push(failing(3));
    let its_own = Asked {
        condition: "`make lint` passes",
        step: None,
        try_number: None,
        ..asked(1)
    };
    let told = until::held(&host, &at(), &its_own, &workdir).await?;
    assert!(!told.holds && !told.stop, "{told:?}");
    assert_eq!(statuses(&sink.events()), [(GoalStatus::Met, None)]);

    // Who keeps it is cli or argonauts, and nothing else.
    let unknown = Asked {
        kept_by: Some("codex"),
        task: Some(&task),
        ..asked(1)
    };
    let refused = until::work_as(&host, &unknown).expect_err("an unknown keeper");
    assert!(
        refused.to_string().contains("kept_by is cli or argonauts"),
        "{refused}"
    );
    Ok(())
}

/// A step that works on no task has no goal to record its tries under: its `until` is still
/// checked, and it stops after the tries a stuck goal gets.
#[tokio::test]
async fn a_step_with_no_task_is_checked_and_stops_when_it_never_holds() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.goal.stuck_after = 2;
    let (host, sink) = crew_run(&mut crew)?;
    let workdir = crew.repo.root().to_path_buf();
    let asked = |try_number: u32| Asked {
        condition: CONDITION,
        commands: &[],
        step: Some("Review"),
        task: None,
        kept_by: None,
        try_number: Some(try_number),
        round: None,
        max_rounds: None,
    };
    let mut answers = Vec::new();
    for try_number in 1..=3 {
        crew.services.checks.push(failing(1));
        answers.push(until::held(&host, &at(), &asked(try_number), &workdir).await?);
    }
    assert!(!answers[0].stop && answers[0].failed.contains("make check: 1 of 12 fails"));
    assert!(answers[1].failed.contains("Take a different approach"));
    assert_eq!(
        answers[2],
        Held {
            holds: false,
            failed: "it didn't hold in 3 tries, after a change of approach".to_owned(),
            stop: true,
        }
    );
    let holds = until::held(&host, &at(), &asked(4), &workdir).await?;
    assert!(holds.holds);
    // The run took no goal from it, and no goal's events were recorded.
    assert!(host.read(|state| state.goal.pursuit.is_none()));
    assert!(goal_events(&sink.events()).is_empty());
    Ok(())
}

/// `/goal`, or a request's own words, around a workflow whose file states no goal: the run's
/// goal is what its rounds are asked about, and it comes before a file's.
#[tokio::test]
async fn the_goal_a_run_was_started_with_goes_round_its_workflow() -> anyhow::Result<()> {
    use argonauts_core::goal::GoalFrom;

    let mut crew = Crew::new()?;
    let (host, _) = crew_run(&mut crew)?;
    assert_eq!(until::for_run(&host), None, "no goal, so no rounds");

    let mut crew = Crew::new()?;
    crew.settings.goal.until = Some("`make docs` passes".to_owned());
    let (host, sink) = crew_run(&mut crew)?;
    let workdir = crew.repo.root().to_path_buf();
    assert_eq!(until::for_run(&host).as_deref(), Some("`make docs` passes"));

    // The file states another goal: the one the run was started with is kept.
    let first = until::held(&host, &at(), &the_files_goal(1), &workdir).await?;
    assert!(first.holds, "{first:?}");
    let goal = host
        .read(|state| state.goal.pursuit.clone())
        .expect("the run's goal");
    assert_eq!(goal.objective, "`make docs` passes");
    assert_eq!(goal.from, Some(GoalFrom::Setting));
    let ran: Vec<String> = crew
        .services
        .checks
        .requests()
        .iter()
        .map(|request| request.name.clone())
        .collect();
    assert_eq!(ran, ["make docs"]);
    assert_eq!(statuses(&sink.events()), [(GoalStatus::Met, None)]);
    assert_eq!(until::for_run(&host).as_deref(), Some("`make docs` passes"));
    Ok(())
}

/// A script may ask about a condition of its own. It's told whether that condition holds, and
/// the question is never mistaken for the run's goal: nothing is kept, and a run that has a
/// goal is still asked about the script's condition.
#[tokio::test]
async fn a_script_that_asks_by_itself_is_told_about_its_own_condition() -> anyhow::Result<()> {
    let its_own = Asked {
        try_number: None,
        round: None,
        ..the_files_goal(1)
    };

    let mut crew = Crew::new()?;
    let (host, sink) = crew_run(&mut crew)?;
    let workdir = crew.repo.root().to_path_buf();
    crew.services.checks.push(failing(2));
    let first = until::held(&host, &at(), &its_own, &workdir).await?;
    assert!(!first.holds && !first.stop, "{first:?}");
    assert!(
        first.failed.contains("make check: 2 of 12 fail"),
        "{first:?}"
    );
    assert!(until::held(&host, &at(), &its_own, &workdir).await?.holds);
    assert!(host.read(|state| state.goal.pursuit.is_none()));
    assert!(goal_events(&sink.events()).is_empty());

    // The run's own goal holds, and the script's condition is still what is checked.
    let mut crew = Crew::new()?;
    crew.settings.goal.until = Some("`make docs` passes".to_owned());
    let (host, sink) = crew_run(&mut crew)?;
    let workdir = crew.repo.root().to_path_buf();
    assert!(
        until::held(&host, &at(), &the_files_goal(1), &workdir)
            .await?
            .holds
    );
    crew.services.checks.push(failing(1));
    let asked = until::held(&host, &at(), &its_own, &workdir).await?;
    assert!(!asked.holds && !asked.stop, "{asked:?}");
    assert!(
        asked.failed.contains("make check: 1 of 12 fails"),
        "{asked:?}"
    );
    assert_eq!(statuses(&sink.events()), [(GoalStatus::Met, None)]);
    Ok(())
}

/// A workflow file's `check = [...]` beside its `goal`: the runner asks about the condition
/// alone, and the commands are read from the file in use.
#[tokio::test]
async fn a_files_goal_is_checked_by_the_commands_beside_it() -> anyhow::Result<()> {
    const GOAL: &str = "every acceptance test passes";
    const FILE: &str = r#"name  = "spec-first"
goal  = "every acceptance test passes"
check = ["cargo test --test acceptance"]

[[step]]
name = "Build"
role = "worker"
"#;
    let mut crew = Crew::new()?;
    crew.settings.workflow = "spec-first".to_owned();
    crew.repo
        .write(".argonauts/workflows/spec-first.toml", FILE)?;
    let (host, sink) = crew_run(&mut crew)?;
    let workdir = crew.repo.root().to_path_buf();
    let asked = Asked {
        condition: GOAL,
        ..the_files_goal(1)
    };

    assert!(until::held(&host, &at(), &asked, &workdir).await?.holds);
    let goal = host
        .read(|state| state.goal.pursuit.clone())
        .expect("the run took the file's goal");
    assert_eq!(goal.commands, ["cargo test --test acceptance"]);
    let ran: Vec<String> = crew
        .services
        .checks
        .requests()
        .iter()
        .map(|request| request.name.clone())
        .collect();
    assert_eq!(ran, ["cargo test --test acceptance"]);
    assert_eq!(statuses(&sink.events()), [(GoalStatus::Met, None)]);

    // Commands the runner hands over are the ones used, and another file's goal gets none.
    let mut crew = Crew::new()?;
    crew.settings.workflow = "spec-first".to_owned();
    crew.repo
        .write(".argonauts/workflows/spec-first.toml", FILE)?;
    let (host, _) = crew_run(&mut crew)?;
    let handed = ["make acceptance".to_owned()];
    let asked = Asked {
        condition: GOAL,
        commands: &handed,
        ..the_files_goal(1)
    };
    until::around(&host, &at(), &asked)?;
    let goal = host.read(|state| state.goal.pursuit.clone()).expect("set");
    assert_eq!(goal.commands, ["make acceptance"]);

    let mut crew = Crew::new()?;
    crew.settings.workflow = "spec-first".to_owned();
    crew.repo
        .write(".argonauts/workflows/spec-first.toml", FILE)?;
    let (host, _) = crew_run(&mut crew)?;
    until::around(&host, &at(), &the_files_goal(1))?;
    let goal = host.read(|state| state.goal.pursuit.clone()).expect("set");
    assert!(goal.commands.is_empty(), "{:?}", goal.commands);
    Ok(())
}
