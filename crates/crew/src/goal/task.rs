//! A task worked as a goal: its agent tries, the check runs, and what still fails goes back to
//! the same session as the next try, inside the task's own phase. A role works its tasks this
//! way when `[roles.<role>] goal` says so, and a Captain run's one task does when the run has a
//! goal.
//!
//! Who keeps it going is the CLI's own goal where it has one and you asked for it (`cli`, which is
//! Codex's), else Argonauts Code. Either way Argonauts Code runs the check after each try: the
//! task's own checks first, then the goal's lines. When it holds the task goes to review like any
//! other. Stuck, it changes approach once, then stops being sent round and says why.

use argonauts_core::TaskId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::goal::{
    CliGoalSaid, Criterion, CriterionCheck, GoalChecked, GoalEvent, GoalRun, GoalStatus,
};
use argonauts_core::events::run::{Party, RunMode};
use argonauts_core::events::session::InputPart;
use argonauts_core::events::task::{CheckKind, CheckOutcome, TaskKind};
use argonauts_core::goal::keep::{self, Limits, Next};
use argonauts_core::goal::{GoalDefault, GoalFrom, GoalScope, GoalSpec, KeptBy, RoleGoal};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::Settings;
use argonauts_core::steps::task::{ReportStatus, TaskReport};

use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::labels;
use crate::merge::final_check::FinalCheck;
use crate::state::RunState;
use crate::state::tasks::TaskEntry;
use crate::task::prompt;
use crate::task::support::{record_of, worker_of};
use crate::task::worktree;
use crate::workflow::{StepAt, runner};

use super::check::{self, Place};
use super::cli::{self, Says};
use super::pursue;

/// What follows a try, once the task's own checks ran.
#[derive(Debug, Clone, PartialEq)]
pub enum AfterTry {
    /// The goal holds: the task goes on to review.
    Holds,
    /// Not yet: the same session gets this as its next try.
    Again(InputPart),
    /// The goal stopped being kept, so the task goes on as its own checks say.
    GaveUp,
}

/// Whether `task` is worked as a goal right now: set, and still pursued.
pub fn pursued(state: &RunState, task: &TaskId) -> bool {
    state
        .goal
        .tasks
        .get(task)
        .is_some_and(|kept| kept.goal.status == GoalStatus::Pursuing)
}

/// Before a session of `task` starts. A task whose role works tasks as goals takes its goal
/// here, and a task whose worker changed takes the keeper that worker has. Returns what a fresh
/// session is told about the goal.
pub fn brief(host: &Host, task: &TaskId, worker: &ModelRef) -> EngineResult<Option<InputPart>> {
    let record = record_of(host, task)?;
    let kept = host.read(|state| state.goal.tasks.get(task).map(|kept| kept.goal.clone()));
    let goal = match kept {
        Some(goal) if goal.status != GoalStatus::Pursuing => return Ok(None),
        Some(mut goal) => {
            let asked = match goal.kept_by {
                KeptBy::Cli => RoleGoal::Cli,
                // A goal Argonauts Code keeps in a CLI's place goes back to a CLI that has one.
                KeptBy::Argonauts if goal.kept_note.is_some() => RoleGoal::Cli,
                KeptBy::Argonauts => RoleGoal::Argonauts,
            };
            let (by, note) = keeper(worker, asked);
            if by != goal.kept_by || note != goal.kept_note {
                goal.kept_by = by;
                goal.kept_note = note;
                set(host, task, goal.clone())?;
            }
            goal
        }
        None => {
            let settings = host.settings();
            let Some((spec, asked, scope)) = host.read(|state| wanted(state, settings, &record))
            else {
                return Ok(None);
            };
            begin(host, task, worker, &spec, asked, scope)?
        }
    };
    let spec = goal.spec();
    let text = match goal.kept_by {
        KeptBy::Cli => cli::brief(&goal.criteria, Says::InReport),
        KeptBy::Argonauts => {
            let mut text = String::from(
                "This task is worked as a goal. It is done when all of these hold, and not \
                 before:\n",
            );
            for line in &goal.criteria {
                text.push_str(&format!(
                    "- {} (checked by {})\n",
                    line.text,
                    spec.line_checked_by(line)
                ));
            }
            text.push_str(
                "\nArgonauts Code runs the check itself after you report done, and sends back what \
                 still fails.",
            );
            text
        }
    };
    Ok(Some(prompt::message_part("goal", text)))
}

/// A step's `until`, with its `kept_by`: `task` is worked as `spec` whatever its role's
/// setting says. For the workflow runner, before the task's first session. A task that
/// already has a goal keeps it.
pub fn work_as(host: &Host, task: &TaskId, spec: &GoalSpec, asked: RoleGoal) -> EngineResult<()> {
    if asked == RoleGoal::Off || host.read(|state| state.goal.tasks.contains_key(task)) {
        return Ok(());
    }
    let worker = worker_of(host, &record_of(host, task)?);
    begin(host, task, &worker, spec, asked, GoalScope::Step)?;
    Ok(())
}

/// Sets `task`'s goal and starts its first try.
fn begin(
    host: &Host,
    task: &TaskId,
    worker: &ModelRef,
    spec: &GoalSpec,
    asked: RoleGoal,
    scope: GoalScope,
) -> EngineResult<GoalRun> {
    let (by, note) = keeper(worker, asked);
    let mut goal = GoalRun::new(spec, scope, by);
    goal.task = Some(task.clone());
    goal.kept_note = note;
    goal.max_rounds = goal.max_rounds.or(host.settings().goal.max_rounds);
    set(host, task, goal.clone())?;
    try_started(host, task, 1)?;
    Ok(goal)
}

/// The goal `record`'s task is worked as, who was asked to keep it, and what it sits around:
/// a Captain run's goal for its one task, or the task's own done-when lines where its role works
/// tasks as goals.
fn wanted(
    state: &RunState,
    settings: &Settings,
    record: &TaskEntry,
) -> Option<(GoalSpec, RoleGoal, GoalScope)> {
    let works = matches!(
        record.spec.kind,
        TaskKind::Captain | TaskKind::Build | TaskKind::Chore | TaskKind::Docs
    );
    if !works {
        return None;
    }
    let role = |role: RoleName| settings.roles.get(&role).and_then(|role| role.goal);
    if runner::run_mode(state) == Some(RunMode::Captain) {
        let asked = role(RoleName::Main).unwrap_or(RoleGoal::Cli);
        if asked == RoleGoal::Off {
            return None;
        }
        let spec = match pursue::configured(settings) {
            Some(spec) => spec,
            None if settings.goal.default == GoalDefault::FromRequest => {
                let lines = pursue::planned_criteria(state);
                if lines.is_empty() {
                    return None;
                }
                let mut spec = GoalSpec::from_lines(
                    &pursue::objective(state),
                    lines,
                    settings.goal.check,
                    GoalFrom::Request,
                );
                spec.max_rounds = settings.goal.max_rounds;
                spec
            }
            None => return None,
        };
        return Some((spec, asked, GoalScope::Captain));
    }
    let asked = role(RoleName::Worker).filter(|asked| *asked != RoleGoal::Off)?;
    let lines = own_lines(state, settings, record);
    let spec = GoalSpec::from_lines(
        &record.spec.title,
        lines,
        settings.goal.check,
        GoalFrom::Role,
    );
    Some((spec, asked, GoalScope::Task))
}

/// A task's own done-when lines: the criteria it serves, as the plan wrote them. A task that
/// names none is done when its own tests pass, which the project's checks decide where there
/// are some and the judge reads where there aren't.
fn own_lines(state: &RunState, settings: &Settings, record: &TaskEntry) -> Vec<Criterion> {
    let known = pursue::planned_criteria(state);
    let lines: Vec<Criterion> = record
        .spec
        .criteria
        .iter()
        .filter_map(|id| known.iter().find(|line| &line.id == id).cloned())
        .collect();
    if !lines.is_empty() {
        return lines;
    }
    let mut line = Criterion::new("T1", "the task's own tests pass");
    if settings.checks.run.is_empty() {
        line.text = format!("the task is done: {}", record.spec.title);
    } else {
        line.check = CriterionCheck::Checks;
    }
    vec![line]
}

/// Who keeps a goal `asked` of `worker`'s CLI, and why it's Argonauts Code when the CLI was asked.
fn keeper(worker: &ModelRef, asked: RoleGoal) -> (KeptBy, Option<String>) {
    match asked {
        RoleGoal::Cli if cli::has_own_goal(worker) => (KeptBy::Cli, None),
        RoleGoal::Cli => (
            KeptBy::Argonauts,
            Some(format!("{} has no goal of its own", cli::name(worker))),
        ),
        RoleGoal::Argonauts | RoleGoal::Off => (KeptBy::Argonauts, None),
    }
}

/// After the task's agent reported done. Where its CLI keeps the goal, what the CLI says about
/// it is recorded, and a CLI that says the goal is still active gets its session back to carry
/// on, which this returns.
pub fn after_report(
    host: &Host,
    task: &TaskId,
    report: &TaskReport,
) -> EngineResult<Option<InputPart>> {
    let Some((worker, continues)) = host.read(|state| {
        let kept = state.goal.tasks.get(task)?;
        let by_cli = kept.goal.status == GoalStatus::Pursuing && kept.goal.kept_by == KeptBy::Cli;
        let record = state.tasks.get(task)?;
        by_cli.then(|| (worker_of(host, record), kept.continues))
    }) else {
        return Ok(None);
    };
    let Some(said) =
        cli::said(&worker, &report.summary).or_else(|| cli::said(&worker, &report.answer))
    else {
        return Ok(None);
    };
    let still_going = cli::still_going(&said);
    cli_said(host, task, said)?;
    let carries_on =
        still_going && report.status == ReportStatus::Done && continues < cli::MAX_CONTINUES;
    Ok(carries_on.then(|| prompt::message_part("goal", cli::carry_on(Says::InReport))))
}

/// After a try, with the task's own checks run: checks the goal and says what follows.
pub async fn after_try(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    checks_passed: bool,
) -> EngineResult<AfterTry> {
    let Some(goal) = host.read(|state| {
        state
            .goal
            .tasks
            .get(task)
            .filter(|kept| kept.goal.status == GoalStatus::Pursuing)
            .map(|kept| kept.goal.clone())
    }) else {
        return Ok(AfterTry::GaveUp);
    };
    let n = goal.tries.last().map_or(1, |a_try| a_try.n);
    // A check already recorded for this try stands, so a re-entered task never asks again.
    let recorded = host.read(|state| {
        state
            .goal
            .checks
            .iter()
            .rev()
            .find(|checked| checked.try_number == n && checked.task.as_ref() == Some(task))
            .cloned()
    });
    let checked = match recorded {
        Some(checked) => checked,
        None => check_try(host, task, at, &goal, n, checks_passed).await?,
    };
    let settings = host.settings();
    let next = host.read(|state| {
        keep::next(
            &state.goal.checks_of(Some(task)),
            Limits {
                stuck_after: settings.goal.stuck_after,
                max_rounds: goal.max_rounds.or(settings.goal.max_rounds),
            },
            state.goal.approach_changed(Some(task)),
        )
    });
    let change = match next {
        Next::Met => {
            status(host, task, GoalStatus::Met, Party::ArgonautsCode, None)?;
            return Ok(AfterTry::Holds);
        }
        Next::Stop { why } => {
            status(
                host,
                task,
                GoalStatus::Stuck,
                Party::ArgonautsCode,
                Some(why.clone()),
            )?;
            let label = host.read(|state| labels::task_label(state, task));
            pursue::note(host, format!("The goal of {label} is stuck: {why}"))?;
            return Ok(AfterTry::GaveUp);
        }
        Next::Again => None,
        Next::ChangeApproach { why } => {
            host.record(
                Some(task),
                None,
                RunEventKind::Goal(GoalEvent::ApproachChanged {
                    after: n,
                    why: why.clone(),
                    task: Some(task.clone()),
                }),
            )?;
            Some(why)
        }
    };
    try_started(host, task, n + 1)?;
    Ok(AfterTry::Again(send_back(
        &goal,
        &checked,
        change.as_deref(),
    )))
}

/// What goes back to the session as its next try.
fn send_back(goal: &GoalRun, checked: &GoalChecked, change: Option<&str>) -> InputPart {
    let mut text = keep::send_back(&goal.criteria, checked, change);
    if goal.kept_by == KeptBy::Cli {
        text.push_str("\n\n");
        text.push_str(&cli::not_achieved(Says::InReport));
    }
    prompt::message_part("goal check", text)
}

/// The goal's check in the task's own worktree. The task's checks just ran there, so they
/// aren't run again, and the goal doesn't hold while they fail.
async fn check_try(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    goal: &GoalRun,
    n: u32,
    checks_passed: bool,
) -> EngineResult<GoalChecked> {
    let record = record_of(host, task)?;
    let tree = worktree::ensure(host, task).await?;
    let place = Place {
        base: Some(tree.base.clone()),
        task: Some(task.clone()),
        at: Some(at.clone()),
        authors: vec![worker_of(host, &record)],
        project: Some(own_checks(&record, checks_passed)),
        ..Place::new(tree.path, n)
    };
    check::goal(host, &goal.spec(), &place).await
}

/// The task's own checks as they last ran: each command's newest outcome, and any built-in
/// check that failed.
fn own_checks(record: &TaskEntry, passed: bool) -> FinalCheck {
    let mut newest: Vec<&CheckOutcome> = Vec::new();
    for outcome in record.checks.iter().rev() {
        if !newest.iter().any(|seen| seen.name == outcome.name) {
            newest.push(outcome);
        }
    }
    newest.reverse();
    FinalCheck {
        passed,
        checks: newest
            .into_iter()
            .filter(|outcome| !outcome.skipped)
            .filter(|outcome| outcome.kind == CheckKind::Command || !outcome.passed)
            .cloned()
            .collect(),
        problem: None,
    }
}

/// `p` on a task's goal: its try finishes and is checked by the task's own checks alone, and
/// nothing is sent back until it's resumed.
pub fn pause(host: &Host, task: &TaskId) -> EngineResult<()> {
    if host.read(|state| pursued(state, task)) {
        status(
            host,
            task,
            GoalStatus::Paused,
            Party::User,
            Some(pursue::YOU_PAUSED.to_owned()),
        )?;
    }
    Ok(())
}

pub fn resume(host: &Host, task: &TaskId) -> EngineResult<()> {
    let paused = host.read(|state| {
        state
            .goal
            .tasks
            .get(task)
            .is_some_and(|kept| kept.goal.status == GoalStatus::Paused)
    });
    if paused {
        status(host, task, GoalStatus::Pursuing, Party::User, None)?;
    }
    Ok(())
}

fn set(host: &Host, task: &TaskId, goal: GoalRun) -> EngineResult<()> {
    host.record(
        Some(task),
        None,
        RunEventKind::Goal(GoalEvent::Set(Box::new(goal))),
    )?;
    Ok(())
}

fn try_started(host: &Host, task: &TaskId, n: u32) -> EngineResult<()> {
    host.record(
        Some(task),
        None,
        RunEventKind::Goal(GoalEvent::TryStarted {
            n,
            title: format!("try {n}"),
            phase: 0,
            task: Some(task.clone()),
        }),
    )?;
    Ok(())
}

fn cli_said(host: &Host, task: &TaskId, said: CliGoalSaid) -> EngineResult<()> {
    host.record(
        Some(task),
        None,
        RunEventKind::Goal(GoalEvent::CliSaid {
            task: Some(task.clone()),
            said,
        }),
    )?;
    Ok(())
}

fn status(
    host: &Host,
    task: &TaskId,
    status: GoalStatus,
    by: Party,
    why: Option<String>,
) -> EngineResult<()> {
    host.record(
        Some(task),
        None,
        RunEventKind::Goal(GoalEvent::StatusChanged {
            status,
            by,
            task: Some(task.clone()),
            why,
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_keeps_a_goal_only_where_it_has_one() {
        let codex: ModelRef = "codex/gpt-5.6-luna".parse().expect("a model");
        let claude: ModelRef = "claude-code/haiku".parse().expect("a model");
        assert_eq!(keeper(&codex, RoleGoal::Cli), (KeptBy::Cli, None));
        assert_eq!(
            keeper(&claude, RoleGoal::Cli),
            (
                KeptBy::Argonauts,
                Some("Claude Code has no goal of its own".to_owned())
            )
        );
        // Asked of Argonauts Code, it's Argonauts Code's on any CLI, and there's nothing to
        // explain.
        assert_eq!(
            keeper(&codex, RoleGoal::Argonauts),
            (KeptBy::Argonauts, None)
        );
    }
}
